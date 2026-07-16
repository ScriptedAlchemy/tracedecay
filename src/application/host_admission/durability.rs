//! Shared directory-fsync durability primitive.
//!
//! Publishing a file durably requires flushing the containing directory so a
//! create, rename, or remove survives a crash. Directory fsync is a Unix
//! concept; platforms without it treat this as a successful no-op. Callers pick
//! how a genuine failure is surfaced via [`DirectorySyncPolicy`].

use std::io;
use std::path::Path;

/// How a directory fsync failure is surfaced to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectorySyncPolicy {
    /// Surface every fsync failure.
    Strict,
    /// Surface genuine IO failures but tolerate `InvalidInput` from filesystems
    /// that do not support directory fsync.
    TolerateUnsupported,
    /// Never surface a fsync failure.
    BestEffort,
}

/// Flush a directory's metadata so a preceding create/rename/remove is durable.
///
/// On platforms without portable directory fsync this is a no-op that succeeds
/// under every policy.
pub(crate) fn sync_directory(dir: &Path, policy: DirectorySyncPolicy) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::fs::File;
        match File::open(dir).and_then(|directory| directory.sync_all()) {
            Ok(()) => Ok(()),
            Err(_) if matches!(policy, DirectorySyncPolicy::BestEffort) => Ok(()),
            Err(error)
                if matches!(policy, DirectorySyncPolicy::TolerateUnsupported)
                    && error.kind() == io::ErrorKind::InvalidInput =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (dir, policy);
        Ok(())
    }
}
