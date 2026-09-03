//! Shared Remote Brain credential registry mounted by the session registry.
//!
//! Credential bytes are fingerprinted before lookup and never retained. The
//! only routing entries come from exact registered Remote-node runtimes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use thiserror::Error;
use tracedecay_application::remote::auth::OpaqueRemoteCredential;
use tracedecay_application::remote::credential_admission::{
    RemoteCredentialAuthorityRecordV1, RemoteCredentialClassV1, RemoteCredentialLookupErrorV1,
    RemoteCredentialLookupPortV1,
};
use tracedecay_application::remote::status::RemoteOperationalStatusReadV1;
use tracedecay_domain::{
    BrainId, BrainNodeId, CurrentRemoteAuthorityStateV1, RemoteAuthorityUnavailableReasonV1,
    RemoteCredentialFingerprintV1, UserProfileId, UtcMicros,
};
use tracedecay_rusqlite_runtime::remote::{
    RemoteCredentialInventoryErrorV1, RemoteCredentialRegistrationV1,
    RemoteRecoverySqliteAuthorityV1, RemoteSqliteStorageV1,
};
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1};

pub const MAX_REGISTERED_REMOTE_NODES: usize = 128;
const MAX_REGISTERED_REMOTE_CREDENTIALS: usize = 8_192;

#[derive(Clone)]
pub struct RegisteredRemoteNodeStoreV1 {
    pub node_id: BrainNodeId,
    pub binding: StoreRuntimeBindingV1,
    pub storage: RemoteSqliteStorageV1,
    pub recovery: Option<Arc<RemoteRecoverySqliteAuthorityV1>>,
}

#[derive(Default)]
struct RemoteCredentialRegistryStateV1 {
    nodes: BTreeMap<BrainNodeId, RegisteredRemoteNodeStoreV1>,
    grants: BTreeMap<RemoteCredentialFingerprintV1, BrainNodeId>,
    enrollments: BTreeMap<RemoteCredentialFingerprintV1, BrainNodeId>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DaemonRemoteCredentialRegistryErrorV1 {
    #[error("remote credential registry has stopped accepting work")]
    Cancelled,
    #[error("remote credential registry capacity is exhausted")]
    CapacityExceeded,
    #[error("remote credential registry identity conflicts with a registered store")]
    IdentityConflict,
    #[error("remote credential registry store is unavailable")]
    Unavailable,
    #[error("remote credential registry store requires explicit reset")]
    ResetRequired,
}

/// Shared provider of the canonical Remote Brain operational read. The
/// composition root builds exactly one from the mounted session runtime
/// registry; MCP, dashboard, and Doctor surfaces all read through it.
pub type RemoteOperationalStatusProviderV1 =
    Arc<dyn tracedecay_application::remote::status::RemoteOperationalStatusReadPort>;

const REMOTE_LISTENER_STOPPED: u8 = 0;
const REMOTE_LISTENER_SERVING: u8 = 1;
const REMOTE_LISTENER_DEGRADED: u8 = 2;

pub struct DaemonRemoteCredentialAuthorityV1 {
    brain_id: BrainId,
    profile_id: UserProfileId,
    maximum_nodes: usize,
    maximum_credentials: usize,
    accepting: AtomicBool,
    /// Observed Remote Brain TLS listener state, published by the daemon HTTP
    /// application service that owns the listener task.
    listener: AtomicU8,
    state: RwLock<RemoteCredentialRegistryStateV1>,
}

#[derive(Clone)]
pub struct DaemonRemoteCredentialLookupV1 {
    authority: Arc<DaemonRemoteCredentialAuthorityV1>,
}

impl DaemonRemoteCredentialLookupV1 {
    pub fn new(authority: Arc<DaemonRemoteCredentialAuthorityV1>) -> Self {
        Self { authority }
    }
}

impl DaemonRemoteCredentialAuthorityV1 {
    /// Brain identity this authority admits against. Not credential material.
    pub fn brain_id(&self) -> &BrainId {
        &self.brain_id
    }

    /// Profile identity this authority admits against. Not credential material.
    pub fn profile_id(&self) -> &UserProfileId {
        &self.profile_id
    }

    pub fn new(brain_id: BrainId, profile_id: UserProfileId) -> Self {
        Self::with_limits(
            brain_id,
            profile_id,
            MAX_REGISTERED_REMOTE_NODES,
            MAX_REGISTERED_REMOTE_CREDENTIALS,
        )
    }

