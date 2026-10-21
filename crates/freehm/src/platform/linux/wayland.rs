//! Wayland backend: a `zwlr_layer_shell_v1` surface on the Overlay layer (above fullscreen
//! windows on wlroots compositors, KDE Plasma, Hyprland, niri, COSMIC …), with keyboard
//! interactivity off and an empty input region, so it never takes focus and all input
//! passes through. GNOME (Mutter) does not implement layer-shell; `connect` fails there and
//! the caller falls back to XWayland.
//!
//! The Wayland connection lives on its own thread; the UI thread sends it commands through
//! a channel and wakes it with a pipe, so nothing polls while idle.

use crate::app::{AppEvent, EventSink, Placement};
use freehm_core::config::Anchor as CfgAnchor;
use freehm_core::ipc::MonitorInfo;
use freehm_core::render::Pixmap;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, mpsc};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

/// Logical size, anchor and (top, right, bottom, left) margins.
type LayerGeom = ((u32, u32), Anchor, (i32, i32, i32, i32));

enum Cmd {
    Show(Pixmap, Placement),
    Hide,
    Quit,
}

pub struct Wayland {
    tx: mpsc::Sender<Cmd>,
    wake: OwnedFd,
    monitors: Arc<Mutex<Vec<MonitorInfo>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct State {
    registry: RegistryState,
    outputs: OutputState,
    compositor: CompositorState,
    shm: Shm,
    layer_shell: LayerShell,
    pool: Option<SlotPool>,
    layer: Option<LayerSurface>,
    layer_output: Option<String>,
    /// Logical size and placement the surface was last configured with.
    layer_geom: Option<LayerGeom>,
    configured: bool,
    pending: Option<(Pixmap, i32)>,
    monitors: Arc<Mutex<Vec<MonitorInfo>>>,
    sink: EventSink,
}

impl State {
    fn refresh_monitors(&mut self) {
        let mut v = Vec::new();
        for (i, o) in self.outputs.outputs().enumerate() {
            let Some(info) = self.outputs.info(&o) else { continue };
            let scale = info.scale_factor.max(1);
            let (pw, ph) = info.modes.iter().find(|m| m.current).map(|m| m.dimensions).unwrap_or((0, 0));
            let (lx, ly) = info.logical_position.unwrap_or(info.location);
            // Physical pixels: the logical size times the (integer) buffer scale we render at.
            let (lw, lh) = info.logical_size.unwrap_or((pw / scale, ph / scale));
            v.push(MonitorInfo {
                name: info.name.clone().unwrap_or_else(|| format!("wayland-{}", info.id)),
                primary: i == 0, // Wayland has no "primary" output; the first one stands in
                x: lx * scale,
                y: ly * scale,
                width: (lw * scale).max(1) as u32,
                height: (lh * scale).max(1) as u32,
                scale: scale as f32,
            });
        }
        *self.monitors.lock().unwrap() = v;
    }

    fn output_named(&self, name: &str) -> Option<wl_output::WlOutput> {
        self.outputs.outputs().find(|o| self.outputs.info(o).and_then(|i| i.name).as_deref() == Some(name))
    }

    fn destroy_layer(&mut self) {
        self.layer = None;
        self.layer_geom = None;
        self.configured = false;
    }

    fn show(&mut self, qh: &QueueHandle<State>, pm: Pixmap, place: Placement) {
        let scale = place.monitor.scale.round().max(1.0) as i32;
        let size = (pm.width.div_ceil(scale as u32).max(1), pm.height.div_ceil(scale as u32).max(1));
        let (anchor, margins) = match place.anchor {
            CfgAnchor::TopLeft => (Anchor::TOP | Anchor::LEFT, (place.margin_y, 0, 0, place.margin_x)),
            CfgAnchor::TopCenter => (Anchor::TOP, (place.margin_y, 0, 0, 0)),
            CfgAnchor::TopRight => (Anchor::TOP | Anchor::RIGHT, (place.margin_y, place.margin_x, 0, 0)),
            CfgAnchor::BottomLeft => (Anchor::BOTTOM | Anchor::LEFT, (0, 0, place.margin_y, place.margin_x)),
            CfgAnchor::BottomCenter => (Anchor::BOTTOM, (0, 0, place.margin_y, 0)),
            CfgAnchor::BottomRight => (Anchor::BOTTOM | Anchor::RIGHT, (0, place.margin_x, place.margin_y, 0)),
            CfgAnchor::Custom => (Anchor::TOP | Anchor::LEFT, (place.margin_y, 0, 0, place.margin_x)),
        };
        if self.layer.is_some() && self.layer_output.as_deref() != Some(place.monitor.name.as_str()) {
            self.destroy_layer();
        }
        if self.layer.is_none() {
            let surface = self.compositor.create_surface(qh);
            let output = self.output_named(&place.monitor.name);
            let layer = self.layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("freehm"), output.as_ref());
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_exclusive_zone(-1);
            if let Ok(region) = Region::new(&self.compositor) {
                layer.wl_surface().set_input_region(Some(region.wl_region()));
            }
            self.layer = Some(layer);
            self.layer_output = Some(place.monitor.name.clone());
            self.configured = false;
        }
        let layer = self.layer.as_ref().unwrap();
        let geom = (size, anchor, margins);
        if self.layer_geom != Some(geom) {
            layer.set_anchor(anchor);
            layer.set_margin(margins.0, margins.1, margins.2, margins.3);
            layer.set_size(size.0, size.1);
            self.layer_geom = Some(geom);
            if self.configured {
                // Ask for a configure for the new size before attaching a new buffer.
                self.configured = false;
            }
            layer.commit();
        }
        self.pending = Some((pm, scale));
        if self.configured {
            self.draw();
        }
    }

