//! X11 (and XWayland) backend: a 32-bit ARGB override-redirect window with an empty input
//! shape — the window manager never manages, focuses or decorates it, and every click
//! passes through to the window below. Also provides monitor enumeration (RandR), the
//! global shortcut (XGrabKey on the root window) and foreground tracking
//! (`_NET_ACTIVE_WINDOW`). Transparency needs a running compositor; without one the
//! background is drawn opaque.

use crate::app::{AppEvent, EventSink, Placement};
use freehm_core::hotkey::{Hotkey, Key};
use freehm_core::ipc::{MonitorInfo, edid_monitor_name};
use freehm_core::render::Pixmap;
use std::sync::{Arc, Mutex};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub struct X11 {
    pub conn: Arc<RustConnection>,
    pub root: Window,
    win: Window,
    gc: Gcontext,
    size: (u32, u32),
    pos: (i32, i32),
    mapped: bool,
    composited: bool,
    grabs: Arc<Mutex<Vec<(Keycode, u16)>>>,
}

fn intern(conn: &RustConnection, name: &str) -> Option<Atom> {
    conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok().map(|r| r.atom)
}

impl X11 {
    pub fn connect() -> Result<X11, String> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("cannot connect to the X server: {e}"))?;
        let screen = conn.setup().roots[screen_num].clone();
        let root = screen.root;
        // A 32-bit TrueColor visual gives per-pixel alpha.
        let visual = screen
            .allowed_depths
            .iter()
            .filter(|d| d.depth == 32)
            .flat_map(|d| d.visuals.iter())
            .find(|v| v.class == VisualClass::TRUE_COLOR)
            .map(|v| v.visual_id)
            .ok_or("the X server offers no 32-bit ARGB visual")?;
        let cmap = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_colormap(ColormapAlloc::NONE, cmap, root, visual).map_err(|e| e.to_string())?;
        let win = conn.generate_id().map_err(|e| e.to_string())?;
        let aux = CreateWindowAux::new().background_pixel(0).border_pixel(0).colormap(cmap).override_redirect(1).event_mask(EventMask::NO_EVENT);
        conn.create_window(32, win, root, 0, 0, 1, 1, 0, WindowClass::INPUT_OUTPUT, visual, &aux).map_err(|e| e.to_string())?;
        // Empty input shape: all pointer input goes to whatever is underneath.
        conn.shape_rectangles(shape::SO::SET, shape::SK::INPUT, ClipOrdering::UNSORTED, win, 0, 0, &[]).map_err(|e| e.to_string())?;
        if let (Some(wm_name), Some(utf8)) = (intern(&conn, "_NET_WM_NAME"), intern(&conn, "UTF8_STRING")) {
            let _ = conn.change_property8(PropMode::REPLACE, win, wm_name, utf8, b"FreeHM overlay");
        }
        if let (Some(t), Some(v)) = (intern(&conn, "_NET_WM_WINDOW_TYPE"), intern(&conn, "_NET_WM_WINDOW_TYPE_NOTIFICATION")) {
            let _ = conn.change_property32(PropMode::REPLACE, win, t, AtomEnum::ATOM, &[v]);
        }
        let gc = conn.generate_id().map_err(|e| e.to_string())?;
        conn.create_gc(gc, win, &CreateGCAux::new()).map_err(|e| e.to_string())?;
        // Is a compositing manager running? (Owner of _NET_WM_CM_S<n>.)
        let composited = intern(&conn, &format!("_NET_WM_CM_S{screen_num}"))
            .and_then(|a| conn.get_selection_owner(a).ok()?.reply().ok())
            .is_some_and(|r| r.owner != x11rb::NONE);
        // Root events: monitor changes (RandR) and active-window changes.
        let _ = conn.randr_select_input(root, randr::NotifyMask::SCREEN_CHANGE);
        let _ = conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE));
        conn.flush().map_err(|e| e.to_string())?;
        Ok(X11 { conn: Arc::new(conn), root, win, gc, size: (0, 0), pos: (0, 0), mapped: false, composited, grabs: Arc::new(Mutex::new(Vec::new())) })
    }

    pub fn composited(&self) -> bool {
        self.composited
    }

    /// Forward hotkey presses, monitor changes and focus changes to the app.
    pub fn spawn_event_thread(&self, sink: EventSink) {
        let conn = self.conn.clone();
        let grabs = self.grabs.clone();
        let active_atom = intern(&conn, "_NET_ACTIVE_WINDOW");
        let _ = std::thread::Builder::new().name("x11-events".into()).spawn(move || {
            // Ignore CapsLock / NumLock when matching.
            let ignore = u16::from(KeyButMask::LOCK) | u16::from(KeyButMask::MOD2);
            loop {
                let ev = match conn.wait_for_event() {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("X11 connection lost: {e}");
                        return;
                    }
                };
                match ev {
                    Event::KeyPress(k) => {
                        let state = u16::from(k.state) & !ignore;
                        if grabs.lock().unwrap().iter().any(|&(kc, m)| kc == k.detail && m == state) {
                            sink.send(AppEvent::Hotkey);
                        }
                    }
                    Event::RandrScreenChangeNotify(_) => sink.send(AppEvent::DisplayChanged),
                    Event::PropertyNotify(p) if Some(p.atom) == active_atom => sink.send(AppEvent::ForegroundChanged),
                    _ => {}
                }
            }
        });
    }

    pub fn monitors(&self) -> Vec<MonitorInfo> {
        monitors(&self.conn, self.root)
    }

    /// A thread-safe handle for querying the active window (used by game detection).
    pub fn active_window_source(&self) -> ActiveWindow {
        ActiveWindow { conn: self.conn.clone(), root: self.root, active: intern(&self.conn, "_NET_ACTIVE_WINDOW"), pid: intern(&self.conn, "_NET_WM_PID") }
    }

    pub fn show(&mut self, pm: &Pixmap, place: &Placement) {
        let c = &*self.conn;
        if self.size != (pm.width, pm.height) || self.pos != (place.x, place.y) || !self.mapped {
            let aux = ConfigureWindowAux::new().x(place.x).y(place.y).width(pm.width).height(pm.height).stack_mode(StackMode::ABOVE);
            let _ = c.configure_window(self.win, &aux);
            self.size = (pm.width, pm.height);
            self.pos = (place.x, place.y);
        }
        if !self.mapped {
            let _ = c.map_window(self.win);
            self.mapped = true;
        }
        // ARGB32 on a little-endian server is BGRA in memory: the pixmap's native layout.
        let _ = c.put_image(ImageFormat::Z_PIXMAP, self.win, self.gc, pm.width as u16, pm.height as u16, 0, 0, 0, 32, &pm.data);
        let _ = c.flush();
    }

    pub fn hide(&mut self) {
        if self.mapped {
            let _ = self.conn.unmap_window(self.win);
            let _ = self.conn.flush();
            self.mapped = false;
        }
    }

    pub fn raise(&self) {
        if self.mapped {
            let _ = self.conn.configure_window(self.win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
            let _ = self.conn.flush();
        }
    }

    /// Grab `hk` on the root window (with and without CapsLock/NumLock).
    pub fn grab_hotkey(&self, hk: &Hotkey) -> Result<String, String> {
        let c = &*self.conn;
        for (kc, m) in self.grabs.lock().unwrap().drain(..) {
            for extra in [0u16, KeyButMask::LOCK.into(), KeyButMask::MOD2.into(), u16::from(KeyButMask::LOCK) | u16::from(KeyButMask::MOD2)] {
                let _ = c.ungrab_key(kc, self.root, ModMask::from(m | extra));
            }
        }
        let keysym = keysym_for(hk.key);
        let kc = keycode_for(c, keysym).ok_or_else(|| format!("no key on this keyboard layout produces {}", hk.key))?;
        let mut mods = 0u16;
        if hk.ctrl {
            mods |= u16::from(ModMask::CONTROL);
        }
        if hk.shift {
            mods |= u16::from(ModMask::SHIFT);
        }
        if hk.alt {
            mods |= u16::from(ModMask::M1);
        }
        if hk.meta {
            mods |= u16::from(ModMask::M4);
        }
        for extra in [0u16, KeyButMask::LOCK.into(), KeyButMask::MOD2.into(), u16::from(KeyButMask::LOCK) | u16::from(KeyButMask::MOD2)] {
            let r = c.grab_key(false, self.root, ModMask::from(mods | extra), kc, GrabMode::ASYNC, GrabMode::ASYNC).map_err(|e| e.to_string())?.check();
            if r.is_err() {
                for e2 in [0u16, KeyButMask::LOCK.into(), KeyButMask::MOD2.into(), u16::from(KeyButMask::LOCK) | u16::from(KeyButMask::MOD2)] {
                    let _ = c.ungrab_key(kc, self.root, ModMask::from(mods | e2));
                }
                let _ = c.flush();
                return Err(format!("{hk} is already used by another application; choose a different shortcut"));
            }
        }
        self.grabs.lock().unwrap().push((kc, mods));
        let _ = c.flush();
        Ok("X11 key grab".into())
    }
}

