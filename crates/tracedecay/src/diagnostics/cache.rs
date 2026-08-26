use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Notify;

use super::fingerprint::DiagnosticsFingerprint;
use super::{Diagnostic, Scope, run_all};
use crate::errors::Result;

#[derive(Debug, Default)]
pub struct DiagnosticsCache {
    entries: tokio::sync::Mutex<HashMap<DiagnosticsCacheKey, Arc<DiagnosticsCacheSlot>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DiagnosticsCacheKey {
    project_root: PathBuf,
    scope: Scope,
}

#[derive(Debug, Clone)]
struct CachedDiagnostics {
    fingerprint: DiagnosticsFingerprint,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Default)]
struct DiagnosticsCacheSlot {
    state: tokio::sync::Mutex<DiagnosticsCacheSlotState>,
    notify: Notify,
}

#[derive(Debug, Default)]
enum DiagnosticsCacheSlotState {
    #[default]
    Idle,
    Ready(CachedDiagnostics),
    Running {
        fingerprint: DiagnosticsFingerprint,
    },
}

struct RunningSlotGuard {
    slot: Arc<DiagnosticsCacheSlot>,
    fingerprint: DiagnosticsFingerprint,
    completed: bool,
}

impl RunningSlotGuard {
    fn new(slot: Arc<DiagnosticsCacheSlot>, fingerprint: DiagnosticsFingerprint) -> Self {
        Self {
            slot,
            fingerprint,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for RunningSlotGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let slot = Arc::clone(&self.slot);
        let fingerprint = self.fingerprint.clone();
        tokio::spawn(async move {
            let mut state = slot.state.lock().await;
            if matches!(
                &*state,
                DiagnosticsCacheSlotState::Running {
                    fingerprint: running
                } if running == &fingerprint
            ) {
                *state = DiagnosticsCacheSlotState::Idle;
            }
            drop(state);
            slot.notify.notify_waiters();
        });
    }
}

impl DiagnosticsCache {
    pub async fn run(&self, project_root: &Path, scope: &Scope) -> Result<Vec<Diagnostic>> {
        self.run_with(project_root, scope, || run_all(project_root, scope))
            .await
    }

    pub(crate) async fn run_with<F, Fut>(
        &self,
        project_root: &Path,
        scope: &Scope,
        run: F,
    ) -> Result<Vec<Diagnostic>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<Diagnostic>>>,
    {
        let key = DiagnosticsCacheKey {
            project_root: project_root
                .canonicalize()
                .unwrap_or_else(|_| project_root.to_path_buf()),
            scope: scope.clone(),
        };
        let fingerprint = DiagnosticsFingerprint::capture(project_root, scope).await?;
        let slot = {
            let mut entries = self.entries.lock().await;
            Arc::clone(
                entries
                    .entry(key)
                    .or_insert_with(|| Arc::new(DiagnosticsCacheSlot::default())),
            )
        };

        loop {
            let mut state = slot.state.lock().await;
            match &*state {
                DiagnosticsCacheSlotState::Ready(cached) if cached.fingerprint == fingerprint => {
                    return Ok(cached.diagnostics.clone());
                }
                DiagnosticsCacheSlotState::Running {
                    fingerprint: running,
                } if running == &fingerprint => {
                    let notified = slot.notify.notified();
                    drop(state);
                    notified.await;
                }
                _ => {
                    *state = DiagnosticsCacheSlotState::Running {
                        fingerprint: fingerprint.clone(),
                    };
                    break;
                }
            }
        }

        let mut guard = RunningSlotGuard::new(Arc::clone(&slot), fingerprint.clone());
        let result = run().await;
        let mut state = slot.state.lock().await;
        let still_current = matches!(
            &*state,
            DiagnosticsCacheSlotState::Running {
                fingerprint: running
            } if running == &fingerprint
        );
        if still_current {
            match &result {
                Ok(diagnostics) => {
                    *state = DiagnosticsCacheSlotState::Ready(CachedDiagnostics {
                        fingerprint,
                        diagnostics: diagnostics.clone(),
                    });
                }
                Err(_) => {
                    *state = DiagnosticsCacheSlotState::Idle;
                }
            }
        }
        drop(state);
        guard.complete();
        slot.notify.notify_waiters();
        result
    }
}