    fn with_limits(
        brain_id: BrainId,
        profile_id: UserProfileId,
        maximum_nodes: usize,
        maximum_credentials: usize,
    ) -> Self {
        Self {
            brain_id,
            profile_id,
            maximum_nodes,
            maximum_credentials,
            accepting: AtomicBool::new(true),
            listener: AtomicU8::new(REMOTE_LISTENER_STOPPED),
            state: RwLock::new(RemoteCredentialRegistryStateV1::default()),
        }
    }

    /// Records that the Remote Brain TLS listener is bound and serving.
    pub fn publish_listener_serving(&self) {
        self.listener
            .store(REMOTE_LISTENER_SERVING, Ordering::Release);
    }

    /// Records that the Remote Brain TLS listener task failed while it was
    /// expected to serve.
    pub fn publish_listener_degraded(&self) {
        self.listener
            .store(REMOTE_LISTENER_DEGRADED, Ordering::Release);
    }

    /// Records that the Remote Brain TLS listener stopped through an ordinary
    /// shutdown (or was never configured).
    pub fn publish_listener_stopped(&self) {
        self.listener
            .store(REMOTE_LISTENER_STOPPED, Ordering::Release);
    }

    fn listener_read(&self) -> tracedecay_application::RemoteListenerReadV1 {
        match self.listener.load(Ordering::Acquire) {
            REMOTE_LISTENER_SERVING => tracedecay_application::RemoteListenerReadV1::Serving,
            REMOTE_LISTENER_DEGRADED => tracedecay_application::RemoteListenerReadV1::Degraded,
            _ => tracedecay_application::RemoteListenerReadV1::Disabled,
        }
    }

    /// Composes the canonical Remote Brain operational read from the mounted
    /// listener, enrollment, spool, replay, and recovery-journal authorities.
    /// `Unconfigured` is returned only when the optional remote plane has no
    /// listener and no registered node; `Unavailable` only when a mounted
    /// authority genuinely cannot be read.
    #[hotpath::measure(label = "daemon.remote.operational_status")]
    pub fn operational_status(&self) -> RemoteOperationalStatusReadV1 {
        let now = tracedecay_application::clock::now_micros();
        if !self.accepting.load(Ordering::Acquire) {
            return RemoteOperationalStatusReadV1::Unavailable;
        }
        let Ok(state) = self.state.read() else {
            return RemoteOperationalStatusReadV1::Unavailable;
        };
        let listener = self.listener_read();
        if listener == tracedecay_application::RemoteListenerReadV1::Disabled
            && state.nodes.is_empty()
            && state.grants.is_empty()
            && state.enrollments.is_empty()
        {
            return RemoteOperationalStatusReadV1::Unconfigured;
        }
        let enrollment_configured = !state.enrollments.is_empty();
        let mut pending_count = 0_u64;
        let mut quarantined_count = 0_u64;
        let mut has_sequence_gap = false;
        let mut coverage_complete = true;
        let mut authorities = Vec::new();
        let mut current_backup_verified = !state.nodes.is_empty();
        let mut failover_in_progress = false;
        let mut recovery_required = false;
        for node in state.nodes.values() {
            match node.storage.status_at(&self.brain_id, now) {
                Ok(snapshot) => {
                    pending_count = pending_count.saturating_add(snapshot.pending_spool_items);
                    quarantined_count =
                        quarantined_count.saturating_add(snapshot.quarantined_spool_items);
                    has_sequence_gap |= snapshot.has_sequence_gap;
                    authorities.push(snapshot.authority);
                }
                Err(_) => coverage_complete = false,
            }
            match node.storage.recovery_operational_snapshot() {
                Ok(snapshot) => {
                    current_backup_verified &= snapshot.current_backup_verified;
                    failover_in_progress |= snapshot.failover_in_progress;
                    recovery_required |= snapshot.recovery_required;
                }
                Err(_) => {
                    coverage_complete = false;
                    current_backup_verified = false;
                }
            }
        }
        drop(state);
        let authority = aggregate_authority_states(authorities, now);
        let spool = tracedecay_application::remote::status::RemoteSpoolOperationalStatusV1 {
            pending_count,
            quarantined_count,
            has_sequence_gap,
        };
        let replay_coverage_complete = pending_count == 0 && !has_sequence_gap;
        match tracedecay_application::remote::status::RemoteOperationalStatusV1::compose(
            enrollment_configured,
            authority,
            spool,
            replay_coverage_complete,
            current_backup_verified,
            failover_in_progress,
            recovery_required,
            now,
        ) {
            Ok(status) => RemoteOperationalStatusReadV1::Observed {
                listener,
                status,
                coverage: if coverage_complete {
                    tracedecay_application::DoctorCoverageCompletenessV1::Complete
                } else {
                    tracedecay_application::DoctorCoverageCompletenessV1::Partial
                },
            },
            Err(_) => RemoteOperationalStatusReadV1::Unavailable,
        }
    }

