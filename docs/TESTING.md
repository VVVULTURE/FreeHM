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
| freehm-core | 107 | config schema/migration/atomic save/recovery from a corrupt file, metric formatting, frame statistics (FPS and frame-time definitions, 1% and 0.1% lows, late or out-of-order events, bounded memory), update logic (version order, GitHub's release description, trusted addresses, checksum files), monitor identity, game detection rules (launchers, manual/excluded/selected lists, priorities), shortcut parsing and conflicts, overlay policy (when to show, fullscreen warnings), the telemetry engine (precedence, freshness, stale/expired values, coverage hints, provider failures), the mock provider, IPC wire format, layout/rendering |
| freehm | 17 on Windows, 25 on Linux (adds `/proc` probe and permission tests) | overlay placement and the monitor used during exclusive fullscreen, downloads against a local server (checksum mismatch, unlisted file, plain HTTP refused), replacing an AppImage, path handling, IPC framing, Linux `/proc` probe and launcher roots, permission-setup argument validation |
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
* A test presenter in exclusive fullscreen (`present_loop --fullscreen --native`): FreeHM
  reports that the overlay cannot be seen there (the runner has one monitor). Skipped with a
  note if the runner cannot enter exclusive fullscreen.

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

Some of GitHub's Windows runners drop a presenter out of exclusive fullscreen within 0.2 s, with
the same code that keeps it elsewhere. The exclusive fullscreen check is skipped on those, with
a note in the log.

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
| Settings window: all eight pages opened and screenshotted, live preview updating (2026-10-22) | pass, three bugs found and fixed (below), fixes rechecked on screen |
| Tray menu (Windows): items and check marks; Open settings; Show overlay on/off; game detection off/on; Start with Windows off/on (Run entry removed and restored); Quit; left-click on the icon opens settings | pass (see note) |
| FPS on the real GPU (RTX 5070, 180 Hz) through the installed helper service | pass: 179.8–180.1 FPS vs. the presenter's own 179.7–180.1 |
| Helper service CPU while serving FPS | 0.05 % of a core polled every 1 s, 0.10 % every 200 ms; 9.6 MB |
| Startup to IPC ready | 130–150 ms |
| Settings ▸ Overlay (2026-10-24): opening the page shows the real overlay; dragging the box on the position map moved the real overlay live (sampled every 250 ms along the drag) and saved a custom position (1275, 613); "Custom" starts from the current position | pass |
| Exclusive fullscreen (2026-10-24), see below | overlay moved to the second monitor and drawn there; warning text correct with the option off |

Bugs found by the settings hand-check (fixed in `a7af198`):
* Status dots, the metric reorder arrows and the icon on "Repair helper" were drawn as
  empty boxes, because egui 0.37 bundles its symbol fonts only behind an optional feature.
  All three fixes were rechecked on screen after the change.
* The label fields on the Metrics page were cut off ("Fr…").
* With no game detected, the Hardware page labelled the foreground application's frame
  rate as "Active game".

Note on the tray check: FreeHM's icon sits in the taskbar's overflow area, which could not
be opened by script. The menu was therefore opened by posting the exact callback message the
shell sends for a right-click (`WM_CONTEXTMENU` and, for a left-click, `NIN_SELECT`) to
FreeHM's window. The menu items were then clicked with the real mouse. The shell's own
delivery of those clicks was not exercised.

One transient rendering glitch (a green block in one screenshot) was seen once and could
not be reproduced in the next 12 frames. Its cause is unknown.

## Updates

### End to end, in CI

`tools/ci/windows-update-test.ps1` and `tools/ci/linux-update-test.sh` run the real installer
and the real AppImage, with a release served from the CI machine in GitHub's place. All checks
pass in run 42332279544.

| Check | Windows installer, all users | AppImage |
|---|---|---|
| The check at start finds the release | yes, and it waits to be asked | yes, and it goes ahead |
| A file that does not match its checksum is deleted and nothing changes | yes | yes |
| A failed installer is reported and can be retried | yes, with `where.exe` (exit code 2) as the "installer" | not applicable |
| Nothing is installed while a game runs | yes: downloaded, verified and held for as long as the test game ran | not tested |
| Install, then restart | 0.1.0 → 9.9.9 in the registry, FreeHM restarted with its settings | file replaced, executable, restarted by itself |

Also on Linux: a plain binary only points to the release (`--install-update` refuses), and a real
request to GitHub completes over TLS with the system's certificates.

### On the developer PC, with Microsoft Defender on

The real updater, downloading from GitHub, with real-time protection and behaviour monitoring
on (signatures 1.463.165.0). A temporary pre-release held the CI installer, and the test used a
copy installed for one user, so no administrator prompt was involved. The copy was then removed
by hand, because the per-user uninstaller would ask for rights to remove the helper service.

| Step | Result |
|---|---|
| Found by the check at start | after 15 s, method `Installer` |
| Download and checksum | 1 s for 4.8 MB |
| Installer run with a settings window open | settings window closed, old process gone |
| New version started | 4 s after the download |
| Defender | no new detection in `Get-MpThreatDetection` or `Get-MpThreat`, and nothing in its event log |

This is one PC. Defender's verdict can differ elsewhere, and FreeMic, a different unsigned
program, was flagged on this same PC when it saved a downloaded file.

## Frame rate lows

The test presenter stalls for 40 ms every 50 frames and measures its own lows with its own code
at `Present_Start`, the point FreeHM uses. After both had 60 s of frames, in CI:

| Run | Frames | 1% low, FreeHM vs presenter | 0.1% low, FreeHM vs presenter | Average |
|---|---|---|---|---|
| 42311284335 | 2704 | 13.4 vs 12.99 | 9.9 vs 9.93 | 31 FPS |
| 42332279544 | 2945 | 14.1 vs 14.30 | 12.0 vs 11.96 | 51.3 FPS |

The first attempt at this comparison disagreed by 32%, because the presenter timed its frames
after `Present` returned. Lows depend on that choice, so they need not match a tool that times
frames differently.

## Overlay position and monitor names (Windows, on screen)

Run on the developer PC against a clean configuration, on the General page, which is where the
person who reported the bug was:

* Dragging the green box on the General page's map moved the real overlay window to 1106,579,
  set the position to Custom and saved it. The overlay showed during the move and hid again
  after about 4 s.
* The Monitor list showed `PG27QRT1B — 2560×1440 (Primary)` and `E2411K — 1920×1080`. Choosing
  E2411K moved the overlay to it, and the choice survived a restart.
* Windows renumbered the same two monitors from `DISPLAY1`/`DISPLAY2` to `DISPLAY7`/`DISPLAY8`
  during that day, which is why the choice stores the monitor's device path. Finding a monitor
  after renumbering is covered by unit tests; it was not done on screen.

The original report had a simpler cause: the only published release, v0.1.0-rc3, predated the
drag feature.

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

**Real game compared with PresentMon (2026-10-22).** The game was Geometry Dash, fullscreen on the 180 Hz monitor.
It presented at sync interval 0 with tearing allowed, and its own limiter held it at 120 FPS.
FreeHM detected it automatically ("installed in Steam library"). Over the same 40 s:

| Source | FPS | Frame time |
|---|---|---|
| PresentMon 2.6.0 (Intel-signed release), 4778 presents | 119.45 (per second: 114–121) | 8.372 ms mean |
| FreeHM helper service, polled every 200 ms (200/200 answers) | 119.44 mean (112.9–120.2) | 8.365 ms mean |
| FreeHM overlay readings, sampled every 1 s | 119.98 mean, all Fresh | |

The newest present was 368 ms old on average (at most 603 ms). FreeHM used 0.39 % of a core
with the overlay over the game at a 50 ms refresh interval. The overlay hid whenever the
user switched away from the game, as designed.

The NVIDIA FrameView copy of PresentMon on this PC exits at once with code 1 and no
output, so the standalone release was used.

## Accuracy

Every value FreeHM shows was compared with independent sources on the developer PC on
2026-10-22. There are two kinds of reference:
* tools that read the hardware themselves: LibreHardwareMonitor 0.9.6 (run as administrator) and `nvidia-smi`;
* Windows' own figures: the Task Manager counters, the Energy Meter and `GlobalMemoryStatusEx`.

`tools/verify/accuracy.ps1` repeats the Windows half of this on demand, and CI runs it (and
`accuracy.py` on Linux) on every change.

**Automated check** (`accuracy.ps1 -Samples 30 -Load`, all 18 PASS):

| Value | FreeHM | Reference | Source and method |
|---|---|---|---|
| CPU usage | 4.00 % / 9.73 % | 4.05 % / 9.79 % | Windows % Processor Utility (Task Manager), same 12 s window; idle / one core busy |
| CPU clock | 3933 / 4408 MHz | 3916 / 4407 MHz | Windows frequency × % Processor Performance, same window |
| CPU power | 15.45 / 32.43 W | 15.52 / 32.46 W | Windows Energy Meter energy counter (RAPL), same window |
| CPU power, instant | 15.23 W | 14.98 W | helper MSR RAPL |
| CPU temperature | 33.37 °C | 32.70 °C | helper raw package sensor |
| RAM used / total / % | 8047 / 32478 MiB / 24.78 % | 8048 / 32478 MiB / 24.78 % | GlobalMemoryStatusEx |
| GPU temperature | 37.00 °C | 36.97 °C | nvidia-smi |
| GPU usage, clock, power | 0.93 %, 369.7 MHz, 12.35 W | 0.87 %, 369.0 MHz, 12.35 W | nvidia-smi |
| VRAM used / total / % | 865.6 / 12227 MiB / 7.08 % | 866.0 / 12227 MiB / 7.08 % | nvidia-smi |

"Same window": Windows' averaging counters are read at the start and end of a 12 s window,
and every snapshot FreeHM publishes in that window is collected (by sequence number) and
averaged. Both sides then cover exactly the same time. Comparing single reads instead
differs by several % on a busy machine, because FreeHM's newest poll and a counter's
window never cover the same moments. % Processor Performance averages over busy time, so
FreeHM's clocks are weighted by each poll's CPU usage for that comparison. Unweighted, idle
clocks read 3–4 % lower than the long-window value; under steady load the two methods agree
to 1 MHz.

**Against LibreHardwareMonitor**, whose samples were taken by a separate process up to a
second apart:
* CPU package temperature: 33.7 vs 35.2 °C.
* CPU package power: 14.9 vs 16.1 W.
* GPU temperature: 41.17 vs 41.15 °C.
* RAM used: 8.558 vs 8.578 GiB.

Both programs read the same Intel package temperature register with TjMax 100 °C, so the
CPU differences come from sampling at different moments, not calibration. The package
sensor moves several degrees within milliseconds.

**FPS**: compared with PresentMon, see [FPS and frame time](#fps-and-frame-time-windows).

What was learned about measuring:
* Starting a process per sample (`freehm --status`, `nvidia-smi`, a WMI query) loads the
  CPU right when one side samples. That alone made FreeHM look 1.3 °C and 1 W "too high".
  The scripts therefore read the CPU only through open pipes and in-process counters, and
  query the GPU in a separate phase.
* On an idle machine both sides read about 0 % CPU, which proves little. CI therefore also
  runs the CPU comparisons with a steady one-core load (`-Load` / `--load`).
* `% Processor Utility` exceeds 100 % when cores run above base clock. Task Manager and
  FreeHM cap at 100 %, so a saturated machine (over 95 %) is reported as skipped, not
  compared.
* The CPU temperature tolerance is 3 °C, because two reads milliseconds apart differ by
  that much. Temperature is compared without the artificial load, under which the package
  sensor swings about 5 °C between reads.

Values without an independent source here: GPU shared memory (FreeHM reads it from the same
WDDM counters any other tool would use); AMD and Intel GPUs; AMD CPUs; Linux sensors on
real hardware (CI checks Linux CPU usage and RAM only).

The engine also refuses to display values that cannot be right:
* out-of-range values, such as a temperature outside −30…150 °C or usage above 100 %;
* memory "used" larger than that device's total.

Both are unit-tested.

## Exclusive fullscreen (Windows)

Measured on the RTX 5070 PC (Windows 11, two monitors) with the Direct3D 11 test presenter
in exclusive fullscreen (`present_loop --fullscreen --native`: flip model,
`SetFullscreenState(TRUE)` at the monitor's own resolution and 180 Hz). Visibility was
judged from screen captures of the window's area, not from z-order (the overlay was above
the game in z-order in every case).

| Window over the game | Drawn? | Game |
|---|---|---|
| Click-through layered topmost window (FreeHM's kind; `UpdateLayeredWindow` or `SetLayeredWindowAttributes`) | no | keeps exclusive fullscreen, 179–180 FPS |
| Layered topmost window that takes mouse input | yes | `Present` returns `DXGI_STATUS_OCCLUDED`, the game loses exclusive fullscreen |
| Ordinary topmost window | yes | same: loses exclusive fullscreen |
| On-Screen Keyboard (a UIAccess window) | yes | same: loses exclusive fullscreen |
| Click-through layered window on the *other* monitor | yes | keeps exclusive fullscreen |

`SHQueryUserNotificationState` reported `QUNS_RUNNING_D3D_FULL_SCREEN` (3) during exclusive
fullscreen and `QUNS_BUSY` (2) otherwise. So without injecting into the game nothing can be
drawn over it, and FreeHM now moves the overlay to another monitor while a game holds the
overlay's monitor in exclusive fullscreen.

End to end with the new build (a copy of the user's configuration):

| Case | Result |
|---|---|
| Option on (default), two monitors | overlay moved from `\\.\DISPLAY1` to `\\.\DISPLAY2` and was drawn there; the presenter stayed exclusive at 179.8 FPS; status explained the move; after the game exited the overlay hid again (no game) |
| Option off | overlay stayed on `DISPLAY1`, not drawn; status: "… uses exclusive fullscreen on \\.\DISPLAY1: Windows draws no other window over it, so the overlay cannot be seen there …" |

StellarDrive (Unity 6, D3D11), set to exclusive fullscreen: its log reports
`SetFullscreenState failed (887a0022)` and "reverting to FullscreenWindow" at start, both on
the test run and on the run where the overlay was reported missing; it then runs borderless,
where the overlay was drawn over it (verified by screen capture, 1 min). Exclusive mode in
that game is only entered from its own menu, which was not driven by the test.

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

* **More real games**: only one DXGI game (Geometry Dash) has been compared with PresentMon; D3D9, OpenGL and Vulkan games are untested.
* **Lows against PresentMon or in a real game**: lows have only been compared with the test presenter's own measurement.
* **The administrator prompt during an update**: CI machines show none, and the test on the developer PC used an install for one user. Saying No to the prompt was simulated with an installer that fails.
* **Update notifications on screen**: clicking the Windows notification, the tray menu's update item, and Linux notifications on a real desktop.
* **AppImage over FUSE**: CI runs AppImages in extract-and-run mode, and a game during an AppImage update was not tested.
* **Monitor names on Linux**: read from EDID (X11) and make/model (Wayland), but CI's virtual outputs have none, so they have not been seen working.
* **Exclusive fullscreen in a real game**: measured with the Direct3D 11 test presenter only (see "Exclusive fullscreen"). StellarDrive (Unity 6, D3D11) failed to enter exclusive mode at its start and fell back to borderless, where the overlay was visible.
* **Exclusive fullscreen with one monitor**: the notification balloon was not seen on screen (the test PC has two monitors; CI checks the warning text).
* **AMD and Intel GPUs**: the Windows WDDM path and the Linux amdgpu, i915 and xe paths have only run where there is no such GPU. AMD CPUs (PawnIO SMN module, `k10temp`) are untested too.
* **Tray menus on Linux**: on a real desktop (KDE, GNOME with AppIndicator). On Windows the menu was checked as described above.
* **Real Linux desktops**: the GlobalShortcuts portal on KDE or GNOME 48+, and the overlay on KDE, GNOME, Hyprland and others. CI covers only Xvfb and headless sway.
* **Linux permission setup**: the RAPL `pkexec` setup has not been run on a real Linux machine.
* **Sleep/resume**: on both platforms, only the code path exists.
* **Multi-monitor placement**: placement on the second monitor and DPI changes while running.
* **Anti-cheat**: behaviour with anti-cheat products is unknown.
