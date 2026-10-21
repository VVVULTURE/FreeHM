//! D-Bus integrations that run on their own threads:
//!
//! * **GlobalShortcuts portal** (`org.freedesktop.portal.GlobalShortcuts`) — the only
//!   sanctioned way for a Wayland client to receive a key combination while another
//!   application has focus. The desktop (KDE Plasma, GNOME 48+, Hyprland, …) shows its own
//!   confirmation and lets the user change the trigger; the shortcut configured in FreeHM is
//!   sent as the *preferred* trigger. Where the portal is missing, `freehm --toggle` can be
//!   bound to a key in the desktop's own keyboard settings.
//! * **logind `PrepareForSleep`** — tells the app the system resumed, so sensors are
//!   re-initialised and the overlay is re-placed.

use crate::app::{AppEvent, EventSink};
use freehm_core::hotkey::{Hotkey, Key};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

pub const APP_ID: &str = "io.github.vvvulture.FreeHM";
const SHORTCUT_ID: &str = "toggle-overlay";
const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// Trigger in the XDG shortcuts format, e.g. `CTRL+SHIFT+o`.
pub fn portal_trigger(hk: &Hotkey) -> String {
    let mut s = String::new();
    for (on, m) in [(hk.ctrl, "CTRL+"), (hk.alt, "ALT+"), (hk.shift, "SHIFT+"), (hk.meta, "LOGO+")] {
        if on {
            s.push_str(m);
        }
    }
    let key = match hk.key {
        Key::Letter(c) => c.to_ascii_lowercase().to_string(),
        Key::Digit(c) => c.to_string(),
        Key::F(n) => format!("F{n}"),
        Key::Insert => "Insert".into(),
        Key::Delete => "Delete".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::PageUp => "Page_Up".into(),
        Key::PageDown => "Page_Down".into(),
        Key::Pause => "Pause".into(),
        Key::ScrollLock => "Scroll_Lock".into(),
        Key::PrintScreen => "Print".into(),
        Key::Backquote => "grave".into(),
    };
    s.push_str(&key);
    s
}

fn request_path(conn: &Connection, token: &str) -> Result<String, String> {
    let sender = conn.unique_name().ok_or("no D-Bus unique name")?.as_str().trim_start_matches(':').replace('.', "_");
    Ok(format!("{PATH}/request/{sender}/{token}"))
}

/// Call a portal method that answers through a `Request` object, and wait for its Response.
fn portal_request<B>(conn: &Connection, iface: &str, method: &str, token: &str, body: &B) -> Result<HashMap<String, OwnedValue>, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    // Subscribe before calling so the Response cannot be missed.
    let req = Proxy::new(conn, DEST, request_path(conn, token)?, "org.freedesktop.portal.Request").map_err(|e| e.to_string())?;
    let mut responses = req.receive_signal("Response").map_err(|e| e.to_string())?;
    let p = Proxy::new(conn, DEST, PATH, iface.to_string()).map_err(|e| e.to_string())?;
    let _: OwnedObjectPath = p.call(method, body).map_err(|e| format!("{method}: {e}"))?;
    let msg = responses.next().ok_or("the portal closed the request")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().map_err(|e| e.to_string())?;
    match code {
        0 => Ok(results),
        1 => Err("the request was cancelled in the desktop's dialog".into()),
        _ => Err("the desktop refused the request".into()),
    }
}

enum Cmd {
    Bind(Hotkey),
}

pub struct GlobalShortcuts {
    tx: mpsc::Sender<Cmd>,
}

