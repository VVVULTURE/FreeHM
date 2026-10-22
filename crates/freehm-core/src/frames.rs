//! Frame-rate and frame-time statistics from present timestamps.
//!
//! Definitions (identical to PresentMon's "MsBetweenPresents"-based metrics):
//! - **Frame time** is the interval between two successive Present() calls made by the
//!   application. FreeHM reports the mean of those intervals over the trailing
//!   [`FRAMETIME_WINDOW_NS`] (at least two intervals), so it reacts within a quarter second.
//! - **FPS** is the number of present intervals in the trailing [`FPS_WINDOW_NS`] divided by
//!   their span — i.e. the reciprocal of the mean frame time over one second.
//!
//! Both windows end at the newest present received, not at "now": real-time ETW hands
//! events over in buffers, up to [`DELIVERY_LAG_NS`] late. A process whose newest present
//! is older than that has stopped presenting and gets no statistics.
//!
//! The timestamps come from ETW event headers in the helper service; nothing is read from
//! the game process.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

pub const FPS_WINDOW_NS: u64 = 1_000_000_000;
pub const FRAMETIME_WINDOW_NS: u64 = 250_000_000;
/// Longest expected delay between a present and its event reaching the helper. The helper
/// flushes its ETW session on every request and the session's flush timer is one second,
/// so delivery is normally well under a second; the margin keeps FPS from flickering to
/// N/A when a flush is late.
pub const DELIVERY_LAG_NS: u64 = 2_000_000_000;
/// Presents older than this are discarded (FPS window + delivery lag + margin).
const KEEP_NS: u64 = 4_000_000_000;
/// A process with no presents for this long is forgotten.
const FORGET_NS: u64 = 10_000_000_000;
const MAX_PER_RING: usize = 8192;
const MAX_PROCESSES: usize = 128;

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
}

/// Compute statistics from ascending timestamps (ns) at time `now`.
/// Returns `None` when the newest present is older than [`DELIVERY_LAG_NS`] or fewer than
/// two presents fall in the FPS window.
pub fn compute(ts: &VecDeque<u64>, now: u64) -> Option<FrameStats> {
    let end = ts.iter().rev().copied().find(|&t| t <= now)?;
    if now - end > DELIVERY_LAG_NS {
        return None;
    }
    let fps_start = end.saturating_sub(FPS_WINDOW_NS);
    let in_fps: Vec<u64> = ts.iter().copied().filter(|&t| t >= fps_start && t <= end).collect();
    if in_fps.len() < 2 {
        return None;
    }
    let span = (in_fps[in_fps.len() - 1] - in_fps[0]) as f64;
    if span <= 0.0 {
        return None;
    }
    let intervals = (in_fps.len() - 1) as f64;
    let fps = intervals * 1e9 / span;

    let ft_start = end.saturating_sub(FRAMETIME_WINDOW_NS);
    let mut first_ft = in_fps.iter().position(|&t| t >= ft_start).unwrap_or(in_fps.len() - 1);
    // Need at least two intervals; extend backwards into the FPS window if necessary.
    first_ft = first_ft.min(in_fps.len().saturating_sub(3));
    let ft_slice = &in_fps[first_ft..];
    let frametime_ms = (ft_slice[ft_slice.len() - 1] - ft_slice[0]) as f64 / 1e6 / (ft_slice.len() - 1) as f64;

    Some(FrameStats { fps, frametime_ms, samples: intervals as u32, last_present_age_ms: (now - end) / 1_000_000 })
}

#[derive(Default)]
struct ProcessRings {
    rings: HashMap<PresentSource, VecDeque<u64>>,
    last_seen: u64,
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
    }

    /// Statistics for every process that presented in the last second, highest FPS first.
    /// API-level events (DXGI/D3D9) are preferred; kernel presents are used only for
    /// processes with no API-level events, which avoids counting one frame twice.
    pub fn snapshot(&mut self, now: u64) -> Vec<ProcessFrames> {
        self.prune(now);
        let mut out = Vec::new();
        for (&pid, p) in &self.procs {
            let pick = [PresentSource::Dxgi, PresentSource::D3d9]
                .into_iter()
                .filter_map(|s| p.rings.get(&s).and_then(|r| compute(r, now)).map(|st| (s, st)))
                .max_by_key(|(_, st)| st.samples)
                .or_else(|| p.rings.get(&PresentSource::Kernel).and_then(|r| compute(r, now)).map(|st| (PresentSource::Kernel, st)));
            if let Some((source, stats)) = pick {
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
