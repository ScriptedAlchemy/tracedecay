//! `ProfileLost`: the profile this daemon serves was deleted or replaced.
//!
//! A daemon whose profile directory is deleted keeps writing into unlinked
//! files that no census sees (#4099). It watches the profile identity file by
//! inode and, once that file is gone or names another inode, shuts down so its
//! supervisor restarts it on the profile now at the path.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracedecay_runtime_core::native_runtime;
use tracedecay_runtime_core::storage::PROFILE_IDENTITY_FILENAME;

/// How often the daemon checks its profile identity file.
pub(super) const PROFILE_WATCH_INTERVAL: Duration = Duration::from_secs(1);

pub(super) struct ProfileIdentityWatch {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl ProfileIdentityWatch {
    /// Records the identity file's inode; it holds no descriptor on it.
    pub(super) fn pin(profile_root: &Path) -> std::io::Result<Self> {
        let path = profile_root.join(PROFILE_IDENTITY_FILENAME);
        let metadata = std::fs::metadata(&path)?;
        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    /// A file that cannot be inspected for another reason is not lost.
    fn lost(&self) -> bool {
        match std::fs::metadata(&self.path) {
            Ok(current) => current.dev() != self.device || current.ino() != self.inode,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }

    /// Resolves once the profile is lost.
    pub(super) async fn profile_lost(&self) {
        while !self.lost() {
            let _ = native_runtime::sleep_until(Instant::now() + PROFILE_WATCH_INTERVAL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_file_deleted_or_replaced_is_a_lost_profile() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(PROFILE_IDENTITY_FILENAME);
        std::fs::write(&path, "{}").unwrap();
        let watch = ProfileIdentityWatch::pin(root.path()).unwrap();
        assert!(!watch.lost(), "the pinned file is the profile");

        let replacement = root.path().join("replacement.json");
        std::fs::write(&replacement, "{}").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(
            watch.lost(),
            "a file renamed over the identity is another profile"
        );

        let watch = ProfileIdentityWatch::pin(root.path()).unwrap();
        std::fs::remove_dir_all(root.path()).unwrap();
        assert!(watch.lost(), "a deleted profile is lost");
    }
}
