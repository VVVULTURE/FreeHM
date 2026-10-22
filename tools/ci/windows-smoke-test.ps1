# End-to-end checks of the Windows build on a CI machine (shows an overlay on that
# machine's desktop; do not run on a PC where someone is playing):
#   overlay window styles, click-through hit testing, Ctrl+Shift+O, --quit cleanup,
#   helper service install / selftest (ETW) / uninstall (needs an elevated shell).
# Usage: tools\ci\windows-smoke-test.ps1 [-Bin target\release] [-Out ci-out] [-Helper]
param([string]$Bin = "target\release", [string]$Out = "ci-out", [switch]$Helper)
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $Out | Out-Null
$exe = (Resolve-Path "$Bin\freehm.exe").Path
$env:FREEHM_CONFIG_DIR = Join-Path ([IO.Path]::GetTempPath()) ("freehm-ci-" + [guid]::NewGuid())

function Fail($m) { Write-Host "FAIL: $m"; exit 1 }
function Pass($m) { Write-Host "PASS: $m" }
function Status { (& $exe --status | Out-String) | ConvertFrom-Json }
# freehm.exe is a GUI-subsystem program: PowerShell waits for it (and sets $LASTEXITCODE)
# only when its output is piped, so every command whose result matters is piped.

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
    [DllImport("user32.dll")] public static extern long GetWindowLongPtr(IntPtr h, int i);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    public static void Chord(byte[] keys) {
        foreach (var k in keys) keybd_event(k, 0, 0, UIntPtr.Zero);
        Array.Reverse(keys);
        foreach (var k in keys) keybd_event(k, 0, 2, UIntPtr.Zero);
    }
}
"@

# Start with autostart off so the machine's own startup entry is never pointed at this build.
New-Item -ItemType Directory -Force $env:FREEHM_CONFIG_DIR | Out-Null
@{ schema_version = 1; general = @{ start_with_system = $false }; permissions = @{ first_run_done = $true } } | ConvertTo-Json -Depth 5 | Set-Content "$env:FREEHM_CONFIG_DIR\config.json" -Encoding utf8
$runKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
$prevRun = (Get-ItemProperty $runKey -Name FreeHM -ErrorAction SilentlyContinue).FreeHM

$app = Start-Process $exe -ArgumentList "--mock" -PassThru
$ok = $false
for ($i = 0; $i -lt 75; $i++) { Start-Sleep -Milliseconds 200; try { $null = Status; $ok = $true; break } catch {} }
if (-not $ok) { Fail "FreeHM did not start" }

& $exe --show; Start-Sleep -Seconds 1
$s = Status
$s | ConvertTo-Json -Depth 12 | Set-Content "$Out\windows-status.json"
if (-not $s.overlay.visible) { Fail "overlay not visible: $($s.overlay.reason)" }
Pass "overlay visible with backend: $($s.overlay.backend)"

$h = [W]::FindWindow("FreeHM.Overlay", "FreeHM Overlay")
if ($h -eq [IntPtr]::Zero) { Fail "overlay window not found" }
$ex = [W]::GetWindowLongPtr($h, -20)
$need = @{ LAYERED = 0x80000; TRANSPARENT = 0x20; TOPMOST = 0x8; TOOLWINDOW = 0x80; NOACTIVATE = 0x8000000 }
foreach ($k in $need.Keys) { if (($ex -band $need[$k]) -eq 0) { Fail "overlay lacks WS_EX_$k" } }
Pass ("overlay extended style 0x{0:X} has LAYERED|TRANSPARENT|TOPMOST|TOOLWINDOW|NOACTIVATE" -f $ex)
if ([W]::GetForegroundWindow() -eq $h) { Fail "overlay took the foreground" }
Pass "overlay is not the foreground window"

$r = New-Object W+RECT; [void][W]::GetWindowRect($h, [ref]$r)
$p = New-Object W+POINT; $p.X = $r.L + 6; $p.Y = $r.T + 6
$under = [W]::WindowFromPoint($p)
if ($under -eq $h) { Fail "hit testing at the overlay returns the overlay (not click-through)" }
Pass "click-through: WindowFromPoint($($p.X),$($p.Y)) = 0x$('{0:X}' -f [int64]$under), not the overlay"

if (-not $s.hotkey.registered) { Fail "shortcut not registered: $($s.hotkey.error)" }
[W]::Chord([byte[]](0x11, 0x10, 0x4F)); Start-Sleep -Milliseconds 700
if ((Status).overlay.visible) { Fail "Ctrl+Shift+O did not hide the overlay" }
if ([W]::IsWindowVisible($h)) { Fail "overlay window still visible after hide" }
[W]::Chord([byte[]](0x11, 0x10, 0x4F)); Start-Sleep -Milliseconds 700
if (-not (Status).overlay.visible) { Fail "Ctrl+Shift+O did not show the overlay again" }
Pass "Ctrl+Shift+O (RegisterHotKey) toggles the overlay"

