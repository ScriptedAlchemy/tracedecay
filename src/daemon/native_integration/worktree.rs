//! Daemon-native worktree reads and holder admission.
//!
//! Physical roots come only from the persisted authorized scope set. Native
//! Git is used as evidence for those roots; it never discovers a replacement
//! project, repository, or worktree identity.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tracedecay_application::git::{
    AuthorizedScopeSetPort, NativeWorktreePort, NativeWorktreeTargetV1,
    WorktreeCleanupConfirmRequestV1, WorktreeCleanupConfirmationV1,
    WorktreeCleanupInspectRequestV1, WorktreeCleanupReconcileRequestV1,
    WorktreeCleanupReconciliationV1, WorktreeCleanupRemovalV1, WorktreeCleanupRemoveRequestV1,
    WorktreeConfirmationOutcomeV1, WorktreeContractError, WorktreeCoverageV1,
    WorktreeInspectionOutcomeV1, WorktreeInspectionV1, WorktreeInventoryEntryV1,
    WorktreeInventoryOutcomeV1, WorktreeInventoryRequestV1, WorktreeInventorySnapshotV1,
    WorktreeKindV1, WorktreeObservationV1, WorktreePresenceV1, worktree_confirmation_digest,
    worktree_inspection_digest,
};
use tracedecay_application::{AuthorizedRoot, AuthorizedScopeSet, CancellationSignal};
use tracedecay_domain::git::{GitHeadStateV1, GitOperationStateV1};
use tracedecay_domain::{
    ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId, WorktreeInventoryEpoch,
    WorktreeInventorySnapshotId, canonical_sha256,
};
use tracedecay_runtime_core::git_repository::GitRepositoryAuthority;
use tracedecay_rusqlite_runtime::repository::AuthorizedScopeSetSqliteStorage;

use super::store::SharedDaemonNativeIntegrationStore;

#[derive(Clone)]
pub(crate) struct DaemonAuthorizedScopeSetReader {
    storage: AuthorizedScopeSetSqliteStorage,
}

impl DaemonAuthorizedScopeSetReader {
    pub(crate) const fn new(storage: AuthorizedScopeSetSqliteStorage) -> Self {
        Self { storage }
    }
}

impl AuthorizedScopeSetPort for DaemonAuthorizedScopeSetReader {
    fn read(
        &self,
        scope_set_id: &tracedecay_domain::ScopeSetId,
    ) -> Result<Option<AuthorizedScopeSet>, WorktreeContractError> {
        self.storage
            .read(scope_set_id)
            .map_err(|_| WorktreeContractError::ScopeSetUnavailable)
    }
}

#[derive(Default)]
struct HolderFenceStateV1 {
    roots: BTreeMap<PathBuf, HolderFenceRootV1>,
}

struct HolderFenceRootV1 {
    gate: Arc<RwLock<()>>,
    recovery_requested: bool,
    recovery_guard: Option<OwnedRwLockWriteGuard<()>>,
}

impl Default for HolderFenceRootV1 {
    fn default() -> Self {
        Self {
            gate: Arc::new(RwLock::new(())),
            recovery_requested: false,
            recovery_guard: None,
        }
    }
}

/// Process-wide exact-root admission authority shared by Work, LSP, and
/// native cleanup. A durable cleanup journal keeps its write fence across
/// requests and daemon-owned runtime mounts until reconciliation is terminal.
#[derive(Clone, Default)]
pub(crate) struct WorktreeHolderAdmissionFenceV1 {
    state: Arc<Mutex<HolderFenceStateV1>>,
}

impl WorktreeHolderAdmissionFenceV1 {
    pub(crate) async fn admit_holders(
        &self,
        roots: impl IntoIterator<Item = PathBuf>,
    ) -> Option<Vec<OwnedRwLockReadGuard<()>>> {
        let mut roots = roots.into_iter().collect::<Vec<_>>();
        if roots.iter().any(|root| !root.is_absolute()) {
            return None;
        }
        roots.sort();
        roots.dedup();

        let gates = {
            let mut state = self.state.lock().ok()?;
            let mut gates = Vec::with_capacity(roots.len());
            for root in roots {
                let entry = state.roots.entry(root).or_default();
                if entry.recovery_requested {
                    return None;
                }
                gates.push(Arc::clone(&entry.gate));
            }
            gates
        };

        let mut admissions = Vec::with_capacity(gates.len());
        for gate in gates {
            admissions.push(gate.try_read_owned().ok()?);
        }
        Some(admissions)
    }