    #[hotpath::measure(label = "daemon.remote.register_storage")]
    pub fn register_storage(
        &self,
        node_id: BrainNodeId,
        storage: RemoteSqliteStorageV1,
    ) -> std::result::Result<(), DaemonRemoteCredentialRegistryErrorV1> {
        self.ensure_accepting()?;
        validate_store_binding(
            &self.brain_id,
            &self.profile_id,
            &node_id,
            storage.binding(),
        )?;
        let registrations = storage
            .credential_registrations(self.maximum_credentials)
            .map_err(map_inventory_error)?;
        for registration in &registrations {
            validate_registration(&self.brain_id, &node_id, registration)?;
        }

        let mut state = self
            .state
            .write()
            .map_err(|_| DaemonRemoteCredentialRegistryErrorV1::Unavailable)?;
        self.ensure_accepting()?;
        if !state.nodes.contains_key(&node_id) && state.nodes.len() >= self.maximum_nodes {
            return Err(DaemonRemoteCredentialRegistryErrorV1::CapacityExceeded);
        }

        let mut grants = state.grants.clone();
        let mut enrollments = state.enrollments.clone();
        grants.retain(|_, registered_node| registered_node != &node_id);
        enrollments.retain(|_, registered_node| registered_node != &node_id);
        for registration in &registrations {
            let index = match registration.class {
                RemoteCredentialClassV1::EnrollmentGrant => &mut grants,
                RemoteCredentialClassV1::Enrollment => &mut enrollments,
            };
            if index
                .insert(registration.fingerprint.clone(), node_id.clone())
                .is_some_and(|registered_node| registered_node != node_id)
            {
                return Err(DaemonRemoteCredentialRegistryErrorV1::IdentityConflict);
            }
        }
        if grants.len().saturating_add(enrollments.len()) > self.maximum_credentials {
            return Err(DaemonRemoteCredentialRegistryErrorV1::CapacityExceeded);
        }

        state.grants = grants;
        state.enrollments = enrollments;
        let recovery = state
            .nodes
            .get(&node_id)
            .and_then(|registered| registered.recovery.clone());
        state.nodes.insert(
            node_id.clone(),
            RegisteredRemoteNodeStoreV1 {
                node_id,
                binding: storage.binding().clone(),
                storage,
                recovery,
            },
        );
        Ok(())
    }

    pub fn register_recovery_authority(
        &self,
        node_id: &BrainNodeId,
        recovery: Arc<RemoteRecoverySqliteAuthorityV1>,
    ) -> std::result::Result<(), DaemonRemoteCredentialRegistryErrorV1> {
        self.ensure_accepting()?;
        let mut state = self
            .state
            .write()
            .map_err(|_| DaemonRemoteCredentialRegistryErrorV1::Unavailable)?;
        let registered = state
            .nodes
            .get_mut(node_id)
            .ok_or(DaemonRemoteCredentialRegistryErrorV1::Unavailable)?;
        registered.recovery = Some(recovery);
        Ok(())
    }

    pub fn refresh_storage(
        &self,
        node_id: &BrainNodeId,
    ) -> std::result::Result<(), DaemonRemoteCredentialRegistryErrorV1> {
        let storage = {
            let state = self
                .state
                .read()
                .map_err(|_| DaemonRemoteCredentialRegistryErrorV1::Unavailable)?;
            state
                .nodes
                .get(node_id)
                .map(|registered| registered.storage.clone())
                .ok_or(DaemonRemoteCredentialRegistryErrorV1::Unavailable)?
        };
        self.register_storage(node_id.clone(), storage)
    }

