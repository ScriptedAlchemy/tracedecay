use std::{error::Error, fmt};

/// Actor-owned points where a retained write capability must still be valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeWriteAuthorityStage {
    BeforeAdmission,
    Dequeued,
    BeforeCommit,
}

/// Dynamic authority retained with one admitted writer request.
///
/// Implementations must validate the same originating capability on every
/// call. Reacquiring authority from a path or mutable label is not valid.
pub trait RuntimeWriteAuthority: Send + Sync {
    fn verify(&self, stage: RuntimeWriteAuthorityStage) -> Result<(), RuntimeWriteAuthorityError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeWriteAuthorityError {
    message: String,
}

impl RuntimeWriteAuthorityError {
    pub fn denied(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RuntimeWriteAuthorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for RuntimeWriteAuthorityError {}