    pub(crate) async fn mark_recovery_required(&self, roots: impl IntoIterator<Item = PathBuf>) {
        let mut roots = roots.into_iter().collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        for root in roots {
            if !root.is_absolute() {
                continue;
            }
            let gate = {
                let Ok(mut state) = self.state.lock() else {
                    return;
                };
                let entry = state.roots.entry(root.clone()).or_default();
                if entry.recovery_requested {
                    continue;
                }
                entry.recovery_requested = true;
                Arc::clone(&entry.gate)
            };
            let guard = gate.write_owned().await;
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            if let Some(entry) = state.roots.get_mut(&root)
                && entry.recovery_requested
                && entry.recovery_guard.is_none()
            {
                entry.recovery_guard = Some(guard);
            }
        }
    }

    pub(super) fn holder_observation(&self, root: &Path) -> WorktreeObservationV1 {
        let Ok(mut state) = self.state.lock() else {
            return WorktreeObservationV1::Unknown;
        };
        let entry = state.roots.entry(root.to_path_buf()).or_default();
        if entry.recovery_requested {
            return WorktreeObservationV1::Yes;
        }
        match Arc::clone(&entry.gate).try_write_owned() {
            Ok(guard) => {
                drop(guard);
                WorktreeObservationV1::No
            }
            Err(_) => WorktreeObservationV1::Yes,
        }
    }

    pub(super) fn try_cleanup(&self, root: &Path) -> Option<WorktreeCleanupAdmissionV1> {
        let gate = {
            let mut state = self.state.lock().ok()?;
            let entry = state.roots.entry(root.to_path_buf()).or_default();
            if entry.recovery_requested {
                return None;
            }
            entry.recovery_requested = true;
            Arc::clone(&entry.gate)
        };
        match gate.try_write_owned() {
            Ok(guard) => Some(WorktreeCleanupAdmissionV1 {
                fence: self.clone(),
                root: root.to_path_buf(),
                guard: Some(guard),
            }),
            Err(_) => {
                self.clear_request(root);
                None
            }
        }
    }

    pub(super) fn take_recovery(&self, root: &Path) -> Option<WorktreeCleanupAdmissionV1> {
        let mut state = self.state.lock().ok()?;
        let entry = state.roots.entry(root.to_path_buf()).or_default();
        if let Some(guard) = entry.recovery_guard.take() {
            return Some(WorktreeCleanupAdmissionV1 {
                fence: self.clone(),
                root: root.to_path_buf(),
                guard: Some(guard),
            });
        }
        drop(state);
        self.try_cleanup(root)
    }

    fn clear_request(&self, root: &Path) {
        if let Ok(mut state) = self.state.lock()
            && let Some(entry) = state.roots.get_mut(root)
        {
            entry.recovery_requested = false;
            entry.recovery_guard = None;
        }
    }

    fn retain_recovery(&self, root: PathBuf, guard: OwnedRwLockWriteGuard<()>) {
        if let Ok(mut state) = self.state.lock() {
            let entry = state.roots.entry(root).or_default();
            entry.recovery_requested = true;
            entry.recovery_guard = Some(guard);
        }
    }
}

pub(super) struct WorktreeCleanupAdmissionV1 {
    fence: WorktreeHolderAdmissionFenceV1,
    root: PathBuf,
    guard: Option<OwnedRwLockWriteGuard<()>>,
}

impl WorktreeCleanupAdmissionV1 {
    pub(super) fn retain_recovery(mut self) {
        if let Some(guard) = self.guard.take() {
            self.fence.retain_recovery(self.root.clone(), guard);
        }
    }
}

