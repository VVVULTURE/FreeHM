# Testing

This page lists what has been checked, how, and on which machines. Anything not listed
here has **not** been verified. "CI" means the GitHub Actions workflow in
`.github/workflows/ci.yml` (hosted `windows-latest` and `ubuntu-latest` runners: no GPU,
no game, virtual displays). "Local" means the developer PC:

| | |
|---|---|
| CPU | Intel Core i7-12700KF (12 cores / 20 threads) |
| GPU | NVIDIA (NVML available) |
| OS | Windows 11 Pro 10.0.26200, 2560×1440 + second monitor |

## Automated tests

`cargo test --workspace` runs on every push, on Windows and on Linux.

| Crate | Tests | Covers |
|---|---|---|
| freehm-core | 84 | config schema/migration/atomic save/recovery from a corrupt file, metric formatting, frame statistics (FPS and frame-time definitions, late or out-of-order events, bounded memory), game detection rules (launchers, manual/excluded/selected lists, priorities), shortcut parsing and conflicts, overlay policy (when to show, fullscreen warnings), the telemetry engine (precedence, freshness, stale/expired values, coverage hints, provider failures), the mock provider, IPC wire format, layout/rendering |
| freehm | 10 on Windows, 19 on Linux (adds `/proc` probe and permission tests) | path handling, IPC framing, Linux `/proc` probe and launcher roots, permission-setup argument validation |
| freehm-helper | 4 | protocol, embedded PawnIO module hashes, request validation |

The telemetry engine and overlay are also tested through the **mock provider**
(`freehm --mock`), which produces moving values for every metric, including unavailable
and stale ones, so the whole pipeline can be exercised without real sensors.

## End-to-end tests

### Windows (`tools/ci/windows-smoke-test.ps1`): all pass in CI; the first part also passed locally

The script starts the real application (with the mock provider) and checks it from outside.

* The overlay window is shown and has `WS_EX_LAYERED | TRANSPARENT | TOPMOST | TOOLWINDOW | NOACTIVATE`.
* The overlay never becomes the foreground window.
* Click-through: `WindowFromPoint` over the overlay returns the window underneath.
* Ctrl+Shift+O (RegisterHotKey) hides and shows it, driven by synthetic key presses.
* Settings applied through the settings window's IPC request (`--apply-config`):
  * A new shortcut (Ctrl+Shift+Alt+K) works at once, and the old one is released.
  * "Start with Windows" creates the per-user `Run` entry, and turning it off removes it again.
  * An out-of-range value is rejected and nothing changes.
* `--quit` exits and leaves no window behind.
* After a restart, the changed settings are all still there: shortcut, interval, position,
  font, metric selection, manual game list and startup.

  *The settings and startup checks were added after the local run, so they have passed only in CI.*
* With `-Helper` (elevated):
  * The helper service installs as LocalSystem.
  * Its required privileges are restricted to SeChangeNotify and SeCreateGlobal.
  * It starts automatically and restarts on failure.
  * It uninstalls cleanly.
  * Frame timing is checked against a real D3D11 swap chain, `present_loop` (FPS results below):
    in-process selftest, the service at a 1 s request interval, recovery after the idle
    stop, and the app's readings.

The CI job also builds the Inno Setup installer and checks it:
1. Installs it silently with the helper.
2. Checks the installed files and licence texts.
3. Uninstalls it silently and confirms that the service and files are gone.

### Linux (`tools/ci/linux-overlay-test.sh`): CI, all 17 checks pass

* **X11 (Xvfb):**
  * The overlay window is mapped and override-redirect.
  * Click-through: the window under the pointer is not the overlay, because its input shape is empty.
  * The Ctrl+Shift+O key grab toggles it.
  * A second program holding the same grab is reported as a conflict.
  * The same settings checks as on Windows pass: the shortcut changes at runtime, the XDG
    autostart entry is written and removed, invalid values are rejected, and settings
    survive a restart.
  * It quits cleanly.
* **Wayland (headless sway):**
  * The layer-shell backend is selected and outputs are reported.
  * The shortcut status honestly says that no GlobalShortcuts portal is available.
  * `freehm --toggle` works as the fallback.
  * It quits cleanly.
