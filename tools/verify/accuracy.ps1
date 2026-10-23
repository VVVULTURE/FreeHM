<#
.SYNOPSIS
  Checks that the values FreeHM shows match independent sources on this PC (Windows).

.DESCRIPTION
  FreeHM must be running. Over -Samples samples (0.5 s apart) the script reads FreeHM's
  readings through its IPC pipe (asking for live polling of every metric) and, at the same
  moment, the same quantities from sources FreeHM does not go through:

    CPU usage / clock   Windows "% Processor Utility" and "Processor Frequency" x
                        "% Processor Performance" (what Task Manager shows)
    CPU power           Windows Energy Meter (RAPL), and the helper's own MSR read
    CPU temperature     the helper service's raw package sensor (checks FreeHM's handling
                        of it; the sensor itself was checked against LibreHardwareMonitor)
    GPU (NVIDIA)        nvidia-smi
    RAM                 GlobalMemoryStatusEx (total and available physical memory)

  Sensors move between two reads taken a few milliseconds apart, so the means over all
  samples are compared, each against a stated tolerance. A metric without an independent
  source on this machine is reported as SKIP, never as PASS. Exit code 1 if anything FAILs.

  Measuring must not disturb what is measured: starting a process or a WMI query per sample
  loads the CPU, raising its usage, clock, temperature and power right when one side samples.
  So the CPU and RAM phase uses only in-process reads (open pipes, performance counters,
  GlobalMemoryStatusEx), and nvidia-smi runs in a separate GPU phase afterwards.

  CPU usage, clock and power are compared over one 12 s window in which every FreeHM
  snapshot is collected (see CpuWindow).

  If any FreeHM poll in a window hit the 100 % cap (machine saturated), CPU usage for that
  window is reported as skipped: the cap makes the averages differ by construction.

  -Load adds a phase with one core kept busy by a separate process and compares CPU usage,
  clock and power again there (as CpuUsage@load, CpuClock@load, CpuPower@load). On an idle machine both sides
  read almost 0 % CPU, which says little; CI uses it. Temperature and power are only
  compared without the artificial load: under it the package sensor swings several
  degrees between two reads milliseconds apart.

.EXAMPLE
  ./tools/verify/accuracy.ps1 -Samples 60
#>
param([int]$Samples = 40, [string]$Json, [switch]$Load)
$ErrorActionPreference = 'Stop'

$rights = [IO.Pipes.PipeAccessRights]::ReadData -bor [IO.Pipes.PipeAccessRights]::WriteData -bor [IO.Pipes.PipeAccessRights]::ReadAttributes -bor [IO.Pipes.PipeAccessRights]::Synchronize
function Connect($name) {
    $c = New-Object IO.Pipes.NamedPipeClientStream(".", $name, $rights, [IO.Pipes.PipeOptions]::None, [Security.Principal.TokenImpersonationLevel]::Identification, [IO.HandleInheritability]::None)
    $c.Connect(5000); $c
}
function Frame($c, [string]$json) {
    $body = [Text.Encoding]::UTF8.GetBytes($json)
    $c.Write([BitConverter]::GetBytes([uint32]$body.Length), 0, 4); $c.Write($body, 0, $body.Length); $c.Flush()
    $read = { param($b, $len) $n = 0; while ($n -lt $len) { $k = $c.Read($b, $n, $len - $n); if ($k -le 0) { throw "pipe closed" }; $n += $k } }
    $hdr = New-Object byte[] 4; & $read $hdr 4
    $buf = New-Object byte[] ([BitConverter]::ToUInt32($hdr, 0)); & $read $buf $buf.Length
    [Text.Encoding]::UTF8.GetString($buf) | ConvertFrom-Json
}
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class FreeHMVerifyMem {
    [StructLayout(LayoutKind.Sequential)] public struct MEMORYSTATUSEX { public uint dwLength, dwMemoryLoad; public ulong ullTotalPhys, ullAvailPhys, ullTotalPageFile, ullAvailPageFile, ullTotalVirtual, ullAvailVirtual, ullAvailExtendedVirtual; }
    [DllImport("kernel32.dll")] static extern bool GlobalMemoryStatusEx(ref MEMORYSTATUSEX m);
    public static ulong[] Read() { var m = new MEMORYSTATUSEX(); m.dwLength = (uint)Marshal.SizeOf(m); GlobalMemoryStatusEx(ref m); return new[] { m.ullTotalPhys, m.ullAvailPhys }; }
}
'@
function Counter($cat, $name, $inst) { try { $c = New-Object Diagnostics.PerformanceCounter($cat, $name, $inst); [void]$c.NextValue(); $c } catch { $null } }

