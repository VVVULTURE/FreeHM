//! Provider-based telemetry engine.
//!
//! Providers wrap one data source each (PDH, NVML, sysfs, the helper service, ...). The
//! engine owns them on a dedicated thread, polls only what is currently needed, keeps the
//! last good value per (device, metric), and classifies each value as fresh / cached /
//! stale / unavailable. Failing providers are re-initialised with exponential backoff and
//! only state *changes* are logged, so a missing sensor does not flood the log.

use crate::metrics::{DeviceInfo, Freshness, MetricId, ProviderStatus, Reading, Snapshot};
use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// Bit set of metrics (cheap to copy across threads).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MetricSet(u32);

impl MetricSet {
    pub fn all() -> MetricSet {
        MetricSet(MetricId::ALL.iter().fold(0, |a, &m| a | (1 << m as u32)))
    }
    pub fn insert(&mut self, m: MetricId) {
        self.0 |= 1 << m as u32;
    }
    pub fn contains(&self, m: MetricId) -> bool {
        self.0 & (1 << m as u32) != 0
    }
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
    pub fn any_of(&self, ms: &[MetricId]) -> bool {
        ms.iter().any(|&m| self.contains(m))
    }
}

impl FromIterator<MetricId> for MetricSet {
    fn from_iter<I: IntoIterator<Item = MetricId>>(it: I) -> Self {
        let mut s = MetricSet::default();
        for m in it {
            s.insert(m);
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SampleValue {
    Value(f64),
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub device: String,
    pub metric: MetricId,
    pub value: SampleValue,
}

impl Sample {
    pub fn value(device: &str, metric: MetricId, v: f64) -> Sample {
        Sample { device: device.to_string(), metric, value: SampleValue::Value(v) }
    }
    pub fn unavailable(device: &str, metric: MetricId, reason: impl Into<String>) -> Sample {
        Sample { device: device.to_string(), metric, value: SampleValue::Unavailable(reason.into()) }
    }
}

/// What the consumer currently needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PollRequest {
    pub metrics: MetricSet,
    /// Processes of the active game (root first) for FPS/frame-time providers.
    pub game_pids: Vec<u32>,
}

pub trait Provider: Send {
    fn name(&self) -> &'static str;
    /// Open the data source and enumerate devices. Called once, and again after failures.
    fn init(&mut self) -> Result<Vec<DeviceInfo>, String>;
    /// Read current values for the requested metrics. Must not block for long; a provider
    /// with a slow source should read it on its own thread and return the latest value.
    /// An `Err` marks all of this provider's values as failing and schedules re-init after
    /// repeated failures.
    fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String>;
    /// Sources that update slowly declare it here; polls in between reuse cached values.
    fn min_interval(&self) -> Duration {
        Duration::ZERO
    }
    /// Called when the system resumes from sleep or the display configuration changes.
    fn on_resume(&mut self) {}
    /// Called before each `poll` with the (device, metric) pairs that higher-priority
    /// providers have already read fresh in this cycle, so an expensive read whose result
    /// would be discarded can be skipped.
    fn set_covered(&mut self, _covered: &[(String, MetricId)]) {}
}

enum SlotState {
    Uninit,
    Ready,
    Failed { reason: String, retry_at: Instant, backoff: Duration },
}

struct Slot {
    provider: Box<dyn Provider>,
    state: SlotState,
    devices: Vec<DeviceInfo>,
    last_read: Option<Instant>,
    failures: u32,
    last_error: Option<String>,
}

struct Entry {
    value: Option<f64>,
    obtained: Option<Instant>,
    reason: Option<String>,
    provider: usize,
    /// Set when this cycle produced a new value.
    fresh: bool,
}

const MAX_BACKOFF: Duration = Duration::from_secs(30);
const FAILS_BEFORE_REINIT: u32 = 3;
/// Values with no update for this long become unavailable instead of stale.
const EXPIRE: Duration = Duration::from_secs(30);

pub struct Engine {
    slots: Vec<Slot>,
    entries: HashMap<(String, MetricId), Entry>,
    seq: u64,
    scratch: Vec<Sample>,
    covered: Vec<(String, MetricId)>,
    /// `FREEHM_PROFILE=1`: per-provider poll time, logged every 50 polls.
    profile: Option<(Vec<Duration>, u32)>,
}

impl Engine {
    /// Providers earlier in the list take precedence when two supply the same metric.
    pub fn new(providers: Vec<Box<dyn Provider>>) -> Engine {
        let slots = providers
            .into_iter()
            .map(|p| Slot { provider: p, state: SlotState::Uninit, devices: vec![], last_read: None, failures: 0, last_error: None })
            .collect();
        let profile = std::env::var_os("FREEHM_PROFILE").map(|_| (Vec::new(), 0));
        Engine { slots, entries: HashMap::new(), seq: 0, scratch: Vec::new(), covered: Vec::new(), profile }
    }

    pub fn on_resume(&mut self, now: Instant) {
        for s in &mut self.slots {
            s.provider.on_resume();
            // Retry failed providers right away: a driver reset or resume often fixes them.
            if let SlotState::Failed { retry_at, .. } = &mut s.state {
                *retry_at = now;
            }
        }
    }

    pub fn poll(&mut self, now: Instant, req: &PollRequest, interval: Duration) -> Snapshot {
        for e in self.entries.values_mut() {
            e.fresh = false;
        }
        let mut cached_providers = Vec::new();
        for idx in 0..self.slots.len() {
            let slot = &mut self.slots[idx];
            let needs_init = match &slot.state {
                SlotState::Uninit => true,
                SlotState::Failed { retry_at, .. } => now >= *retry_at,
                SlotState::Ready => false,
            };
            if needs_init {
                match slot.provider.init() {
                    Ok(devs) => {
                        if !matches!(slot.state, SlotState::Uninit) {
                            log::info!("provider {} recovered", slot.provider.name());
                        }
                        slot.devices = devs;
                        slot.state = SlotState::Ready;
                        slot.failures = 0;
                        slot.last_read = None;
                        slot.last_error = None;
                    }
                    Err(reason) => {
                        let backoff = match &slot.state {
                            SlotState::Failed { backoff, .. } => (*backoff * 2).min(MAX_BACKOFF),
                            _ => Duration::from_secs(1),
                        };
                        if slot.last_error.as_deref() != Some(reason.as_str()) {
                            log::warn!("provider {} unavailable: {reason}", slot.provider.name());
                        }
                        slot.last_error = Some(reason.clone());
                        slot.state = SlotState::Failed { reason, retry_at: now + backoff, backoff };
                        continue;
                    }
                }
            }
            if !matches!(slot.state, SlotState::Ready) {
                continue;
            }
            let min = slot.provider.min_interval();
            if let Some(last) = slot.last_read
                && now.duration_since(last) < min
            {
                cached_providers.push(idx);
                continue;
            }
            self.scratch.clear();
            if idx > 0 {
                self.covered.clear();
                self.covered.extend(self.entries.iter().filter(|(_, e)| e.fresh && e.value.is_some() && e.provider < idx).map(|(k, _)| k.clone()));
                slot.provider.set_covered(&self.covered);
            }
            let started = Instant::now();
            let result = slot.provider.poll(req, &mut self.scratch);
            if let Some((times, _)) = &mut self.profile {
                times.resize(self.slots.len(), Duration::ZERO);
                times[idx] += started.elapsed();
            }
            let slot = &mut self.slots[idx];
            match result {
                Ok(()) => {
                    slot.last_read = Some(now);
                    if slot.failures > 0 {
                        log::info!("provider {} reading again", slot.provider.name());
                    }
                    slot.failures = 0;
                    slot.last_error = None;
                    for s in self.scratch.drain(..) {
                        apply_sample(&mut self.entries, idx, s, now);
                    }
                }
                Err(e) => {
                    slot.failures += 1;
                    if slot.failures == 1 {
                        log::warn!("provider {} read failed: {e}", slot.provider.name());
                    }
                    slot.last_error = Some(e.clone());
                    if slot.failures >= FAILS_BEFORE_REINIT {
                        slot.state = SlotState::Failed { reason: e, retry_at: now + Duration::from_secs(1), backoff: Duration::from_secs(1) };
                    }
                }
            }
        }

        self.seq += 1;
        if let Some((times, n)) = &mut self.profile {
            *n += 1;
            if *n == 50 {
                let parts: Vec<String> =
                    times.iter().enumerate().map(|(i, t)| format!("{} {:.2} ms", self.slots[i].provider.name(), t.as_secs_f64() * 1000.0 / 50.0)).collect();
                log::info!("profile: mean poll time per provider: {}", parts.join(", "));
                times.iter_mut().for_each(|t| *t = Duration::ZERO);
                *n = 0;
            }
        }
        self.build_snapshot(now, interval, &cached_providers)
    }

    fn build_snapshot(&self, now: Instant, interval: Duration, cached_providers: &[usize]) -> Snapshot {
        let mut devices: Vec<DeviceInfo> = Vec::new();
        for s in &self.slots {
            for d in &s.devices {
                if !devices.iter().any(|x| x.id == d.id) {
                    devices.push(d.clone());
                }
            }
        }
        let mut readings: Vec<Reading> = Vec::with_capacity(self.entries.len());
        for ((device, metric), e) in &self.entries {
            let slot = &self.slots[e.provider];
            let source = slot.provider.name().to_string();
            let stale_after = (interval * 3).max(slot.provider.min_interval() * 2).max(Duration::from_millis(1000));
            let reading = match (e.value, e.obtained) {
                (Some(v), Some(t)) => {
                    let age = now.duration_since(t);
                    let provider_error = slot.last_error.clone();
                    if age > EXPIRE {
                        Reading::unavailable(
                            *metric,
                            device,
                            &source,
                            format!("no update for {} s{}", age.as_secs(), provider_error.map(|r| format!(": {r}")).unwrap_or_default()),
                        )
                    } else {
                        let freshness = if e.fresh {
                            Freshness::Fresh
                        } else if age > stale_after {
                            Freshness::Stale
                        } else {
                            debug_assert!(cached_providers.contains(&e.provider) || age <= stale_after);
                            Freshness::Cached
                        };
                        Reading {
                            metric: *metric,
                            device: device.clone(),
                            value: Some(v),
                            unit: metric.unit(),
                            freshness,
                            age_ms: Some(age.as_millis() as u64),
                            reason: if freshness == Freshness::Stale { provider_error.or(Some("sensor not updating".into())) } else { None },
                            source,
                        }
                    }
                }
                _ => Reading::unavailable(*metric, device, &source, e.reason.clone().unwrap_or_else(|| "no data".into())),
            };
            readings.push(reading);
        }
        readings.sort_by(|a, b| a.device.cmp(&b.device).then(a.metric.cmp(&b.metric)));
        let provider_status = self
            .slots
            .iter()
            .map(|s| match &s.state {
                SlotState::Ready => ProviderStatus {
                    provider: s.provider.name().into(),
                    ok: s.last_error.is_none(),
                    message: s.last_error.clone().unwrap_or_else(|| format!("{} device(s)", s.devices.len())),
                },
                SlotState::Failed { reason, .. } => ProviderStatus { provider: s.provider.name().into(), ok: false, message: reason.clone() },
                SlotState::Uninit => ProviderStatus { provider: s.provider.name().into(), ok: false, message: "not started".into() },
            })
            .collect();
        Snapshot { devices, readings, provider_status, seq: self.seq }
    }
}

fn apply_sample(entries: &mut HashMap<(String, MetricId), Entry>, idx: usize, s: Sample, now: Instant) {
    let key = (s.device, s.metric);
    match s.value {
        SampleValue::Value(v) => {
            let (lo, hi) = s.metric.valid_range();
            if !v.is_finite() || v < lo || v > hi {
                // Treat implausible values as a sensor fault, never display them.
                let reason = format!("sensor reported an implausible value ({v})");
                let e = entries.entry(key).or_insert(Entry { value: None, obtained: None, reason: None, provider: idx, fresh: false });
                if e.provider >= idx || e.value.is_none() {
                    *e = Entry { value: None, obtained: None, reason: Some(reason), provider: idx, fresh: false };
                }
                return;
            }
            let v = if s.metric.unit() == crate::metrics::Unit::Percent { v.clamp(0.0, 100.0) } else { v };
            match entries.get_mut(&key) {
                // A higher-priority provider already has a current value: keep it.
                Some(e) if e.provider < idx && e.value.is_some() && e.fresh => {}
                Some(e) => *e = Entry { value: Some(v), obtained: Some(now), reason: None, provider: idx, fresh: true },
                None => {
                    entries.insert(key, Entry { value: Some(v), obtained: Some(now), reason: None, provider: idx, fresh: true });
                }
            }
        }
        SampleValue::Unavailable(reason) => match entries.get_mut(&key) {
            // Never let one provider's "unsupported" hide another provider's value.
            Some(e) if e.provider != idx && e.value.is_some() => {}
            Some(e) if e.provider < idx => {}
            Some(e) => *e = Entry { value: None, obtained: None, reason: Some(reason), provider: idx, fresh: false },
            None => {
                entries.insert(key, Entry { value: None, obtained: None, reason: Some(reason), provider: idx, fresh: false });
            }
        },
    }
}

/// Commands to the telemetry thread.
#[derive(Debug, Clone)]
pub enum Command {
    /// Poll `req` every `interval`. An empty metric set stops polling entirely.
    Configure {
        interval: Duration,
        req: PollRequest,
    },
    Resume,
    Quit,
}

pub struct TelemetryThread {
    tx: mpsc::Sender<Command>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TelemetryThread {
    /// `publish` is called on the telemetry thread with each new snapshot.
    pub fn spawn(providers: Vec<Box<dyn Provider>>, publish: Box<dyn Fn(Snapshot) + Send>) -> TelemetryThread {
        let (tx, rx) = mpsc::channel::<Command>();
        let handle = std::thread::Builder::new().name("telemetry".into()).spawn(move || run(providers, rx, publish)).expect("spawn telemetry thread");
        TelemetryThread { tx, handle: Some(handle) }
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

impl Drop for TelemetryThread {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run(providers: Vec<Box<dyn Provider>>, rx: mpsc::Receiver<Command>, publish: Box<dyn Fn(Snapshot) + Send>) {
    // Providers are initialised lazily on the first poll, on this thread, so slow driver
    // libraries never delay application start-up or the UI thread.
    let mut engine = Engine::new(providers);
    let mut interval = Duration::from_millis(crate::config::DEFAULT_REFRESH_MS as u64);
    let mut req = PollRequest::default();
    let mut next = Instant::now();
    loop {
        let active = !req.metrics.is_empty();
        let cmd = if active {
            let wait = next.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            // Idle: block until told to do something. No polling, no wake-ups.
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };
        match cmd {
            Some(Command::Quit) => return,
            Some(Command::Resume) => engine.on_resume(Instant::now()),
            Some(Command::Configure { interval: i, req: r }) => {
                let was_active = active;
                interval = i;
                req = r;
                // Poll immediately when becoming active so the overlay never shows old data.
                if !was_active || Instant::now() >= next {
                    next = Instant::now();
                }
                continue_or_poll(&mut engine, &req, interval, &mut next, &*publish);
                continue;
            }
            None => {}
        }
        if !req.metrics.is_empty() && Instant::now() >= next {
            continue_or_poll(&mut engine, &req, interval, &mut next, &*publish);
        }
    }
}

fn continue_or_poll(engine: &mut Engine, req: &PollRequest, interval: Duration, next: &mut Instant, publish: &dyn Fn(Snapshot)) {
    let now = Instant::now();
    if req.metrics.is_empty() || now < *next {
        return;
    }
    let snap = engine.poll(now, req, interval);
    publish(snap);
    // Schedule from the previous deadline to avoid drift, but never "catch up" in a burst
    // after a long stall (sleep/resume).
    *next += interval;
    if *next <= now {
        *next = now + interval;
    }
}

pub mod mock {
    //! Deterministic, scriptable provider for tests and the `--mock` demo mode.
    use super::*;
    use crate::metrics::{DEVICE_GAME, DEVICE_MEMORY, DeviceKind, Vendor};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub struct MockState {
        pub values: HashMap<(String, MetricId), SampleValue>,
        pub fail_init: u32,
        pub fail_poll: u32,
        pub polls: u32,
        pub inits: u32,
    }

    pub struct MockProvider {
        pub devices: Vec<DeviceInfo>,
        pub state: Arc<Mutex<MockState>>,
        pub min_interval: Duration,
        pub name: &'static str,
    }

    impl MockProvider {
        pub fn new(devices: Vec<DeviceInfo>) -> (MockProvider, Arc<Mutex<MockState>>) {
            let state = Arc::new(Mutex::new(MockState::default()));
            (MockProvider { devices, state: state.clone(), min_interval: Duration::ZERO, name: "mock" }, state)
        }

        /// A plausible gaming PC with one discrete GPU, for the demo mode. Values move over
        /// time so the overlay visibly updates; device names say "Mock" so nobody mistakes
        /// them for real readings.
        pub fn demo() -> (MockProvider, Arc<Mutex<MockState>>) {
            let dev =
                |id: &str, kind, name: &str, vendor| DeviceInfo { id: id.into(), kind, name: name.into(), vendor, integrated: false, provider: "mock".into() };
            MockProvider::new(vec![
                dev("cpu:mock", DeviceKind::Cpu, "Mock CPU (demo data)", Vendor::Other),
                dev("gpu:mock", DeviceKind::Gpu, "Mock GPU (demo data)", Vendor::Other),
                dev(DEVICE_MEMORY, DeviceKind::Memory, "System memory (demo data)", Vendor::Other),
            ])
        }
    }

    impl Provider for MockProvider {
        fn name(&self) -> &'static str {
            self.name
        }
        fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
            let mut s = self.state.lock().unwrap();
            s.inits += 1;
            if s.fail_init > 0 {
                s.fail_init -= 1;
                return Err("mock init failure".into());
            }
            Ok(self.devices.clone())
        }
        fn poll(&mut self, req: &PollRequest, out: &mut Vec<Sample>) -> Result<(), String> {
            let mut s = self.state.lock().unwrap();
            s.polls += 1;
            if s.fail_poll > 0 {
                s.fail_poll -= 1;
                return Err("mock read failure".into());
            }
            if s.values.is_empty() && self.devices.iter().any(|d| d.id == "gpu:mock") {
                // Demo mode: synthesise values from the poll counter.
                let t = s.polls as f64 * 0.2;
                let w = |base: f64, amp: f64, f: f64| base + amp * (t * f).sin();
                let gib = 1024.0f64.powi(3);
                let vals = [
                    ("cpu:mock", MetricId::CpuUsage, w(35.0, 15.0, 0.7)),
                    ("cpu:mock", MetricId::CpuTemp, w(62.0, 5.0, 0.3)),
                    ("cpu:mock", MetricId::CpuClock, w(4700.0, 150.0, 1.1)),
                    ("cpu:mock", MetricId::CpuPower, w(85.0, 20.0, 0.5)),
                    ("gpu:mock", MetricId::GpuUsage, w(92.0, 6.0, 0.9)),
                    ("gpu:mock", MetricId::GpuTemp, w(66.0, 3.0, 0.2)),
                    ("gpu:mock", MetricId::GpuClock, w(2800.0, 60.0, 1.3)),
                    ("gpu:mock", MetricId::GpuPower, w(230.0, 25.0, 0.6)),
                    ("gpu:mock", MetricId::VramUsed, w(7.5, 0.4, 0.1) * gib),
                    ("gpu:mock", MetricId::VramTotal, 12.0 * gib),
                    ("gpu:mock", MetricId::VramUsage, w(7.5, 0.4, 0.1) / 12.0 * 100.0),
                    ("gpu:mock", MetricId::GpuSharedMem, 0.3 * gib),
                    (DEVICE_MEMORY, MetricId::RamUsed, w(14.2, 0.3, 0.2) * gib),
                    (DEVICE_MEMORY, MetricId::RamTotal, 31.7 * gib),
                    (DEVICE_MEMORY, MetricId::RamUsage, w(14.2, 0.3, 0.2) / 31.7 * 100.0),
                    (DEVICE_GAME, MetricId::Fps, w(144.0, 12.0, 1.7)),
                    (DEVICE_GAME, MetricId::FrameTime, 1000.0 / w(144.0, 12.0, 1.7)),
                ];
                for (d, m, v) in vals {
                    if req.metrics.contains(m) {
                        out.push(Sample::value(d, m, v));
                    }
                }
                return Ok(());
            }
            for ((d, m), v) in &s.values {
                if req.metrics.contains(*m) {
                    out.push(Sample { device: d.clone(), metric: *m, value: v.clone() });
                }
            }
            Ok(())
        }
        fn min_interval(&self) -> Duration {
            self.min_interval
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::*;
    use super::*;
    use crate::metrics::{DeviceKind, Vendor};
    use std::sync::{Arc, Mutex};

    fn gpu(id: &str) -> DeviceInfo {
        DeviceInfo { id: id.into(), kind: DeviceKind::Gpu, name: id.into(), vendor: Vendor::Nvidia, integrated: false, provider: "mock".into() }
    }

    fn all() -> PollRequest {
        PollRequest { metrics: MetricSet::all(), game_pids: vec![] }
    }

    fn set(st: &Arc<Mutex<MockState>>, dev: &str, m: MetricId, v: SampleValue) {
        st.lock().unwrap().values.insert((dev.into(), m), v);
    }

    const MS: Duration = Duration::from_millis(1);
    const IV: Duration = Duration::from_millis(200);

    #[test]
    fn valid_reading_is_fresh() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        set(&st, "gpu:0", MetricId::GpuTemp, SampleValue::Value(65.0));
        let mut e = Engine::new(vec![Box::new(p)]);
        let s = e.poll(Instant::now(), &all(), IV);
        let r = s.get("gpu:0", MetricId::GpuTemp).unwrap();
        assert_eq!(r.value, Some(65.0));
        assert_eq!(r.freshness, Freshness::Fresh);
        assert_eq!(s.devices.len(), 1);
    }

    #[test]
    fn unsupported_metric_is_unavailable_with_reason() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        set(&st, "gpu:0", MetricId::GpuPower, SampleValue::Unavailable("not supported by this GPU".into()));
        let mut e = Engine::new(vec![Box::new(p)]);
        let r = e.poll(Instant::now(), &all(), IV).get("gpu:0", MetricId::GpuPower).cloned().unwrap();
        assert_eq!(r.value, None);
        assert_eq!(r.freshness, Freshness::Unavailable);
        assert_eq!(r.reason.as_deref(), Some("not supported by this GPU"));
    }

    #[test]
    fn missing_sensor_produces_no_reading() {
        let (p, _st) = MockProvider::new(vec![gpu("gpu:0")]);
        let mut e = Engine::new(vec![Box::new(p)]);
        assert!(e.poll(Instant::now(), &all(), IV).get("gpu:0", MetricId::GpuTemp).is_none());
    }

    #[test]
    fn values_go_stale_then_expire_when_reads_fail() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        set(&st, "gpu:0", MetricId::GpuTemp, SampleValue::Value(60.0));
        let mut e = Engine::new(vec![Box::new(p)]);
        let t0 = Instant::now();
        e.poll(t0, &all(), IV);
        st.lock().unwrap().fail_poll = 1000;
        let s = e.poll(t0 + 300 * MS, &all(), IV);
        assert_eq!(s.get("gpu:0", MetricId::GpuTemp).unwrap().freshness, Freshness::Cached);
        let s = e.poll(t0 + 1500 * MS, &all(), IV);
        let r = s.get("gpu:0", MetricId::GpuTemp).unwrap();
        assert_eq!(r.freshness, Freshness::Stale);
        assert_eq!(r.value, Some(60.0));
        assert!(r.reason.as_deref().unwrap().contains("mock read failure"));
        let s = e.poll(t0 + Duration::from_secs(40), &all(), IV);
        let r = s.get("gpu:0", MetricId::GpuTemp).unwrap();
        assert_eq!(r.freshness, Freshness::Unavailable);
        assert_eq!(r.value, None);
    }

    #[test]
    fn slow_provider_values_are_cached_between_reads() {
        let (mut p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        p.min_interval = Duration::from_millis(1000);
        set(&st, "gpu:0", MetricId::GpuTemp, SampleValue::Value(60.0));
        let mut e = Engine::new(vec![Box::new(p)]);
        let t0 = Instant::now();
        e.poll(t0, &all(), IV);
        let s = e.poll(t0 + 200 * MS, &all(), IV);
        assert_eq!(s.get("gpu:0", MetricId::GpuTemp).unwrap().freshness, Freshness::Cached);
        assert_eq!(st.lock().unwrap().polls, 1, "provider must not be read faster than its interval");
        e.poll(t0 + 1001 * MS, &all(), IV);
        assert_eq!(st.lock().unwrap().polls, 2);
    }

    #[test]
    fn init_failure_retries_with_backoff_and_recovers() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        st.lock().unwrap().fail_init = 2;
        set(&st, "gpu:0", MetricId::GpuUsage, SampleValue::Value(50.0));
        let mut e = Engine::new(vec![Box::new(p)]);
        let t0 = Instant::now();
        let s = e.poll(t0, &all(), IV);
        assert!(!s.provider_status[0].ok);
        assert!(s.get("gpu:0", MetricId::GpuUsage).is_none());
        e.poll(t0 + 500 * MS, &all(), IV); // before 1 s backoff: no retry
        assert_eq!(st.lock().unwrap().inits, 1);
        e.poll(t0 + 1000 * MS, &all(), IV); // retry 2 fails, backoff 2 s
        assert_eq!(st.lock().unwrap().inits, 2);
        e.poll(t0 + 2500 * MS, &all(), IV);
        assert_eq!(st.lock().unwrap().inits, 2);
        let s = e.poll(t0 + 3100 * MS, &all(), IV);
        assert_eq!(st.lock().unwrap().inits, 3);
        assert!(s.provider_status[0].ok);
        assert_eq!(s.get("gpu:0", MetricId::GpuUsage).unwrap().value, Some(50.0));
    }

    #[test]
    fn repeated_poll_failures_trigger_reinit_and_resume_retries_immediately() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        set(&st, "gpu:0", MetricId::GpuUsage, SampleValue::Value(1.0));
        let mut e = Engine::new(vec![Box::new(p)]);
        let t0 = Instant::now();
        e.poll(t0, &all(), IV);
        st.lock().unwrap().fail_poll = 3;
        for i in 1..=3 {
            e.poll(t0 + i * IV, &all(), IV);
        }
        st.lock().unwrap().fail_init = 1;
        e.poll(t0 + 2000 * MS, &all(), IV); // re-init fails -> backoff
        let inits = st.lock().unwrap().inits;
        e.on_resume(t0 + 2100 * MS);
        e.poll(t0 + 2100 * MS, &all(), IV);
        assert_eq!(st.lock().unwrap().inits, inits + 1);
    }

    #[test]
    fn multiple_gpus_are_reported_separately() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0"), gpu("gpu:1")]);
        set(&st, "gpu:0", MetricId::GpuTemp, SampleValue::Value(50.0));
        set(&st, "gpu:1", MetricId::GpuTemp, SampleValue::Value(70.0));
        let mut e = Engine::new(vec![Box::new(p)]);
        let s = e.poll(Instant::now(), &all(), IV);
        assert_eq!(s.devices.len(), 2);
        assert_eq!(s.get("gpu:1", MetricId::GpuTemp).unwrap().value, Some(70.0));
        assert_eq!(s.get("gpu:0", MetricId::GpuTemp).unwrap().value, Some(50.0));
    }

    #[test]
    fn implausible_values_are_rejected() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        set(&st, "gpu:0", MetricId::GpuTemp, SampleValue::Value(6553.5));
        set(&st, "gpu:0", MetricId::GpuUsage, SampleValue::Value(f64::NAN));
        set(&st, "gpu:0", MetricId::GpuClock, SampleValue::Value(-5.0));
        let mut e = Engine::new(vec![Box::new(p)]);
        let s = e.poll(Instant::now(), &all(), IV);
        for m in [MetricId::GpuTemp, MetricId::GpuUsage, MetricId::GpuClock] {
            let r = s.get("gpu:0", m).unwrap();
            assert_eq!(r.value, None, "{m:?}");
            assert!(r.reason.as_deref().unwrap().contains("implausible"));
        }
    }