pub struct ActiveWindow {
    conn: Arc<RustConnection>,
    root: Window,
    active: Option<Atom>,
    pid: Option<Atom>,
}

impl ActiveWindow {
    /// PID of the active window (`_NET_ACTIVE_WINDOW` → `_NET_WM_PID`) and whether it
    /// covers a whole monitor. `Err` when the window manager does not publish EWMH hints.
    pub fn query(&self) -> Result<Option<(u32, bool)>, ()> {
        let c = &*self.conn;
        let (active, pid_atom) = (self.active.ok_or(())?, self.pid.ok_or(())?);
        let r = c.get_property(false, self.root, active, AtomEnum::WINDOW, 0, 1).map_err(|_| ())?.reply().map_err(|_| ())?;
        if r.type_ == x11rb::NONE {
            return Err(()); // no EWMH window manager
        }
        let Some(w) = r.value32().and_then(|mut v| v.next()).filter(|&w| w != 0) else {
            return Ok(None);
        };
        let pid = c
            .get_property(false, w, pid_atom, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|ck| ck.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()));
        let Some(pid) = pid else { return Ok(None) };
        let fullscreen = (|| {
            let g = c.get_geometry(w).ok()?.reply().ok()?;
            let t = c.translate_coordinates(w, self.root, 0, 0).ok()?.reply().ok()?;
            let (x, y, ww, hh) = (t.dst_x as i32, t.dst_y as i32, g.width as i32, g.height as i32);
            Some(monitors(c, self.root).iter().any(|m| x <= m.x && y <= m.y && x + ww >= m.x + m.width as i32 && y + hh >= m.y + m.height as i32))
        })()
        .unwrap_or(false);
        Ok(Some((pid, fullscreen)))
    }
}