* Screenshots of both are kept as CI artifacts.
* `.deb` and AppImage packages are built.

### Manual checks on the local PC

| Check | Result |
|---|---|
| Overlay renders over the desktop at 200 ms with real sensors (screenshots) | pass |
| Installer: elevated silent install with helper, uninstall, reinstall | pass |
| Helper service account/privileges/ACLs on a real install | pass |
| CPU temperature (28–42 °C) and package power via the helper reach the unprivileged app | pass |
| NVIDIA GPU usage, temperature and VRAM via NVML, shown in the overlay (GPU clock and power not displayed in that test) | pass |
| Settings window: all eight pages opened and screenshotted, live preview updating (2026-10-22) | pass, three bugs found and fixed (below) |
| FPS on the real GPU (RTX 5070, 180 Hz) through the installed helper service | pass: 179.8–180.1 FPS vs. the presenter's own 179.7–180.1 |
| Helper service CPU while serving FPS | 0.05 % of a core polled every 1 s, 0.10 % every 200 ms; 9.6 MB |
| Startup to IPC ready | 130–150 ms |

Bugs found by the settings hand-check (fixed in `a7af198`):
* Status dots, the metric reorder arrows and the icon on "Repair helper" were drawn as
  empty boxes, because egui 0.37 bundles its symbol fonts only behind an optional feature.
  The dot and the "Repair helper" button were rechecked after the fix. The arrows were not,
  because the user had come back to the PC by then.
* The label fields on the Metrics page were cut off ("Fr…").
* With no game detected, the Hardware page labelled the foreground application's frame
  rate as "Active game".

One transient rendering glitch (a green block in one screenshot) was seen once and could
not be reproduced in the next 12 frames. Its cause is unknown.

## FPS and frame time (Windows)

Checked in CI with `present_loop` (`crates/freehm-helper/examples/present_loop.rs`), a
minimal "game". It is a window that presents through a Direct3D 11 flip-model swap chain
with vsync. The CI runner has a 60 Hz virtual display and no GPU, so frame pacing is uneven.

| CI run | Path | Result |
|---|---|---|
| 39693863255 | `freehm-helper selftest` (ETW session → present tracker) | 587 present events in 4 s; presenter at **61.2 FPS / 16.90 ms** |
| 39693863255 | App → helper service (named pipe) → overlay readings, with `present_loop.exe` detected as the game | **FPS 62.1, frame time 15.81 ms, Fresh** |
| 39696184621 | selftest, compared with the presenter's own count | helper 61.2 FPS; presenter counted 2441 frames in 40 s = **61.0 FPS** |
| 39697763172 | selftest / app | 49.4 FPS / **57.3 FPS, 18.0 ms**, overlay "shown: game detected" |

Without a flush, events reached the helper up to 0.9 s after the present, because ETW hands
real-time events over in buffers. The helper now flushes its session on each request
(at most every 100 ms), and the newest event is then 24–46 ms old. The statistics use the
last second of *received* presents and allow up to 2 s of delivery delay before reporting
that the game stopped presenting (unit-tested).

History: before 2026-10-21 the helper produced no FPS at all. Its counters showed present
events arriving and being recorded, yet none fell inside the statistics window. The
probable cause was raw event timestamps being compared with a clock in a different domain.
That was never confirmed directly. The fix removes the possibility: events are now
delivered as system time and compared with `GetSystemTimePreciseAsFileTime`. The window was
also made tolerant of delivery delay, and since then the checks above pass.

**Local, real GPU (2026-10-22).** These used `present_loop` on an RTX 5070 with a 180 Hz display,
through the installed helper service:
* The admin selftest measured 179.9 FPS.
* The service, polled every 200 ms, reported 179.8–180.1 FPS for all 150 requests. The
  presenter's own count was 179.7.