impl Drop for WorktreeCleanupAdmissionV1 {
    fn drop(&mut self) {
        if self.guard.take().is_some() {
            self.fence.clear_request(&self.root);
        }
    }
}

pub(crate) fn daemon_worktree_holder_admission_fence() -> WorktreeHolderAdmissionFenceV1 {
    static FENCE: OnceLock<WorktreeHolderAdmissionFenceV1> = OnceLock::new();
    FENCE.get_or_init(Default::default).clone()
}

pub(crate) struct DaemonNativeWorktreeAuthority {
    pub(super) project_id: ProjectId,
    pub(super) repository_id: RepositoryId,
    pub(super) repository_root: PathBuf,
    pub(super) repository_common_dir: PathBuf,
    pub(super) store: SharedDaemonNativeIntegrationStore,
    pub(super) holder_fence: WorktreeHolderAdmissionFenceV1,
    inventory_epoch: AtomicU64,
}

impl DaemonNativeWorktreeAuthority {
    pub(crate) fn open(
        project_id: ProjectId,
        repository_id: RepositoryId,
        repository_root: &Path,
        store: SharedDaemonNativeIntegrationStore,
    ) -> Result<Self, WorktreeContractError> {
        project_id.validate()?;
        repository_id.validate()?;
        let repository = GitRepositoryAuthority::discover(repository_root)
            .map_err(|_| WorktreeContractError::AuthorityUnavailable)?;
        let repository_root = repository
            .worktree_root()
            .ok_or(WorktreeContractError::AuthorityUnavailable)?
            .to_path_buf();
        Ok(Self {
            project_id,
            repository_id,
            repository_root,
            repository_common_dir: repository.common_dir().to_path_buf(),
            store,
            holder_fence: daemon_worktree_holder_admission_fence(),
            inventory_epoch: AtomicU64::new(1),
        })
    }