    fn storage_for_credential(
        &self,
        class: RemoteCredentialClassV1,
        fingerprint: &RemoteCredentialFingerprintV1,
    ) -> std::result::Result<RegisteredRemoteNodeStoreV1, RemoteCredentialLookupErrorV1> {
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RemoteCredentialLookupErrorV1::Unavailable);
        }
        let registered = {
            let state = self
                .state
                .read()
                .map_err(|_| RemoteCredentialLookupErrorV1::Unavailable)?;
            let index = match class {
                RemoteCredentialClassV1::EnrollmentGrant => &state.grants,
                RemoteCredentialClassV1::Enrollment => &state.enrollments,
            };
            let node_id = index
                .get(fingerprint)
                .ok_or(RemoteCredentialLookupErrorV1::NotFound)?;
            state
                .nodes
                .get(node_id)
                .cloned()
                .ok_or(RemoteCredentialLookupErrorV1::Corruption)?
        };
        validate_store_binding(
            &self.brain_id,
            &self.profile_id,
            &registered.node_id,
            &registered.binding,
        )
        .map_err(|_| RemoteCredentialLookupErrorV1::Corruption)?;
        if registered.storage.binding() != &registered.binding {
            return Err(RemoteCredentialLookupErrorV1::Corruption);
        }
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RemoteCredentialLookupErrorV1::Unavailable);
        }
        Ok(registered)
    }

    pub fn storage_for_presented(
        &self,
        class: RemoteCredentialClassV1,
        presented: &OpaqueRemoteCredential,
    ) -> std::result::Result<RegisteredRemoteNodeStoreV1, RemoteCredentialLookupErrorV1> {
        let fingerprint = presented
            .credential_fingerprint()
            .map_err(|_| RemoteCredentialLookupErrorV1::NotFound)?;
        self.storage_for_credential(class, &fingerprint)
    }

    pub fn cancel(&self) {
        self.accepting.store(false, Ordering::Release);
        if let Ok(mut state) = self.state.write() {
            *state = RemoteCredentialRegistryStateV1::default();
        }
    }

    pub fn ensure_accepting(
        &self,
    ) -> std::result::Result<(), DaemonRemoteCredentialRegistryErrorV1> {
        if self.accepting.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(DaemonRemoteCredentialRegistryErrorV1::Cancelled)
        }
    }
}

impl RemoteCredentialLookupPortV1 for DaemonRemoteCredentialAuthorityV1 {
    #[hotpath::measure(label = "daemon.remote.credential_lookup")]
    fn credential_by_fingerprint(
        &self,
        class: RemoteCredentialClassV1,
        fingerprint: &RemoteCredentialFingerprintV1,
    ) -> std::result::Result<RemoteCredentialAuthorityRecordV1, RemoteCredentialLookupErrorV1> {
        let registered = self.storage_for_credential(class, fingerprint)?;
        let record = registered
            .storage
            .credential_by_fingerprint(class, fingerprint)?;
        validate_record_route(
            &self.brain_id,
            &registered.node_id,
            class,
            fingerprint,
            &record,
        )?;
        if !self.accepting.load(Ordering::Acquire) {
            return Err(RemoteCredentialLookupErrorV1::Unavailable);
        }
        Ok(record)
    }
}

impl RemoteCredentialLookupPortV1 for DaemonRemoteCredentialLookupV1 {
    fn credential_by_fingerprint(
        &self,
        class: RemoteCredentialClassV1,
        fingerprint: &RemoteCredentialFingerprintV1,
    ) -> std::result::Result<RemoteCredentialAuthorityRecordV1, RemoteCredentialLookupErrorV1> {
        self.authority.credential_by_fingerprint(class, fingerprint)
    }
}

