use std::fmt;

/// Typed failure states for complete profile backup and restore.
///
/// Restore callers can distinguish corrupt or tampered backup material,
/// destination conflicts, partially published restores owned by another
/// attempt, denied engine access, and plain infrastructure failures without
/// parsing messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileBackupError {
    /// The request itself is malformed: bad identity, unsafe path, or a
    /// missing prerequisite in the source profile.
    InvalidRequest { message: String },
    /// An authority refused the operation (lifecycle lease, database lock,
    /// busy engine).
    Denied { message: String },
    /// The destination already exists or is owned by other state.
    Conflict { message: String },
    /// Backup material is missing, tampered with, inconsistent with its
    /// manifest, or fails engine verification.
    CorruptBackup { message: String },
    /// The backup was produced under an unsupported (non-final) schema and
    /// is not a restore input.
    ResetRequired { message: String },
    /// A partially published restore or staging directory exists and is
    /// owned by a different restore attempt; recovery must not clear it.
    PartialRestoreConflict { message: String },
    /// Filesystem or engine infrastructure failed.
    Unavailable { message: String },
}

impl ProfileBackupError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidRequest {
            message: message.into(),
        }
    }

    pub(crate) fn denied(message: impl Into<String>) -> Self {
        Self::Denied {
            message: message.into(),
        }
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
        }
    }

    pub(crate) fn corrupt(message: impl Into<String>) -> Self {
        Self::CorruptBackup {
            message: message.into(),
        }
    }

    pub(crate) fn reset_required(message: impl Into<String>) -> Self {
        Self::ResetRequired {
            message: message.into(),
        }
    }

    pub(crate) fn partial_restore_conflict(message: impl Into<String>) -> Self {
        Self::PartialRestoreConflict {
            message: message.into(),
        }
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::Unavailable {
            message: message.into(),
        }
    }
}

impl fmt::Display for ProfileBackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest { message } => {
                write!(formatter, "invalid profile backup request: {message}")
            }
            Self::Denied { message } => write!(formatter, "profile backup denied: {message}"),
            Self::Conflict { message } => {
                write!(formatter, "profile backup conflict: {message}")
            }
            Self::CorruptBackup { message } => {
                write!(formatter, "profile backup material is corrupt: {message}")
            }
            Self::ResetRequired { message } => {
                write!(formatter, "profile backup reset required: {message}")
            }
            Self::PartialRestoreConflict { message } => {
                write!(formatter, "partial profile restore conflict: {message}")
            }
            Self::Unavailable { message } => {
                write!(formatter, "profile backup unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for ProfileBackupError {}
