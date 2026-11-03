//! Sensor collection, sampled once a second on a dedicated thread.
//!
//! Windows has no user-mode API for CPU temperature, so temperatures come from
//! the documented shared-memory interfaces of MSI Afterburner and HWiNFO. Both
//! are read-only and need neither elevation nor a kernel driver of our own.
//! Load, memory, network and disk come straight from Win32.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::System::Memory::{
    MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS,
};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCollectQueryData, PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_FMT_DOUBLE, PDH_HCOUNTER,
    PDH_HQUERY,
};
use windows::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

#[derive(Clone, Debug, Serialize)]
pub struct Reading {
    pub id: String,
    pub name: String,
    pub group: String,
    pub unit: String,
    pub value: f64,
}

#[derive(Default)]
pub struct Snapshot {
    pub seq: u64,
    pub list: Vec<Reading>,
    pub index: HashMap<String, usize>,
}

impl Snapshot {
    pub fn get(&self, id: &str) -> Option<&Reading> {
        self.index.get(id).map(|&i| &self.list[i])
    }
}

pub type SharedSensors = Arc<RwLock<Snapshot>>;

pub fn spawn(shared: SharedSensors) {
    std::thread::Builder::new()
        .name("sensors".into())
        .spawn(move || {
            let mut native = Native::new();
            let mut seq = 0;
            loop {
                let started = Instant::now();
                let mut list = Vec::with_capacity(128);
                native.sample(&mut list);
                read_afterburner(&mut list);
                read_hwinfo(&mut list);
                let index = list.iter().enumerate().map(|(i, r)| (r.id.clone(), i)).collect();
                seq += 1;
                *shared.write().unwrap() = Snapshot { seq, list, index };
                std::thread::sleep(Duration::from_secs(1).saturating_sub(started.elapsed()));
            }
        })
        .expect("spawn sensor thread");
}

fn push(list: &mut Vec<Reading>, id: &str, name: &str, group: &str, unit: &str, value: f64) {
    list.push(Reading { id: id.into(), name: name.into(), group: group.into(), unit: unit.into(), value });
}

struct Native {
    last_cpu: (u64, u64),
    last_net: (u64, u64),
    last_at: Instant,
    pdh: PDH_HQUERY,
    disk_read: PDH_HCOUNTER,
    disk_write: PDH_HCOUNTER,
    perf: PDH_HCOUNTER,
    base_mhz: f64,
}

fn ft(t: FILETIME) -> u64 {
    ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
}

