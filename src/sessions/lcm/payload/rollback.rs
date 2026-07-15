use std::path::{Path, PathBuf};

use super::{existing_payload_dir_opt, safe_remove_payload_file};

/// Tracks only payload files created by a caller-managed database transaction.
/// Existing files are never journaled, making cleanup O(new files).
pub(crate) struct PayloadFileRollback {
    storage_root: PathBuf,
    created_refs: Vec<String>,
    cleanup_on_drop: bool,
}

impl PayloadFileRollback {
    /// Arms synchronous file cleanup when the owning database transaction is
    /// dropped before commit. The caller must disarm this guard only after the
    /// transaction has committed successfully.
    pub(crate) fn begin_cancellation_safe(storage_root: &Path) -> Self {
        Self {
            storage_root: storage_root.to_path_buf(),
            created_refs: Vec::new(),
            cleanup_on_drop: true,
        }
    }

    pub(crate) fn disarm(mut self) {
        self.cleanup_on_drop = false;
    }

    pub(super) fn record_created(&mut self, payload_ref: &str) {
        self.created_refs.push(payload_ref.to_string());
    }
}

impl Drop for PayloadFileRollback {
    fn drop(&mut self) {
        if !self.cleanup_on_drop || self.created_refs.is_empty() {
            return;
        }
        let Ok(Some(dir)) = existing_payload_dir_opt(&self.storage_root) else {
            return;
        };
        for payload_ref in &self.created_refs {
            let _ = safe_remove_payload_file(&dir, payload_ref);
        }
    }
}