$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$app = Connect "FreeHM.App.$sid"
$helper = try { Connect "FreeHM.Helper.v1" } catch { $null }
$util = Counter "Processor Information" "% Processor Utility" "_Total"
$freq = Counter "Processor Information" "Processor Frequency" "_Total"
$perf = Counter "Processor Information" "% Processor Performance" "_Total"
$energy = Counter "Energy Meter" "Power" "RAPL_Package0_PKG"
$energyE = Counter "Energy Meter" "Energy" "RAPL_Package0_PKG"
$energyT = Counter "Energy Meter" "Time" "RAPL_Package0_PKG"
$smi = [bool](Get-Command nvidia-smi -ErrorAction SilentlyContinue)
# Warm up: the first live request starts polling every metric.
[void](Frame $app '{"GetStatus":{"live":true}}'); Start-Sleep -Seconds 2

function FreeHM {
    $st = (Frame $app '{"GetStatus":{"live":true}}').Status
    $f = @{}
    foreach ($r in $st.snapshot.readings) {
        if ($null -ne $r.value -and $r.device -ne 'game:active' -and -not $f.ContainsKey($r.metric)) { $f[$r.metric] = [double]$r.value }
    }
    $f['_seq'] = $st.snapshot.seq
    $f
}

# CPU usage, clock and power over one long window. Windows' counters are read at the start
# and the end, so they average over exactly the window (% Processor Utility and
# % Processor Performance are averages since the previous read; the Energy Meter's Energy
# counter is cumulative, in picowatt-hours, with its Time counter in milliseconds). Every
# snapshot FreeHM publishes in between is collected once (by sequence number) and averaged.
# Both sides then cover the same time, including background load and this script's own
# work. Comparing short windows does not work: FreeHM's newest poll and a counter's window
# never cover the same moments, and on a busy machine that alone differs by several %.
function CpuWindow([string]$suffix, [int]$seconds) {
    $seen = @{}; $vals = @{ CpuUsage = @(); CpuClock = @(); CpuPower = @() }; $clockW = 0.0; $busyW = 0.0
    if ($util) { [void]$util.NextValue() }; if ($perf) { [void]$perf.NextValue() }
    $e0 = if ($energyE) { $energyE.RawValue } else { $null }; $t0 = if ($energyT) { $energyT.RawValue } else { $null }
    $end = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $end) {
        $f = FreeHM
        if (-not $seen.ContainsKey($f['_seq'])) {
            $seen[$f['_seq']] = $true
            foreach ($m in 'CpuUsage', 'CpuClock', 'CpuPower') { if ($f.ContainsKey($m)) { $vals[$m] += $f[$m] } }
            if ($f.ContainsKey('CpuClock') -and $f.ContainsKey('CpuUsage')) { $clockW += $f['CpuClock'] * $f['CpuUsage']; $busyW += $f['CpuUsage'] }
        }
        Start-Sleep -Milliseconds 40
    }
    $ref = @{}
    if ($util) {
        $u = $util.NextValue()
        # % Processor Utility goes above 100 % when cores run faster than base clock; Task
        # Manager and FreeHM cap each reading at 100 %. If FreeHM capped any poll in the
        # window, its average is lower by construction and cannot be compared; otherwise
        # nothing was capped and the averages must agree, however high they are.
        $capped = @($vals.CpuUsage | Where-Object { $_ -ge 99.95 }).Count
        if ($capped -eq 0) { $ref.CpuUsage = $u } else { Write-Host "note: CpuUsage$suffix skipped: FreeHM capped $capped of $($vals.CpuUsage.Count) polls at 100 % (machine saturated, utility $([math]::Round($u)) %)" }
    }
    if ($freq -and $perf) { $ref.CpuClock = $freq.NextValue() * $perf.NextValue() / 100 }
    if ($energyE -and $energyT) {
        $dt = $energyT.RawValue - $t0
        if ($dt -gt 0) { $ref.CpuPower = ($energyE.RawValue - $e0) * 3600 / 1e12 / ($dt / 1000) }
    }
    # % Processor Performance averages over the time the cores were busy, so over a long
    # window busier moments (which also run at higher clocks) count more. Weight FreeHM's
    # per-poll clocks by that poll's CPU usage to average the same way. Under steady load
    # the weights are equal and this is the plain mean.
    if ($busyW -gt 0) { $wm = $clockW / $busyW; $vals.CpuClock = @($vals.CpuClock | ForEach-Object { $wm }) }
    foreach ($m in $ref.Keys) {
        foreach ($v in $vals[$m]) { [pscustomobject]@{ f = @{ ($m + $suffix) = $v }; ('win_' + $m + $suffix) = $ref[$m] } }
    }
}

