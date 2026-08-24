use std::path::Path;

use crate::errors::TraceDecayError;

pub(super) fn read_only_upgrade_error(db_path: &Path, operation: &str) -> TraceDecayError {
    TraceDecayError::Database {
        message: format!(
            "cannot upgrade the daemon's shared read-only connection at '{}' to writable; acquire writable ownership before opening read handles",
            db_path.display()
        ),
        operation: operation.to_string(),
    }
}
