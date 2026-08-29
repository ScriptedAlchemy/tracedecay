//! Process CPU-tick samples for semantic evaluation.
//!
//! These two readers used to live on the root `runtime_telemetry` module. They
//! are OS process facts, not daemon composition, so they travel with the
//! evaluation owner.

/// Linux process CPU ticks (`utime + stime`) from `/proc/self/stat`.
pub(crate) fn read_linux_process_cpu_ticks() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
        let fields = fields.collect::<Vec<_>>();
        let user_ticks = fields.get(11)?.parse::<u64>().ok()?;
        let system_ticks = fields.get(12)?.parse::<u64>().ok()?;
        user_ticks.checked_add(system_ticks)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub(crate) fn linux_clock_ticks_per_second() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        static TICKS_PER_SECOND: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        if let Some(ticks) = TICKS_PER_SECOND.get() {
            return Some(*ticks);
        }
        let output = std::process::Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let ticks = std::str::from_utf8(&output.stdout)
            .ok()?
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|ticks| *ticks != 0)?;
        let _ = TICKS_PER_SECOND.set(ticks);
        Some(ticks)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
