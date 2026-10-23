#!/usr/bin/env python3
"""Check that the values FreeHM shows match independent sources on this PC (Linux).

FreeHM must be running. Over --samples samples (0.5 s apart) this reads FreeHM's readings
through its IPC socket (asking for live polling of every metric) and, at the same moment,
the same quantities straight from the kernel or vendor tools:

  CPU usage        /proc/stat, busy share between two consecutive samples
  CPU clock        mean of cpufreq scaling_cur_freq
  CPU temperature  hwmon coretemp "Package id 0" / k10temp or zenpower Tdie/Tctl
  CPU power        powercap RAPL energy_uj (only when readable by this user)
  RAM              /proc/meminfo (MemTotal - MemAvailable)
  GPU (NVIDIA)     nvidia-smi

The means over all samples are compared, each against a stated tolerance. A metric with
no independent source on this machine is reported as SKIP, never as PASS. The script only
reads files and sockets in-process while sampling the CPU, so it does not load the CPU
right when FreeHM samples it; nvidia-smi runs in a separate phase afterwards.
Exit code 1 if anything FAILs.

--load keeps one core busy in a separate process during the CPU phase. On an idle machine
both sides read almost 0 % CPU, which says little; a steady known load makes the CPU usage
comparison meaningful (CI uses it).

Usage: tools/verify/accuracy.py [--samples 40] [--json out.json] [--load]
"""
import argparse, glob, json, os, shutil, socket, statistics, struct, subprocess, sys, time

MiB = 1024 * 1024


def sock_path():
    rt = os.environ.get("XDG_RUNTIME_DIR") or "/tmp"
    return os.path.join(rt, "freehm", "app.sock")


class App:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(sock_path())

    def _recv(self, n):
        b = b""
        while len(b) < n:
            chunk = self.s.recv(n - len(b))
            if not chunk:
                raise ConnectionError("FreeHM closed the connection")
            b += chunk
        return b

    def readings(self):
        body = json.dumps({"GetStatus": {"live": True}}).encode()
        self.s.sendall(struct.pack("<I", len(body)) + body)
        (n,) = struct.unpack("<I", self._recv(4))
        st = json.loads(self._recv(n))["Status"]
        out = {}
        for r in st["snapshot"]["readings"]:
            if r["value"] is not None and r["device"] != "game:active":
                out.setdefault(r["metric"], r["value"])
        return out


