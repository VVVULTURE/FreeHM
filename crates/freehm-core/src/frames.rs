//! Frame-rate and frame-time statistics from present timestamps.
//!
//! Definitions (identical to PresentMon's "MsBetweenPresents"-based metrics):
//! - **Frame time** is the interval between two successive Present() calls made by the
//!   application. FreeHM reports the mean of those intervals over the trailing
//!   [`FRAMETIME_WINDOW_NS`] (at least two intervals), so it reacts within a quarter second.
//! - **FPS** is the number of present intervals in the trailing [`FPS_WINDOW_NS`] divided by
//!   their span — i.e. the reciprocal of the mean frame time over one second.
//!
//! - **1 % / 0.1 % low** is the average of the slowest 1 % / 0.1 % of the frames in the
//!   trailing [`LOWS_WINDOW_NS`], shown as FPS: the frames are ordered by frame time, the
//!   slowest ones averaged, and 1000 ms divided by that average. It needs at least
//!   [`LOW_1_MIN_FRAMES`] / [`LOW_01_MIN_FRAMES`] frames; with fewer there is no such thing
//!   as "the slowest 1 %", so nothing is reported rather than a misleading number.
//!
//! The windows end at the newest present received, not at "now": real-time ETW hands
//! events over in buffers, up to [`DELIVERY_LAG_NS`] late. A process whose newest present
//! is older than that has stopped presenting and gets no statistics.
//!
//! The timestamps come from ETW event headers in the helper service; nothing is read from
//! the game process.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

pub const FPS_WINDOW_NS: u64 = 1_000_000_000;
pub const FRAMETIME_WINDOW_NS: u64 = 250_000_000;
/// Rolling window of the 1 % and 0.1 % lows.
pub const LOWS_WINDOW_NS: u64 = 60_000_000_000;
/// Frames needed in that window: 1 % of 100 and 0.1 % of 1000 are one frame.
pub const LOW_1_MIN_FRAMES: u32 = 100;
pub const LOW_01_MIN_FRAMES: u32 = 1000;
/// The lows sort up to [`MAX_PER_RING`] frame times and change slowly; once a second is enough.
const LOWS_PERIOD_NS: u64 = 1_000_000_000;
/// Longest expected delay between a present and its event reaching the helper. The helper
/// flushes its ETW session on every request and the session's flush timer is one second,
/// so delivery is normally well under a second; the margin keeps FPS from flickering to
/// N/A when a flush is late.
pub const DELIVERY_LAG_NS: u64 = 2_000_000_000;
/// Presents older than this are discarded (lows window + delivery lag + margin).
const KEEP_NS: u64 = LOWS_WINDOW_NS + DELIVERY_LAG_NS + 2_000_000_000;
/// A process with no presents for this long is forgotten.
const FORGET_NS: u64 = 10_000_000_000;
/// 60 s at 1090 FPS. Above that the lows cover the newest 65 536 frames.
const MAX_PER_RING: usize = 65_536;
const MAX_PROCESSES: usize = 128;
/// All rings together (2 MB). The least recently active processes go first.
const MAX_TOTAL_PRESENTS: usize = 1 << 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PresentSource {
    /// Microsoft-Windows-DXGI Present::Start (D3D10/11/12, and Vulkan/GL layered on DXGI).
    Dxgi,
    /// Microsoft-Windows-D3D9 Present::Start.
    D3d9,
    /// Microsoft-Windows-DxgKrnl Present::Info — kernel-side presents from any runtime
    /// (used for OpenGL/Vulkan paths that don't go through DXGI/D3D9).
    Kernel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameStats {
    pub fps: f64,
    pub frametime_ms: f64,
    /// Present intervals that contributed to `fps`.
    pub samples: u32,
    pub last_present_age_ms: u64,
    /// `None` until the window holds [`LOW_1_MIN_FRAMES`] frames.
    #[serde(default)]
    pub low_1_fps: Option<f64>,
    /// `None` until the window holds [`LOW_01_MIN_FRAMES`] frames.
    #[serde(default)]
    pub low_01_fps: Option<f64>,
    /// Frames in the lows window. Zero from a helper too old to report lows.
    #[serde(default)]
    pub low_frames: u32,
}

/// 1 % and 0.1 % lows, see the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Lows {
    pub low_1_fps: Option<f64>,
    pub low_01_fps: Option<f64>,
    pub frames: u32,
}