$num = { param($s) $d = 0.0; if ([double]::TryParse($s, [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$d)) { $d } else { $null } }

# Phase 1: CPU and RAM, in-process reads only.
$rows = @()
$rows += CpuWindow '' 12
for ($i = 0; $i -lt $Samples; $i++) {
    # The usage and clock counters average over the time since their previous read. Read
    # them just before and just after the pause, so they cover only the pause, when this
    # script is idle; FreeHM's newest poll window also lies in it. Otherwise the reference
    # would include this script's own work (several % on a 4-core CI machine).
    Start-Sleep -Milliseconds 500
    $f = FreeHM
    $h = if ($helper) { (Frame $helper '{"v":1,"cpu":true,"frames":false}').cpu } else { $null }
    $mem = [FreeHMVerifyMem]::Read()
    $rows += [pscustomobject]@{
        f = $f
        cpu_power_msr = if ($h -and $h.package_power_w.Ok) { [double]$h.package_power_w.Ok } else { $null }
        cpu_temp = if ($h -and $null -ne $h.package_temp_c.Ok) { [double]$h.package_temp_c.Ok } else { $null }
        ram_total = [double]$mem[0]
        ram_used = [double]$mem[0] - [double]$mem[1]
        ram_usage = 100 * ([double]$mem[0] - [double]$mem[1]) / [double]$mem[0]
    }
}
# Phase 1b (-Load): CPU usage and clock again with one core kept busy.
if ($Load) {
    $busy = Start-Process pwsh -ArgumentList '-NoProfile', '-Command', 'while ($true) {}' -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 1
    $rows += CpuWindow '@load' 12
    Stop-Process -Id $busy.Id -Force -ErrorAction SilentlyContinue
}
# Phase 2: NVIDIA GPU through nvidia-smi.
if ($smi) {
    for ($i = 0; $i -lt $Samples; $i++) {
        $f = FreeHM
        $g = (& nvidia-smi --query-gpu=temperature.gpu,utilization.gpu,clocks.gr,power.draw,memory.used,memory.total --format=csv,noheader,nounits | Select-Object -First 1) -split ',\s*'
        $rows += [pscustomobject]@{
            f = $f
            gpu_temp = & $num $g[0]; gpu_usage = & $num $g[1]; gpu_clock = & $num $g[2]; gpu_power = & $num $g[3]
            vram_used = (& $num $g[4]) * 1MB; vram_total = (& $num $g[5]) * 1MB
            vram_usage = 100 * (& $num $g[4]) / (& $num $g[5])
        }
        Start-Sleep -Milliseconds 500
    }
}
$app.Dispose(); if ($helper) { $helper.Dispose() }

# metric, reference column, reference name, absolute tolerance, relative tolerance (the larger applies)
$checks = @(
    @('CpuUsage', 'win_CpuUsage', 'Windows % Processor Utility, same 12 s window', 2.0, 0),
    @('CpuClock', 'win_CpuClock', 'Windows frequency x % Processor Performance, same window (busy-weighted)', 0, 0.02),
    @('CpuPower', 'win_CpuPower', 'Windows Energy Meter energy (RAPL), same window', 0.5, 0.03),
    @('CpuUsage@load', 'win_CpuUsage@load', 'the same three, one core kept busy', 2.0, 0),
    @('CpuClock@load', 'win_CpuClock@load', 'the same three, one core kept busy', 0, 0.02),
    @('CpuPower@load', 'win_CpuPower@load', 'the same three, one core kept busy', 0.5, 0.03),
    @('CpuPower', 'cpu_power_msr', 'helper MSR RAPL, instant reads', 1.5, 0.08),
    # The package sensor moves within milliseconds and the two reads are a few milliseconds
    # apart; 3 degrees covers that, not a calibration error.
    @('CpuTemp', 'cpu_temp', 'helper raw package sensor', 3.0, 0),
    @('RamTotal', 'ram_total', 'GlobalMemoryStatusEx', 0, 0.001),
    @('RamUsed', 'ram_used', 'GlobalMemoryStatusEx', 0, 0.02),
    @('RamUsage', 'ram_usage', 'GlobalMemoryStatusEx used / total', 1.0, 0),
    @('GpuTemp', 'gpu_temp', 'nvidia-smi', 2.0, 0),
    @('GpuUsage', 'gpu_usage', 'nvidia-smi', 5.0, 0),
    @('GpuClock', 'gpu_clock', 'nvidia-smi', 0, 0.10),
    @('GpuPower', 'gpu_power', 'nvidia-smi', 2.0, 0.08),
    @('VramUsed', 'vram_used', 'nvidia-smi', 64MB, 0.03),
    @('VramTotal', 'vram_total', 'nvidia-smi', 1MB, 0),
    @('VramUsage', 'vram_usage', 'nvidia-smi used / total', 1.0, 0)
)
$results = @(); $failed = $false
foreach ($c in $checks) {
    $metric, $col, $src, $abs, $rel = $c
    $pairs = @($rows | Where-Object { $_.f.ContainsKey($metric) -and $_.PSObject.Properties[$col] -and $null -ne $_.PSObject.Properties[$col].Value } | ForEach-Object { , @($_.f[$metric], $_.PSObject.Properties[$col].Value) })
    if ($pairs.Count -lt [math]::Max(3, $Samples / 2)) {
        $results += [pscustomobject]@{ metric = $metric; reference = $src; result = 'SKIP'; freehm = $null; ref = $null; diff = $null; tolerance = $null; n = $pairs.Count }
        continue
    }
    $mf = ($pairs | ForEach-Object { $_[0] } | Measure-Object -Average).Average
    $mr = ($pairs | ForEach-Object { $_[1] } | Measure-Object -Average).Average
    $tol = [math]::Max($abs, $rel * [math]::Abs($mr)); $diff = [math]::Abs($mf - $mr)
    $ok = $diff -le $tol; if (-not $ok) { $failed = $true }
    $results += [pscustomobject]@{ metric = $metric; reference = $src; result = $(if ($ok) { 'PASS' } else { 'FAIL' }); freehm = $mf; ref = $mr; diff = $diff; tolerance = $tol; n = $pairs.Count }
}
$fmt = { param($m, $v) if ($null -eq $v) { '' } elseif ($m -match '^(Ram|Vram)(Used|Total)$') { '{0:N1} MiB' -f ($v / 1MB) } else { '{0:N2}' -f $v } }
$results | ForEach-Object { "{0,-5} {1,-13} FreeHM {2,14}  reference {3,14}  |diff| {4,12} <= {5,12}  {6} (n={7})" -f $_.result, $_.metric, (& $fmt $_.metric $_.freehm), (& $fmt $_.metric $_.ref), (& $fmt $_.metric $_.diff), (& $fmt $_.metric $_.tolerance), $_.reference, $_.n }
if ($Json) { $results | ConvertTo-Json | Set-Content $Json }
if ($failed) { Write-Host "accuracy check FAILED"; exit 1 }
Write-Host "accuracy check passed ($(@($results | Where-Object result -eq 'PASS').Count) checks, $(@($results | Where-Object result -eq 'SKIP').Count) skipped: no independent source here)"
