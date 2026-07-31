use std::path::Path;

use super::VerifiedPayloadAuthority;
use super::filesystem_authority::remove_verified_payload_file;

/// Tracks only payload files created by a caller-managed database transaction.
/// Existing files are never journaled, making cleanup O(new files).
pub(crate) struct PayloadFileRollback {
    created_files: Vec<VerifiedPayloadAuthority>,
    cleanup_on_drop: bool,
}

impl PayloadFileRollback {
    /// Arms synchronous file cleanup when the owning database transaction is
    /// dropped before commit. The caller must disarm this guard only after the
    /// transaction has committed successfully.
    pub(crate) fn begin_cancellation_safe(_storage_root: &Path) -> Self {
        Self {
            created_files: Vec::new(),
            cleanup_on_drop: true,
        }
    }

    pub(crate) fn disarm(mut self) {
        self.cleanup_on_drop = false;
    }

    pub(super) fn record_created(&mut self, authority: VerifiedPayloadAuthority) {
        self.created_files.push(authority);
    }
}

impl Drop for PayloadFileRollback {
    fn drop(&mut self) {
        if !self.cleanup_on_drop || self.created_files.is_empty() {
            return;
        }
        for authority in &self.created_files {
            let _ = remove_verified_payload_file(authority);
        }
    }
}