impl GlobalShortcuts {
    /// Connect to the session bus and check that the portal implements GlobalShortcuts.
    pub fn connect(sink: EventSink) -> Result<GlobalShortcuts, String> {
        let conn = Connection::session().map_err(|e| format!("no D-Bus session bus: {e}"))?;
        let p = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.GlobalShortcuts").map_err(|e| e.to_string())?;
        let version: u32 = p.get_property("version").map_err(|_| "this desktop has no GlobalShortcuts portal".to_string())?;
        log::info!("GlobalShortcuts portal version {version}");
        // Host (non-sandboxed) apps identify themselves so the desktop can remember the
        // binding; older portals lack the registry, which is fine.
        if let Ok(reg) = Proxy::new(&conn, DEST, PATH, "org.freedesktop.host.portal.Registry") {
            let _ = reg.call_method("Register", &(APP_ID, HashMap::<&str, Value>::new()));
        }

        let session: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        // Activations.
        {
            let conn = conn.clone();
            let session = session.clone();
            let sink = sink.clone();
            std::thread::Builder::new()
                .name("portal-activated".into())
                .spawn(move || {
                    let Ok(p) = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.GlobalShortcuts") else { return };
                    let Ok(it) = p.receive_signal("Activated") else { return };
                    for msg in it {
                        let Ok((sess, id, _ts, _opts)) = msg.body().deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>() else {
                            continue;
                        };
                        if id == SHORTCUT_ID && session.lock().unwrap().as_deref() == Some(sess.as_str()) {
                            sink.send(AppEvent::Hotkey);
                        }
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        // Binding requests (may wait on a desktop dialog, so never on the UI thread).
        let (tx, rx) = mpsc::channel::<Cmd>();
        std::thread::Builder::new()
            .name("portal-bind".into())
            .spawn(move || {
                let mut n = 0u32;
                while let Ok(Cmd::Bind(hk)) = rx.recv() {
                    n += 1;
                    let r = bind(&conn, &session, &hk, n);
                    sink.send(AppEvent::HotkeyResult(r));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(GlobalShortcuts { tx })
    }

    /// Ask the desktop to bind `hk`. The outcome arrives later as `AppEvent::HotkeyResult`.
    pub fn bind(&self, hk: &Hotkey) {
        let _ = self.tx.send(Cmd::Bind(*hk));
    }
}

fn bind(conn: &Connection, session: &Mutex<Option<String>>, hk: &Hotkey, n: u32) -> Result<String, String> {
    // A session holds one set of bindings: replace it to change the preferred trigger.
    if let Some(old) = session.lock().unwrap().take()
        && let Ok(s) = Proxy::new(conn, DEST, old.as_str(), "org.freedesktop.portal.Session")
    {
        let _ = s.call_method("Close", &());
    }
    let pid = std::process::id();
    let token = format!("freehm_s{pid}_{n}");
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(format!("freehm_c{pid}_{n}")));
    opts.insert("session_handle_token", Value::from(token));
    let res = portal_request(conn, "org.freedesktop.portal.GlobalShortcuts", "CreateSession", &format!("freehm_c{pid}_{n}"), &(opts,))?;
    let handle = res
        .get("session_handle")
        .and_then(|v| {
            String::try_from(v.try_clone().ok()?).ok().or_else(|| OwnedObjectPath::try_from(v.try_clone().ok()?).ok().map(|p| p.as_str().to_string()))
        })
        .ok_or("the portal returned no session")?;
    *session.lock().unwrap() = Some(handle.clone());

    let trigger = portal_trigger(hk);
    let mut props: HashMap<&str, Value> = HashMap::new();
    props.insert("description", Value::from("Show or hide the FreeHM overlay"));
    props.insert("preferred_trigger", Value::from(trigger.clone()));
    let shortcuts = vec![(SHORTCUT_ID, props)];
    let bind_token = format!("freehm_b{pid}_{n}");
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(bind_token.clone()));
    let session_path = zbus::zvariant::ObjectPath::try_from(handle.as_str()).map_err(|e| e.to_string())?;
    let res = portal_request(conn, "org.freedesktop.portal.GlobalShortcuts", "BindShortcuts", &bind_token, &(session_path, shortcuts, "", opts))?;
    // What the desktop actually assigned (the user may have chosen a different key).
    let assigned = res
        .get("shortcuts")
        .and_then(|v| <Vec<(String, HashMap<String, OwnedValue>)>>::try_from(v.try_clone().ok()?).ok())
        .and_then(|list| list.into_iter().find(|(id, _)| id == SHORTCUT_ID))
        .map(|(_, p)| p.get("trigger_description").and_then(|t| String::try_from(t.try_clone().ok()?).ok()).unwrap_or_default());
    match assigned {
        Some(t) if !t.is_empty() => Ok(format!("XDG GlobalShortcuts portal (desktop assigned: {t})")),
        Some(_) => Ok(format!("XDG GlobalShortcuts portal (requested {trigger}; the desktop shows the assigned key in its shortcut settings)")),
        None => Err("the desktop did not bind the shortcut; bind `freehm --toggle` in the desktop's keyboard settings instead".into()),
    }
}

/// Post `AppEvent::Resumed` after every resume from suspend/hibernate.
pub fn watch_sleep(sink: EventSink) {
    let _ = std::thread::Builder::new().name("logind".into()).spawn(move || {
        let Ok(conn) = Connection::system() else {
            log::info!("no system bus; resume detection relies on sensor error recovery");
            return;
        };
        let Ok(p) = Proxy::new(&conn, "org.freedesktop.login1", "/org/freedesktop/login1", "org.freedesktop.login1.Manager") else { return };
        let Ok(it) = p.receive_signal("PrepareForSleep") else { return };
        for msg in it {
            if let Ok(false) = msg.body().deserialize::<bool>() {
                sink.send(AppEvent::Resumed);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use freehm_core::hotkey::Hotkey;

    #[test]
    fn triggers() {
        assert_eq!(super::portal_trigger(&Hotkey::parse("Ctrl+Shift+O").unwrap()), "CTRL+SHIFT+o");
        assert_eq!(super::portal_trigger(&Hotkey::parse("Super+Alt+F10").unwrap()), "ALT+LOGO+F10");
        assert_eq!(super::portal_trigger(&Hotkey::parse("Ctrl+PageUp").unwrap()), "CTRL+Page_Up");
    }
}