    #[test]
    fn higher_priority_provider_wins_and_fallback_fills_gaps() {
        let (mut a, sa) = MockProvider::new(vec![gpu("cpu:0")]);
        a.name = "primary";
        let (mut b, sb) = MockProvider::new(vec![]);
        b.name = "fallback";
        set(&sa, "cpu:0", MetricId::CpuPower, SampleValue::Value(40.0));
        set(&sb, "cpu:0", MetricId::CpuPower, SampleValue::Value(99.0));
        set(&sa, "cpu:0", MetricId::CpuTemp, SampleValue::Unavailable("primary can't".into()));
        set(&sb, "cpu:0", MetricId::CpuTemp, SampleValue::Value(55.0));
        let mut e = Engine::new(vec![Box::new(a), Box::new(b)]);
        let s = e.poll(Instant::now(), &all(), IV);
        assert_eq!(s.get("cpu:0", MetricId::CpuPower).unwrap().value, Some(40.0));
        assert_eq!(s.get("cpu:0", MetricId::CpuPower).unwrap().source, "primary");
        assert_eq!(s.get("cpu:0", MetricId::CpuTemp).unwrap().value, Some(55.0));
        assert_eq!(s.get("cpu:0", MetricId::CpuTemp).unwrap().source, "fallback");
        // Later cycles keep the same assignment (the unavailable sample doesn't evict it).
        let s = e.poll(Instant::now() + IV, &all(), IV);
        assert_eq!(s.get("cpu:0", MetricId::CpuTemp).unwrap().value, Some(55.0));
    }

