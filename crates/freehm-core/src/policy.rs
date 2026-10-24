//! Decisions shared by all platforms: when the overlay is visible, and how much telemetry
//! work that requires.
//!
//! Manual control: the shortcut (or tray item) toggles the overlay regardless of game
//! detection. The manual choice lasts until the active game changes (a game starts, exits,
//! or another game takes the foreground); then automatic behaviour resumes. This lets a
//! user hide the overlay for one game without disabling it for the next, and show it on the
//! desktop briefly without changing the "show on desktop" setting.

use crate::config::Config;
use crate::metrics::MetricId;
use crate::telemetry::{MetricSet, PollRequest};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Manual {
    #[default]
    Auto,
    Show,
    Hide,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct VisibilityState {
    pub manual: Manual,
    /// Identity of the active game (pid) the manual choice was made under.
    pub manual_game: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inputs {
    pub supported: bool,
    pub active_game: Option<u32>,
    pub show_on_desktop: bool,
    /// The settings window is showing the overlay's appearance and position: show it so
    /// that changes can be seen where they happen.
    pub settings_preview: bool,
}

impl VisibilityState {
    /// Resolve visibility. Clears a manual override whose game context has changed.
    pub fn decide(&mut self, i: Inputs) -> (bool, String) {
        if self.manual != Manual::Auto && self.manual_game != i.active_game {
            self.manual = Manual::Auto;
            self.manual_game = None;
        }
        if !i.supported {
            return (false, "hidden: overlay not supported in this session".into());
        }
        if i.settings_preview {
            return (true, "shown: the settings window is editing its appearance or position".into());
        }
        match self.manual {
            Manual::Show => return (true, "shown manually (shortcut/tray)".into()),
            Manual::Hide => return (false, "hidden manually (shortcut/tray)".into()),
            Manual::Auto => {}
        }
        match (i.active_game, i.show_on_desktop) {
            (Some(_), _) => (true, "shown: game detected".into()),
            (None, true) => (true, "shown: 'show on desktop' is on".into()),
            (None, false) => (false, "hidden: no game detected".into()),
        }
    }

    /// The shortcut/tray toggle: flip whatever is currently displayed.
    pub fn toggle(&mut self, currently_visible: bool, active_game: Option<u32>) {
        self.set(!currently_visible, active_game);
    }

    pub fn set(&mut self, visible: bool, active_game: Option<u32>) {
        self.manual = if visible { Manual::Show } else { Manual::Hide };
        self.manual_game = active_game;
    }
}

/// What the telemetry thread should poll.
/// - Settings window open (live): every metric, so the Hardware page shows everything.
/// - Overlay visible: only the enabled metrics (plus totals needed for "used / total").
/// - Otherwise: nothing at all — the telemetry thread sleeps.
pub fn poll_plan(cfg: &Config, overlay_visible: bool, settings_live: bool, game_pids: &[u32]) -> (Duration, PollRequest) {
    let interval = Duration::from_millis(cfg.general.refresh_interval_ms as u64);
    let metrics = if settings_live {
        MetricSet::all()
    } else if overlay_visible {
        let mut s = MetricSet::default();
        for e in cfg.enabled_metrics() {
            s.insert(e.id);
            if e.show_total {
                match e.id {
                    MetricId::RamUsed => s.insert(MetricId::RamTotal),
                    MetricId::VramUsed => s.insert(MetricId::VramTotal),
                    _ => {}
                }
            }
        }
        s
    } else {
        MetricSet::default()
    };
    let wants_frames = metrics.any_of(&[MetricId::Fps, MetricId::FrameTime]);
    let game_pids = if wants_frames { game_pids.to_vec() } else { Vec::new() };
    (interval, PollRequest { metrics, game_pids })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MetricEntry;

    fn inputs(game: Option<u32>, desktop: bool) -> Inputs {
        Inputs { supported: true, active_game: game, show_on_desktop: desktop, settings_preview: false }
    }

    #[test]
    fn settings_preview_shows_even_when_hidden_manually() {
        let mut v = VisibilityState::default();
        v.set(false, None);
        let (vis, reason) = v.decide(Inputs { settings_preview: true, ..inputs(None, false) });
        assert!(vis);
        assert!(reason.contains("settings"));
        assert!(!v.decide(inputs(None, false)).0, "the manual choice applies again once the preview ends");
        assert!(!v.decide(Inputs { supported: false, settings_preview: true, ..inputs(None, true) }).0, "never where unsupported");
    }

    #[test]
    fn automatic_behaviour() {
        let mut v = VisibilityState::default();
        assert!(!v.decide(inputs(None, false)).0);
        assert!(v.decide(inputs(Some(1), false)).0);
        assert!(v.decide(inputs(None, true)).0);
    }

    #[test]
    fn toggle_hides_in_game_until_game_changes() {
        let mut v = VisibilityState::default();
        let (vis, _) = v.decide(inputs(Some(10), false));
        v.toggle(vis, Some(10));
        assert!(!v.decide(inputs(Some(10), false)).0, "hidden while same game");
        assert!(v.decide(inputs(Some(11), false)).0, "new game resets to automatic");
    }

    #[test]
    fn toggle_shows_on_desktop_until_game_starts() {
        let mut v = VisibilityState::default();
        let (vis, _) = v.decide(inputs(None, false));
        assert!(!vis);
        v.toggle(vis, None);
        assert!(v.decide(inputs(None, false)).0);
        // A game starting resets manual mode; automatic shows it anyway.
        assert!(v.decide(inputs(Some(5), false)).0);
        assert_eq!(v.manual, Manual::Auto);
        // Game exits: back to hidden on desktop.
        assert!(!v.decide(inputs(None, false)).0);
    }

    #[test]
    fn unsupported_session_never_shows() {
        let mut v = VisibilityState::default();
        v.set(true, None);
        let (vis, reason) = v.decide(Inputs { supported: false, active_game: None, show_on_desktop: true, settings_preview: false });
        assert!(!vis);
        assert!(reason.contains("not supported"));
    }

    #[test]
    fn poll_plan_scales_with_state() {
        let mut cfg = Config { metrics: vec![MetricEntry::new(MetricId::GpuTemp, true), MetricEntry::new(MetricId::VramUsed, true)], ..Config::default() };
        let (iv, req) = poll_plan(&cfg, false, false, &[1]);
        assert_eq!(iv, Duration::from_millis(200));
        assert!(req.metrics.is_empty(), "hidden + no settings = idle");
        let (_, req) = poll_plan(&cfg, true, false, &[1]);
        assert!(req.metrics.contains(MetricId::GpuTemp));
        assert!(req.metrics.contains(MetricId::VramTotal), "total needed for used/total display");
        assert!(!req.metrics.contains(MetricId::CpuTemp));
        assert!(req.game_pids.is_empty(), "no frame data requested when FPS is not shown");
        let (_, req) = poll_plan(&cfg, false, true, &[1]);
        assert_eq!(req.metrics, MetricSet::all());
        assert_eq!(req.game_pids, vec![1]);
        cfg.general.refresh_interval_ms = 1000;
        assert_eq!(poll_plan(&cfg, true, false, &[]).0, Duration::from_millis(1000));
    }
}
