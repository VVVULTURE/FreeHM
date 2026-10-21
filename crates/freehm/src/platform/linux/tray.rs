//! Tray icon via the StatusNotifierItem D-Bus protocol (KDE Plasma, Xfce, LXQt, Cinnamon,
//! MATE, Budgie, waybar, … and GNOME with the AppIndicator extension, which Ubuntu and
//! Fedora ship). Left click (Activate) opens settings; the menu has the other actions.

use crate::app::{AppEvent, EventSink, TrayAction, TrayState};
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{Icon, MenuItem, ToolTip};

struct FreeHmTray {
    sink: EventSink,
    st: TrayState,
}

fn icon(size: u32) -> Icon {
    // ARGB32 in network byte order, from straight-alpha RGBA.
    let rgba = crate::icon::icon_rgba(size);
    let mut data = Vec::with_capacity(rgba.len());
    for p in rgba.as_chunks::<4>().0 {
        data.extend_from_slice(&[p[3], p[0], p[1], p[2]]);
    }
    Icon { width: size as i32, height: size as i32, data }
}

impl ksni::Tray for FreeHmTray {
    fn id(&self) -> String {
        "freehm".into()
    }
    fn title(&self) -> String {
        "FreeHM".into()
    }
    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![icon(16), icon(24), icon(32), icon(48)]
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: "FreeHM".into(), description: self.st.tooltip.clone(), ..Default::default() }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        self.sink.send(AppEvent::Tray(TrayAction::OpenSettings));
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let send = |a: TrayAction| move |t: &mut Self| t.sink.send(AppEvent::Tray(a));
        vec![
            StandardItem { label: "Open settings".into(), activate: Box::new(send(TrayAction::OpenSettings)), ..Default::default() }.into(),
            StandardItem {
                label: if self.st.overlay_visible { "Hide overlay".into() } else { "Show overlay".into() },
                activate: Box::new(send(TrayAction::ToggleOverlay)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Automatic game detection".into(),
                checked: self.st.detection_enabled,
                activate: Box::new(send(TrayAction::ToggleDetection)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Start at login".into(),
                checked: self.st.autostart,
                activate: Box::new(send(TrayAction::ToggleAutostart)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem { label: "Quit FreeHM".into(), activate: Box::new(send(TrayAction::Quit)), ..Default::default() }.into(),
        ]
    }
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        log::warn!("tray host went away ({reason:?}); the icon reappears when it returns");
        true
    }
}

pub struct Tray {
    handle: Handle<FreeHmTray>,
}

impl Tray {
    /// Register the icon. Fails when no StatusNotifierItem host is running (e.g. stock
    /// GNOME without the AppIndicator extension).
    pub fn new(sink: EventSink, st: TrayState) -> Result<Tray, String> {
        // Assume a host may appear later (session start-up races) instead of failing hard.
        let handle = FreeHmTray { sink, st }.assume_sni_available(true).spawn().map_err(|e| e.to_string())?;
        Ok(Tray { handle })
    }

    pub fn update(&self, st: &TrayState) {
        let st = st.clone();
        self.handle.update(move |t| t.st = st);
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.handle.shutdown().wait();
    }
}
