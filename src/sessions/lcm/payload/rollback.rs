use std::path::{Path, PathBuf};

use libsql::Connection;

use super::{
    LcmError, existing_payload_dir_opt, payload_metadata_exists, safe_remove_payload_file,
};

/// Tracks only payload files created by a caller-managed database transaction.
/// Existing files are never journaled, making cleanup O(new files).
pub(crate) struct PayloadFileRollback {
    storage_root: PathBuf,
    created_refs: Vec<String>,
}

impl PayloadFileRollback {
    pub(crate) fn begin(storage_root: &Path) -> Self {
        Self {
            storage_root: storage_root.to_path_buf(),
            created_refs: Vec::new(),
        }
    }

    pub(super) fn record_created(&mut self, payload_ref: &str) {
        self.created_refs.push(payload_ref.to_string());
    }

    pub(crate) async fn rollback(self, conn: &Connection) -> Result<usize, LcmError> {
        if self.created_refs.is_empty() {
            return Ok(0);
        }
        conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = self.rollback_while_writer_locked(conn).await;
        match result {
            Ok(removed) => {
                conn.execute("COMMIT", ()).await?;
                Ok(removed)
            }
            Err(error) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(error)
            }
        }
    }

    async fn rollback_while_writer_locked(self, conn: &Connection) -> Result<usize, LcmError> {
        let Some(dir) = existing_payload_dir_opt(&self.storage_root)? else {
            return Ok(0);
        };
        let mut removed = 0;
        let mut first_error = None;
        for payload_ref in self.created_refs {
            if payload_metadata_exists(conn, &payload_ref).await? {
                continue;
            }
            match safe_remove_payload_file(&dir, &payload_ref) {
                Ok(true) => removed += 1,
                Ok(false) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(removed), Err)
    }
}
