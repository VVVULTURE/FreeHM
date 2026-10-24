# FreeHM architecture

## Goals that drove the design

1. Low overhead while a game runs: no continuous redraw, no polling of unused sensors.
2. Honest telemetry: every value has a freshness state and a reason when missing.
3. Non-invasive: an external overlay window and documented OS/driver interfaces only.
4. Least privilege: the app always runs as the user. The one privileged piece (Windows
   helper service) is optional, narrow, and installed once.

## Technology choices

| Concern | Options considered | Chosen | Why |
|---|---|---|---|
| Language | C++, C#/.NET, Rust, Electron/Tauri | **Rust** | Native speed and memory use without a runtime, memory safety for the privileged helper, one language for Windows and Linux, mature `windows`, `x11rb`, `wayland-client` bindings. |
| Overlay window | Web view (Electron/Tauri), GUI toolkit window, raw platform window | **Raw platform window per OS** | The overlay must be transparent, click-through, topmost and never focusable. Web views can do parts of this but cost 50–150 MB and a GPU process, and each platform still needs native hacks for click-through. Raw `UpdateLayeredWindow` / X11 ARGB / layer-shell windows do exactly what is needed for a few MB. |
| Overlay rendering | GPU (D3D/GL/Vulkan), software | **Software (fontdue → premultiplied BGRA)** | The overlay is ~10 lines of text, redrawn only when a value changes. A CPU rasteriser with a glyph cache costs microseconds, needs no GPU context (nothing to lose on driver reset), and the same pixmap feeds all three backends. |
| Settings UI | Native per-OS UI, Qt, GTK, web, egui | **egui/eframe, in a separate process** | One cross-platform UI. Running it as a child process (`freehm --settings`) means its GL context and memory exist only while the window is open; the background process stays small. |
| Telemetry, Windows | WMI, LibreHardwareMonitor (.NET, WinRing0), PDH, NVML, ADLX, D3DKMT | **PDH + D3DKMT + NVML + helper (PawnIO, ETW)** | PDH and D3DKMT are documented, un-elevated and vendor-neutral (the same sources Task Manager uses). NVML is NVIDIA's supported API. LibreHardwareMonitor needs the .NET runtime and a much larger surface. Its long-time WinRing0 driver is flagged by Microsoft Defender as vulnerable; it has since moved to PawnIO, which FreeHM uses directly. ADLX was not added: D3DKMT already gives AMD/Intel utilisation, memory and (where the driver reports them) clock and temperature. GPU power in watts for non-NVIDIA GPUs on Windows is reported unavailable rather than adding a vendor SDK. |
| CPU temperature, Windows | WinRing0, custom driver, WMI thermal zones, PawnIO | **PawnIO (optional)** | No documented user-mode API exists. `MSAcpi_ThermalZoneTemperature` is absent on most desktops (verified absent on the dev PC). PawnIO is a signed, open-source, maintained driver that runs small verified modules instead of exposing raw port/memory access. FreeHM does not ship it. |
| FPS, Windows | Injection/hooks (RTSS style), PresentMon (exe or SDK service), ETW directly | **ETW in the helper** | The same Microsoft-Windows-DXGI / D3D9 / DxgKrnl present events PresentMon uses, read in a private real-time session. No injection, no separate collector process per refresh. Starting a kernel-provider session needs admin rights, hence the helper. |
| Telemetry, Linux | lm-sensors (libsensors), sysfs directly, NVML | **procfs/sysfs + NVML** | sysfs/hwmon is the kernel's documented interface; reading it directly avoids a C dependency. |
| FPS, Linux | MangoHud (Vulkan/GL layer), gamescope stats, DRM vblank events | **Not provided** | Every per-game frame-timing source on Linux loads a layer into the game or only exists inside gamescope. Reported as unavailable with that reason. |
| Tray | Win32 Shell_NotifyIcon; Linux: GTK/AppIndicator, StatusNotifierItem | **Shell_NotifyIcon; ksni (SNI over D-Bus)** | SNI is supported by KDE, most other desktops, and GNOME via the AppIndicator extension, with no GTK dependency. |
| Shortcut | RegisterHotKey; XGrabKey; Wayland GlobalShortcuts portal; evdev | **RegisterHotKey / XGrabKey / portal** | All three are the sanctioned mechanisms. Reading `/dev/input` (evdev) would be a keylogger-class permission and is rejected. |
| Startup | Task Scheduler, Run key, service; XDG autostart, systemd user unit | **HKCU Run key; XDG autostart** | Standard, per-user, no elevation, visible in Task Manager / desktop settings. |
| Packaging | MSI (WiX), MSIX, Inno Setup; AppImage, deb, rpm, Flatpak | **Inno Setup; AppImage + .deb** | Inno Setup supports per-user and per-machine installs from one package and optional service registration. MSIX cannot register the helper service. Flatpak's sandbox blocks the sysfs/proc access and layer-shell detection the app needs. |

