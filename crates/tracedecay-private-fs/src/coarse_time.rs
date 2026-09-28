//! Kernel coarse-clock witness for filesystem change times.
//!
//! Linux stamps inode mtime and ctime from `CLOCK_REALTIME_COARSE`. Two
//! writes inside that quantum can share one timestamp, so a change time that
//! is still inside the current quantum is not proof the bytes are stable.

/// Whether `changed_at_nanos` is strictly older than the kernel clock that
/// stamps inode change times.
///
/// On Linux that clock is `CLOCK_REALTIME_COARSE`. A change time still inside
/// the current quantum can be shared with a later write, so callers must not
/// treat it as proof of unchanged bytes. Other platforms advance a
/// finer-grained change time for the writes this witness exists to see, and
/// a missing clock fails closed as unsettled only on Linux.
pub fn change_time_settled(changed_at_nanos: i128) -> bool {
    #[cfg(target_os = "linux")]
    {
        linux_coarse_now_nanos().is_some_and(|now| changed_at_nanos < now)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = changed_at_nanos;
        true
    }
}

#[cfg(target_os = "linux")]
fn linux_coarse_now_nanos() -> Option<i128> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` points at a writable `timespec`, and `CLOCK_REALTIME_COARSE`
    // is a clock id this process is allowed to read.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_REALTIME_COARSE, &mut time) };
    if result != 0 {
        return None;
    }
    i128::from(time.tv_sec)
        .checked_mul(1_000_000_000)
        .and_then(|seconds| seconds.checked_add(i128::from(time.tv_nsec)))
}

#[cfg(test)]
mod tests {
    use super::change_time_settled;

    #[test]
    fn epoch_change_time_is_settled() {
        assert!(change_time_settled(0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_future_change_time_is_not_settled() {
        assert!(!change_time_settled(i128::MAX / 4));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_fresh_write_shares_the_current_coarse_quantum() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fresh");
        let mut saw_unsettled = false;
        for _ in 0..64 {
            std::fs::write(&path, b"x").unwrap();
            let metadata = std::fs::metadata(&path).unwrap();
            let changed_at_nanos =
                i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec());
            if !change_time_settled(changed_at_nanos) {
                saw_unsettled = true;
                break;
            }
        }
        assert!(
            saw_unsettled,
            "a write inside the coarse quantum must not count as a settled change time"
        );
    }
}
