//! Process CPU and memory sampling for the diagnostics screen.

use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessMetrics {
    pub cpu_percent: f64,
    pub working_set_mb: f64,
}

static LAST: Mutex<Option<(Instant, u64)>> = Mutex::new(None);

/// CPU usage is averaged since the previous call (across all cores).
pub fn sample() -> ProcessMetrics {
    let (cpu_100ns, ws) = raw();
    let now = Instant::now();
    let mut last = LAST.lock().unwrap_or_else(|p| p.into_inner());
    let cpu_percent = match *last {
        Some((t, prev)) => {
            let wall = now.duration_since(t).as_secs_f64();
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
            if wall > 0.0 {
                (cpu_100ns.saturating_sub(prev) as f64 / 1e7) / (wall * cores) * 100.0
            } else {
                0.0
            }
        }
        None => 0.0,
    };
    *last = Some((now, cpu_100ns));
    ProcessMetrics { cpu_percent, working_set_mb: ws as f64 / (1024.0 * 1024.0) }
}

#[cfg(windows)]
fn raw() -> (u64, u64) {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn ft(f: FILETIME) -> u64 {
        ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
    }
    unsafe {
        let process = GetCurrentProcess();
        let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) = (zeroed(), zeroed(), zeroed(), zeroed());
        let cpu = if GetProcessTimes(process, &mut c, &mut e, &mut k, &mut u) != 0 { ft(k) + ft(u) } else { 0 };
        let mut pmc: PROCESS_MEMORY_COUNTERS = zeroed();
        pmc.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ws = if K32GetProcessMemoryInfo(process, &mut pmc, pmc.cb) != 0 { pmc.WorkingSetSize as u64 } else { 0 };
        (cpu, ws)
    }
}

#[cfg(not(windows))]
fn raw() -> (u64, u64) {
    (0, 0)
}
