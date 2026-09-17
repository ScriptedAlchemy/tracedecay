use std::fmt;

use serde::{Deserialize, Serialize};

use crate::lsp_wire::{LspSessionAccess, LspSessionCredential, LspSessionId};

use super::DaemonInvocationProblem;

/// Credential-bearing access data exchanged only between a bridge and the
/// authenticated daemon. Its debug representation never prints the secret.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DaemonLspSessionAccess {
    pub session_id: String,
    credential: String,
}

impl fmt::Debug for DaemonLspSessionAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonLspSessionAccess")
            .field("session_id", &self.session_id)
            .field("credential", &"[redacted]")
            .finish()
    }
}

impl DaemonLspSessionAccess {
    pub fn from_access(access: &LspSessionAccess) -> Self {
        Self {
            session_id: access.session_id().as_str().to_owned(),
            credential: hex::encode(access.credential().as_bytes()),
        }
    }

    pub fn into_access(self) -> Result<LspSessionAccess, DaemonInvocationProblem> {
        let session_id = LspSessionId::new(self.session_id)
            .map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let credential = hex::decode(self.credential)
            .ok()
            .and_then(|credential| LspSessionCredential::new(credential).ok())
            .ok_or(DaemonInvocationProblem::InvalidRequest)?;
        Ok(LspSessionAccess::new(session_id, credential))
    }
}