/// Average of the `slowest` largest frame times (ns) as FPS. Reorders `frame_ns` so that
/// they come first, so a smaller share can be taken from the front afterwards.
fn average_of_slowest(frame_ns: &mut [u64], slowest: usize) -> f64 {
    frame_ns.select_nth_unstable_by(slowest - 1, |a, b| b.cmp(a));
    let sum: u64 = frame_ns[..slowest].iter().sum();
    slowest as f64 * 1e9 / sum.max(1) as f64
}

/// Lows of the frames in the [`LOWS_WINDOW_NS`] ending at `end`. `scratch` is reused between
/// calls so a once-a-second recalculation allocates nothing.
pub fn compute_lows(ts: &VecDeque<u64>, end: u64, scratch: &mut Vec<u64>) -> Lows {
    let first = ts.partition_point(|&t| t < end.saturating_sub(LOWS_WINDOW_NS));
    let last = ts.partition_point(|&t| t <= end);
    scratch.clear();
    if last > first {
        scratch.extend(ts.range(first..last).zip(ts.range(first + 1..last)).map(|(a, b)| b - a));
    }
    let frames = scratch.len() as u32;
    let share = |percent: f64| ((scratch.len() as f64 * percent / 100.0).ceil() as usize).max(1);
    let (one, tenth) = (share(1.0), share(0.1));
    let mut lows = Lows { frames, ..Default::default() };
    if frames >= LOW_1_MIN_FRAMES {
        lows.low_1_fps = Some(average_of_slowest(scratch, one));
    }
    if frames >= LOW_01_MIN_FRAMES {
        // The slowest 0.1 % are among the slowest 1 % moved to the front above.
        lows.low_01_fps = Some(average_of_slowest(&mut scratch[..one], tenth));
    }
    lows
}

/// Compute statistics from ascending timestamps (ns) at time `now`.
/// Returns `None` when the newest present is older than [`DELIVERY_LAG_NS`] or fewer than
/// two presents fall in the FPS window.
pub fn compute(ts: &VecDeque<u64>, now: u64) -> Option<FrameStats> {
    let last = ts.partition_point(|&t| t <= now);
    let end = *ts.get(last.checked_sub(1)?)?;
    if now - end > DELIVERY_LAG_NS {
        return None;
    }
    // The ring holds a minute of presents; these windows are its last second, found by
    // bisection instead of walking it on every poll.
    let first = ts.partition_point(|&t| t < end.saturating_sub(FPS_WINDOW_NS));
    let count = last - first;
    if count < 2 {
        return None;
    }
    let span = (end - ts[first]) as f64;
    if span <= 0.0 {
        return None;
    }
    let intervals = (count - 1) as f64;
    let fps = intervals * 1e9 / span;

    let ft_start = end.saturating_sub(FRAMETIME_WINDOW_NS);
    // Need at least two intervals; extend backwards into the FPS window if necessary.
    let first_ft = ts.partition_point(|&t| t < ft_start).clamp(first, last - 1).min(first + count.saturating_sub(3));
    let frametime_ms = (end - ts[first_ft]) as f64 / 1e6 / (last - 1 - first_ft) as f64;

    Some(FrameStats {
        fps,
        frametime_ms,
        samples: intervals as u32,
        last_present_age_ms: (now - end) / 1_000_000,
        low_1_fps: None,
        low_01_fps: None,
        low_frames: 0,
    })
}