**Bug found locally and fixed (`e257fd2`).** Polled only once a second, the service returned the presenter for
just 7 of 30 requests, with the newest event 1.3 s old on average. Real-time ETW buffers are
per CPU, so on a 20-thread machine they fill slowly. The flush that ran on each request was
asynchronous and only helped the request after it. A helper thread now flushes the session
every 200 ms.
* CI checks this directly: the service is asked once a second for 12 s, and every answer
  must include the presenter, with the newest present under 800 ms old. In run
  39860420511 the presenter was in 10 of 10 answers, at most 242 ms old.
* Installed on the developer PC on 2026-10-22 and rechecked with the 180 Hz presenter:
  * Polled every 1 s, 24 of 25 answers had FPS; the only miss was the first request, before frame counting had started.
  * Polled every 200 ms, all 125 answers had FPS.
  * Readings averaged 179.89 FPS; the presenter counted 180.0 itself.
  * The newest present was 340 ms old on average (at most 568 ms).
  * Helper CPU stayed at or below 0.06 % of a core.

One CI run (on `e257fd2`) failed the app-level FPS check once. The service's own check had
passed seconds earlier, and the next run passed. The cause is unknown. CI now saves the
helper log, reports what the service sees when the app check fails, and tests that frame
timing recovers after the helper's 10 s idle stop. That check, and everything else, passed
in run 39861489268 (app 54.1 FPS).

Not yet compared against PresentMon in a real game. The PresentMon bundled with NVIDIA FrameView
on this PC exits at once with code 1 and no output, both with `--process_id` and with
`--process_name`. A standalone PresentMon release would be needed.

## Performance

Measured with `tools/perf/measure.ps1`. Each state starts a fresh instance, lets it run for
5 s, then samples it for 30 s. CPU is the process's CPU time ÷ wall time, as a percentage of
one logical core (the machine has 20). Local PC, release build, NVML loaded.

| State | CPU (one core) | Working set | Private |
|---|---|---|---|
| Tray only, no game, overlay hidden (nothing polled) | 0.00 % | 20 MB | 9 MB |
| Overlay shown, 200 ms (default) | 0.21 % | 48 MB | 33 MB |
| Overlay shown, 1000 ms | < 0.05 % (below Windows' 15.6 ms CPU-time resolution over 30 s) | 48 MB | 33 MB |
| Overlay shown, 50 ms (fastest) | 0.88 % | 48 MB | 33 MB |
| Overlay over a running app (game detection, 200 ms) | 0.26 % | 48 MB | 33 MB |
| Helper service, idle | 0.00 % | 9 MB | |

The first measurement found 6 % of a core at 200 ms. Per-provider timing
(`FREEHM_PROFILE=1`) showed the WDDM GPU statistics queries costing about 10 ms per poll,
duplicating what NVML already provided. Two changes fixed this:
* Lower-priority providers now skip metrics that a higher-priority provider has already delivered.
* Those queries run at most every 500 ms.

Most of the 28 MB difference in working set between hidden and visible is the NVIDIA
driver's NVML library, which is loaded only while metrics are polled.

The "over a game" state used a desktop application added to the manual game list, not a
real game. The overlay's cost does not depend on what is underneath it, but this has not
been measured over a real game.

## Not tested

These have **not** been verified. Treat them as unknown:

* **Real games**: no real game has been run with FreeHM yet, so there is no FPS comparison against PresentMon or the NVIDIA overlay.
* **Exclusive fullscreen**: warning detection is unit-tested, but has not been tried with a real exclusive-fullscreen game.
* **AMD and Intel GPUs**: the Windows WDDM path and the Linux amdgpu, i915 and xe paths have only run where there is no such GPU. AMD CPUs (PawnIO SMN module, `k10temp`) are untested too.
* **Tray menus**: clicking the tray menus on Windows and on a real Linux desktop (KDE, GNOME with AppIndicator).
* **Real Linux desktops**: the GlobalShortcuts portal on KDE or GNOME 48+, and the overlay on KDE, GNOME, Hyprland and others. CI covers only Xvfb and headless sway.
* **Linux permission setup**: the RAPL `pkexec` setup has not been run on a real Linux machine.
* **Sleep/resume**: on both platforms, only the code path exists.
* **Multi-monitor placement**: placement on the second monitor and DPI changes while running.
* **Anti-cheat**: behaviour with anti-cheat products is unknown.
