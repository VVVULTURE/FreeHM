//! Thin safe wrapper over the Performance Data Helper (PDH) API. One query collects all of
//! its counters with a single `PdhCollectQueryData` call per poll.

use super::util::wide;
use windows::Win32::System::Performance::*;
use windows::core::PCWSTR;

/// Value fits in a double and may exceed 100 (e.g. "% Processor Performance" under turbo).
const PDH_FMT_NOCAP100: u32 = 0x8000;
const PDH_CSTATUS_VALID_DATA: u32 = 0;
const PDH_CSTATUS_NEW_DATA: u32 = 1;

pub struct Query {
    q: PDH_HQUERY,
}

// PDH handles may be used from any thread; each Query is owned by one provider.
unsafe impl Send for Query {}

#[derive(Clone, Copy)]
pub struct Counter(PDH_HCOUNTER);
unsafe impl Send for Counter {}

#[derive(Debug, PartialEq)]
pub enum Read {
    Value(f64),
    /// Rate counters need two collections before they have a value.
    NotReady,
    Error(u32),
}

impl Query {
    pub fn open() -> Result<Query, String> {
        let mut q = PDH_HQUERY::default();
        let r = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut q) };
        if r != 0 {
            return Err(format!("PdhOpenQuery failed: 0x{r:08X}"));
        }
        Ok(Query { q })
    }

    /// Add a counter by its English (locale-independent) path.
    pub fn add(&mut self, path: &str) -> Result<Counter, u32> {
        let p = wide(path);
        let mut c = PDH_HCOUNTER::default();
        let r = unsafe { PdhAddEnglishCounterW(self.q, PCWSTR(p.as_ptr()), 0, &mut c) };
        if r != 0 { Err(r) } else { Ok(Counter(c)) }
    }

    pub fn collect(&mut self) -> Result<(), u32> {
        let r = unsafe { PdhCollectQueryData(self.q) };
        if r != 0 { Err(r) } else { Ok(()) }
    }

    pub fn read(&self, c: Counter) -> Read {
        let mut v = PDH_FMT_COUNTERVALUE::default();
        let r = unsafe { PdhGetFormattedCounterValue(c.0, PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100), None, &mut v) };
        if r == PDH_INVALID_DATA || r == PDH_CSTATUS_INVALID_DATA || r == PDH_NO_DATA {
            return Read::NotReady;
        }
        if r != 0 {
            return Read::Error(r);
        }
        if v.CStatus != PDH_CSTATUS_VALID_DATA && v.CStatus != PDH_CSTATUS_NEW_DATA {
            return Read::NotReady;
        }
        Read::Value(unsafe { v.Anonymous.doubleValue })
    }

    /// Read a wildcard counter as (instance name, value) pairs.
    pub fn read_array(&self, c: Counter) -> Result<Vec<(String, f64)>, u32> {
        let fmt = PDH_FMT(PDH_FMT_DOUBLE.0 | PDH_FMT_NOCAP100);
        let mut size = 0u32;
        let mut count = 0u32;
        let r = unsafe { PdhGetFormattedCounterArrayW(c.0, fmt, &mut size, &mut count, None) };
        if r != PDH_MORE_DATA {
            return if r == 0 { Ok(Vec::new()) } else { Err(r) };
        }
        // The buffer holds the item array followed by the instance-name strings.
        let mut buf: Vec<u64> = vec![0; (size as usize).div_ceil(8) + 1];
        let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        let r = unsafe { PdhGetFormattedCounterArrayW(c.0, fmt, &mut size, &mut count, Some(items)) };
        if r != 0 {
            return Err(r);
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let it = unsafe { &*items.add(i) };
            if it.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA && it.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA {
                continue;
            }
            let name = unsafe { it.szName.to_string() }.unwrap_or_default();
            out.push((name, unsafe { it.FmtValue.Anonymous.doubleValue }));
        }
        Ok(out)
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe {
            let _ = PdhCloseQuery(self.q);
        }
    }
}

pub fn describe(code: u32) -> String {
    match code {
        PDH_CSTATUS_NO_OBJECT => "performance object not present on this system".into(),
        PDH_CSTATUS_NO_COUNTER => "performance counter not present on this system".into(),
        PDH_CSTATUS_NO_INSTANCE => "counter instance not present on this system".into(),
        _ => format!("PDH error 0x{code:08X}"),
    }
}