#[derive(Default)]
struct ProcessRings {
    rings: HashMap<PresentSource, VecDeque<u64>>,
    last_seen: u64,
    /// Last lows calculated: when, and for which source.
    lows: Option<(u64, PresentSource, Lows)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessFrames {
    pub pid: u32,
    pub source: PresentSource,
    pub stats: FrameStats,
}

/// Per-process present history with bounded memory.
#[derive(Default)]
pub struct PresentTracker {
    procs: HashMap<u32, ProcessRings>,
    scratch: Vec<u64>,
}

impl PresentTracker {
    pub fn record(&mut self, pid: u32, source: PresentSource, ts: u64) {
        if !self.procs.contains_key(&pid) && self.procs.len() >= MAX_PROCESSES {
            // Evict the least recently active process.
            if let Some((&old, _)) = self.procs.iter().min_by_key(|(_, p)| p.last_seen) {
                self.procs.remove(&old);
            }
        }
        let p = self.procs.entry(pid).or_default();
        p.last_seen = p.last_seen.max(ts);
        let ring = p.rings.entry(source).or_default();
        // ETW buffers are flushed per CPU, so events can arrive slightly out of order.
        if ring.back().is_some_and(|&b| ts < b) {
            let pos = ring.partition_point(|&x| x <= ts);
            ring.insert(pos, ts);
        } else {
            ring.push_back(ts);
        }
        if ring.len() > MAX_PER_RING {
            ring.pop_front();
        }
    }

    /// Drop old data. Call periodically (e.g. on every query).
    pub fn prune(&mut self, now: u64) {
        let keep_from = now.saturating_sub(KEEP_NS);
        self.procs.retain(|_, p| {
            for r in p.rings.values_mut() {
                while r.front().is_some_and(|&t| t < keep_from) {
                    r.pop_front();
                }
            }
            now.saturating_sub(p.last_seen) < FORGET_NS
        });
        let mut total: usize = self.procs.values().flat_map(|p| p.rings.values()).map(VecDeque::len).sum();
        while total > MAX_TOTAL_PRESENTS {
            let Some(&pid) = self.procs.iter().min_by_key(|(_, p)| p.last_seen).map(|(pid, _)| pid) else { break };
            if let Some(p) = self.procs.remove(&pid) {
                total -= p.rings.values().map(VecDeque::len).sum::<usize>();
            }
        }
    }

    /// Statistics for every process that presented in the last second, highest FPS first.
    /// API-level events (DXGI/D3D9) are preferred; kernel presents are used only for
    /// processes with no API-level events, which avoids counting one frame twice.
    pub fn snapshot(&mut self, now: u64) -> Vec<ProcessFrames> {
        self.prune(now);
        let mut out = Vec::new();
        for (&pid, p) in &mut self.procs {
            let pick = [PresentSource::Dxgi, PresentSource::D3d9]
                .into_iter()
                .filter_map(|s| p.rings.get(&s).and_then(|r| compute(r, now)).map(|st| (s, st)))
                .max_by_key(|(_, st)| st.samples)
                .or_else(|| p.rings.get(&PresentSource::Kernel).and_then(|r| compute(r, now)).map(|st| (PresentSource::Kernel, st)));
            if let Some((source, mut stats)) = pick {
                let lows = match p.lows {
                    Some((at, s, l)) if s == source && now.saturating_sub(at) < LOWS_PERIOD_NS => l,
                    _ => {
                        let end = now.saturating_sub(stats.last_present_age_ms * 1_000_000);
                        let l = compute_lows(&p.rings[&source], end, &mut self.scratch);
                        p.lows = Some((now, source, l));
                        l
                    }
                };
                stats.low_1_fps = lows.low_1_fps;
                stats.low_01_fps = lows.low_01_fps;
                stats.low_frames = lows.frames;
                out.push(ProcessFrames { pid, source, stats });
            }
        }
        out.sort_by(|a, b| b.stats.fps.total_cmp(&a.stats.fps));
        out
    }

    pub fn last_seen(&self, pid: u32) -> Option<u64> {
        self.procs.get(&pid).map(|p| p.last_seen)
    }

