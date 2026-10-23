# FreeHM

A lightweight hardware statistics overlay for games on **Windows 11** and **Linux** (X11 and
Wayland), in the spirit of the NVIDIA performance overlay and MSI Afterburner/RTSS, but
built only on documented operating-system and driver interfaces. FreeHM never injects
code into games, never reads game memory and ships no kernel driver.

* CPU: utilisation, temperature, clock, package power
* GPU: utilisation, temperature, clock, power (NVIDIA, AMD, Intel; integrated and discrete)
* RAM: used, utilisation, total
* VRAM: used, utilisation, total (shared/GTT memory of integrated GPUs is labelled as such)
* FPS and frame time (Windows; see [limitations](#known-limitations))

Every value carries its freshness and, when it cannot be read, the reason. A missing sensor
shows `N/A`, never a made-up `0`.

> **Status:** see [docs/TESTING.md](docs/TESTING.md) for exactly what has and has not been
> verified, on which machines.

## Install

Downloads are on the repository's **Releases** page:

| File | For |
|---|---|
| `FreeHM-<version>-windows-x64-setup.exe` | Windows 11 installer (recommended) |
| `FreeHM-<version>-windows-x64-portable.zip` | Windows without installing (no helper service, so no FPS or CPU temperature) |
| `freehm_<version>-1_amd64.deb` | Debian, Ubuntu and derivatives |
| `FreeHM-<version>-x86_64.AppImage` | Most other Linux distributions |
| `FreeHM-<version>-linux-x86_64.tar.gz` | The plain Linux binary |
| `SHA256SUMS.txt` | Checksums of all of the above |

### Windows 11 (x64)

Run `FreeHM-<version>-windows-x64-setup.exe`.

* **Install for all users** (default, one administrator prompt): installs into
  `Program Files` and optionally registers the **hardware helper service**, which provides
  FPS/frame time and, if [PawnIO](https://pawnio.eu) is installed, CPU temperature.
* **Install for me only**: no administrator prompt. Everything except the helper works.
  You can add the helper later under *Settings ▸ Startup & permissions*. That asks for
  administrator rights once.

FreeHM starts at sign-in by default (per-user `Run` key, no elevation). The first start
opens the settings window once.

CPU temperature on Windows needs PawnIO, a signed open-source driver that FreeHM uses but
does not bundle or download. Install it from <https://pawnio.eu> if you want CPU
temperature. FreeHM detects it automatically.

### Linux

* **AppImage**: `chmod +x FreeHM-<version>-x86_64.AppImage && ./FreeHM-<version>-x86_64.AppImage`
* **Debian/Ubuntu**: `sudo apt install ./freehm_<version>_amd64.deb`

FreeHM adds an XDG autostart entry (`~/.config/autostart/freehm.desktop`) on first run.

**CPU power on Linux** needs read access to the RAPL energy counter, which kernels since 5.10
reserve for root (CVE-2020-8694). *Settings ▸ Startup & permissions ▸ Set up sensor access*
runs a one-time `pkexec` step. It adds you to a `freehm` group and installs a udev rule that
makes the counter readable (read-only) by that group. Log out and back in afterwards. All
other Linux metrics need no setup.

## Use

| Action | How |
|---|---|
| Show/hide overlay | **Ctrl+Shift+O** (configurable), tray menu, or `freehm --toggle` |
| Settings | left-click the tray icon, or run `freehm` again |
| Quit | tray menu ▸ Quit, or `freehm --quit` |

By default the overlay appears automatically while a game is in the foreground and hides on
the desktop. A shortcut press overrides that until the active game changes.

Command line: `freehm [--background | --settings | --toggle | --show | --hide | --quit |
--status | --apply-config FILE | --mock | --version]`.
* `--status` prints the running instance's full status as JSON.
* `--apply-config` validates a settings file (same format as `config.json`), then applies and
  saves it. It rejects out-of-range values rather than adjusting them.
* `--mock` runs with simulated sensors for testing.

On Windows, `freehm.exe` is a GUI program, so PowerShell waits for it and reports its exit
code only when its output is piped (`freehm --apply-config f.json | Out-String`).

### Settings

All changes apply immediately, with a live preview, and never need administrator rights.

* **General**: show/hide now, update interval (default **200 ms**, 50–5000 ms), units
  (°C/°F, MHz/GHz), restore defaults.
* **Overlay**: font (bundled Exo 2, any installed font, or a font file), size, weight,
  colours, background and overall opacity, padding/spacing, stacked, grouped or single-line layout,
  compact labels, scale, monitor, corner presets or custom position, edge margins, and
  "hide from screen capture" (Windows).
* **Metrics**: enable, reorder, rename and group each metric individually; optional
  "used / total".
* **Hardware**: choose the CPU/GPU to monitor, see every sensor's state and provider.
* **Game detection**: automatic detection on/off, "all detected games" or "selected games
  only", manual game list, exclusion list, and a "right now" panel that shows why the
  current foreground app is or isn't treated as a game.
* **Shortcuts**: record a new shortcut. Conflicts are reported, never hidden.
* **Startup & permissions**: start at login, helper service / sensor-access setup,
  repair and removal.
* **About & diagnostics**: versions, providers, paths, a "copy diagnostics" button and
  licences.

Settings live in `%APPDATA%\FreeHM\config.json` (Windows) or
`$XDG_CONFIG_HOME/freehm/config.json` (Linux). They are versioned, saved atomically and
validated on load. A damaged file is kept as `config.json.corrupt-<n>` and defaults are used.

## Hardware support

| Metric | Windows | Linux |
|---|---|---|
| CPU % | PDH `% Processor Utility` | `/proc/stat` |
| CPU clock | PDH processor frequency × performance | cpufreq `scaling_cur_freq` (mean) |
| CPU temp | helper + PawnIO (Intel MSR / AMD SMN) | hwmon `coretemp`, `k10temp`, `zenpower` |
| CPU power | Intel: Windows Energy Meter (PDH); AMD: helper + PawnIO | powercap RAPL (one-time setup) |
| GPU (NVIDIA) | NVML | NVML |
| GPU (AMD/Intel) | WDDM `D3DKMT` statistics + PDH GPU memory | amdgpu / i915 / xe sysfs + hwmon |
| RAM | `GlobalMemoryStatusEx` | `/proc/meminfo` |
| FPS / frame time | helper: ETW present events (as PresentMon) | not available (see below) |

Values a device does not expose (for example power draw on many integrated GPUs) are shown
as unavailable, with the reason in *Settings ▸ Hardware*.

## Known limitations

* **Exclusive fullscreen (Windows):** an exclusive-fullscreen Direct3D game can draw
  above every window, so the overlay may be hidden. FreeHM detects this and says so. Use
  borderless/windowed fullscreen. FreeHM does not hook games to get around this.
* **FPS on Linux:** no system-wide, non-invasive frame-timing interface exists. Tools that
  show FPS on Linux (MangoHud and similar) load a layer into the game. FreeHM doesn't, so
  FPS and frame time show "unavailable" with that reason.
* **Wayland:** the overlay needs the `wlr-layer-shell` protocol (KDE Plasma, Hyprland,
  Sway, niri, COSMIC, …). GNOME doesn't offer it; there FreeHM uses an XWayland window,
  which may not appear above native Wayland fullscreen windows. Global shortcuts on Wayland
  go through the desktop's GlobalShortcuts portal (KDE, GNOME 48+, Hyprland). The desktop
  asks you to confirm and may assign a different key. Without the portal, bind
  `freehm --toggle` in your desktop's keyboard settings. Wayland doesn't say which window
  has focus (except on Hyprland and Sway), so game detection there falls back to process
  scanning.
* **Tray on GNOME** needs the AppIndicator extension (enabled by default on Ubuntu and
  Fedora). Without it, run `freehm` again to open settings.
* **Anti-cheat:** FreeHM is designed to look like what it is, a transparent hardware
  monitor using documented APIs. No promise can be made about any particular anti-cheat
  product. You can exclude a game, hide the overlay, or disable the helper entirely
  (*Settings ▸ Hardware*).

## Troubleshooting

* **Overlay doesn't appear in a game:** *Settings ▸ Game detection ▸ Right now* shows how
  the game was classified. Add it to the manual list if needed. Check for an
  exclusive-fullscreen warning on the General page.
* **Shortcut doesn't work:** *Settings ▸ Shortcuts* shows whether registration succeeded
  and why not. Pick another combination if another program owns it.
* **A metric says N/A:** *Settings ▸ Hardware* lists the reason, for example "PawnIO is not
  installed" or "needs the one-time sensor permission setup".
* **Logs:** `%LOCALAPPDATA%\FreeHM\logs` (Windows), `$XDG_STATE_HOME/freehm` (Linux), and
  `%ProgramData%\FreeHM\helper.log` for the helper. *About ▸ Copy diagnostics* collects
  everything useful for a bug report.

## Uninstall

* **Windows:** *Settings ▸ Apps ▸ FreeHM ▸ Uninstall*. This removes the helper service,
  the startup entry and, if you choose, your settings.
* **Linux:** remove the package or AppImage. Run `freehm --remove-permissions` (as root)
  first if you did the sensor setup; the `.deb` package does this automatically. Delete
  `~/.config/autostart/freehm.desktop` if you used the AppImage.

## Checking that the values are correct

`tools/verify/accuracy.ps1` (Windows) and `tools/verify/accuracy.py` (Linux) compare every
value FreeHM shows with sources FreeHM does not go through. These include Task Manager's
counters or `/proc`, the Windows Energy Meter or RAPL, hwmon, and `nvidia-smi`. Each
check passes or fails against a stated tolerance; a value with no independent source on
your machine is reported as SKIP. FreeHM must be running:

```sh
pwsh tools/verify/accuracy.ps1          # Windows
python3 tools/verify/accuracy.py        # Linux
```

CI runs both on every change. Results from real hardware are in
[docs/TESTING.md](docs/TESTING.md#accuracy).

## Releases

Pushing a tag `vX.Y.Z` that matches the version in `crates/freehm/Cargo.toml` runs
`.github/workflows/release.yml`. It runs the full CI pipeline (build, tests, end-to-end
tests and the accuracy check on Windows and Linux), then publishes a GitHub release with
the files listed under [Install](#install). Starting the workflow by hand makes a dry run:
everything is built and kept as workflow artifacts, and nothing is published.

## Build from source

Requires Rust 1.88+.

```sh
cargo test --workspace
cargo build --release --workspace     # target/release/freehm(.exe), freehm-helper.exe
```

* Windows installer: Inno Setup 6, `ISCC /DAppVersion=0.1.0 installer\windows\freehm.iss`
* Linux: `cargo deb -p freehm`; AppImage steps are in `.github/workflows/ci.yml`
* Licence list: `cargo about generate about.hbs -o dist/THIRD_PARTY_LICENSES.html`

## Documentation

* [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): components, technology choices and why
* [docs/SECURITY.md](docs/SECURITY.md): permission model, helper service, anti-cheat design
* [docs/TESTING.md](docs/TESTING.md): test matrix, results, performance measurements,
  untested areas
* [docs/REPORT.md](docs/REPORT.md): engineering report: what was built, how, what is verified

## License

MIT. Bundled third-party components are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). The Exo 2 font (SIL OFL 1.1) stands in
for NVIDIA Sans, which may not be redistributed. FreeHM is not affiliated with NVIDIA.