## Process layout

```
Windows                                              Linux
┌──────────────────────────────┐                     ┌──────────────────────────────┐
│ freehm.exe (user)            │                     │ freehm (user)                │
│  UI thread: controller,      │                     │  UI thread: controller loop  │
│   overlay window, tray,      │                     │  threads: wayland | x11      │
│   hotkey (message loop)      │                     │   events, tray (D-Bus), IPC, │
│  threads: telemetry,         │                     │   portal, logind, telemetry, │
│   game detection, IPC,       │                     │   game detection             │
│   helper client              │                     └──────────────┬───────────────┘
└──────┬───────────────▲───────┘                                    │ Unix socket (0700 dir,
       │ named pipe    │ named pipe (ACL: user)                     │ peer uid check)
       │ (ACL'd)       │                                            ▼
       ▼               │                              freehm --settings (egui, on demand)
freehm-helper.exe   freehm.exe --settings
(LocalSystem service, (egui, on demand)
 optional)
```

The `freehm-core` crate is platform-neutral: configuration, metrics model, telemetry engine,
renderer, visibility policy, game classification, frame statistics, IPC protocol. Platform
crates and modules only provide facts and windows.

## Components

| Component | Location | Responsibility |
|---|---|---|
| Lifecycle / CLI | `crates/freehm/src/main.rs` | single instance, forwarding commands to the running instance, first-run |
| Controller | `crates/freehm/src/app.rs` | owns state; turns events (snapshot, game, hotkey, tray, IPC, display, resume) into overlay/telemetry actions via the `Platform` trait |
| Visibility policy | `freehm-core/src/policy.rs` | when the overlay shows; what to poll and how often |
| Telemetry engine | `freehm-core/src/telemetry.rs` | provider thread, poll plan, last-good cache, freshness (fresh/cached/stale/unavailable), backoff re-init, change-only logging |
| Providers | `platform/windows/{sensors,pdh,helper_client}.rs`, `platform/linux/sensors.rs`, `sensors/nvml.rs` | one data source each |
| Renderer | `freehm-core/src/render.rs` | rows → premultiplied BGRA pixmap (Exo 2 bundled, system fonts via fontdb) |
| Overlay backends | `platform/windows/overlay.rs`, `platform/linux/{wayland,x11}.rs` | show / hide / place a pixmap; never focusable; click-through |
| Game detection | `crates/freehm/src/detect.rs`, `freehm-core/src/gamedetect.rs`, `platform/*/probe.rs` | foreground-first detection; full process scan only when the foreground changes (or on Wayland without focus info) |
| Tray | `platform/windows/tray.rs`, `platform/linux/tray.rs` | menu: settings, show/hide, detection, autostart, quit |
| Shortcut | `platform/windows/mod.rs`, `platform/linux/{x11,dbus}.rs` | registration + conflict reporting |
| Settings UI | `crates/freehm/src/settings/` | egui pages, live preview with the real renderer, debounced live apply |
| IPC | `freehm-core/src/{ipc,wire}.rs`, `platform/*/ipc.rs` | length-prefixed JSON over a per-user pipe/socket |
| Helper service | `crates/freehm-helper` | PawnIO CPU temperature/power, ETW frame timing, ACL'd pipe |
| Config | `freehm-core/src/config.rs` | schema version, defaults, validation, migration, atomic save, corrupt-file backup |
| Logging | `crates/freehm/src/logging.rs` | size-capped rotating file, state changes only |

