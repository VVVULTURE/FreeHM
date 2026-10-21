//! CPU package temperature and RAPL package power through PawnIO.
//!
//! - Intel: TjMax from MSR_TEMPERATURE_TARGET (0x1A2, bits 23:16); package temperature =
//!   TjMax − digital readout of IA32_PACKAGE_THERM_STATUS (0x1B1, bits 22:16).
//!   Energy: MSR_RAPL_POWER_UNIT (0x606, bits 12:8) and MSR_PKG_ENERGY_STATUS (0x611).
//! - AMD family 17h–1Ah: Tctl from SMN 0x00059800 (bits 31:21 × 0.125 °C, −49 °C when the
//!   range-select bit 19 is set), minus the documented Tctl offset of a few early models
//!   (as in Linux k10temp). Energy: 0xC0010299 (unit) and 0xC001029B (package counter).
//!
//! The PawnIO module is loaded on the first request and released after
//! [`IDLE_RELEASE`] without requests.

use super::pawnio::PawnIo;
use freehm_core::helper_proto::{CpuSensors, Value};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::PCWSTR;

static INTEL_MSR: &[u8] = include_bytes!("../../modules/IntelMSR.bin");
static AMD_FAMILY17: &[u8] = include_bytes!("../../modules/AMDFamily17.bin");

pub const IDLE_RELEASE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Intel { package_thermal: bool },
    Amd17 { tctl_offset: f64 },
    Unsupported(String),
}

pub struct CpuReader {
    pub kind: Kind,
    pub name: String,
    pio: Option<PawnIo>,
    open_error: Option<(String, Instant)>,
    last_used: Instant,
    tjmax: Option<f64>,
    energy_unit_j: Option<f64>,
    last_energy: Option<(u32, Instant)>,
    last_power: Option<(f64, Instant)>,
}

#[cfg(target_arch = "x86_64")]
fn cpuid(leaf: u32) -> [u32; 4] {
    let r = std::arch::x86_64::__cpuid_count(leaf, 0);
    [r.eax, r.ebx, r.ecx, r.edx]
}

#[cfg(target_arch = "x86_64")]
pub fn detect() -> (Kind, String) {
    let v = cpuid(0);
    let mut vendor = Vec::new();
    for r in [v[1], v[3], v[2]] {
        vendor.extend_from_slice(&r.to_le_bytes());
    }
    let vendor = String::from_utf8_lossy(&vendor).to_string();
    let mut name = String::new();
    if cpuid(0x8000_0000)[0] >= 0x8000_0004 {
        let mut b = Vec::new();
        for leaf in 0x8000_0002..=0x8000_0004u32 {
            for r in cpuid(leaf) {
                b.extend_from_slice(&r.to_le_bytes());
            }
        }
        name = String::from_utf8_lossy(&b).trim_matches(char::from(0)).trim().to_string();
    }
    let sig = cpuid(1)[0];
    let base_family = (sig >> 8) & 0xF;
    let family = if base_family == 0xF { base_family + ((sig >> 20) & 0xFF) } else { base_family };
    let kind = match vendor.as_str() {
        "GenuineIntel" => {
            let leaf6 = if v[0] >= 6 { cpuid(6)[0] } else { 0 };
            if leaf6 & 1 == 0 {
                Kind::Unsupported("this Intel CPU has no digital thermal sensor".into())
            } else {
                Kind::Intel { package_thermal: leaf6 & (1 << 6) != 0 }
            }
        }
        "AuthenticAMD" | "HygonGenuine" if (0x17..=0x1A).contains(&family) => Kind::Amd17 { tctl_offset: amd_tctl_offset(&name) },
        "AuthenticAMD" => Kind::Unsupported(format!("AMD CPU family {family:#x} is not supported (Zen / family 17h or newer is)")),
        other => Kind::Unsupported(format!("CPU vendor '{other}' is not supported")),
    };
    (kind, name)
}

#[cfg(not(target_arch = "x86_64"))]
pub fn detect() -> (Kind, String) {
    (Kind::Unsupported("CPU temperature is only supported on x86-64".into()), String::new())
}