impl Native {
    fn new() -> Self {
        let mut me = Self {
            last_cpu: (0, 0),
            last_net: (0, 0),
            last_at: Instant::now(),
            pdh: PDH_HQUERY::default(),
            disk_read: PDH_HCOUNTER::default(),
            disk_write: PDH_HCOUNTER::default(),
            perf: PDH_HCOUNTER::default(),
            base_mhz: base_mhz(),
        };
        unsafe {
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut me.pdh) == 0 {
                PdhAddEnglishCounterW(me.pdh, w!("\\PhysicalDisk(_Total)\\Disk Read Bytes/sec"), 0, &mut me.disk_read);
                PdhAddEnglishCounterW(me.pdh, w!("\\PhysicalDisk(_Total)\\Disk Write Bytes/sec"), 0, &mut me.disk_write);
                PdhAddEnglishCounterW(me.pdh, w!("\\Processor Information(_Total)\\% Processor Performance"), 0, &mut me.perf);
                PdhCollectQueryData(me.pdh);
            }
        }
        me.last_net = net_totals();
        me.last_cpu = cpu_times();
        me
    }

    fn counter(&self, c: PDH_HCOUNTER) -> Option<f64> {
        if c.is_invalid() {
            return None;
        }
        let mut v = Default::default();
        unsafe {
            (PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut v) == 0).then(|| v.Anonymous.doubleValue)
        }
    }

    fn sample(&mut self, list: &mut Vec<Reading>) {
        let dt = self.last_at.elapsed().as_secs_f64().max(0.001);
        self.last_at = Instant::now();

        let (total, idle) = cpu_times();
        let (dtotal, didle) = (total.wrapping_sub(self.last_cpu.0), idle.wrapping_sub(self.last_cpu.1));
        self.last_cpu = (total, idle);
        if dtotal > 0 {
            let load = (1.0 - didle as f64 / dtotal as f64) * 100.0;
            push(list, "sys:cpu_load", "CPU load", "CPU", "%", load.clamp(0.0, 100.0));
        }

        let mut mem = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
            let gib = 1024.0 * 1024.0 * 1024.0;
            push(list, "sys:ram_load", "RAM load", "Memory", "%", mem.dwMemoryLoad as f64);
            push(list, "sys:ram_used", "RAM used", "Memory", "GB", (mem.ullTotalPhys - mem.ullAvailPhys) as f64 / gib);
            push(list, "sys:ram_total", "RAM total", "Memory", "GB", mem.ullTotalPhys as f64 / gib);
        }

        let (rx, tx) = net_totals();
        let (drx, dtx) = (rx.saturating_sub(self.last_net.0), tx.saturating_sub(self.last_net.1));
        self.last_net = (rx, tx);
        let mbit = 1_000_000.0 / 8.0;
        push(list, "sys:net_down", "Download", "Network", "Mb/s", drx as f64 / dt / mbit);
        push(list, "sys:net_up", "Upload", "Network", "Mb/s", dtx as f64 / dt / mbit);

        if !self.pdh.is_invalid() {
            unsafe { PdhCollectQueryData(self.pdh) };
            let mb = 1024.0 * 1024.0;
            if let Some(v) = self.counter(self.disk_read) {
                push(list, "sys:disk_read", "Disk read", "Storage", "MB/s", v / mb);
            }
            if let Some(v) = self.counter(self.disk_write) {
                push(list, "sys:disk_write", "Disk write", "Storage", "MB/s", v / mb);
            }
            if let (Some(v), true) = (self.counter(self.perf), self.base_mhz > 0.0) {
                push(list, "sys:cpu_clock", "CPU clock", "CPU", "MHz", v / 100.0 * self.base_mhz);
            }
        }
        push(list, "sys:uptime", "Uptime", "System", "h", unsafe { GetTickCount64() } as f64 / 3_600_000.0);
    }
}

fn cpu_times() -> (u64, u64) {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe {
        if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_err() {
            return (0, 0);
        }
    }
    // Kernel time includes idle time.
    (ft(kernel) + ft(user), ft(idle))
}

fn base_mhz() -> f64 {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};
    let mut value = 0u32;
    let mut size = 4u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0"),
            w!("~MHz"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        );
    }
    value as f64
}

fn net_totals() -> (u64, u64) {
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() {
            return (0, 0);
        }
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let mut totals = (0u64, 0u64);
        for r in rows {
            // Hardware interfaces only; filter stacks would double-count every packet.
            if r.OperStatus == IfOperStatusUp && r.InterfaceAndOperStatusFlags._bitfield & 1 != 0 {
                totals.0 += r.InOctets;
                totals.1 += r.OutOctets;
            }
        }
        FreeMibTable(table as *const _);
        totals
    }
}