    fn scope_roots<'a>(
        &self,
        target: &NativeWorktreeTargetV1,
        scope_set: &'a AuthorizedScopeSet,
    ) -> Result<Vec<&'a AuthorizedRoot>, WorktreeContractError> {
        if target.project_id() != &self.project_id || target.repository_id() != &self.repository_id
        {
            return Err(WorktreeContractError::Denied);
        }
        let roots = scope_set
            .roots()
            .iter()
            .filter(|root| {
                root.scope().project_id == self.project_id
                    && root.scope().repository_id == self.repository_id
                    && target
                        .worktree_id()
                        .is_none_or(|worktree_id| root.scope().worktree_id == *worktree_id)
            })
            .collect::<Vec<_>>();
        if roots.is_empty() {
            return Err(WorktreeContractError::Denied);
        }
        Ok(roots)
    }

    pub(super) fn target_root<'a>(
        &self,
        target: &NativeWorktreeTargetV1,
        scope_set: &'a AuthorizedScopeSet,
    ) -> Result<(&'a AuthorizedRoot, PathBuf), WorktreeContractError> {
        let roots = self.scope_roots(target, scope_set)?;
        if roots.len() != 1 {
            return Err(WorktreeContractError::Denied);
        }
        let authorized = roots[0];
        let locator = authorized
            .locator()
            .ok_or(WorktreeContractError::AuthorityUnavailable)?;
        Ok((authorized, locator.canonical_root.clone()))
    }

    pub(super) fn observe_target(
        &self,
        target: &NativeWorktreeTargetV1,
        scope_set: &AuthorizedScopeSet,
        observed_at: UtcMicros,
        ignore_cleanup_fence: bool,
    ) -> Result<WorktreeInspectionV1, WorktreeContractError> {
        let (authorized, root) = self.target_root(target, scope_set)?;
        self.observe_authorized_root(authorized, root, observed_at, ignore_cleanup_fence)
    }

    fn observe_authorized_root(
        &self,
        authorized: &AuthorizedRoot,
        root: PathBuf,
        observed_at: UtcMicros,
        ignore_cleanup_fence: bool,
    ) -> Result<WorktreeInspectionV1, WorktreeContractError> {
        let worktree_id = authorized.scope().worktree_id.clone();
        let target = NativeWorktreeTargetV1::Worktree {
            project_id: self.project_id.clone(),
            repository_id: self.repository_id.clone(),
            worktree_id: worktree_id.clone(),
        };
        if !root.exists() {
            return self.seal_inspection(WorktreeInspectionV1 {
                target,
                presence: WorktreePresenceV1::Stale,
                kind: None,
                worktree_id,
                reference: authorized.scope().reference.clone(),
                head: None,
                clean: WorktreeObservationV1::Unknown,
                locked: WorktreeObservationV1::Unknown,
                holder: WorktreeObservationV1::Unknown,
                unique_data: WorktreeObservationV1::Unknown,
                operation: None,
                observed_at,
                inspection_digest: zero_digest()?,
            });
        }
        let canonical_root = root
            .canonicalize()
            .map_err(|_| WorktreeContractError::AuthorityUnavailable)?;
        if canonical_root != root {
            return self.foreign_inspection(target, authorized, worktree_id, observed_at);
        }
        let repository = match GitRepositoryAuthority::discover(&root) {
            Ok(repository) => repository,
            Err(_) => {
                return self.foreign_inspection(target, authorized, worktree_id, observed_at);
            }
        };
        if repository.common_dir() != self.repository_common_dir
            || repository.worktree_root() != Some(root.as_path())
        {
            return self.foreign_inspection(target, authorized, worktree_id, observed_at);
        }
        let kind = if repository.git_dir() == repository.common_dir() {
            WorktreeKindV1::Main
        } else {
            WorktreeKindV1::Linked
        };
        let status = repository
            .status()
            .map_err(|_| WorktreeContractError::AuthorityUnavailable)?;
        let actual_reference = head_reference(&status.head)?;
        let presence = if authorized.scope().reference.is_some()
            && authorized.scope().reference != actual_reference
        {
            WorktreePresenceV1::Foreign
        } else {
            WorktreePresenceV1::Present
        };
        let clean = if status.entries.is_empty() {
            // The application cleanup predicate treats `No` as absence of a
            // dirty-worktree blocker, alongside the other risk observations.
            WorktreeObservationV1::No
        } else {
            WorktreeObservationV1::Yes
        };
        let unique_data = match (&status.head, clean) {
            (_, WorktreeObservationV1::Yes) | (GitHeadStateV1::Detached { .. }, _) => {
                WorktreeObservationV1::Yes
            }
            _ => WorktreeObservationV1::No,
        };
        let operation = (status.operation != GitOperationStateV1::None).then_some(status.operation);
        let holder = if ignore_cleanup_fence {
            WorktreeObservationV1::No
        } else {
            self.holder_fence.holder_observation(&root)
        };
        self.seal_inspection(WorktreeInspectionV1 {
            target,
            presence,
            kind: Some(kind),
            worktree_id,
            reference: actual_reference,
            head: status.head.commit().cloned(),
            clean,
            locked: if repository.git_dir().join("locked").is_file() {
                WorktreeObservationV1::Yes
            } else {
                WorktreeObservationV1::No
            },
            holder,
            unique_data,
            operation,
            observed_at,
            inspection_digest: zero_digest()?,
        })
    }

    fn foreign_inspection(
        &self,
        target: NativeWorktreeTargetV1,
        authorized: &AuthorizedRoot,
        worktree_id: WorktreeId,
        observed_at: UtcMicros,
    ) -> Result<WorktreeInspectionV1, WorktreeContractError> {
        self.seal_inspection(WorktreeInspectionV1 {
            target,
            presence: WorktreePresenceV1::Foreign,
            kind: None,
            worktree_id,
            reference: authorized.scope().reference.clone(),
            head: None,
            clean: WorktreeObservationV1::Unknown,
            locked: WorktreeObservationV1::Unknown,
            holder: WorktreeObservationV1::Unknown,
            unique_data: WorktreeObservationV1::Unknown,
            operation: None,
            observed_at,
            inspection_digest: zero_digest()?,
        })
    }

    fn seal_inspection(
        &self,
        mut inspection: WorktreeInspectionV1,
    ) -> Result<WorktreeInspectionV1, WorktreeContractError> {
        inspection.inspection_digest = worktree_inspection_digest(&inspection)?;
        Ok(inspection)
    }

    fn inventory_entry(
        &self,
        authorized: &AuthorizedRoot,
        observed_at: UtcMicros,
    ) -> Result<WorktreeInventoryEntryV1, WorktreeContractError> {
        let Some(locator) = authorized.locator() else {
            let target = NativeWorktreeTargetV1::Worktree {
                project_id: self.project_id.clone(),
                repository_id: self.repository_id.clone(),
                worktree_id: authorized.scope().worktree_id.clone(),
            };
            return seal_entry(WorktreeInventoryEntryV1 {
                target,
                presence: WorktreePresenceV1::Unavailable,
                kind: None,
                worktree_id: Some(authorized.scope().worktree_id.clone()),
                reference: authorized.scope().reference.clone(),
                head: None,
                clean: WorktreeObservationV1::Unknown,
                locked: WorktreeObservationV1::Unknown,
                holder: WorktreeObservationV1::Unknown,
                unique_data: WorktreeObservationV1::Unknown,
                operation: None,
                observed_at,
                evidence_digest: zero_digest()?,
            });
        };
        let inspection = self.observe_authorized_root(
            authorized,
            locator.canonical_root.clone(),
            observed_at,
            false,
        )?;
        seal_entry(WorktreeInventoryEntryV1 {
            target: inspection.target,
            presence: inspection.presence,
            kind: inspection.kind,
            worktree_id: Some(inspection.worktree_id),
            reference: inspection.reference,
            head: inspection.head,
            clean: inspection.clean,
            locked: inspection.locked,
            holder: inspection.holder,
            unique_data: inspection.unique_data,
            operation: inspection.operation,
            observed_at,
            evidence_digest: zero_digest()?,
        })
    }
}

