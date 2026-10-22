# FreeHM 0.1.0: engineering report

Status as of 2026-10-21, commit on `main` with CI run
[39697763172](https://github.com/VVVULTURE/FreeHM/actions/runs/39697763172) green on Windows
and Linux. **Verified** marks things checked by a test or by direct observation. Everything
else is design intent, assumption or recommendation, and is labelled as such.
[TESTING.md](TESTING.md) has the full evidence.

## 1. Architecture and technologies

FreeHM is a Rust workspace with three crates:

| Crate | Role |
|---|---|
| `freehm-core` | Platform-neutral: config, telemetry engine, frame statistics, game-detection rules, renderer, IPC |
| `freehm` | The background app (tray, overlay, shortcut, detection, IPC server), plus the settings window as a child process (`freehm --settings`, egui) |
| `freehm-helper` | Optional Windows service: CPU temperature and power through PawnIO, and FPS through ETW |

Why these choices:
* **Rust:** native footprint with no runtime, memory safety for the privileged helper, and one codebase for both OSes.
* **Raw platform windows and a software renderer:** these give exactly the needed overlay properties (transparent, click-through, topmost, never focused) for a few MB. The same pixmap feeds every backend.
* **Settings as a separate egui process:** its GPU context and memory exist only while the window is open.

The full comparison of the alternatives considered is in [ARCHITECTURE.md](ARCHITECTURE.md).

## 2. Major features (implemented)

* **Metrics:** CPU, GPU, RAM, VRAM, FPS and frame time, each one selectable.
* **Honest readings:**
  * Every reading carries a freshness state (fresh / cached / stale / unavailable) and a reason when it is missing.
  * Nothing is ever fabricated.
  * Only what the overlay shows is polled.
* **Overlay:** transparent, topmost and click-through.
  * Fully configurable appearance: layout, colours, font (Exo 2 bundled), size, anchor or offset, monitor.
  * The settings window shows a live preview.
  * Warns about exclusive fullscreen.
* **Shortcut:** Ctrl+Shift+O by default, configurable, with conflict reporting.
* **Game detection:** launcher-aware (Steam, Epic, GOG, EA, Ubisoft, Battle.net, Xbox; Heroic, Lutris, itch on Linux).
  * Manual, excluded and "selected games only" lists.
* **Tray:** settings, show/hide, detection on/off, startup on/off, quit.
* **Startup:** autostart per user (Run key or XDG autostart), on by default.
* **Settings file:** schema version, atomic save, migration, and recovery from a corrupt file.
* **Command line:** `--toggle/--show/--hide/--quit/--status/--apply-config/--mock`.
* **Packaging:** Windows installer (Inno Setup), `.deb` and AppImage, all with licence notices.

## 3. Telemetry providers

The order of each list is precedence: the first source that has a value wins.

| | Windows | Linux |
|---|---|---|
| CPU % / clock | PDH | `/proc/stat`, cpufreq |
| CPU temperature | helper + PawnIO (Intel MSR, AMD SMN) | hwmon `coretemp` / `k10temp` / `zenpower` |
| CPU power | Intel: Energy Meter (PDH); AMD: helper + PawnIO | RAPL `energy_uj` (one-time permission setup) |
| NVIDIA GPU | NVML (usage, temperature, clock, power, VRAM) | NVML |
| AMD / Intel GPU | D3DKMT statistics + PDH GPU memory; power reported unavailable | amdgpu / i915 / xe sysfs + hwmon |
| RAM | `GlobalMemoryStatusEx` | `/proc/meminfo` |
| FPS / frame time | helper: ETW present events | unavailable, with the reason |

Verified on real hardware (i7-12700KF + NVIDIA, Windows 11):
* CPU %, CPU temperature and package power through the helper.
* NVIDIA GPU usage, temperature and VRAM.
* RAM.

The AMD/Intel GPU paths and AMD CPU paths have never run on such hardware.

## 4. FPS and frame time

* **Windows:**
  * The helper service runs a private real-time ETW session on the same providers PresentMon uses:
    * Microsoft-Windows-DXGI Present_Start
    * D3D9 Present_Start
    * DxgKrnl Present_Info (for OpenGL/Vulkan paths)
  * The session is filtered by event ID, started on demand and stopped after 10 s idle.
  * Only event headers are read (process ID, timestamp). Nothing touches the game.
  * API-level events take precedence over kernel events, so no frame is counted twice.
  * **Frame time** is the mean interval between presents over the last 250 ms.
  * **FPS** is the number of present intervals in the last second divided by their span.
* **Verified in CI:**
  * Against a real D3D11 swap chain, the helper and the presenter's own count agree: 61.2 vs 61.0 FPS.
  * The full app → helper → overlay path reported 57–62 FPS, Fresh, for the detected test "game".
  * The service, asked once a second, includes the presenter in every answer, with the newest present at most 242 ms old.
* **Verified locally (RTX 5070, 180 Hz):** the installed service reported 179.8–180.1 FPS for the same presenter, which counted 179.7 itself.
* **Fixed after that local test:** at a 1 s refresh the service missed most readings, because ETW delivered events 1–2 s late. The helper now flushes the session every 200 ms. The fix is verified in CI but not yet installed on the developer PC.
* **Windows limitations:** FPS needs the helper service, which needs admin rights once at install. It has not been compared with PresentMon in a real game.
* **Linux:** not provided. Every per-game frame-timing source loads a layer into the game (MangoHud) or exists only inside gamescope. Both are excluded by the no-injection rule.

## 5. Overlay rendering

All three backends receive the same premultiplied BGRA pixmap from the software renderer.
It is redrawn only when a displayed value changes.

* **Windows:** a layered window (`UpdateLayeredWindow`) with `WS_EX_LAYERED | TRANSPARENT | TOPMOST | TOOLWINDOW | NOACTIVATE`.
  * It can optionally be excluded from screen capture.
  * Exclusive-fullscreen games can cover it; FreeHM detects this and says so.
* **X11:** an override-redirect 32-bit ARGB window with an empty XShape input region, which makes it click-through.
  * It is opaque without a compositor, and the status says so.
* **Wayland:** a `wlr-layer-shell` surface on the Overlay layer with an empty input region and no keyboard focus.
  * On compositors without layer-shell (GNOME), it falls back to XWayland.

Verified:
* **Windows:** styles, click-through, never taking focus, and toggle by shortcut, both in CI and locally.
* **X11:** under Xvfb in CI.
* **Wayland:** layer-shell under headless sway in CI.

## 6. Permissions

* **Routine use never elevates.** Launching, the overlay, settings, the shortcut, game detection and autostart all run as the user (HKCU Run key, XDG autostart).
* **Windows:**
  * The installer, run as admin, can register `FreeHMHelper`. The same can be done later from *Settings ▸ Startup & permissions*, which costs one UAC prompt.
  * The service runs as LocalSystem, with its required privileges reduced to SeChangeNotify and SeCreateGlobal.
  * It answers on a named pipe that only grants interactive users read/write access to fixed, read-only questions (CPU sensors, frame statistics). There are no commands that change anything.
  * The service can be repaired or removed from settings, and the uninstaller removes it.
  * The PawnIO driver is optional and installed by its own installer; FreeHM does not ship it.
  * Verified on the local PC: account, privileges, ACLs, and install/uninstall/reinstall.
* **Linux:** a one-time `pkexec freehm --setup-permissions` creates a `freehm` group and a udev rule that makes RAPL `energy_uj` readable by that group. `--remove-permissions` undoes it. This setup has not been run on a real Linux machine.

## 7. Build commands and results

| Command | Where | Result |
|---|---|---|
| `cargo fmt --all --check` | CI Windows | pass |
| `cargo clippy --workspace --all-targets` (`RUSTFLAGS=-D warnings`) | CI Windows + Linux, local | pass, no warnings |
| `cargo build --release --workspace` | CI Windows + Linux, local Windows | pass |
| `cargo build --release -p freehm-helper --example present_loop` | CI Windows, local | pass |
| `cargo about generate about.hbs -o dist/THIRD_PARTY_LICENSES.html` | CI | pass |
| `ISCC /DAppVersion=0.1.0 installer\windows\freehm.iss` | CI, local (Inno Setup 6.7.3) | pass |
| `cargo deb -p freehm`, appimagetool | CI Linux | pass |

## 8. Tests

* **Unit tests (`cargo test --workspace`), all pass:**
  * core 84
  * app 10 on Windows, 19 on Linux
  * helper 4
* **Windows end-to-end, all pass in CI:**
  * Overlay styles and click-through.
  * Shortcut toggle, runtime shortcut change and release of the old shortcut.
  * Autostart on and off.
  * Rejection of invalid settings.
  * Persistence across restart.
  * `--quit` cleanup.
  * Helper install, selftest and uninstall.
  * ETW FPS from a real swap chain, both in the helper and in the app.
  * Silent install and uninstall with the installer.
* **Linux end-to-end, all 17 checks pass in CI:** the X11 equivalents of the above, plus the Wayland layer-shell backend, outputs, the honest no-portal status and the `--toggle` fallback.
* **Local manual checks:** listed in TESTING.md.

## 9. Measured resource use

Measured on the local PC (i7-12700KF, 20 threads, Windows 11, release build, NVML loaded)
with `tools/perf/measure.ps1`: 30 s samples after 5 s of settling. CPU is given as a share
of one logical core.

| State | CPU | Working set |
|---|---|---|
| Tray only, no game, overlay hidden | 0.00 % | 20 MB |
| Overlay shown, 200 ms (default) | 0.21 % | 48 MB |
| Overlay shown, 1000 ms | < 0.05 % | 48 MB |
| Overlay shown, 50 ms | 0.88 % | 48 MB |
| Overlay over a detected app, 200 ms | 0.26 % | 48 MB |
| Helper service, idle | 0.00 % | 9 MB |
| Helper service serving FPS (polled every 1 s / 200 ms) | 0.05 % / 0.10 % | 9.6 MB |

* Startup to IPC ready took 130–150 ms.
* One avoidable cost was found and fixed: duplicate WDDM GPU queries took 6 % of a core and now take 0.21 %.
* Not measured: the overlay over a real game.

## 10. Generated artifacts

Built by CI run 39697763172 and downloaded:

| File | SHA-256 |
|---|---|
| `dist/ci/windows-installer/FreeHM-0.1.0-windows-x64-setup.exe` | `6688ff95…a628d` |
| `dist/ci/linux-packages/freehm_0.1.0-1_amd64.deb` | `00bfebdc…c89f` |
| `dist/ci/linux-packages/FreeHM-0.1.0-x86_64.AppImage` | `1695e62d…eac28` |

* Local binaries: `target/release/freehm.exe`, `target/release/freehm-helper.exe`.
* The installer was installed and run on the local PC (an earlier build, from 2026-10-20). The `.deb` and AppImage were built and checked only in CI.

## 11. Limitations and untested areas

* **Limitations:**
  * No FPS on Linux.
  * Exclusive fullscreen can hide the overlay on Windows.
  * Non-NVIDIA GPU power on Windows is unavailable.
  * Wayland needs layer-shell, otherwise XWayland is used.
  * Global shortcuts on Wayland need the GlobalShortcuts portal.
  * The GNOME tray needs the AppIndicator extension.
  * Anti-cheat compatibility is unknown.
* **Not tested:**
  * Real games, and comparison with PresentMon or the NVIDIA overlay.
  * Real exclusive fullscreen.
  * AMD/Intel GPUs and AMD CPUs.
  * Clicking the tray menus.
  * The settings fixes for the Metrics page (arrows, label widths) and the Hardware heading, checked by hand after the fix.
  * Real Linux desktops: KDE, GNOME, Hyprland, the portal, the tray.
  * Linux permission setup.
  * Sleep/resume.
  * Multi-monitor placement and DPI changes.

## 12. Remaining necessary work

1. **Install the current helper on the developer PC** (one UAC prompt), so the 200 ms flush fix runs there. Then compare FPS with PresentMon in a real game.
2. **Hand-check the tray menus** and the re-fixed settings items (needs someone at the PC).
3. **Run the Linux build on a real desktop** (KDE and GNOME): tray, portal shortcut, overlay over a fullscreen game, `--setup-permissions`.
4. **Recommendation:** test on AMD and Intel GPUs before claiming support for them.
