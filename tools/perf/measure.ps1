# Measures FreeHM's CPU and memory use in the states the spec asks for (Windows).
# Each state starts a fresh instance with its own configuration (autostart off, so the
# machine's startup entries are not touched), lets it settle, then samples CPU time and
# memory for -Seconds.
#
#   tools\perf\measure.ps1 [-Bin target\release] [-Seconds 30] [-States hidden,desktop200,...]
#
# States:
#   hidden        tray only: no game, overlay hidden (nothing is polled)
#   desktop200    overlay shown on the desktop (`--show`), 200 ms interval (default)
#   desktop1000   same, 1000 ms (slower interval)
#   desktop50     same, 50 ms (fastest supported interval)
#   game200       overlay over a running game, 200 ms: start a game first and pass its
#                 executable name with -Game (it is added to the manual game list)
#
# The overlay IS visible during the desktop*/game* states.
param(
    [string]$Bin = "target\release",
    [int]$Seconds = 30,
    [string[]]$States = @("hidden", "desktop200", "desktop1000", "desktop50"),
    [string]$Game = "",
    [string]$Out = "perf-results.json"
)
$ErrorActionPreference = "Stop"
$exe = (Resolve-Path "$Bin\freehm.exe").Path
$cores = [Environment]::ProcessorCount

# Automatic detection is off so a game that happens to be running does not change the
# state being measured; the game state uses the manual list instead. The shortcut is moved
# to an unusual combination so the measurement never takes Ctrl+Shift+O from anything.
function Config([int]$interval, [bool]$desktop, [string[]]$games) {
    @{
        schema_version = 1
        general        = @{ refresh_interval_ms = $interval; start_with_system = $false }
        overlay        = @{ show_on_desktop = $desktop }
        detection      = @{ auto_detect = $false; manual_games = $games }
        hotkeys        = @{ toggle_overlay = "Ctrl+Alt+Shift+F12" }
        permissions    = @{ first_run_done = $true }
    } | ConvertTo-Json -Depth 5
}

function Measure-State([string]$name) {
    $interval = 200; $desktop = $false; $games = @()
    switch -Regex ($name) {
        '^hidden$' { }
        '^desktop(\d+)$' { $interval = [int]$Matches[1]; $desktop = $true }
        '^game(\d+)$' { $interval = [int]$Matches[1]; if (-not $Game) { throw "-Game is required for $name" }; $games = @($Game) }
        default { throw "unknown state $name" }
    }
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("freehm-perf-" + [guid]::NewGuid())
    New-Item -ItemType Directory $dir | Out-Null
    Config $interval $desktop $games | Set-Content (Join-Path $dir "config.json") -Encoding utf8
    $env:FREEHM_CONFIG_DIR = $dir

    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process $exe -ArgumentList "--background" -PassThru
    $ready = $null
    for ($i = 0; $i -lt 100; $i++) {
        Start-Sleep -Milliseconds 50
        try { $null = (& $exe --status | Out-String) | ConvertFrom-Json; $ready = $sw.ElapsedMilliseconds; break } catch {}
    }
    if ($null -eq $ready) { throw "$name : FreeHM did not start" }
    Start-Sleep -Seconds 5   # settle: providers initialised, first frames drawn

    $p.Refresh()
    # The helper runs as LocalSystem, so an unelevated shell cannot open its process handle;
    # the Process performance counters are readable by everyone. NextValue() averages over
    # the time between its two calls.
    $hcpu = $null
    if (Get-Process freehm-helper -ErrorAction SilentlyContinue) {
        try { $hcpu = New-Object Diagnostics.PerformanceCounter("Process", "% Processor Time", "freehm-helper"); $null = $hcpu.NextValue() } catch { $hcpu = $null }
    }
    $cpu0 = $p.TotalProcessorTime; $t0 = [Diagnostics.Stopwatch]::StartNew()
    $ws = @(); $priv = @()
    for ($s = 0; $s -lt $Seconds; $s++) {
        Start-Sleep -Seconds 1
        $p.Refresh(); $ws += $p.WorkingSet64; $priv += $p.PrivateMemorySize64
    }
    $p.Refresh()
    $cpuMs = ($p.TotalProcessorTime - $cpu0).TotalMilliseconds
    $wall = $t0.Elapsed.TotalMilliseconds
    $helperPct = $null; $helperWs = $null
    if ($hcpu) {
        try {
            $helperPct = [math]::Round($hcpu.NextValue(), 3)
            $helperWs = [math]::Round((New-Object Diagnostics.PerformanceCounter("Process", "Working Set", "freehm-helper")).NextValue() / 1MB, 1)
        } catch {}
    }
    $st = (& $exe --status | Out-String) | ConvertFrom-Json
    & $exe --quit | Out-Null
    $null = $p.WaitForExit(10000)
    Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue

    [pscustomobject]@{
        state            = $name
        interval_ms      = $interval
        overlay_visible  = $st.overlay.visible
        overlay_reason   = $st.overlay.reason
        game             = $st.game.active.name
        startup_ms       = $ready
        cpu_percent_one_core = [math]::Round($cpuMs / $wall * 100, 3)
        cpu_percent_total    = [math]::Round($cpuMs / $wall * 100 / $cores, 3)
        working_set_mb_avg   = [math]::Round(($ws | Measure-Object -Average).Average / 1MB, 1)
        working_set_mb_max   = [math]::Round(($ws | Measure-Object -Maximum).Maximum / 1MB, 1)
        private_mb_avg       = [math]::Round(($priv | Measure-Object -Average).Average / 1MB, 1)
        helper_cpu_percent_one_core = $helperPct
        helper_working_set_mb       = $helperWs
        seconds              = $Seconds
    }
}

$results = foreach ($s in $States) { Write-Host "measuring $s ..."; Measure-State $s }
$results | Format-Table -AutoSize | Out-String | Write-Host
$meta = [pscustomobject]@{
    when    = (Get-Date).ToString("s")
    cpu     = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name
    cores   = $cores
    os      = (Get-CimInstance Win32_OperatingSystem).Caption
    results = $results
}
$meta | ConvertTo-Json -Depth 5 | Set-Content $Out -Encoding utf8
Write-Host "saved $Out"