## Data flow and scheduling

* **Poll plan** (`policy::poll_plan`): what the telemetry thread reads, decided by the
  controller from (overlay visible?, settings window open?, enabled metrics). With the
  overlay hidden and settings closed, nothing is polled. With the overlay visible, only
  enabled metrics are polled, at the user's interval (default 200 ms). While the settings
  window is open, every metric is polled so the Hardware page can show them all.
* **Sensor cadence:** a provider whose source updates slowly can declare a minimum
  interval (`Provider::min_interval`); polls in between reuse its last value, marked
  *cached* rather than *fresh*. None of the current providers needs one: their sources
  (PDH, D3DKMT, NVML, sysfs) are cheap to read at 200 ms, and PDH/RAPL/`/proc/stat` rates
  are computed over the real elapsed time. A value older than max(3 × interval, 1 s) is
  *stale*, and one older than 30 s becomes *unavailable* with the provider's error.
* **Redraw:** the overlay is re-rendered only when the formatted text, style or placement
  changes.
* **Game detection:** every 2 s by default (0.5–30 s), plus immediately on a foreground
  change (Windows WinEvent hook, X11 `_NET_ACTIVE_WINDOW`). Each tick costs one foreground
  query. The process list is read only when the foreground process changes.
* **Frame timing:** the helper's ETW session runs only while a client asks for frames, and
  stops 10 s after the last request. PawnIO is released 30 s after the last CPU request.

## Representing missing data

`Sample { device, metric, value: Option<f64>, unit, at, state, reason }`, where `state` is
one of Fresh, Cached, Stale or Unavailable. Providers return `unavailable(reason)` instead
of omitting a value, so the UI can say *why*. When two providers report the same metric
(NVML and D3DKMT for an NVIDIA GPU), the earlier provider wins, but an unavailable sample
from one provider never hides a real value from another. Values outside physical
plausibility (temperature below -30 °C or above 150 °C, power above 2 kW, …) are rejected as
unavailable ("sensor reported an implausible value").

## Overlay backends

* **Windows:** a layered window (`WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST |
  WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`) updated with `UpdateLayeredWindow` from a
  premultiplied DIB. `WM_NCHITTEST → HTTRANSPARENT` adds a second layer of click-through.
  It is shown with `SW_SHOWNOACTIVATE`. Topmost is re-asserted on foreground changes and
  whenever another visible window overlapping it has been put above it (checked on every
  redraw and once a second, at most four times a second).
  Per-monitor DPI v2 aware, placed in physical pixels per monitor.
  `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` is an option. Exclusive fullscreen is
  detected with `SHQueryUserNotificationState` (QUNS_RUNNING_D3D_FULL_SCREEN) and the
  foreground window's monitor. Windows draws nothing else on that monitor then (measured:
  a click-through layered window is not drawn; any other window over the game takes it out
  of exclusive mode), so the overlay moves to another monitor, or the user is told why it
  cannot be seen.
* **X11:** a 32-bit ARGB override-redirect window, so the window manager never manages,
  focuses or decorates it. It has an empty XShape input region (click-through), monitors
  come from RandR, and scale from `Xft.dpi`. Without a compositing manager the background
  is opaque, and the status says so.
* **Wayland:** a `zwlr_layer_shell_v1` surface on the *Overlay* layer, with keyboard
  interactivity `None`, an empty input region and exclusive zone -1, anchored per the
  configured corner with margins on the chosen output. It is rendered at the output's
  integer scale. The connection runs on its own thread. If layer-shell is missing (GNOME),
  the X11 backend runs through XWayland and the limitation is reported.

## Permission model (summary — details in SECURITY.md)

| Platform | Needs elevation | When | For what |
|---|---|---|---|
| Windows | once (installer or Settings button) | install / explicit setup | helper service registration |
| Windows | once (PawnIO's own installer) | user choice | PawnIO driver, for CPU temperature |
| Linux | once (`pkexec`) | explicit setup | `freehm` group + udev rule for RAPL `energy_uj` (read-only) |
| both | never | start, overlay, settings, shortcut, autostart | — |