/// Models whose Tctl is reported with a fixed offset above the real die temperature
/// (from the Linux k10temp driver).
pub fn amd_tctl_offset(name: &str) -> f64 {
    const TABLE: [(&str, f64); 6] = [
        ("AMD Ryzen 5 1600X", 20.0),
        ("AMD Ryzen 7 1700X", 20.0),
        ("AMD Ryzen 7 1800X", 20.0),
        ("AMD Ryzen 7 2700X", 10.0),
        ("AMD Ryzen Threadripper 19", 27.0),
        ("AMD Ryzen Threadripper 29", 27.0),
    ];
    TABLE.iter().find(|(m, _)| name.contains(m)).map(|(_, o)| *o).unwrap_or(0.0)
}

/// Decode the AMD THM_TCON_CUR_TMP register.
pub fn amd_tctl(reg: u32, offset: f64) -> f64 {
    let mut t = (reg >> 21) as f64 * 0.125;
    if reg & (1 << 19) != 0 {
        t -= 49.0;
    }
    t - offset
}

/// Holds the system-wide `Global\Access_PCI` mutex, the convention hardware monitors use
/// to avoid interleaving SMN index/data accesses.
struct PciLock(HANDLE);

impl PciLock {
    fn acquire() -> Result<PciLock, String> {
        let name: Vec<u16> = "Global\\Access_PCI\0".encode_utf16().collect();
        unsafe {
            let h = CreateMutexW(None, false, PCWSTR(name.as_ptr())).map_err(|e| format!("Access_PCI mutex: {e}"))?;
            let w = WaitForSingleObject(h, 100);
            if w == WAIT_OBJECT_0 || w == WAIT_ABANDONED {
                Ok(PciLock(h))
            } else {
                let _ = CloseHandle(h);
                Err("another hardware tool is holding the PCI lock".into())
            }
        }
    }
}

impl Drop for PciLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

impl CpuReader {
    pub fn new() -> CpuReader {
        let (kind, name) = detect();
        CpuReader { kind, name, pio: None, open_error: None, last_used: Instant::now(), tjmax: None, energy_unit_j: None, last_energy: None, last_power: None }
    }

    fn pio(&mut self) -> Result<&PawnIo, String> {
        if self.pio.is_none() {
            // Don't hammer the driver after a failure; retry every 10 s.
            if let Some((e, at)) = &self.open_error
                && at.elapsed() < Duration::from_secs(10)
            {
                return Err(e.clone());
            }
            let module = match self.kind {
                Kind::Intel { .. } => INTEL_MSR,
                Kind::Amd17 { .. } => AMD_FAMILY17,
                Kind::Unsupported(ref r) => return Err(r.clone()),
            };
            match PawnIo::open(module) {
                Ok(p) => {
                    log::info!("PawnIO module loaded ({:?})", self.kind);
                    self.pio = Some(p);
                    self.open_error = None;
                }
                Err(e) => {
                    log::warn!("PawnIO unavailable: {e}");
                    self.open_error = Some((e.clone(), Instant::now()));
                    return Err(e);
                }
            }
        }
        Ok(self.pio.as_ref().unwrap())
    }

    /// Unload the PawnIO module when nobody asked for CPU values recently.
    pub fn release_if_idle(&mut self) {
        if self.pio.is_some() && self.last_used.elapsed() >= IDLE_RELEASE {
            log::info!("PawnIO module released (idle)");
            self.pio = None;
            self.last_energy = None;
        }
    }

    pub fn release(&mut self) {
        self.pio = None;
    }

    fn read_temp(&mut self) -> Result<(f64, String), String> {
        match self.kind.clone() {
            Kind::Intel { package_thermal } => {
                if self.tjmax.is_none() {
                    let v = self.pio()?.read1(c"ioctl_read_msr", 0x1A2)?;
                    let t = ((v >> 16) & 0xFF) as f64;
                    if t < 50.0 || t > 130.0 {
                        return Err(format!("implausible TjMax {t} °C"));
                    }
                    self.tjmax = Some(t);
                }
                let tj = self.tjmax.unwrap();
                // Without package thermal management, fall back to the core sensor of
                // whichever core runs this thread (IA32_THERM_STATUS, 0x19C).
                let (msr, source) = if package_thermal { (0x1B1u64, "Intel package DTS") } else { (0x19C, "Intel core DTS") };
                let v = self.pio()?.read1(c"ioctl_read_msr", msr)?;
                if msr == 0x19C && v & (1 << 31) == 0 {
                    return Err("thermal reading not valid".into());
                }
                let readout = ((v >> 16) & 0x7F) as f64;
                Ok((tj - readout, format!("{source} (TjMax {tj:.0} °C)")))
            }
            Kind::Amd17 { tctl_offset } => {
                let _lock = PciLock::acquire()?;
                let v = self.pio()?.read1(c"ioctl_read_smn", 0x0005_9800)? as u32;
                let src = if tctl_offset > 0.0 { format!("AMD Tdie (Tctl − {tctl_offset:.0} °C)") } else { "AMD Tctl".to_string() };
                Ok((amd_tctl(v, tctl_offset), src))
            }
            Kind::Unsupported(r) => Err(r),
        }
    }