/// Aggregates per-node authority states into one truthful read. All nodes
/// available yields the highest-epoch authority; a mix yields `Partial` with
/// the union of missing evidence; nothing available yields `Unavailable`.
fn aggregate_authority_states(
    states: Vec<CurrentRemoteAuthorityStateV1>,
    observed_at: UtcMicros,
) -> CurrentRemoteAuthorityStateV1 {
    let mut best_available: Option<tracedecay_domain::CurrentRemoteAuthorityV1> = None;
    let mut missing = BTreeSet::new();
    let mut partial_fence = None;
    let mut degraded = false;
    let total = states.len();
    for state in states {
        match state {
            CurrentRemoteAuthorityStateV1::Available(current) => {
                let replace = best_available.as_ref().is_none_or(|best| {
                    current.fence.authority_epoch.0 > best.fence.authority_epoch.0
                });
                if replace {
                    best_available = Some(current);
                }
            }
            CurrentRemoteAuthorityStateV1::Partial {
                known_fence,
                missing: node_missing,
                ..
            } => {
                degraded = true;
                if partial_fence.is_none() {
                    partial_fence = known_fence;
                }
                missing.extend(node_missing);
            }
            CurrentRemoteAuthorityStateV1::Unavailable { reason, .. } => {
                degraded = true;
                missing.insert(reason);
            }
        }
    }
    if total == 0 {
        return CurrentRemoteAuthorityStateV1::Unavailable {
            reason: RemoteAuthorityUnavailableReasonV1::RegistryUnavailable,
            observed_at,
        };
    }
    match (best_available, degraded) {
        (Some(current), false) => CurrentRemoteAuthorityStateV1::Available(current),
        (Some(current), true) => CurrentRemoteAuthorityStateV1::Partial {
            known_fence: Some(current.fence),
            missing,
            observed_at,
        },
        (None, _) => match missing.len() {
            1 => CurrentRemoteAuthorityStateV1::Unavailable {
                reason: missing
                    .into_iter()
                    .next()
                    .unwrap_or(RemoteAuthorityUnavailableReasonV1::RegistryUnavailable),
                observed_at,
            },
            _ => CurrentRemoteAuthorityStateV1::Partial {
                known_fence: partial_fence,
                missing,
                observed_at,
            },
        },
    }
}

fn validate_store_binding(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    node_id: &BrainNodeId,
    binding: &StoreRuntimeBindingV1,
) -> std::result::Result<(), DaemonRemoteCredentialRegistryErrorV1> {
    if &binding.shard_id.brain_id != brain_id
        || &binding.shard_id.profile_id != profile_id
        || !matches!(
            &binding.shard_id.scope,
            StoreShardScopeV1::RemoteNode {
                node_id: registered_node
            } if registered_node == node_id
        )
    {
        return Err(DaemonRemoteCredentialRegistryErrorV1::IdentityConflict);
    }
    Ok(())
}

fn validate_registration(
    brain_id: &BrainId,
    node_id: &BrainNodeId,
    registration: &RemoteCredentialRegistrationV1,
) -> std::result::Result<(), DaemonRemoteCredentialRegistryErrorV1> {
    if &registration.brain_id != brain_id || &registration.node_id != node_id {
        return Err(DaemonRemoteCredentialRegistryErrorV1::IdentityConflict);
    }
    Ok(())
}

fn validate_record_route(
    brain_id: &BrainId,
    node_id: &BrainNodeId,
    class: RemoteCredentialClassV1,
    fingerprint: &RemoteCredentialFingerprintV1,
    record: &RemoteCredentialAuthorityRecordV1,
) -> std::result::Result<(), RemoteCredentialLookupErrorV1> {
    let matches = match (class, record) {
        (
            RemoteCredentialClassV1::EnrollmentGrant,
            RemoteCredentialAuthorityRecordV1::Grant { grant, .. },
        ) => {
            &grant.brain_id == brain_id
                && &grant.node_id == node_id
                && &grant.fingerprint == fingerprint
        }
        (
            RemoteCredentialClassV1::Enrollment,
            RemoteCredentialAuthorityRecordV1::Enrollment { enrollment, .. },
        ) => {
            &enrollment.brain_id == brain_id
                && &enrollment.node_id == node_id
                && &enrollment.fingerprint == fingerprint
        }
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(RemoteCredentialLookupErrorV1::Corruption)
    }
}

fn map_inventory_error(
    error: RemoteCredentialInventoryErrorV1,
) -> DaemonRemoteCredentialRegistryErrorV1 {
    match error {
        RemoteCredentialInventoryErrorV1::InvalidLimit
        | RemoteCredentialInventoryErrorV1::CapacityExceeded => {
            DaemonRemoteCredentialRegistryErrorV1::CapacityExceeded
        }
        RemoteCredentialInventoryErrorV1::Lookup(RemoteCredentialLookupErrorV1::ResetRequired) => {
            DaemonRemoteCredentialRegistryErrorV1::ResetRequired
        }
        RemoteCredentialInventoryErrorV1::Lookup(
            RemoteCredentialLookupErrorV1::Corruption | RemoteCredentialLookupErrorV1::NotFound,
        ) => DaemonRemoteCredentialRegistryErrorV1::IdentityConflict,
        RemoteCredentialInventoryErrorV1::Lookup(RemoteCredentialLookupErrorV1::Unavailable) => {
            DaemonRemoteCredentialRegistryErrorV1::Unavailable
        }
    }
}