# Settings changes go through the same IPC request the settings window sends.
$cfgFile = Join-Path $env:FREEHM_CONFIG_DIR "edited.json"
$c = (Status).config
$c.hotkeys.toggle_overlay = "Ctrl+Shift+Alt+K"
$c.general.refresh_interval_ms = 500
$c.general.start_with_system = $true
$c.overlay.anchor = "BottomLeft"; $c.overlay.font_size = 21.0; $c.overlay.font = @{ System = @{ family = "Segoe UI"; path = $null } }
($c.metrics | Where-Object id -eq "CpuClock").enabled = $true
$c.detection.manual_games = @("mygame.exe")
$c | ConvertTo-Json -Depth 12 | Set-Content $cfgFile -Encoding utf8
$msg = & $exe --apply-config $cfgFile 2>&1 | Out-String; if ($LASTEXITCODE -ne 0) { Fail "--apply-config failed: $msg" }
$s = Status
if (-not $s.hotkey.registered -or $s.hotkey.shortcut -ne "Ctrl+Shift+Alt+K") { Fail "new shortcut not active: $($s.hotkey | ConvertTo-Json -Compress)" }
[W]::Chord([byte[]](0x11, 0x10, 0x4F)); Start-Sleep -Milliseconds 700
if (-not (Status).overlay.visible) { Fail "the old shortcut Ctrl+Shift+O still works after changing it" }
[W]::Chord([byte[]](0x11, 0x10, 0x12, 0x4B)); Start-Sleep -Milliseconds 700
if ((Status).overlay.visible) { Fail "the new shortcut Ctrl+Shift+Alt+K does not toggle the overlay" }
[W]::Chord([byte[]](0x11, 0x10, 0x12, 0x4B)); Start-Sleep -Milliseconds 700
Pass "shortcut changed at runtime: Ctrl+Shift+Alt+K toggles, Ctrl+Shift+O released"
$run = (Get-ItemProperty $runKey -Name FreeHM -ErrorAction SilentlyContinue).FreeHM
if (-not $s.autostart.enabled -or $run -notlike "*freehm.exe*") { Fail "enabling startup did not create the Run entry ($run)" }
Pass "start with Windows on: $runKey\FreeHM = $run"
$c.general.start_with_system = $false
$c | ConvertTo-Json -Depth 12 | Set-Content $cfgFile -Encoding utf8
$msg = & $exe --apply-config $cfgFile 2>&1 | Out-String; if ($LASTEXITCODE -ne 0) { Fail "--apply-config failed: $msg" }
if ((Get-ItemProperty $runKey -Name FreeHM -ErrorAction SilentlyContinue) -or (Status).autostart.enabled) { Fail "disabling startup left the Run entry" }
Pass "start with Windows off: Run entry removed"
$bad = $c | ConvertTo-Json -Depth 12 | ConvertFrom-Json; $bad.general.refresh_interval_ms = 5
$bad | ConvertTo-Json -Depth 12 | Set-Content $cfgFile -Encoding utf8
$msg = & $exe --apply-config $cfgFile 2>&1 | Out-String
if ($LASTEXITCODE -eq 0) { Fail "an out-of-range interval was accepted" }
if ((Status).config.general.refresh_interval_ms -ne 500) { Fail "a rejected configuration changed the running settings" }
Pass "invalid configuration rejected without changing anything: $($msg.Trim())"

& $exe --quit
if (-not $app.WaitForExit(10000)) { Fail "FreeHM did not exit after --quit" }
if ([W]::FindWindow("FreeHM.Overlay", "FreeHM Overlay") -ne [IntPtr]::Zero) { Fail "overlay window left behind" }
Pass "--quit exits (code $($app.ExitCode)) and leaves no overlay window"
Copy-Item "$env:FREEHM_CONFIG_DIR\logs\*.log" $Out -ErrorAction SilentlyContinue

# The edited settings survive a restart.
$app = Start-Process $exe -ArgumentList "--mock" -PassThru
$ok = $false
for ($i = 0; $i -lt 75; $i++) { Start-Sleep -Milliseconds 200; try { $s = Status; $ok = $true; break } catch {} }
if (-not $ok) { Fail "FreeHM did not restart" }
$k = $s.config
$checks = @{ shortcut = $k.hotkeys.toggle_overlay -eq "Ctrl+Shift+Alt+K"; interval = $k.general.refresh_interval_ms -eq 500; anchor = $k.overlay.anchor -eq "BottomLeft"
             font = $k.overlay.font.System.family -eq "Segoe UI" -and $k.overlay.font_size -eq 21; metric = ($k.metrics | Where-Object id -eq "CpuClock").enabled; games = $k.detection.manual_games -contains "mygame.exe"
             startup = -not $k.general.start_with_system }
