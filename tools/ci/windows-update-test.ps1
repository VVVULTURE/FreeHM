# End-to-end check of the Windows updater with the real installer, on a CI machine (shows
# windows on that machine's desktop; do not run on a PC where someone is playing). A release
# served from this machine plays GitHub's part:
#   the update is found and waits to be asked for; a file that does not match its checksum is
#   refused; an installer that fails is reported; nothing is installed while a game runs; then
#   the silent install replaces the running app (and an open settings window) and restarts it.
# Needs an elevated shell. Usage:
#   tools\ci\windows-update-test.ps1 -Setup <setup> -NewSetup <setup of 9.9.9> -PresentLoop <present_loop.exe> [-Out ci-out]
param([string]$Setup, [string]$NewSetup, [string]$PresentLoop, [string]$Out = "ci-out")
$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force $Out | Out-Null
$Setup = (Resolve-Path $Setup).Path; $NewSetup = (Resolve-Path $NewSetup).Path; $PresentLoop = (Resolve-Path $PresentLoop).Path

function Fail($m) { Write-Host "FAIL: $m"; Copy-Item "$cfg\logs\*.log" $Out -ErrorAction SilentlyContinue; Cleanup; exit 1 }
function Pass($m) { Write-Host "PASS: $m" }
# freehm.exe is a GUI-subsystem program: piped, so that PowerShell waits for it.
function Status { (& $exe --status | Out-String) | ConvertFrom-Json }
function Installed { (Get-ItemProperty $uninstall -ErrorAction SilentlyContinue).DisplayVersion }
function WaitFor($what, [scriptblock]$cond, $seconds = 30) {
    for ($i = 0; $i -lt $seconds * 2; $i++) {
        try { if (& $cond) { return } } catch {}
        Start-Sleep -Milliseconds 500
    }
    $u = try { (Status).update | ConvertTo-Json -Compress } catch { "no status: $_" }
    Fail "timed out waiting for $what; update status: $u"
}
function Sums($hash) { "$hash  $name`n" | Set-Content "$web\SHA256SUMS.txt" -NoNewline -Encoding utf8 }
function Cleanup {
    if ($presenter -and -not $presenter.HasExited) { Stop-Process -Id $presenter.Id -Force -ErrorAction SilentlyContinue }
    if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
    if ($exe -and (Test-Path $exe)) { & $exe --quit 2>$null | Out-Null }
    Start-Sleep -Seconds 2
    if (Test-Path "$dir\unins000.exe") { Start-Process "$dir\unins000.exe" -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART' -Wait }
}

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class W {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string t);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
}
"@

$uninstall = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{F0309FCD-477A-4D75-A8ED-61F6BFC20E4C}_is1"
$dir = "$env:ProgramFiles\FreeHM"
$exe = "$dir\freehm.exe"
$name = "FreeHM-9.9.9-windows-x64-setup.exe"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("freehm-update-" + [guid]::NewGuid())
$web = "$tmp\web"; $cfg = "$tmp\config"
$updates = "$cfg\updates"
New-Item -ItemType Directory -Force $web, $cfg | Out-Null

# The version under test, installed the normal way.
$p = Start-Process $Setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/MERGETASKS=!helper', "/LOG=$(Resolve-Path $Out)\update-first-install.log" -Wait -PassThru
if ($p.ExitCode -ne 0) { Fail "the first install failed: $($p.ExitCode)" }
$old = Installed
if (-not $old -or $old -eq "9.9.9") { Fail "unexpected installed version '$old'" }

# GitHub's part.
$port = 18377
$base = "http://127.0.0.1:$port"
$server = Start-Process python -ArgumentList '-m', 'http.server', "$port", '--bind', '127.0.0.1', '--directory', $web -PassThru -WindowStyle Hidden -RedirectStandardError "$Out\update-server.txt"
$assets = @(@{ name = $name; browser_download_url = "$base/$name" }, @{ name = "SHA256SUMS.txt"; browser_download_url = "$base/SHA256SUMS.txt" })
@{ tag_name = "v9.9.9"; html_url = "https://github.com/VVVULTURE/FreeHM/releases/tag/v9.9.9"; draft = $false; assets = $assets } | ConvertTo-Json -Depth 5 | Set-Content "$web\release.json" -Encoding utf8
Copy-Item $NewSetup "$web\$name"