    pub fn process_count(&self) -> usize {
        self.procs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn ring(period_ns: u64, count: usize, start: u64) -> VecDeque<u64> {
        (0..count).map(|i| start + i as u64 * period_ns).collect()
    }

    #[test]
    fn steady_144_fps() {
        let period = 1_000_000_000 / 144;
        let r = ring(period, 400, 0);
        let now = *r.back().unwrap();
        let s = compute(&r, now).unwrap();
        assert!((s.fps - 144.0).abs() < 0.5, "{}", s.fps);
        assert!((s.frametime_ms - 6.944).abs() < 0.01, "{}", s.frametime_ms);
        assert_eq!(s.last_present_age_ms, 0);
    }

    #[test]
    fn frametime_reacts_to_recent_change_faster_than_fps() {
        // 1 s at 10 ms frames followed by 250 ms at 50 ms frames.
        let mut r = ring(10 * MS, 101, 0);
        let mut t = *r.back().unwrap();
        for _ in 0..5 {
            t += 50 * MS;
            r.push_back(t);
        }
        let s = compute(&r, t).unwrap();
        assert!((s.frametime_ms - 50.0).abs() < 1.0, "frametime {}", s.frametime_ms);
        assert!(s.fps > 20.0 && s.fps < 100.0, "fps {}", s.fps);
    }

    /// `frames` frames of `slow_ms` milliseconds spread evenly through `count` frames that
    /// otherwise take `normal_ms`.
    fn with_slow_frames(count: usize, normal_ms: u64, frames: usize, slow_ms: u64) -> VecDeque<u64> {
        let mut r = VecDeque::from([0u64]);
        let mut t = 0;
        for i in 0..count {
            t += if i * frames / count != (i + 1) * frames / count { slow_ms } else { normal_ms } * MS;
            r.push_back(t);
        }
        r
    }

    #[test]
    fn lows_are_the_average_of_the_slowest_frames_as_fps() {
        // 5000 frames: 50 (exactly 1 %) take 40 ms, the rest 10 ms.
        let r = with_slow_frames(5000, 10, 50, 40);
        let l = compute_lows(&r, *r.back().unwrap(), &mut Vec::new());
        assert_eq!(l.frames, 5000);
        assert!((l.low_1_fps.unwrap() - 25.0).abs() < 1e-9, "{l:?}");
        assert!((l.low_01_fps.unwrap() - 25.0).abs() < 1e-9, "the slowest 5 are all 40 ms: {l:?}");

        // Worst five frames differ: the 0.1 % low averages only those, the 1 % low all 50.
        let mut r = VecDeque::from([0u64]);
        let mut t = 0;
        for i in 0..5000 {
            t += match i {
                0..5 => 100,
                5..50 => 20,
                _ => 10,
            } * MS;
            r.push_back(t);
        }
        let l = compute_lows(&r, t, &mut Vec::new());
        assert!((l.low_01_fps.unwrap() - 10.0).abs() < 1e-9, "{l:?}");
        let mean_ms = (5.0 * 100.0 + 45.0 * 20.0) / 50.0;
        assert!((l.low_1_fps.unwrap() - 1000.0 / mean_ms).abs() < 1e-9, "{l:?}");
    }

    #[test]
    fn lows_match_a_full_sort() {
        // select_nth is an optimisation; the result must equal the plain definition.
        let mut r = VecDeque::from([0u64]);
        let (mut t, mut x) = (0u64, 12345u64);
        let mut frames = Vec::new();
        for _ in 0..7777 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let ft = 1_000_000 + (x >> 40) % 6_000_000; // 31 s in all: inside the window
            t += ft;
            frames.push(ft);
            r.push_back(t);
        }
        frames.sort_unstable_by(|a, b| b.cmp(a));
        let avg = |n: usize| n as f64 * 1e9 / frames[..n].iter().sum::<u64>() as f64;
        let l = compute_lows(&r, t, &mut Vec::new());
        assert!((l.low_1_fps.unwrap() - avg(78)).abs() < 1e-9, "ceil(77.77) frames: {l:?}");
        assert!((l.low_01_fps.unwrap() - avg(8)).abs() < 1e-9, "ceil(7.777) frames: {l:?}");
    }

