//! Peak resident-set observation for resource samples.

use std::fs;

use tracedecay_query::search_quality::candidate_output::{
    ResourceMeasurementStatusV1, ResourceSampleV1,
};
#[cfg(windows)]
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::GetCurrentProcess;

#[derive(Debug, Eq, PartialEq)]
pub(super) enum PeakRssObservation {
    Measured(u64),
    Pending(PeakRssPendingReason),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum PeakRssPendingReason {
    #[cfg(any(target_os = "linux", test))]
    LinuxStatusReadFailure(String),
    #[cfg(any(target_os = "linux", test))]
    LinuxMissingNonzeroVmHwm,
    #[cfg(any(target_os = "macos", test))]
    MacOsGetrusageFailure(String),
    #[cfg(any(target_os = "macos", test))]
    MacOsNonPositiveMaxRss,
    #[cfg(any(windows, test))]
    WindowsK32GetProcessMemoryInfoFailure(String),
    #[cfg(any(windows, test))]
    WindowsZeroPeakWorkingSetSize,
    #[cfg(any(not(any(target_os = "linux", target_os = "macos", windows)), test))]
    UnsupportedPlatform(&'static str),
}

impl std::fmt::Display for PeakRssPendingReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(any(target_os = "linux", test))]
            Self::LinuxStatusReadFailure(error) => write!(
                formatter,
                "Linux peak_rss_bytes is unavailable because /proc/self/status could not be read: {error}"
            ),
            #[cfg(any(target_os = "linux", test))]
            Self::LinuxMissingNonzeroVmHwm => formatter.write_str(
                "Linux peak_rss_bytes is unavailable because /proc/self/status has no nonzero VmHWM value",
            ),
            #[cfg(any(target_os = "macos", test))]
            Self::MacOsGetrusageFailure(error) => write!(
                formatter,
                "macOS peak_rss_bytes is unavailable because getrusage(RUSAGE_SELF) failed: {error}"
            ),
            #[cfg(any(target_os = "macos", test))]
            Self::MacOsNonPositiveMaxRss => formatter.write_str(
                "macOS peak_rss_bytes is unavailable because getrusage(RUSAGE_SELF) returned a non-positive ru_maxrss",
            ),
            #[cfg(any(windows, test))]
            Self::WindowsK32GetProcessMemoryInfoFailure(error) => write!(
                formatter,
                "Windows peak_rss_bytes is unavailable because K32GetProcessMemoryInfo failed before PeakWorkingSetSize could be read: {error}"
            ),
            #[cfg(any(windows, test))]
            Self::WindowsZeroPeakWorkingSetSize => formatter.write_str(
                "Windows peak_rss_bytes is unavailable because K32GetProcessMemoryInfo returned zero PeakWorkingSetSize",
            ),
            #[cfg(any(not(any(target_os = "linux", target_os = "macos", windows)), test))]
            Self::UnsupportedPlatform(platform) => write!(
                formatter,
                "{platform} peak_rss_bytes is unavailable because the platform is unsupported"
            ),
        }
    }
}

impl PeakRssObservation {
    pub(super) fn max(self, other: Self) -> Self {
        match (self, other) {
            (Self::Measured(left), Self::Measured(right)) => Self::Measured(left.max(right)),
            (Self::Pending(reason), _) | (Self::Measured(_), Self::Pending(reason)) => {
                Self::Pending(reason)
            }
        }
    }

    #[cfg(test)]
    pub(super) fn is_measured(&self) -> bool {
        matches!(self, Self::Measured(_))
    }
}

pub(super) fn completed_resource_sample(
    eligible_chunks: u64,
    peak_rss: PeakRssObservation,
    latency_samples_us: Vec<u64>,
    measured_queries: u64,
) -> ResourceSampleV1 {
    let (status, peak_rss_bytes, pending_reason) = match peak_rss {
        PeakRssObservation::Measured(bytes) => {
            (ResourceMeasurementStatusV1::Measured, Some(bytes), None)
        }
        PeakRssObservation::Pending(reason) => (
            ResourceMeasurementStatusV1::Pending,
            None,
            Some(reason.to_string()),
        ),
    };
    ResourceSampleV1 {
        status,
        eligible_chunks,
        peak_rss_bytes,
        latency_samples_us,
        measured_queries,
        pending_reason,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn peak_rss_bytes() -> PeakRssObservation {
    let status = match fs::read_to_string("/proc/self/status") {
        Ok(status) => status,
        Err(error) => {
            return PeakRssObservation::Pending(PeakRssPendingReason::LinuxStatusReadFailure(
                error.to_string(),
            ));
        }
    };
    match peak_rss_bytes_from_status(&status) {
        Some(bytes) => PeakRssObservation::Measured(bytes),
        None => PeakRssObservation::Pending(PeakRssPendingReason::LinuxMissingNonzeroVmHwm),
    }
}

/// macOS reports the peak resident set in bytes through `getrusage`; Linux
/// keeps `/proc` because `ru_maxrss` there is kilobytes and `VmHWM` is exact.
#[cfg(target_os = "macos")]
pub(super) fn peak_rss_bytes() -> PeakRssObservation {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: `getrusage` fully initialises the out-parameter when it returns 0.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if rc != 0 {
        return PeakRssObservation::Pending(PeakRssPendingReason::MacOsGetrusageFailure(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    // SAFETY: checked above that the call succeeded and wrote the struct.
    let usage = unsafe { usage.assume_init() };
    match u64::try_from(usage.ru_maxrss)
        .ok()
        .filter(|bytes| *bytes > 0)
    {
        Some(bytes) => PeakRssObservation::Measured(bytes),
        None => PeakRssObservation::Pending(PeakRssPendingReason::MacOsNonPositiveMaxRss),
    }
}

#[cfg(windows)]
pub(super) fn peak_rss_bytes() -> PeakRssObservation {
    let counter_size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: counter_size,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    // SAFETY: `GetCurrentProcess` returns a valid pseudo-handle, and `counters`
    // points to a writable value whose exact size is supplied to the API.
    let succeeded =
        unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counter_size) };
    let api_error = (succeeded == 0).then(|| std::io::Error::last_os_error().to_string());
    windows_peak_rss_observation(counters.PeakWorkingSetSize, api_error)
}

#[cfg(any(windows, test))]
pub(super) fn windows_peak_rss_observation(
    peak_working_set_size: usize,
    api_error: Option<String>,
) -> PeakRssObservation {
    if let Some(error) = api_error {
        return PeakRssObservation::Pending(
            PeakRssPendingReason::WindowsK32GetProcessMemoryInfoFailure(error),
        );
    }
    match u64::try_from(peak_working_set_size)
        .ok()
        .filter(|bytes| *bytes > 0)
    {
        Some(bytes) => PeakRssObservation::Measured(bytes),
        None => PeakRssObservation::Pending(PeakRssPendingReason::WindowsZeroPeakWorkingSetSize),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(super) fn peak_rss_bytes() -> PeakRssObservation {
    PeakRssObservation::Pending(PeakRssPendingReason::UnsupportedPlatform(
        std::env::consts::OS,
    ))
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn peak_rss_bytes_from_status(status: &str) -> Option<u64> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmHWM:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())?;
            return kb.checked_mul(1024).filter(|bytes| *bytes > 0);
        }
    }
    None
}