impl NativeWorktreePort for DaemonNativeWorktreeAuthority {
    fn inventory(
        &self,
        request: &WorktreeInventoryRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeInventoryOutcomeV1, WorktreeContractError> {
        if cancellation.is_cancelled() {
            return Ok(WorktreeInventoryOutcomeV1::Unavailable);
        }
        let roots = self.scope_roots(&request.target, scope_set)?;
        let observed_at = crate::daemon_client::invocation_now_micros();
        let mut entries = Vec::with_capacity(roots.len());
        for root in roots {
            if cancellation.is_cancelled() {
                return Ok(WorktreeInventoryOutcomeV1::Unavailable);
            }
            entries.push(self.inventory_entry(root, observed_at)?);
        }
        entries.sort_by(|left, right| {
            left.worktree_id
                .as_ref()
                .map(WorktreeId::as_str)
                .cmp(&right.worktree_id.as_ref().map(WorktreeId::as_str))
        });
        let coverage = if entries
            .iter()
            .all(|entry| entry.presence == WorktreePresenceV1::Present)
        {
            WorktreeCoverageV1::Complete
        } else {
            WorktreeCoverageV1::Partial
        };
        let epoch = self
            .inventory_epoch
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| WorktreeContractError::AuthorityUnavailable)?;
        let epoch = WorktreeInventoryEpoch::new(epoch)?;
        let identity_digest = canonical_sha256(&(
            "tracedecay.native-worktree-inventory-identity.v1",
            request.scope_set_id.clone(),
            request.scope_set_revision,
            request.scope_set_digest.clone(),
            epoch,
            &entries,
        ))?;
        let snapshot_id = WorktreeInventorySnapshotId::new(format!(
            "worktree-inventory.{}",
            identity_digest.as_str()
        ))?;
        let digest = canonical_sha256(&(
            "tracedecay.native-worktree-inventory.v1",
            &request.scope_set_id,
            request.scope_set_revision,
            &request.scope_set_digest,
            &snapshot_id,
            epoch,
            &entries,
            coverage,
            observed_at,
        ))?;
        Ok(WorktreeInventoryOutcomeV1::Snapshot(Box::new(
            WorktreeInventorySnapshotV1 {
                scope_set_id: request.scope_set_id.clone(),
                scope_set_revision: request.scope_set_revision,
                scope_set_digest: request.scope_set_digest.clone(),
                snapshot_id,
                epoch,
                entries,
                coverage,
                observed_at,
                digest,
            },
        )))
    }

