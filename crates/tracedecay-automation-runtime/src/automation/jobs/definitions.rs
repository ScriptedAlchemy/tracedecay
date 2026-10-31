//! One project's admitted user-job definitions. This is a read-through view,
//! not a second store: every observation reads the current source bytes and
//! only byte-identical documents reuse their immutable admission result.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{AdmittedAutomationJob, admit_scheduled_jobs, decode_jobs, jobs_path};

struct DefinitionSnapshot {
    bytes: Option<Vec<u8>>,
    admitted: std::result::Result<Arc<[AdmittedAutomationJob]>, String>,
}

/// Owned by the registered project handle, never a process-global registry.
/// Retiring the project also retires its last document and admission result.
/// Accepted runs keep their immutable job while later reads see a replacement.
pub struct ScheduledJobDefinitions {
    path: PathBuf,
    snapshot: Mutex<Option<DefinitionSnapshot>>,
}

impl ScheduledJobDefinitions {
    pub fn new(dashboard_root: &Path) -> Self {
        Self {
            path: jobs_path(dashboard_root),
            snapshot: Mutex::new(None),
        }
    }

    #[tracing::instrument(name = "automation.jobs.read_definitions", level = "trace", skip_all)]
    pub async fn read(&self) -> Result<Arc<[AdmittedAutomationJob]>> {
        // Serialize the read and replacement together. A delayed earlier read
        // must not publish an older document over a newer observation.
        let mut snapshot = self.snapshot.lock().await;
        let path = self.path.clone();
        let bytes = match crate::automation::blocking_fs(move || std::fs::read(path)).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to read automation jobs '{}': {error}",
                        self.path.display()
                    ),
                });
            }
        };
        if let Some(previous) = snapshot.as_ref()
            && previous.bytes == bytes
        {
            return previous.jobs();
        }
        let admitted = match bytes.as_deref() {
            Some(bytes) => decode_jobs(&self.path, bytes).and_then(admit_scheduled_jobs),
            None => Ok(Vec::new()),
        };
        let admitted = match admitted {
            Ok(jobs) => Ok(Arc::from(jobs)),
            Err(TraceDecayError::Config { message }) => Err(message),
            // A non-configuration failure must remain retryable as itself.
            Err(error) => return Err(error),
        };
        snapshot
            .insert(DefinitionSnapshot { bytes, admitted })
            .jobs()
    }
}

impl DefinitionSnapshot {
    fn jobs(&self) -> Result<Arc<[AdmittedAutomationJob]>> {
        self.admitted
            .as_ref()
            .map(Arc::clone)
            .map_err(|message| TraceDecayError::Config {
                message: message.clone(),
            })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests;