    #[test]
    fn lower_priority_provider_is_told_what_is_already_covered() {
        struct Spy(std::sync::Arc<std::sync::Mutex<Vec<(String, MetricId)>>>);
        impl Provider for Spy {
            fn name(&self) -> &'static str {
                "spy"
            }
            fn init(&mut self) -> Result<Vec<DeviceInfo>, String> {
                Ok(vec![])
            }
            fn poll(&mut self, _: &PollRequest, _: &mut Vec<Sample>) -> Result<(), String> {
                Ok(())
            }
            fn set_covered(&mut self, covered: &[(String, MetricId)]) {
                *self.0.lock().unwrap() = covered.to_vec();
            }
        }
        let (a, sa) = MockProvider::new(vec![gpu("gpu:0")]);
        set(&sa, "gpu:0", MetricId::GpuUsage, SampleValue::Value(40.0));
        set(&sa, "gpu:0", MetricId::GpuPower, SampleValue::Unavailable("no".into()));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut e = Engine::new(vec![Box::new(a), Box::new(Spy(seen.clone()))]);
        e.poll(Instant::now(), &all(), IV);
        let c = seen.lock().unwrap().clone();
        assert_eq!(c, vec![("gpu:0".to_string(), MetricId::GpuUsage)], "only fresh values from earlier providers count");
    }

    #[test]
    fn only_requested_metrics_are_read() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        set(&st, "gpu:0", MetricId::GpuTemp, SampleValue::Value(60.0));
        set(&st, "gpu:0", MetricId::GpuUsage, SampleValue::Value(10.0));
        let mut e = Engine::new(vec![Box::new(p)]);
        let req = PollRequest { metrics: [MetricId::GpuUsage].into_iter().collect(), game_pids: vec![] };
        let s = e.poll(Instant::now(), &req, IV);
        assert!(s.get("gpu:0", MetricId::GpuTemp).is_none());
        assert!(s.get("gpu:0", MetricId::GpuUsage).is_some());
    }

    #[test]
    fn thread_idles_when_nothing_requested_and_follows_interval_changes() {
        let (p, st) = MockProvider::new(vec![gpu("gpu:0")]);
        set(&st, "gpu:0", MetricId::GpuUsage, SampleValue::Value(10.0));
        let (tx, rx) = mpsc::channel();
        let t = TelemetryThread::spawn(
            vec![Box::new(p)],
            Box::new(move |s| {
                let _ = tx.send(s.seq);
            }),
        );
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(st.lock().unwrap().polls, 0, "no polling before configured");
        let req = PollRequest { metrics: MetricSet::all(), game_pids: vec![] };
        t.send(Command::Configure { interval: Duration::from_millis(50), req: req.clone() });
        std::thread::sleep(Duration::from_millis(520));
        let fast = st.lock().unwrap().polls;
        assert!((7..=13).contains(&fast), "~10 polls at 50 ms, got {fast}");
        t.send(Command::Configure { interval: Duration::from_millis(250), req });
        let before = st.lock().unwrap().polls;
        std::thread::sleep(Duration::from_millis(1010));
        let slow = st.lock().unwrap().polls - before;
        assert!((3..=6).contains(&slow), "~4-5 polls at 250 ms, got {slow}");
        t.send(Command::Configure { interval: Duration::from_millis(50), req: PollRequest::default() });
        std::thread::sleep(Duration::from_millis(60));
        let idle_start = st.lock().unwrap().polls;
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(st.lock().unwrap().polls, idle_start, "no polling when idle");
        drop(t);
        assert!(rx.try_iter().count() > 0);
    }

    #[test]
    fn metric_set_ops() {
        let s: MetricSet = [MetricId::Fps, MetricId::CpuTemp].into_iter().collect();
        assert!(s.contains(MetricId::Fps) && s.contains(MetricId::CpuTemp));
        assert!(!s.contains(MetricId::GpuTemp));
        assert!(s.any_of(&[MetricId::GpuTemp, MetricId::Fps]));
        assert!(MetricSet::default().is_empty());
        assert!(MetricId::ALL.iter().all(|&m| MetricSet::all().contains(m)));
    }
}