    fn inspect(
        &self,
        request: &WorktreeCleanupInspectRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeInspectionOutcomeV1, WorktreeContractError> {
        if cancellation.is_cancelled() {
            return Ok(WorktreeInspectionOutcomeV1::Unavailable);
        }
        let inspection = self.observe_target(
            &request.target,
            scope_set,
            crate::daemon_client::invocation_now_micros(),
            false,
        )?;
        Ok(match inspection.presence {
            WorktreePresenceV1::Foreign => WorktreeInspectionOutcomeV1::Foreign,
            WorktreePresenceV1::Unavailable => WorktreeInspectionOutcomeV1::Unavailable,
            _ => WorktreeInspectionOutcomeV1::Inspection(Box::new(inspection)),
        })
    }

    fn confirm(
        &self,
        request: &WorktreeCleanupConfirmRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeConfirmationOutcomeV1, WorktreeContractError> {
        if cancellation.is_cancelled() {
            return Ok(WorktreeConfirmationOutcomeV1::Unavailable);
        }
        let confirmed_at = crate::daemon_client::invocation_now_micros();
        let inspection = self.observe_target(&request.target, scope_set, confirmed_at, false)?;
        if inspection.presence == WorktreePresenceV1::Foreign {
            return Ok(WorktreeConfirmationOutcomeV1::Denied);
        }
        if inspection.presence != WorktreePresenceV1::Present
            || inspection.inspection_digest != request.inspection_digest
        {
            return Ok(WorktreeConfirmationOutcomeV1::Stale);
        }
        if !inspection.removal_eligible() {
            return Ok(WorktreeConfirmationOutcomeV1::Denied);
        }
        let confirmation_digest = worktree_confirmation_digest(
            &request.target,
            &request.inspection_digest,
            confirmed_at,
        )?;
        Ok(WorktreeConfirmationOutcomeV1::Confirmed(Box::new(
            WorktreeCleanupConfirmationV1 {
                target: request.target.clone(),
                inspection_digest: request.inspection_digest.clone(),
                confirmation_digest,
                confirmed_at,
            },
        )))
    }

    fn remove(
        &self,
        request: &WorktreeCleanupRemoveRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupRemovalV1, WorktreeContractError> {
        self.remove_cleanup(request, scope_set, cancellation)
    }

    fn reconcile(
        &self,
        request: &WorktreeCleanupReconcileRequestV1,
        scope_set: &AuthorizedScopeSet,
        cancellation: &CancellationSignal,
    ) -> Result<WorktreeCleanupReconciliationV1, WorktreeContractError> {
        self.reconcile_cleanup(request, scope_set, cancellation)
    }
}

fn head_reference(head: &GitHeadStateV1) -> Result<Option<RefId>, WorktreeContractError> {
    let Some(branch) = head.branch() else {
        return Ok(None);
    };
    let reference = if branch.starts_with("refs/") {
        branch.to_owned()
    } else {
        format!("refs/heads/{branch}")
    };
    RefId::new(reference)
        .map(Some)
        .map_err(WorktreeContractError::Domain)
}

fn seal_entry(
    mut entry: WorktreeInventoryEntryV1,
) -> Result<WorktreeInventoryEntryV1, WorktreeContractError> {
    entry.evidence_digest = canonical_sha256(&(
        "tracedecay.native-worktree-inventory-entry.v1",
        &entry.target,
        entry.presence,
        entry.kind,
        &entry.worktree_id,
        &entry.reference,
        &entry.head,
        entry.clean,
        entry.locked,
        entry.holder,
        entry.unique_data,
        &entry.operation,
    ))?;
    Ok(entry)
}

pub(super) fn zero_digest() -> Result<ManifestDigest, WorktreeContractError> {
    ManifestDigest::new(format!("sha256:{}", "0".repeat(64))).map_err(WorktreeContractError::Domain)
}