$env:FREEHM_CONFIG_DIR = $cfg
$env:FREEHM_UPDATE_URL = "$base/release.json"
@{ schema_version = 1; general = @{ start_with_system = $false }; detection = @{ auto_detect = $false; manual_games = @("present_loop.exe") }
   hotkeys = @{ toggle_overlay = "Ctrl+Alt+Shift+F12" }; permissions = @{ first_run_done = $true } } | ConvertTo-Json -Depth 5 | Set-Content "$cfg\config.json" -Encoding utf8
$app = Start-Process $exe -ArgumentList "--background" -PassThru
WaitFor "FreeHM to start" { Status }

# Found by the check at start, and waits for the user.
WaitFor "the check at start to find 9.9.9" { (Status).update.phase -eq "Available" }
$u = (Status).update
if ($u.method -ne "Installer") { Fail "an installed copy should update with the installer, got $($u.method)" }
if ($u.latest -ne "9.9.9") { Fail "found version $($u.latest)" }
Start-Sleep -Seconds 3
if ((Status).update.phase -ne "Available" -or (Installed) -ne $old -or (Test-Path "$updates\$name")) { Fail "the update went ahead without being asked for" }
Pass "the update is found at start and waits to be asked for"

# A file that does not match the published checksum.
Sums ("0" * 64)
& $exe --install-update | Out-Null
WaitFor "the checksum refusal" { (Status).update.message -match "does not match" }
if ((Status).update.phase -ne "Available" -or (Installed) -ne $old) { Fail "wrong state after a bad checksum" }
if (Get-ChildItem $updates -ErrorAction SilentlyContinue) { Fail "a refused download was left behind" }
Pass "a file that does not match its checksum is thrown away: $((Status).update.message)"

# An installer that fails (declining the Windows prompt ends the same way): where.exe exits 2.
Copy-Item "$env:SystemRoot\System32\where.exe" "$web\$name"
Sums (Get-FileHash "$web\$name" -Algorithm SHA256).Hash.ToLower()
& $exe --install-update | Out-Null
WaitFor "the failed installer to be reported" { (Status).update.message -match "exit code" }
if ((Status).update.phase -ne "Available" -or (Installed) -ne $old) { Fail "wrong state after a failed installer" }
Pass "an installer that fails is reported and can be retried: $((Status).update.message)"

# Nothing is installed during a game.
Copy-Item $NewSetup "$web\$name" -Force
Sums (Get-FileHash "$web\$name" -Algorithm SHA256).Hash.ToLower()
$presenter = Start-Process $PresentLoop -ArgumentList "180" -PassThru -RedirectStandardOutput "$Out\update-present-loop.txt"
Start-Sleep -Seconds 2
$pw = [W]::FindWindow("FreeHM.PresentLoop", "FreeHM present loop")
[W]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [W]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero); [void][W]::SetForegroundWindow($pw)
WaitFor "the test game to be detected" { (Status).game.active.name -match "present_loop" }
& $exe --install-update | Out-Null
WaitFor "the update to wait for the game" { (Status).update.phase -eq "WaitingForGame" }
Start-Sleep -Seconds 6
if ((Status).update.phase -ne "WaitingForGame" -or (Installed) -ne $old) { Fail "the update did not wait for the game" }
Pass "downloaded and verified, and waiting while a game runs"

# A settings window keeps freehm.exe in use; the installer must deal with it.
$settings = Start-Process $exe -ArgumentList "--settings" -PassThru
Start-Sleep -Seconds 3
$settingsOpen = -not $settings.HasExited
Stop-Process -Id $presenter.Id -Force
WaitFor "the installer to finish" { (Installed) -eq "9.9.9" } 120
Pass "the game ended and the update installed: $old -> $(Installed)"
if (-not $app.WaitForExit(30000)) { Fail "the old FreeHM is still running" }
if ($settingsOpen) {
    if (-not $settings.WaitForExit(30000)) { Fail "the open settings window survived the update" }
    Pass "an open settings window does not get in the way"
} else {
    Write-Host "note: this machine could not open the settings window; that part was skipped"
}
WaitFor "the new FreeHM to start" { Status } 60
$s = Status
if ($s.config.detection.manual_games -notcontains "present_loop.exe") { Fail "the settings did not survive the update" }
Pass "the installer started FreeHM again with its settings"
if ($s.update.phase -eq "Installing") { Fail "the new instance thinks it is installing" }

Copy-Item "$cfg\logs\*.log" $Out -ErrorAction SilentlyContinue
Cleanup
if (Test-Path $exe) { Fail "uninstall left files behind" }
Write-Host "all updater checks passed"
