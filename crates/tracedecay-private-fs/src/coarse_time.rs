//! Filesystem change-time witnesses.
//!
//! The kernel stamps inode mtime and ctime from a coarse clock (Linux:
//! `CLOCK_REALTIME_COARSE`), and the filesystem may truncate that stamp
//! further (1 s on ext3 and HFS+, 2 s on FAT). Two writes inside one
//! timestamp quantum can share a stamp, so a change time is proof the bytes
//! are stable only once the clock has left the quantum it was stamped in.

use std::collections::hash_map::RandomState;
use std::fs::Metadata;
use std::hash::BuildHasher;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// The coarsest timestamp granularity a supported filesystem stores (FAT).
const COARSEST_STAMP_GRANULARITY_NANOS: i128 = 2 * NANOS_PER_SECOND;

/// The stat field that witnesses an in-place rewrite of a file's bytes.
///
/// A stat observation is a negative cache: a moved field proves the file
/// changed, but an unmoved one proves its bytes unchanged only when some field
/// must advance on every write. Where none must, callers settle currency
/// against the bytes' content digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RewriteWitness {
    /// Unix ctime: every write advances it and no API sets it back.
    ChangeTime,
    /// No stat field. NTFS `ChangeTime` stays put when a writer restores
    /// `LastWriteTime` through its handle, so on Windows an unchanged stat
    /// only fails to disprove a same-length rewrite.
    Absent,
}

impl RewriteWitness {
    /// This platform's witness.
    pub const NATIVE: Self = if cfg!(unix) {
        Self::ChangeTime
    } else {
        Self::Absent
    };

    /// Whether an unchanged stat under this witness can prove unchanged bytes.
    #[must_use]
    pub const fn proves_unchanged_bytes(self) -> bool {
        matches!(self, Self::ChangeTime)
    }

    /// The witnessing change time of `metadata` in nanoseconds since the
    /// epoch, `None` when this witness has no such field here.
    #[must_use]
    pub fn change_time_nanos(self, metadata: &Metadata) -> Option<i128> {
        match self {
            Self::ChangeTime => native_change_time_nanos(metadata),
            Self::Absent => None,
        }
    }

    /// Whether a later stat equal to `metadata` proves the file's bytes
    /// unchanged since this one: the witness has a change time and that time
    /// is already behind the clock that stamps the next write.
    ///
    /// Ask before reading the bytes the stat is to vouch for. A change time
    /// that settles only afterwards can be shared with a write during the read.
    #[must_use]
    pub fn vouches_for_unchanged_bytes(self, metadata: &Metadata) -> bool {
        self.stamp(metadata, ChangeClockReading::now()).is_settled()
    }

    /// The change-time component of a cached stat identity for `metadata`,
    /// settled against `reading`. Take `reading` before the stat and before
    /// reading the bytes the identity is to vouch for.
    #[must_use]
    pub fn stamp(self, metadata: &Metadata, reading: ChangeClockReading) -> ChangeStamp {
        match self.change_time_nanos(metadata) {
            Some(changed_at) if reading.settles(changed_at) => ChangeStamp::Settled(changed_at),
            _ => ChangeStamp::unvouched(),
        }
    }
}

/// The change-time component of a cached stat identity.
///
/// Two stamps are equal only when both carry the same settled change time,
/// so an identity equal to a later one proves the bytes unchanged. A stamp no
/// reading settled is unique to its sample and equals only its own copies, so
/// an identity holding one never matches a later stat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeStamp {
    Settled(i128),
    Unvouched(u128),
}

impl ChangeStamp {
    fn unvouched() -> Self {
        // The random half keeps samples from different processes apart where
        // an identity is persisted as a digest.
        static PROCESS: LazyLock<u64> = LazyLock::new(|| RandomState::new().hash_one(0_u8));
        static SAMPLE: AtomicU64 = AtomicU64::new(0);
        let sample = SAMPLE.fetch_add(1, Ordering::Relaxed);
        Self::Unvouched((u128::from(*PROCESS) << 64) | u128::from(sample))
    }

    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Settled(_))
    }

    /// A tagged encoding for hashing the stamp into a digest identity.
    #[must_use]
    pub fn to_le_bytes(self) -> [u8; 17] {
        let (tag, value) = match self {
            Self::Settled(changed_at) => (1, changed_at.to_le_bytes()),
            Self::Unvouched(sample) => (0, sample.to_le_bytes()),
        };
        let mut bytes = [0; 17];
        bytes[0] = tag;
        bytes[1..].copy_from_slice(&value);
        bytes
    }
}

/// One reading of the clock that stamps inode change times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangeClockReading(Option<i128>);

impl ChangeClockReading {
    /// An unreadable clock is a reading that settles nothing.
    #[must_use]
    pub fn now() -> Self {
        Self(change_clock_now_nanos())
    }

    /// Whether every write after this reading stamps a change time other
    /// than `changed_at_nanos`: the clock has left the quantum the stamp was
    /// taken in. The quantum is the stamp's own precision, the largest power
    /// of ten dividing its sub-second part, or the coarsest supported
    /// granularity when it has none. A finer filesystem whose stamp happens
    /// to end in zeros only waits longer; a coarser one never settles early.
    #[must_use]
    pub fn settles(self, changed_at_nanos: i128) -> bool {
        self.0.is_some_and(|now| {
            changed_at_nanos
                .checked_add(stamp_granularity_nanos(changed_at_nanos))
                .is_some_and(|next_quantum| next_quantum <= now)
        })
    }
}