$lost = $checks.Keys | Where-Object { -not $checks[$_] }
if ($lost) { Fail "settings lost across restart: $lost" }
if (-not $s.hotkey.registered) { Fail "saved shortcut not registered after restart: $($s.hotkey.error)" }
Pass "settings persisted across restart (shortcut, interval, position, font, metric selection, manual games, startup)"
& $exe --quit | Out-Null; $null = $app.WaitForExit(10000)
if ($prevRun) { Set-ItemProperty $runKey -Name FreeHM -Value $prevRun }

if ($Helper) {
    $hx = (Resolve-Path "$Bin\freehm-helper.exe").Path
    & $hx install 2>&1 | Tee-Object "$Out\helper-install.txt"
    if ($LASTEXITCODE -ne 0) { Fail "helper install failed" }
    $st = (& $hx status | Out-String).Trim()
    if ($st -notmatch "Running") { Fail "helper not running after install: $st" }
    Pass "helper service installed: $st"
    $svc = Get-CimInstance Win32_Service -Filter "Name='FreeHMHelper'"
    Pass "helper service: account $($svc.StartName), start mode $($svc.StartMode), path $($svc.PathName)"
    & sc.exe qprivs FreeHMHelper | Tee-Object "$Out\helper-privileges.txt"
    # A real D3D11 swap chain (WARP on runners without a GPU) for the frame-timing checks.
    $pl = Join-Path (Split-Path $hx) "examples\present_loop.exe"
    $presenter = if (Test-Path $pl) { Start-Process $pl -ArgumentList "100" -PassThru -RedirectStandardOutput "$Out\present-loop.txt" } else { $null }
    Start-Sleep -Seconds 2
    $self = & $hx selftest 2>&1 | Tee-Object "$Out\helper-selftest.txt" | Out-String
    if ($presenter) {
        $m = [regex]::Match($self, "pid\s+$($presenter.Id)\s+\w+\s+([\d.]+) fps")
        if (-not $m.Success) { Fail "helper ETW did not see the test presenter (pid $($presenter.Id)):`n$self" }
        if ([double]$m.Groups[1].Value -lt 10) { Fail "presenter measured at only $($m.Groups[1].Value) fps" }
        Pass "helper ETW frame timing sees the D3D11 presenter at $($m.Groups[1].Value) fps"

        # The service itself, asked once a second (a 1000 ms refresh): every answer after
        # warm-up must include the presenter. ETW buffers used to reach the helper 1-2 s
        # late unless requests were frequent, so slow refresh rates lost the FPS reading.
        # Ask only for the rights the pipe's ACL grants (InOut would add write-attributes).
        function HelperConnect {
            $rights = [IO.Pipes.PipeAccessRights]::ReadData -bor [IO.Pipes.PipeAccessRights]::WriteData -bor [IO.Pipes.PipeAccessRights]::ReadAttributes -bor [IO.Pipes.PipeAccessRights]::Synchronize
            $c = New-Object IO.Pipes.NamedPipeClientStream(".", "FreeHM.Helper.v1", $rights, [IO.Pipes.PipeOptions]::None, [Security.Principal.TokenImpersonationLevel]::Identification, [IO.HandleInheritability]::None)
            $c.Connect(5000); $c
        }
        function HelperAsk($c) {
            $body = [Text.Encoding]::UTF8.GetBytes('{"v":1,"cpu":false,"frames":true}')
            $c.Write([BitConverter]::GetBytes([uint32]$body.Length), 0, 4); $c.Write($body, 0, $body.Length); $c.Flush()
            $read = { param($b, $len) $n = 0; while ($n -lt $len) { $k = $c.Read($b, $n, $len - $n); if ($k -le 0) { throw "helper pipe closed" }; $n += $k } }
            $hdr = New-Object byte[] 4; & $read $hdr 4
            $buf = New-Object byte[] ([BitConverter]::ToUInt32($hdr, 0)); & $read $buf $buf.Length
            [Text.Encoding]::UTF8.GetString($buf) | ConvertFrom-Json
        }
        $pipe = HelperConnect
        $hits = 0; $ages = @()
        for ($i = 0; $i -lt 12; $i++) {
            $r = HelperAsk $pipe
            $mine = $r.frames.processes | Where-Object pid -eq $presenter.Id
            if ($i -ge 2 -and $mine) { $hits++; $ages += $mine.stats.last_present_age_ms }
            Start-Sleep -Seconds 1
        }
        $pipe.Dispose()
        $maxAge = ($ages | Measure-Object -Maximum).Maximum
        if ($hits -lt 10) { Fail "helper service at a 1 s request interval reported the presenter in only $hits of 10 answers" }
        # The flush thread runs every 200 ms; 800 ms leaves room for a slow runner.
        if ($maxAge -gt 800) { Fail "helper service at a 1 s request interval: newest present was up to $maxAge ms old (events delivered late)" }
        Pass "helper service at a 1 s request interval: presenter in $hits/10 answers, newest present at most $([math]::Round($maxAge)) ms old"

        # With no requests the helper stops its ETW session after 10 s; the next request
        # must start a working one again (the session and its flush thread are recreated).
        Start-Sleep -Seconds 14
        $stops = @(Select-String -Path "$env:ProgramData\FreeHM\helper.log" -Pattern "frame-timing session stopped").Count
        if ($stops -lt 1) { Fail "the helper did not stop its idle frame-timing session within 14 s" }
        $pipe = HelperConnect; $seen = $false
        for ($i = 0; $i -lt 10 -and -not $seen; $i++) {
            Start-Sleep -Milliseconds 500
            $seen = [bool]((HelperAsk $pipe).frames.processes | Where-Object pid -eq $presenter.Id)
        }
        $pipe.Dispose()
        if (-not $seen) {
            Copy-Item "$env:ProgramData\FreeHM\helper.log" "$Out\helper.log" -ErrorAction SilentlyContinue
            Fail "after an idle stop, the restarted frame-timing session did not see the presenter within 5 s"
        }
        Pass "helper restarts its frame-timing session after an idle stop and sees the presenter again"

        # Whole path: app -> helper service (named pipe) -> ETW -> overlay readings.
        $cfg = Join-Path ([IO.Path]::GetTempPath()) ("freehm-ci-fps-" + [guid]::NewGuid())
        New-Item -ItemType Directory $cfg | Out-Null
        @{ schema_version = 1; general = @{ start_with_system = $false }; detection = @{ auto_detect = $false; manual_games = @("present_loop.exe") }
           hotkeys = @{ toggle_overlay = "Ctrl+Alt+Shift+F12" }; permissions = @{ first_run_done = $true } } | ConvertTo-Json -Depth 5 | Set-Content "$cfg\config.json" -Encoding utf8
        $env:FREEHM_CONFIG_DIR = $cfg
        # Game detection follows the foreground window. Windows only lets the process with
        # the last input move the foreground, so press and release Alt first.
        $pw = [W]::FindWindow("FreeHM.PresentLoop", "FreeHM present loop")
        [W]::Chord([byte[]](0x12)); [void][W]::SetForegroundWindow($pw); Start-Sleep -Milliseconds 300
        if ([W]::GetForegroundWindow() -ne $pw) { Fail "could not bring the test presenter to the foreground" }
        $app2 = Start-Process $exe -ArgumentList "--background" -PassThru
        $fps = $null
        for ($i = 0; $i -lt 30 -and -not $fps; $i++) {
            Start-Sleep -Seconds 1
            try { $s2 = Status } catch { continue }
            $fps = $s2.snapshot.readings | Where-Object { $_.metric -eq "Fps" -and $null -ne $_.value } | Select-Object -First 1
        }
        $s2 | ConvertTo-Json -Depth 12 | Set-Content "$Out\windows-status-fps.json"
        & $exe --quit | Out-Null; $null = $app2.WaitForExit(10000)
        if (-not $fps) {
            # Tell a helper problem from an app problem: what does the service itself see now?
            $seen = try { $c = HelperConnect; $r = HelperAsk $c; $c.Dispose(); ($r.frames.processes | ForEach-Object { "$($_.pid) $([math]::Round($_.stats.fps,1)) fps" }) -join ", " } catch { "query failed: $_" }
            Copy-Item "$env:ProgramData\FreeHM\helper.log" "$Out\helper.log" -ErrorAction SilentlyContinue
            Fail "app shows no FPS for the detected game (pid $($presenter.Id), presenter exited: $($presenter.HasExited)): $(($s2.snapshot.readings | Where-Object metric -eq 'Fps').reason); helper sees: [$seen]"
        }
        $ft = $s2.snapshot.readings | Where-Object { $_.metric -eq "FrameTime" } | Select-Object -First 1
        Pass "app reads FPS $([math]::Round($fps.value,1)) / frame time $([math]::Round($ft.value,2)) ms for '$($s2.game.active.name)' through the helper ($($fps.freshness))"
        Stop-Process -Id $presenter.Id -ErrorAction SilentlyContinue
    }
    Copy-Item "$env:ProgramData\FreeHM\helper.log" "$Out\helper.log" -ErrorAction SilentlyContinue
    & $hx uninstall 2>&1 | Tee-Object "$Out\helper-uninstall.txt"
    $st = (& $hx status | Out-String).Trim()
    if ($st -notmatch "not installed") { Fail "helper still installed after uninstall: $st" }
    Pass "helper service removed"
}
Write-Host "all Windows checks passed"