    fn draw(&mut self) {
        let (Some((pm, scale)), Some(layer)) = (self.pending.take(), self.layer.as_ref()) else { return };
        let (w, h) = (pm.width as i32, pm.height as i32);
        let need = (w * h * 4) as usize;
        if self.pool.as_ref().is_none_or(|p| p.len() < need) {
            self.pool = SlotPool::new(need.max(64 * 1024), &self.shm).ok();
        }
        let Some(pool) = self.pool.as_mut() else { return };
        let Ok((buffer, canvas)) = pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else { return };
        // Premultiplied BGRA bytes == little-endian ARGB8888, which is what Wayland expects.
        canvas[..need].copy_from_slice(&pm.data[..need]);
        let s = layer.wl_surface();
        s.set_buffer_scale(scale);
        s.damage_buffer(0, 0, w, h);
        if buffer.attach_to(s).is_ok() {
            layer.commit();
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.refresh_monitors();
        self.sink.send(AppEvent::DisplayChanged);
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.refresh_monitors();
        self.sink.send(AppEvent::DisplayChanged);
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.refresh_monitors();
        self.destroy_layer();
        self.sink.send(AppEvent::DisplayChanged);
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // The compositor removed the surface (output gone); recreate on the next show.
        self.destroy_layer();
        self.sink.send(AppEvent::DisplayChanged);
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface, _: LayerSurfaceConfigure, _: u32) {
        self.configured = true;
        self.draw();
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_registry!(State);
smithay_client_toolkit::delegate_dispatch2!(State);

impl Wayland {
    /// Connect and bind layer-shell. Fails (with the reason) when the compositor has no
    /// layer-shell, e.g. GNOME.
    pub fn connect(sink: EventSink) -> Result<Wayland, String> {
        let (tx, rx) = mpsc::channel::<Cmd>();
        let (wake_r, wake_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK).map_err(|e| e.to_string())?;
        let monitors = Arc::new(Mutex::new(Vec::new()));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let mons = monitors.clone();
        let thread = std::thread::Builder::new()
            .name("wayland".into())
            .spawn(move || {
                let init = (|| -> Result<(Connection, wayland_client::EventQueue<State>, State), String> {
                    let conn = Connection::connect_to_env().map_err(|e| format!("cannot connect to the Wayland compositor: {e}"))?;
                    let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
                    let qh = queue.handle();
                    let compositor = CompositorState::bind(&globals, &qh).map_err(|_| "wl_compositor missing")?;
                    let layer_shell =
                        LayerShell::bind(&globals, &qh).map_err(|_| "this compositor does not support the layer-shell protocol (GNOME does not)")?;
                    let shm = Shm::bind(&globals, &qh).map_err(|_| "wl_shm missing")?;
                    let state = State {
                        registry: RegistryState::new(&globals),
                        outputs: OutputState::new(&globals, &qh),
                        compositor,
                        shm,
                        layer_shell,
                        pool: None,
                        layer: None,
                        layer_output: None,
                        layer_geom: None,
                        configured: false,
                        pending: None,
                        monitors: mons,
                        sink,
                    };
                    Ok((conn, queue, state))
                })();
                let (conn, mut queue, mut state) = match init {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                // Two roundtrips: globals, then output details.
                let _ = queue.roundtrip(&mut state);
                let _ = queue.roundtrip(&mut state);
                state.refresh_monitors();
                let _ = ready_tx.send(Ok(()));
                let qh = queue.handle();
                loop {
                    let mut quit = false;
                    while let Ok(cmd) = rx.try_recv() {
                        match cmd {
                            Cmd::Show(pm, place) => state.show(&qh, pm, place),
                            Cmd::Hide => state.destroy_layer(),
                            Cmd::Quit => quit = true,
                        }
                    }
                    if quit {
                        state.destroy_layer();
                        let _ = queue.flush();
                        return;
                    }
                    if queue.dispatch_pending(&mut state).is_err() || queue.flush().is_err() {
                        log::warn!("Wayland connection lost");
                        return;
                    }
                    let Some(guard) = queue.prepare_read() else { continue };
                    let wl_fd = conn.as_fd();
                    let mut fds =
                        [rustix::event::PollFd::new(&wl_fd, rustix::event::PollFlags::IN), rustix::event::PollFd::new(&wake_r, rustix::event::PollFlags::IN)];
                    if rustix::event::poll(&mut fds, None).is_err() {
                        continue;
                    }
                    let wl_ready = !fds[0].revents().is_empty();
                    let wake_ready = !fds[1].revents().is_empty();
                    if wl_ready {
                        if guard.read().is_err() {
                            log::warn!("Wayland read failed");
                            return;
                        }
                    } else {
                        drop(guard);
                    }
                    if wake_ready {
                        let mut buf = [0u8; 64];
                        while matches!(rustix::io::read(&wake_r, &mut buf), Ok(n) if n > 0) {}
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Wayland { tx, wake: wake_w, monitors, thread: Some(thread) }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("the Wayland thread exited during start-up".into()),
        }
    }

    fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
        let _ = rustix::io::write(&self.wake, &[1]);
    }

    pub fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.lock().unwrap().clone()
    }

    pub fn show(&self, pm: &Pixmap, place: &Placement) {
        self.send(Cmd::Show(pm.clone(), place.clone()));
    }

    pub fn hide(&self) {
        self.send(Cmd::Hide);
    }
}

impl Drop for Wayland {
    fn drop(&mut self) {
        self.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
