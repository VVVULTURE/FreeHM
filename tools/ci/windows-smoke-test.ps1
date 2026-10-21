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

& $exe --quit
if (-not $app.WaitForExit(10000)) { Fail "FreeHM did not exit after --quit" }
if ([W]::FindWindow("FreeHM.Overlay", "FreeHM Overlay") -ne [IntPtr]::Zero) { Fail "overlay window left behind" }
Pass "--quit exits (code $($app.ExitCode)) and leaves no overlay window"
Copy-Item "$env:FREEHM_CONFIG_DIR\logs\*.log" $Out -ErrorAction SilentlyContinue

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
    $presenter = if (Test-Path $pl) { Start-Process $pl -ArgumentList "40" -PassThru -RedirectStandardOutput "$Out\present-loop.txt" } else { $null }
    Start-Sleep -Seconds 2
    $self = & $hx selftest 2>&1 | Tee-Object "$Out\helper-selftest.txt" | Out-String
    if ($presenter) {
        $m = [regex]::Match($self, "pid\s+$($presenter.Id)\s+\w+\s+([\d.]+) fps")
        if (-not $m.Success) { Fail "helper ETW did not see the test presenter (pid $($presenter.Id)):`n$self" }
        if ([double]$m.Groups[1].Value -lt 10) { Fail "presenter measured at only $($m.Groups[1].Value) fps" }
        Pass "helper ETW frame timing sees the D3D11 presenter at $($m.Groups[1].Value) fps"

        # Whole path: app -> helper service (named pipe) -> ETW -> overlay readings.
        $cfg = Join-Path ([IO.Path]::GetTempPath()) ("freehm-ci-fps-" + [guid]::NewGuid())
        New-Item -ItemType Directory $cfg | Out-Null
        @{ schema_version = 1; general = @{ start_with_system = $false }; detection = @{ auto_detect = $false; manual_games = @("present_loop.exe") }
           hotkeys = @{ toggle_overlay = "Ctrl+Alt+Shift+F12" }; permissions = @{ first_run_done = $true } } | ConvertTo-Json -Depth 5 | Set-Content "$cfg\config.json" -Encoding utf8
        $env:FREEHM_CONFIG_DIR = $cfg
        $app2 = Start-Process $exe -ArgumentList "--background" -PassThru
        $fps = $null
        for ($i = 0; $i -lt 30 -and -not $fps; $i++) {
            Start-Sleep -Seconds 1
            try { $s2 = Status } catch { continue }
            $fps = $s2.snapshot.readings | Where-Object { $_.metric -eq "Fps" -and $null -ne $_.value } | Select-Object -First 1
        }
        $s2 | ConvertTo-Json -Depth 12 | Set-Content "$Out\windows-status-fps.json"
        & $exe --quit | Out-Null; $null = $app2.WaitForExit(10000)
        if (-not $fps) { Fail "app shows no FPS for the detected game: $(($s2.snapshot.readings | Where-Object metric -eq 'Fps').reason)" }
        $ft = $s2.snapshot.readings | Where-Object { $_.metric -eq "FrameTime" } | Select-Object -First 1
        Pass "app reads FPS $([math]::Round($fps.value,1)) / frame time $([math]::Round($ft.value,2)) ms for '$($s2.game.active.name)' through the helper ($($fps.freshness))"
        Stop-Process -Id $presenter.Id -ErrorAction SilentlyContinue
    }
    & $hx uninstall 2>&1 | Tee-Object "$Out\helper-uninstall.txt"
    $st = (& $hx status | Out-String).Trim()
    if ($st -notmatch "not installed") { Fail "helper still installed after uninstall: $st" }
    Pass "helper service removed"
}
Write-Host "all Windows checks passed"
