# Security and permission design

## What FreeHM never does

* Inject code or load DLLs/layers into game processes, or hook graphics APIs.
* Open game processes for memory access. On Windows, processes are opened only with
  `PROCESS_QUERY_LIMITED_INFORMATION` (image path and exit code). On Linux, only `/proc`
  metadata (`stat`, `exe`, `cmdline`, and `environ` of the user's own processes) is read.
* Modify game files, or hide itself, its service or its activity from security software.
* Ship or load a kernel driver of its own. The optional PawnIO driver is installed by the
  user from its own project.
* Read keyboard devices (no evdev access). Shortcuts use the OS's registration APIs.
* Contact the network.

## Privilege map

| Piece | Runs as | Elevation |
|---|---|---|
| `freehm` (overlay, tray, telemetry, detection) | the signed-in user | never |
| `freehm --settings` (settings window) | the signed-in user | never; buttons that need admin launch a separate, explicit UAC/pkexec step |
| `freehm-helper` service (Windows, optional) | LocalSystem | installed once |
| `freehm --setup-permissions` (Linux, optional) | root via `pkexec`, once | one prompt |

Routine actions (start, show/hide, every settings change, changing the shortcut,
autostart on/off) never request elevation. Autostart uses the per-user `Run` key or the XDG
autostart directory.

## Windows helper service (`freehm-helper.exe`)

**Why it exists:** two metrics have no unprivileged Windows API.

* **CPU temperature** needs model-specific registers (Intel) or the SMN bus (AMD). FreeHM
  reads them through PawnIO, a signed driver that runs small, signed, purpose-built modules
  instead of exposing raw hardware access. Opening PawnIO requires administrator rights
  (verified: E_ACCESSDENIED for a standard user).
* **FPS/frame time** come from the kernel-mode `Microsoft-Windows-DxgKrnl` ETW provider,
  plus DXGI/D3D9 present events. Real-time sessions with kernel providers need
  administrator rights or membership in *Performance Log Users*.

**What it can do:** exactly one request: `{v, cpu: bool, frames: bool}` (serde
`deny_unknown_fields`, 256-byte limit checked before parsing). The response holds the CPU
package temperature and power, and per-process present statistics (pid, API, FPS, frame
time) for processes presenting frames. A client cannot name an MSR, an SMN address, a file,
a process or a command. The MSR/SMN addresses are constants in the helper. The PawnIO
modules (IntelMSR, AMDFamily17) themselves only allow a fixed set of read-only registers.

**Hardening:**

* Named pipe `\\.\pipe\FreeHM.Helper.v1` with DACL
  `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x120083;;;IU)`: interactive users may read and write but
  not create pipe instances, so they cannot impersonate the server.
  `FILE_FLAG_FIRST_PIPE_INSTANCE` stops another process from pre-creating the name, and
  `PIPE_REJECT_REMOTE_CLIENTS` blocks network access.
* The app verifies that the pipe's server PID (`GetNamedPipeServerProcessId`) is the
  process the Service Control Manager reports for `FreeHMHelper`, so a squatter cannot feed
  it fake readings.
* At most 4 concurrent clients, at least 20 ms between requests per client, and responses
  capped at 64 KB.
* The service's required privileges are reduced to `SeChangeNotifyPrivilege` and
  `SeCreateGlobalPrivilege`. Its binary lives under `Program Files`, where only
  administrators can write; installing from elsewhere copies it there first. It restarts
  on failure.
* `PawnIOLib.dll` is loaded by absolute path from PawnIO's installation directory, never
  from the DLL search path. The embedded module blobs are the unmodified signed release
  files, and their SHA-256 hashes are checked by a unit test.
* PawnIO is opened only while CPU readings are requested, and released after 30 s idle.
  The ETW session runs only while frames are requested, and stops after 10 s idle.
* The log directory `%ProgramData%\FreeHM` is created with a protected ACL (SYSTEM and
  Administrators full control, Users read), or used only if it is owned by SYSTEM or
  Administrators. `install`/`uninstall --log` write only to `freehm-helper-*.log`
  directly inside the caller's temp directory.

**Information exposed to local interactive users:** CPU temperature and power, and the
PIDs and frame rates of processes presenting frames. Other local users can see the same
process information in Task Manager; frame rates are not otherwise exposed.

**Turning it off:** *Settings ▸ Hardware ▸ "Use the FreeHM helper …"* stops FreeHM from
contacting it, for example for a game whose anti-cheat objects to drivers. *Settings ▸
Startup & permissions ▸ Uninstall helper* (one UAC prompt), the Windows uninstaller, or
`freehm-helper.exe uninstall` from an elevated prompt remove it.

## Linux permission setup

Only the RAPL energy counter (`/sys/class/powercap/intel-rapl*/energy_uj`, CPU power) is
root-only. It was restricted because high-resolution energy readings enable side-channel
attacks (CVE-2020-8694, "PLATYPUS"). FreeHM does **not** make it world-readable. The setup,
run once as root through `pkexec` (polkit's standard admin authentication):

1. creates the system group `freehm` and adds the requesting user (the `--user` name must
   match `PKEXEC_UID`);
2. installs `/etc/udev/rules.d/70-freehm.rules`, which sets the counter to
   `root:freehm 0440` (read-only, nothing writable);
3. applies the rule to the current counters.

`freehm --remove-permissions` (and removing the `.deb`) deletes the rule, restores
`root:root 0400`, and deletes the group. Exit codes from `pkexec` are reported in the UI:
126 means cancelled, 127 means not authorised.

The IPC socket lives in `$XDG_RUNTIME_DIR/freehm` (mode 0700, ownership verified), and
connections from other UIDs are rejected (`SO_PEERCRED`).

## Anti-cheat considerations

FreeHM tries to look like what it is: a hardware monitor using the same documented
interfaces as Task Manager, NVIDIA's tools and PresentMon. Its overlay is an ordinary
top-level window owned by FreeHM, and nothing touches the game process. This reduces, but
cannot eliminate, the chance of a conflict. Anti-cheat products make their own decisions,
and some refuse to run alongside *any* third-party driver (which would include PawnIO).
FreeHM therefore makes every potentially sensitive part optional: the helper can be
disabled or uninstalled, a game can be excluded, and the overlay can be turned off.

No claim is made that FreeHM is undetectable or approved by any anti-cheat vendor.

## Reporting a vulnerability

Please open a private security advisory on the GitHub repository rather than a public
issue.