struct View {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl View {
    fn open(name: PCWSTR) -> Option<Self> {
        unsafe {
            let handle = OpenFileMappingW(FILE_MAP_READ.0, false, name).ok()?;
            let view = MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0);
            if view.Value.is_null() {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(Self { handle, view })
        }
    }
    fn ptr(&self) -> *const u8 {
        self.view.Value as *const u8
    }
}

impl Drop for View {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

unsafe fn u32_at(p: *const u8, off: usize) -> u32 {
    std::ptr::read_unaligned(p.add(off) as *const u32)
}
unsafe fn cstr_at(p: *const u8, off: usize, max: usize) -> String {
    let s = std::slice::from_raw_parts(p.add(off), max);
    let end = s.iter().position(|&b| b == 0).unwrap_or(max);
    decode(&s[..end])
}

/// Sources disagree on encoding: UTF-8 where it is valid, otherwise Latin-1,
/// which is how a lone degree-sign byte arrives.
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn degree_sign_decodes_from_either_encoding() {
        assert_eq!(super::decode(&[0xB0, b'C']), "\u{b0}C");
        assert_eq!(super::decode(&[0xC2, 0xB0, b'C']), "\u{b0}C");
        assert_eq!(super::decode(b"MHz"), "MHz");
    }
}

fn group_of(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if n.starts_with("gpu") {
        "GPU"
    } else if n.starts_with("cpu") {
        "CPU"
    } else if n.contains("ram") || n.contains("commit") || n.contains("memory") {
        "Memory"
    } else if n.contains("fan") || n.contains("pump") {
        "Cooling"
    } else {
        "Other"
    }
}

fn read_afterburner(list: &mut Vec<Reading>) {
    const MAHM: u32 = 0x4D41_484D;
    let Some(v) = View::open(w!("MAHMSharedMemory")) else { return };
    unsafe {
        let p = v.ptr();
        if u32_at(p, 0) != MAHM {
            return;
        }
        let (header, count, size) = (u32_at(p, 8) as usize, u32_at(p, 12) as usize, u32_at(p, 16) as usize);
        for i in 0..count.min(512) {
            let e = header + i * size;
            let name = cstr_at(p, e, 260);
            let unit = cstr_at(p, e + 260, 260).trim().to_string();
            let value = std::ptr::read_unaligned(p.add(e + 1300) as *const f32);
            // Afterburner flags an unavailable source with FLT_MAX.
            if value >= f32::MAX || name.is_empty() {
                continue;
            }
            let group = group_of(&name);
            list.push(Reading { id: format!("mahm:{name}"), name, group: group.into(), unit, value: value as f64 });
        }
    }
}

fn read_hwinfo(list: &mut Vec<Reading>) {
    const HWIS: u32 = 0x5369_5748;
    let Some(v) = View::open(w!("Global\\HWiNFO_SENS_SM2")).or_else(|| View::open(w!("HWiNFO_SENS_SM2"))) else {
        return;
    };
    unsafe {
        let p = v.ptr();
        if u32_at(p, 0) != HWIS {
            return;
        }
        let (s_off, s_size, s_num) = (u32_at(p, 20) as usize, u32_at(p, 24) as usize, u32_at(p, 28) as usize);
        let (r_off, r_size, r_num) = (u32_at(p, 32) as usize, u32_at(p, 36) as usize, u32_at(p, 40) as usize);
        for i in 0..r_num.min(4096) {
            let e = r_off + i * r_size;
            let kind = u32_at(p, e);
            let sensor = u32_at(p, e + 4) as usize;
            let reading = u32_at(p, e + 8);
            if kind == 0 || sensor >= s_num {
                continue;
            }
            let label = cstr_at(p, e + 12 + 128, 128);
            let unit = cstr_at(p, e + 12 + 256, 16);
            let value = std::ptr::read_unaligned(p.add(e + 12 + 256 + 16) as *const f64);
            let s_entry = s_off + sensor * s_size;
            let (s_id, s_inst) = (u32_at(p, s_entry), u32_at(p, s_entry + 4));
            let s_name = cstr_at(p, s_entry + 8 + 128, 128);
            list.push(Reading {
                id: format!("hwi:{s_id:x}.{s_inst:x}.{reading:x}"),
                name: label,
                group: s_name,
                unit,
                value,
            });
        }
    }
}

/// CPU time and working set of this process, for the performance panel.
pub struct SelfUsage {
    last_cpu: u64,
    last_at: Instant,
}

impl SelfUsage {
    pub fn new() -> Self {
        Self { last_cpu: 0, last_at: Instant::now() }
    }
    /// Returns (percent of one whole machine, private working set in MB).
    pub fn sample(&mut self) -> (f64, f64) {
        use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        unsafe {
            let me = GetCurrentProcess();
            let (mut a, mut b, mut k, mut u) =
                (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
            let mut pct = 0.0;
            if GetProcessTimes(me, &mut a, &mut b, &mut k, &mut u).is_ok() {
                let cpu = ft(k) + ft(u);
                let dt = self.last_at.elapsed().as_secs_f64();
                if self.last_cpu != 0 && dt > 0.0 {
                    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
                    pct = (cpu - self.last_cpu) as f64 / 1e7 / dt / cores * 100.0;
                }
                self.last_cpu = cpu;
                self.last_at = Instant::now();
            }
            let mut mem = PROCESS_MEMORY_COUNTERS::default();
            let _ = K32GetProcessMemoryInfo(me, &mut mem, size_of::<PROCESS_MEMORY_COUNTERS>() as u32);
            (pct, mem.WorkingSetSize as f64 / (1024.0 * 1024.0))
        }
    }
}
