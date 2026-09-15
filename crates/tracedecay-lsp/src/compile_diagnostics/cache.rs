use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Notify;

use super::fingerprint::DiagnosticsFingerprint;
use super::{Diagnostic, Scope, run_all};
use crate::analyzer::activity::{
    canonicalize_project_root, scoped_project_file_from_canonical_root,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

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
    revision: DiagnosticsCacheRevision,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiagnosticsCacheRevision {
    WorkspaceChange(u64),
    Recovery(DiagnosticsFingerprint),
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
        revision: DiagnosticsCacheRevision,
    },
}

struct RunningSlotGuard {
    slot: Arc<DiagnosticsCacheSlot>,
    revision: DiagnosticsCacheRevision,
    completed: bool,
}

impl RunningSlotGuard {
    fn new(slot: Arc<DiagnosticsCacheSlot>, revision: DiagnosticsCacheRevision) -> Self {
        Self {
            slot,
            revision,
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
        let revision = self.revision.clone();
        tokio::spawn(async move {
            let mut state = slot.state.lock().await;
            if matches!(
                &*state,
                DiagnosticsCacheSlotState::Running {
                    revision: running
                } if running == &revision
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

    /// Run diagnostics under the code index's worktree-change authority.
    ///
    /// A generation is exactly as fresh as the index used by search: hook
    /// hints and Git metadata changes are observed immediately, while other
    /// out-of-band edits are observed by the 30-second stat-signature ladder.
    /// Until that ladder runs, diagnostics intentionally reuse the preceding
    /// generation rather than deriving a second workspace-change authority.
    pub async fn run_for_generation(
        &self,
        project_root: &Path,
        scope: &Scope,
        generation: u64,
    ) -> Result<Vec<Diagnostic>> {
        self.run_with_generation(project_root, scope, generation, || {
            run_all(project_root, scope)
        })
        .await
    }

    #[hotpath::measure(label = "compile_diagnostics.cache.run", future = true)]
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
        let project_root =
            canonicalize_project_root(project_root).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to resolve diagnostics project root '{}': {error}",
                    project_root.display()
                ),
            })?;
        if let Scope::File { path } = scope
            && scoped_project_file_from_canonical_root(&project_root, path).is_none()
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "diagnostics file scope '{path}' is outside project root '{}'",
                    project_root.display()
                ),
            });
        }
        let fingerprint = DiagnosticsFingerprint::capture(&project_root, scope).await?;
        self.run_with_revision(
            project_root,
            scope,
            DiagnosticsCacheRevision::Recovery(fingerprint),
            run,
        )
        .await
    }

    pub(crate) async fn run_with_generation<F, Fut>(
        &self,
        project_root: &Path,
        scope: &Scope,
        generation: u64,
        run: F,
    ) -> Result<Vec<Diagnostic>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<Diagnostic>>>,
    {
        let project_root =
            canonicalize_project_root(project_root).map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to resolve diagnostics project root '{}': {error}",
                    project_root.display()
                ),
            })?;
        if let Scope::File { path } = scope
            && scoped_project_file_from_canonical_root(&project_root, path).is_none()
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "diagnostics file scope '{path}' is outside project root '{}'",
                    project_root.display()
                ),
            });
        }
        self.run_with_revision(
            project_root,
            scope,
            DiagnosticsCacheRevision::WorkspaceChange(generation),
            run,
        )
        .await
    }

    async fn run_with_revision<F, Fut>(
        &self,
        project_root: PathBuf,
        scope: &Scope,
        revision: DiagnosticsCacheRevision,
        run: F,
    ) -> Result<Vec<Diagnostic>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Vec<Diagnostic>>>,
    {
        let key = DiagnosticsCacheKey {
            project_root,
            scope: scope.clone(),
        };
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
                DiagnosticsCacheSlotState::Ready(cached) if cached.revision == revision => {
                    return Ok(cached.diagnostics.clone());
                }
                DiagnosticsCacheSlotState::Running { revision: running }
                    if running == &revision =>
                {
                    let notified = slot.notify.notified();
                    drop(state);
                    notified.await;
                }
                _ => {
                    *state = DiagnosticsCacheSlotState::Running {
                        revision: revision.clone(),
                    };
                    break;
                }
            }
        }

        let mut guard = RunningSlotGuard::new(Arc::clone(&slot), revision.clone());
        let result = run().await;
        let mut state = slot.state.lock().await;
        let still_current = matches!(
            &*state,
            DiagnosticsCacheSlotState::Running {
                revision: running
            } if running == &revision
        );
        if still_current {
            match &result {
                Ok(diagnostics) => {
                    *state = DiagnosticsCacheSlotState::Ready(CachedDiagnostics {
                        revision,
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
