use std::fs::File;
use std::io;
use std::path::Path;

use tracedecay_application::framed_log::{
    DirectorySyncPolicy, append_durable, file_len as shared_file_len,
    sync_parent_directory as shared_sync_parent_directory,
    tighten_existing_file as shared_tighten_existing_file, truncate_file as shared_truncate_file,
    with_owned_temp_publish as shared_with_owned_temp_publish,
};

use super::types::SpoolError;

const DIRECTORY_POLICY: DirectorySyncPolicy = DirectorySyncPolicy::TolerateUnsupported;

pub(crate) fn io_error(_error: impl ToString) -> SpoolError {
    SpoolError::Io
}

pub(crate) fn file_len(path: &Path) -> Result<u64, SpoolError> {
    shared_file_len(path).map_err(io_error)
}

pub(crate) fn tighten_existing_file(path: &Path) -> Result<(), SpoolError> {
    shared_tighten_existing_file(path).map_err(io_error)
}

pub(crate) fn sync_parent_directory(path: &Path) -> Result<(), SpoolError> {
    shared_sync_parent_directory(path, DIRECTORY_POLICY).map_err(io_error)
}

fn replace_file_atomically(
    temporary: &Path,
    destination: &Path,
    label: &str,
) -> Result<(), SpoolError> {
    tracedecay_runtime_core::db::DatabaseAuthority::replace_file_atomically(temporary, destination, label)
        .map_err(|_| SpoolError::Io)
}

pub(crate) fn truncate_file(path: &Path, len: u64) -> Result<(), SpoolError> {
    shared_truncate_file(path, len, DIRECTORY_POLICY).map_err(io_error)
}

pub(crate) fn with_owned_temp_publish<T>(
    destination: &Path,
    kind: &str,
    label: &str,
    write: impl FnOnce(&mut File) -> Result<T, SpoolError>,
) -> Result<T, SpoolError> {
    shared_with_owned_temp_publish(
        destination,
        kind,
        |temporary, destination| {
            replace_file_atomically(temporary, destination, label)
                .map_err(|_| io::Error::other("host admission spool publish failed"))
        },
        |output| write(output).map_err(|_| io::Error::other("host admission spool write failed")),
        DIRECTORY_POLICY,
    )
    .map_err(io_error)
}

pub(crate) fn append_frame_durable(path: &Path, frame: &[u8]) -> Result<u64, SpoolError> {
    append_durable(path, frame, DIRECTORY_POLICY).map_err(io_error)
}