    #[test]
    fn lows_wait_for_enough_frames() {
        let l = compute_lows(&ring(10 * MS, 100, 0), 99 * 10 * MS, &mut Vec::new());
        assert_eq!((l.frames, l.low_1_fps, l.low_01_fps), (99, None, None), "99 frames are not enough for a 1 % low");
        let l = compute_lows(&ring(10 * MS, 101, 0), 100 * 10 * MS, &mut Vec::new());
        assert!(l.low_1_fps.is_some() && l.low_01_fps.is_none(), "{l:?}");
        let l = compute_lows(&ring(10 * MS, 1001, 0), 1000 * 10 * MS, &mut Vec::new());
        assert!(l.low_1_fps.is_some() && l.low_01_fps.is_some(), "{l:?}");
        assert_eq!(compute_lows(&VecDeque::new(), 0, &mut Vec::new()).frames, 0);
        assert_eq!(compute_lows(&VecDeque::from([5]), 5, &mut Vec::new()).frames, 0);
    }

    #[test]
    fn lows_forget_frames_older_than_the_window() {
        // A 500 ms hitch, then 70 s of steady 10 ms frames.
        let mut r = VecDeque::from([0u64, 500 * MS]);
        let mut t = 500 * MS;
        for _ in 0..7000 {
            t += 10 * MS;
            r.push_back(t);
        }
        let l = compute_lows(&r, t, &mut Vec::new());
        assert_eq!(l.frames, 6000, "only the last 60 s count");
        assert!((l.low_1_fps.unwrap() - 100.0).abs() < 1e-9, "{l:?}");
        // While the hitch is inside the window it dominates the 0.1 % low.
        let in_window = 30_000 * MS;
        let l = compute_lows(&r, in_window, &mut Vec::new());
        assert!(l.low_01_fps.unwrap() < 30.0, "{l:?}");
    }

    #[test]
    fn tracker_reports_lows_and_recalculates_once_a_second() {
        let mut t = PresentTracker::default();
        let mut now = 0;
        for i in 0..3000u64 {
            now = (i + 1) * 10 * MS;
            t.record(7, PresentSource::Dxgi, now);
        }
        let s = t.snapshot(now).remove(0).stats;
        assert_eq!(s.low_frames, 2999);
        assert!((s.low_1_fps.unwrap() - 100.0).abs() < 1e-9, "{s:?}");
        // Slow frames arrive; within the second the cached value stands, then it updates.
        for i in 0..50u64 {
            now += 100 * MS;
            t.record(7, PresentSource::Dxgi, now);
            if i == 3 {
                assert_eq!(t.snapshot(now).remove(0).stats.low_frames, 2999, "cached");
            }
        }
        now += 1_000 * MS;
        t.record(7, PresentSource::Dxgi, now);
        let s = t.snapshot(now).remove(0).stats;
        assert_eq!(s.low_frames, 3050);
        assert!(s.low_1_fps.unwrap() < 12.0, "{s:?}");
    }

    #[test]
    fn total_presents_stay_bounded() {
        let mut t = PresentTracker::default();
        // Twenty processes each filling a ring within the keep window.
        let now = 50_000 * MS;
        for pid in 0..20u32 {
            for i in 0..30_000u64 {
                t.record(pid, PresentSource::Dxgi, now - 30_000 * 1000 + i * 1000 + pid as u64);
            }
        }
        t.prune(now);
        let total: usize = t.procs.values().flat_map(|p| p.rings.values()).map(VecDeque::len).sum();
        assert!(total <= MAX_TOTAL_PRESENTS, "{total}");
        assert!(t.process_count() >= 8, "evicts only what it must: {}", t.process_count());
        assert!(t.last_seen(19).is_some(), "the most recently active process is kept");
    }