fn stamp_granularity_nanos(changed_at_nanos: i128) -> i128 {
    let mut subsecond = changed_at_nanos.rem_euclid(NANOS_PER_SECOND);
    if subsecond == 0 {
        return COARSEST_STAMP_GRANULARITY_NANOS;
    }
    let mut granularity = 1;
    while subsecond % 10 == 0 {
        subsecond /= 10;
        granularity *= 10;
    }
    granularity
}

/// Whether `changed_at_nanos` has left the timestamp quantum it was stamped
/// in, by the change clock read now. See [`ChangeClockReading::settles`].
#[must_use]
pub fn change_time_settled(changed_at_nanos: i128) -> bool {
    ChangeClockReading::now().settles(changed_at_nanos)
}

#[cfg(unix)]
fn native_change_time_nanos(metadata: &Metadata) -> Option<i128> {
    Some(
        i128::from(metadata.ctime())
            .saturating_mul(NANOS_PER_SECOND)
            .saturating_add(i128::from(metadata.ctime_nsec())),
    )
}

#[cfg(not(unix))]
fn native_change_time_nanos(_metadata: &Metadata) -> Option<i128> {
    None
}

#[cfg(target_os = "linux")]
fn change_clock_now_nanos() -> Option<i128> {
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
        .checked_mul(NANOS_PER_SECOND)
        .and_then(|seconds| seconds.checked_add(i128::from(time.tv_nsec)))
}

#[cfg(not(target_os = "linux"))]
fn change_clock_now_nanos() -> Option<i128> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i128::try_from(elapsed.as_nanos()).ok())
}

#[cfg(test)]
mod tests {
    use super::{ChangeClockReading, ChangeStamp, RewriteWitness, change_time_settled};

    const SECOND: i128 = 1_000_000_000;

    fn reading_at(nanos: i128) -> ChangeClockReading {
        ChangeClockReading(Some(nanos))
    }

    #[test]
    fn epoch_change_time_is_settled() {
        assert!(change_time_settled(0));
    }

    #[test]
    fn a_future_change_time_is_not_settled() {
        assert!(!change_time_settled(i128::MAX / 4));
    }

    #[test]
    fn a_whole_second_stamp_is_unsettled_until_the_coarsest_quantum_passes() {
        let stamped = 1_759_000_000 * SECOND;

        assert!(!reading_at(stamped + SECOND / 2).settles(stamped));
        assert!(!reading_at(stamped + 2 * SECOND - 1).settles(stamped));
        assert!(reading_at(stamped + 2 * SECOND).settles(stamped));
    }

    #[test]
    fn a_stamp_settles_after_the_quantum_its_precision_shows() {
        let nanosecond = 1_759_000_000 * SECOND + 123_456_789;
        assert!(!reading_at(nanosecond).settles(nanosecond));
        assert!(reading_at(nanosecond + 1).settles(nanosecond));

        let ten_milliseconds = 1_759_000_000 * SECOND + 120_000_000;
        assert!(!reading_at(ten_milliseconds + 9_999_999).settles(ten_milliseconds));
        assert!(reading_at(ten_milliseconds + 10_000_000).settles(ten_milliseconds));

        let before_epoch = -SECOND + 500_000_000;
        assert!(!reading_at(before_epoch + 99_999_999).settles(before_epoch));
        assert!(reading_at(before_epoch + 100_000_000).settles(before_epoch));
    }

    #[test]
    fn an_unreadable_clock_settles_nothing() {
        assert!(!ChangeClockReading(None).settles(0));
    }

    /// A one-second filesystem stamps the current whole second. Under the
    /// kernel clock alone that stamp looked settled a tick after the write,
    /// while a same-size rewrite later in the second still shares it.
    #[test]
    fn a_coarse_stamp_from_this_second_is_not_settled() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let this_second = i128::from(now.as_secs()) * SECOND;

        assert!(!change_time_settled(this_second));
    }

    #[test]
    fn only_settled_stamps_compare_equal() {
        let stamped = 1_759_000_000 * SECOND + 123_456_789;
        let settled = ChangeStamp::Settled(stamped);
        assert_eq!(settled, ChangeStamp::Settled(stamped));
        assert_eq!(settled.to_le_bytes()[0], 1);

        let unvouched = ChangeStamp::unvouched();
        assert_eq!(unvouched, unvouched);
        assert_ne!(unvouched, ChangeStamp::unvouched());
        assert_ne!(
            unvouched.to_le_bytes(),
            ChangeStamp::unvouched().to_le_bytes()
        );
        assert!(!unvouched.is_settled());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_fresh_write_cannot_vouch_for_its_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fresh");
        let mut saw_unvouched = false;
        for _ in 0..64 {
            std::fs::write(&path, b"x").unwrap();
            let metadata = std::fs::metadata(&path).unwrap();
            if !RewriteWitness::ChangeTime.vouches_for_unchanged_bytes(&metadata) {
                saw_unvouched = true;
                break;
            }
        }
        assert!(
            saw_unvouched,
            "a change time inside the coarse quantum must not vouch for unchanged bytes"
        );
    }

    #[test]
    fn only_a_change_time_witness_vouches_for_a_settled_file() {
        let settled = std::fs::metadata(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .expect("stat the crate manifest");

        assert_eq!(
            RewriteWitness::ChangeTime.vouches_for_unchanged_bytes(&settled),
            cfg!(unix)
        );
        assert!(!RewriteWitness::Absent.vouches_for_unchanged_bytes(&settled));
        assert!(
            !RewriteWitness::Absent
                .stamp(&settled, ChangeClockReading::now())
                .is_settled()
        );
    }
}