    fn read_power(&mut self) -> Result<f64, String> {
        let (unit_msr, energy_msr) = match self.kind {
            Kind::Intel { .. } => (0x606u64, 0x611u64),
            Kind::Amd17 { .. } => (0xC001_0299, 0xC001_029B),
            Kind::Unsupported(ref r) => return Err(r.clone()),
        };
        if self.energy_unit_j.is_none() {
            let u = self.pio()?.read1(c"ioctl_read_msr", unit_msr)?;
            let esu = ((u >> 8) & 0x1F) as i32;
            self.energy_unit_j = Some(0.5f64.powi(esu));
        }
        let raw = self.pio()?.read1(c"ioctl_read_msr", energy_msr)? as u32;
        let now = Instant::now();
        let prev = self.last_energy.replace((raw, now));
        let Some((p_raw, p_t)) = prev else {
            return Err("measuring (needs two samples)".into());
        };
        let dt = now.duration_since(p_t).as_secs_f64();
        if dt < 0.05 {
            // Too short for a meaningful average; keep the previous result.
            self.last_energy = prev;
            return self.last_power.map(|(w, _)| w).ok_or_else(|| "measuring".into());
        }
        let joules = raw.wrapping_sub(p_raw) as f64 * self.energy_unit_j.unwrap();
        let w = joules / dt;
        if !(0.0..=2000.0).contains(&w) {
            return Err(format!("implausible package power {w:.0} W"));
        }
        self.last_power = Some((w, now));
        Ok(w)
    }

    pub fn read(&mut self) -> CpuSensors {
        self.last_used = Instant::now();
        let (package_temp_c, temp_source) = match self.read_temp() {
            Ok((t, src)) if (-20.0..=150.0).contains(&t) => (Value::Ok(t), src),
            Ok((t, src)) => (Value::Unavailable(format!("implausible temperature {t:.1} °C")), src),
            Err(e) => (Value::Unavailable(e), String::new()),
        };
        let package_power_w = match self.read_power() {
            Ok(w) => Value::Ok(w),
            Err(e) => Value::Unavailable(e),
        };
        CpuSensors { package_temp_c, temp_source, package_power_w, cpu_name: self.name.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amd_tctl_decoding() {
        // 0x3E8 << 21 = 1000 * 0.125 = 125.0 °C raw; minus 49 with the range bit.
        assert_eq!(amd_tctl(0x3E8 << 21, 0.0), 125.0);
        assert_eq!(amd_tctl((0x3E8 << 21) | (1 << 19), 0.0), 76.0);
        assert_eq!(amd_tctl(400 << 21, 20.0), 30.0);
    }

    #[test]
    fn tctl_offsets() {
        assert_eq!(amd_tctl_offset("AMD Ryzen 7 1800X Eight-Core Processor"), 20.0);
        assert_eq!(amd_tctl_offset("AMD Ryzen 7 5800X 8-Core Processor"), 0.0);
        assert_eq!(amd_tctl_offset("AMD Ryzen Threadripper 2950X 16-Core Processor"), 27.0);
    }

    #[test]
    fn module_hashes_match_readme() {
        // The README lists the release hashes; make sure the embedded files are those.
        let readme = include_str!("../../modules/README.md");
        for (name, data) in [("IntelMSR.bin", INTEL_MSR), ("AMDFamily17.bin", AMD_FAMILY17)] {
            let h = sha256_hex(data);
            assert!(readme.contains(&format!("| {name} | {h} |")), "{name} hash {h} not in README");
        }
    }

    // Compact SHA-256 for the test above (no extra dependency).
    fn sha256_hex(data: &[u8]) -> String {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
            0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
            0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
            0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];
        let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let mut v = h;
            for i in 0..64 {
                let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
                let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
                let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
                let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
                let t2 = s0.wrapping_add(maj);
                v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
            }
            for i in 0..8 {
                h[i] = h[i].wrapping_add(v[i]);
            }
        }
        h.iter().map(|x| format!("{x:08x}")).collect()
    }

    #[test]
    fn sha256_known_answer() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