def read(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def proc_stat():
    p = read("/proc/stat").splitlines()[0].split()[1:]
    v = [int(x) for x in p]
    idle = v[3] + (v[4] if len(v) > 4 else 0)
    return sum(v) - idle, sum(v)


def meminfo():
    d = {}
    for line in read("/proc/meminfo").splitlines():
        k, _, rest = line.partition(":")
        d[k] = int(rest.split()[0]) * 1024
    return d["MemTotal"], d["MemTotal"] - d["MemAvailable"]


def cpu_clock():
    f = [read(p) for p in glob.glob("/sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_cur_freq")]
    f = [int(x) for x in f if x]
    return sum(f) / len(f) / 1000 if f else None


def cpu_temp_file():
    for d in glob.glob("/sys/class/hwmon/hwmon*"):
        name = (read(f"{d}/name") or "").strip()
        wanted = {"coretemp": ["Package id 0"], "k10temp": ["Tdie", "Tctl"], "zenpower": ["Tdie", "Tctl"]}.get(name)
        if not wanted:
            continue
        for label in wanted:
            for lf in glob.glob(f"{d}/temp*_label"):
                if (read(lf) or "").strip() == label:
                    return lf.replace("_label", "_input")
    return None


def rapl_file():
    for d in glob.glob("/sys/class/powercap/intel-rapl:*"):
        if (read(f"{d}/name") or "").strip() == "package-0" and read(f"{d}/energy_uj") is not None:
            return f"{d}/energy_uj"
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", type=int, default=40)
    ap.add_argument("--json")
    ap.add_argument("--load", action="store_true")
    a = ap.parse_args()
    load = subprocess.Popen([sys.executable, "-c", "while True: pass"]) if a.load else None

    app = App()
    app.readings()  # the first live request starts polling every metric
    time.sleep(2)
    tfile, efile = cpu_temp_file(), rapl_file()
    rows = []
    prev_stat = proc_stat()
    prev_e = (int(read(efile)), time.monotonic()) if efile else None
    # Phase 1: CPU and RAM, in-process reads only.
    for _ in range(a.samples):
        time.sleep(0.5)
        f = app.readings()
        busy, total = proc_stat()
        r = {"f": f}
        if total > prev_stat[1]:
            r["cpu_usage"] = 100 * (busy - prev_stat[0]) / (total - prev_stat[1])
        prev_stat = (busy, total)
        r["cpu_clock"] = cpu_clock()
        if tfile:
            r["cpu_temp"] = int(read(tfile)) / 1000
        if efile:
            e, t = int(read(efile)), time.monotonic()
            if e >= prev_e[0]:
                r["cpu_power"] = (e - prev_e[0]) / 1e6 / (t - prev_e[1])
            prev_e = (e, t)
        r["ram_total"], r["ram_used"] = meminfo()
        r["ram_usage"] = 100 * r["ram_used"] / r["ram_total"]
        rows.append(r)
    if load:
        load.kill()
    # Phase 2: NVIDIA GPU through nvidia-smi.
    if shutil.which("nvidia-smi"):
        for _ in range(a.samples):
            f = app.readings()
            q = subprocess.run(["nvidia-smi", "--query-gpu=temperature.gpu,utilization.gpu,clocks.gr,power.draw,memory.used,memory.total",
                                "--format=csv,noheader,nounits"], capture_output=True, text=True).stdout.splitlines()[0].split(", ")
            num = lambda s: float(s) if s.replace(".", "", 1).isdigit() else None
            g = [num(x.strip()) for x in q]
            rows.append({"f": f, "gpu_temp": g[0], "gpu_usage": g[1], "gpu_clock": g[2], "gpu_power": g[3],
                         "vram_used": g[4] * MiB if g[4] is not None else None, "vram_total": g[5] * MiB if g[5] is not None else None,
                         "vram_usage": 100 * g[4] / g[5] if g[4] is not None and g[5] else None})
            time.sleep(0.5)

    # metric, reference key, reference name, absolute tolerance, relative tolerance (the larger applies)
    checks = [
        ("CpuUsage", "cpu_usage", "/proc/stat", 5.0, 0),
        ("CpuClock", "cpu_clock", "cpufreq scaling_cur_freq", 0, 0.10),
        # The package sensor swings several degrees within milliseconds under load, and the
        # two reads are a few milliseconds apart; 3 degrees covers that.
        ("CpuTemp", "cpu_temp", "hwmon package sensor", 3.0, 0),
        ("CpuPower", "cpu_power", "RAPL energy_uj", 1.5, 0.10),
        ("RamTotal", "ram_total", "/proc/meminfo", 0, 0.001),
        ("RamUsed", "ram_used", "/proc/meminfo", 0, 0.03),
        ("RamUsage", "ram_usage", "/proc/meminfo used / total", 1.5, 0),
        ("GpuTemp", "gpu_temp", "nvidia-smi", 2.0, 0),
        ("GpuUsage", "gpu_usage", "nvidia-smi", 5.0, 0),
        ("GpuClock", "gpu_clock", "nvidia-smi", 0, 0.10),
        ("GpuPower", "gpu_power", "nvidia-smi", 2.0, 0.08),
        ("VramUsed", "vram_used", "nvidia-smi", 64 * MiB, 0.03),
        ("VramTotal", "vram_total", "nvidia-smi", 1 * MiB, 0),
        ("VramUsage", "vram_usage", "nvidia-smi used / total", 1.0, 0),
    ]
    results, failed = [], False
    for metric, key, src, abs_tol, rel_tol in checks:
        pairs = [(r["f"][metric], r[key]) for r in rows if metric in r["f"] and r.get(key) is not None]
        if len(pairs) < max(3, a.samples // 2):
            results.append({"metric": metric, "reference": src, "result": "SKIP", "n": len(pairs)})
            continue
        mf = statistics.mean(p[0] for p in pairs)
        mr = statistics.mean(p[1] for p in pairs)
        tol = max(abs_tol, rel_tol * abs(mr))
        ok = abs(mf - mr) <= tol
        failed |= not ok
        results.append({"metric": metric, "reference": src, "result": "PASS" if ok else "FAIL", "freehm": mf, "ref": mr, "diff": abs(mf - mr), "tolerance": tol, "n": len(pairs)})

    def fmt(m, v):
        if v is None:
            return ""
        return f"{v / MiB:,.1f} MiB" if m.endswith(("Used", "Total")) else f"{v:,.2f}"

    for r in results:
        print(f"{r['result']:5} {r['metric']:10} FreeHM {fmt(r['metric'], r.get('freehm')):>14}  reference {fmt(r['metric'], r.get('ref')):>14}  "
              f"|diff| {fmt(r['metric'], r.get('diff')):>12} <= {fmt(r['metric'], r.get('tolerance')):>12}  {r['reference']} (n={r['n']})")
    if a.json:
        with open(a.json, "w") as f:
            json.dump(results, f, indent=1)
    if failed:
        print("accuracy check FAILED")
        sys.exit(1)
    passed = sum(r["result"] == "PASS" for r in results)
    print(f"accuracy check passed ({passed} checks, {len(results) - passed} skipped: no independent source here)")


if __name__ == "__main__":
    main()