fn monitors(conn: &RustConnection, root: Window) -> Vec<MonitorInfo> {
    let scale = xft_scale(conn, root);
    let Ok(Ok(r)) = conn.randr_get_monitors(root, true).map(|c| c.reply()) else {
        let s = &conn.setup().roots[0];
        return vec![MonitorInfo {
            name: "screen".into(),
            primary: true,
            width: s.width_in_pixels as u32,
            height: s.height_in_pixels as u32,
            scale,
            ..Default::default()
        }];
    };
    let edid = conn.intern_atom(false, b"EDID").ok().and_then(|c| c.reply().ok()).map(|r| r.atom);
    // The name the monitor gives itself, from the first output with one.
    let label = |outputs: &[randr::Output]| -> Option<String> {
        outputs.iter().find_map(|&o| {
            let r = conn.randr_get_output_property(o, edid?, AtomEnum::ANY, 0, 64, false, false).ok()?.reply().ok()?;
            edid_monitor_name(&r.data)
        })
    };
    r.monitors
        .iter()
        .map(|m| {
            let name = conn.get_atom_name(m.name).ok().and_then(|c| c.reply().ok()).map(|n| String::from_utf8_lossy(&n.name).into_owned()).unwrap_or_default();
            MonitorInfo {
                name,
                id: String::new(),
                label: label(&m.outputs).unwrap_or_default(),
                primary: m.primary,
                x: m.x as i32,
                y: m.y as i32,
                width: m.width as u32,
                height: m.height as u32,
                scale,
            }
        })
        .collect()
}

/// Scale from the `Xft.dpi` X resource (what desktop environments set for HiDPI).
fn xft_scale(conn: &RustConnection, root: Window) -> f32 {
    let Ok(Ok(r)) = conn.get_property(false, root, AtomEnum::RESOURCE_MANAGER, AtomEnum::STRING, 0, 1 << 16).map(|c| c.reply()) else {
        return 1.0;
    };
    let s = String::from_utf8_lossy(&r.value);
    s.lines().find_map(|l| l.strip_prefix("Xft.dpi:").and_then(|v| v.trim().parse::<f32>().ok())).map(|dpi| (dpi / 96.0).clamp(0.5, 4.0)).unwrap_or(1.0)
}

fn keysym_for(k: Key) -> u32 {
    match k {
        Key::Letter(c) => c.to_ascii_lowercase() as u32,
        Key::Digit(c) => c as u32,
        Key::F(n) => 0xffbe + (n as u32 - 1),
        Key::Insert => 0xff63,
        Key::Delete => 0xffff,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::Pause => 0xff13,
        Key::ScrollLock => 0xff14,
        Key::PrintScreen => 0xff61,
        Key::Backquote => 0x60,
    }
}

fn keycode_for(c: &RustConnection, keysym: u32) -> Option<Keycode> {
    let setup = c.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = c.get_keyboard_mapping(min, max - min + 1).ok()?.reply().ok()?;
    let per = map.keysyms_per_keycode as usize;
    map.keysyms.chunks(per.max(1)).position(|syms| syms.contains(&keysym)).map(|i| min + i as u8)
}