    #[test]
    fn too_few_presents_is_none() {
        let r: VecDeque<u64> = [5 * MS].into_iter().collect();
        assert!(compute(&r, 6 * MS).is_none());
        assert!(compute(&VecDeque::new(), 0).is_none());
        // Presents older than the FPS window don't count.
        let old = ring(10 * MS, 10, 0);
        assert!(compute(&old, 5_000 * MS).is_none());
    }

    #[test]
    fn late_delivered_events_still_count() {
        // 120 FPS, but the newest event received is 600 ms old (ETW buffer delay).
        let r = ring(1_000_000_000 / 120, 240, 0);
        let last = *r.back().unwrap();
        let s = compute(&r, last + 600 * MS).unwrap();
        assert!((s.fps - 120.0).abs() < 0.5, "{}", s.fps);
        assert_eq!(s.last_present_age_ms, 600);
        // The tracker keeps enough history for a late delivery near the limit.
        let mut t = PresentTracker::default();
        r.iter().for_each(|&ts| t.record(9, PresentSource::Dxgi, ts));
        let late = t.snapshot(last + 1_800 * MS);
        assert!((late[0].stats.fps - 120.0).abs() < 0.5);
        // Past the delivery lag the process has stopped presenting.
        assert!(compute(&r, last + DELIVERY_LAG_NS + MS).is_none());
    }

    #[test]
    fn timestamps_after_now_are_ignored() {
        let r = ring(10 * MS, 200, 0);
        let s = compute(&r, 1_000 * MS).unwrap();
        assert!((s.fps - 100.0).abs() < 0.5);
        assert_eq!(s.last_present_age_ms, 0);
        let future: VecDeque<u64> = [5_000 * MS, 5_010 * MS].into_iter().collect();
        assert!(compute(&future, 1_000 * MS).is_none());
    }

    #[test]
    fn slow_frames_extend_frametime_window() {
        // 4 FPS: only one present inside 250 ms, so the window must extend.
        let r = ring(250 * MS, 5, 0);
        let s = compute(&r, 1000 * MS).unwrap();
        assert!((s.frametime_ms - 250.0).abs() < 0.01);
        assert!((s.fps - 4.0).abs() < 0.01);
    }

    #[test]
    fn tracker_prefers_api_events_over_kernel() {
        let mut t = PresentTracker::default();
        for i in 0..120u64 {
            t.record(42, PresentSource::Dxgi, i * 10 * MS);
            t.record(42, PresentSource::Kernel, i * 10 * MS + 1000);
        }
        let s = t.snapshot(1190 * MS);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].source, PresentSource::Dxgi);
        assert!((s[0].stats.fps - 100.0).abs() < 0.5);
    }

    #[test]
    fn tracker_uses_kernel_for_vulkan_gl_style_processes() {
        let mut t = PresentTracker::default();
        for i in 0..70u64 {
            t.record(7, PresentSource::Kernel, i * 16_666_667);
        }
        let s = t.snapshot(69 * 16_666_667);
        assert_eq!(s[0].source, PresentSource::Kernel);
        assert!((s[0].stats.fps - 60.0).abs() < 0.5);
    }

    #[test]
    fn out_of_order_events_are_sorted() {
        let mut t = PresentTracker::default();
        for &ms in &[0u64, 20, 10, 30, 50, 40] {
            t.record(1, PresentSource::Dxgi, ms * MS);
        }
        let s = t.snapshot(50 * MS);
        assert!((s[0].stats.frametime_ms - 10.0).abs() < 0.01);
    }

    #[test]
    fn idle_processes_are_forgotten_and_memory_bounded() {
        let mut t = PresentTracker::default();
        for pid in 0..(MAX_PROCESSES as u32 + 20) {
            t.record(pid, PresentSource::Dxgi, pid as u64 * MS);
        }
        assert!(t.process_count() <= MAX_PROCESSES);
        for i in 0..(MAX_PER_RING as u64 + 100) {
            t.record(1, PresentSource::Dxgi, 1_000 * MS + i);
        }
        t.prune(1_000 * MS + 50_000);
        assert!(t.snapshot(60_000 * MS).is_empty());
        assert_eq!(t.process_count(), 0);
    }
}
