//! Daemon-wide Remote Brain credential routing and protocol composition.
//!
//! Credential bytes are fingerprinted before lookup and never retained. The
//! only routing entries come from exact registered Remote-node runtimes; no
//! path, request body, or caller-supplied node identity can select a store.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use axum::Router;
use thiserror::Error;
use tracedecay_application::remote::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentProtocolAdapterV1,
};
use tracedecay_application::remote::capture::RemoteCaptureReceiptV1;
use tracedecay_application::remote::capture_protocol::{
    RemoteCaptureRequestV1, RemoteOfflineCaptureProtocolAdapterV1,
    RemoteOfflineCaptureProtocolServiceV1,
};
use tracedecay_application::remote::credential_admission::{
    RemoteCredentialAdmissionServiceV1, RemoteCredentialAuthorityRecordV1, RemoteCredentialClassV1,
    RemoteCredentialLookupErrorV1, RemoteCredentialLookupPortV1, RemoteSessionBoundProtocolBodyV1,
};
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, REMOTE_PROTOCOL_VERSION_V1, RemoteEnrollmentProtocolPortV1,
    RemoteProtocolExecutionControlV1, RemoteProtocolFailureV1, RemoteProtocolPortV1,
    RemoteProtocolRequestV1, RemoteProtocolResponseV1, remote_capture_result_contract_v1,
    remote_enrollment_result_contract_v1, remote_protocol_problem,
    remote_replay_result_contract_v1,
};
use tracedecay_application::remote::protocol_owner::RemoteProtocolOwnerV1;
use tracedecay_application::remote::query::{
    RemoteExactObservationQueryProtocolAdapterV1, RemoteExactObservationQueryServiceV1,
    RemoteQueryRequestV1, RemoteQueryResultV1,
};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    RemoteRecoveryControlPortV1, RemoteRecoveryInterruptionV1, RemoteRecoveryProtocolOwnerV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{
    RemoteReplayOutcomeV1, RemoteReplayProtocolAdapterV1, RemoteReplayRequestV1,
    RemoteReplayServiceV1,
};
use tracedecay_application::{CancellationSignal, RequestId, ResultContractRef};
use tracedecay_domain::{
    BrainId, BrainNodeId, CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1,
    RemoteAuthorityUnavailableReasonV1, RemoteCredentialFingerprintV1, UserProfileId, UtcMicros,
};
use tracedecay_rusqlite_runtime::remote::{
    CredentialDerivedSpoolKeyringV1, RemoteCredentialInventoryErrorV1,
    RemoteCredentialRegistrationV1, RemoteRecoverySqliteAuthorityV1, RemoteSpoolKeyringV1,
    RemoteSqliteStorageV1,
};
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1};
use tracedecay_tool_catalog::SchemaId;

use crate::daemon::remote_query::DaemonRemoteExactObservationQueryPortV1;
use crate::daemon::remote_replay_transaction::DaemonRemoteReplayTransactionAuthorityV1;
use crate::errors::{Result, TraceDecayError};

const MAX_REGISTERED_REMOTE_NODES: usize = 128;
const MAX_REGISTERED_REMOTE_CREDENTIALS: usize = 8_192;

#[derive(Clone)]
struct RegisteredRemoteNodeStoreV1 {
    node_id: BrainNodeId,
    binding: StoreRuntimeBindingV1,
    storage: RemoteSqliteStorageV1,
    recovery: Option<Arc<RemoteRecoverySqliteAuthorityV1>>,
}

#[derive(Default)]
struct RemoteCredentialRegistryStateV1 {
    nodes: BTreeMap<BrainNodeId, RegisteredRemoteNodeStoreV1>,
    grants: BTreeMap<RemoteCredentialFingerprintV1, BrainNodeId>,
    enrollments: BTreeMap<RemoteCredentialFingerprintV1, BrainNodeId>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum DaemonRemoteCredentialRegistryErrorV1 {
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

pub(crate) struct DaemonRemoteCredentialAuthorityV1 {
    brain_id: BrainId,
    profile_id: UserProfileId,
    maximum_nodes: usize,
    maximum_credentials: usize,
    accepting: AtomicBool,
    state: RwLock<RemoteCredentialRegistryStateV1>,
}

#[derive(Clone)]
pub(crate) struct DaemonRemoteCredentialLookupV1 {
    authority: Arc<DaemonRemoteCredentialAuthorityV1>,
}

impl DaemonRemoteCredentialLookupV1 {
    pub(crate) fn new(authority: Arc<DaemonRemoteCredentialAuthorityV1>) -> Self {
        Self { authority }
    }
}

impl DaemonRemoteCredentialAuthorityV1 {
    pub(crate) fn new(brain_id: BrainId, profile_id: UserProfileId) -> Self {
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
            state: RwLock::new(RemoteCredentialRegistryStateV1::default()),
        }
    }

    pub(crate) fn register_storage(
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

    pub(crate) fn register_recovery_authority(
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

    pub(crate) fn refresh_storage(
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

    fn storage_for_presented(
        &self,
        class: RemoteCredentialClassV1,
        presented: &OpaqueRemoteCredential,
    ) -> std::result::Result<RegisteredRemoteNodeStoreV1, RemoteCredentialLookupErrorV1> {
        let fingerprint = presented
            .credential_fingerprint()
            .map_err(|_| RemoteCredentialLookupErrorV1::NotFound)?;
        self.storage_for_credential(class, &fingerprint)
    }

    pub(crate) fn cancel(&self) {
        self.accepting.store(false, Ordering::Release);
        if let Ok(mut state) = self.state.write() {
            *state = RemoteCredentialRegistryStateV1::default();
        }
    }

    fn ensure_accepting(&self) -> std::result::Result<(), DaemonRemoteCredentialRegistryErrorV1> {
        if self.accepting.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(DaemonRemoteCredentialRegistryErrorV1::Cancelled)
        }
    }
}

impl RemoteCredentialLookupPortV1 for DaemonRemoteCredentialAuthorityV1 {
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

struct DaemonRemoteEnrollmentProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
}

impl RemoteEnrollmentProtocolPortV1 for DaemonRemoteEnrollmentProtocolPortV1 {
    fn execute_enrollment(
        &self,
        request: RemoteProtocolRequestV1<EnrollmentRequestV1>,
        grant_credential: OpaqueRemoteCredential,
        enrollment_credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<EnrollmentCredentialRecordV1> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::EnrollmentGrant, &grant_credential)
        {
            Ok(registered) => registered,
            Err(_) => {
                return unavailable_response(
                    request_id,
                    observed_at,
                    remote_enrollment_result_contract_v1(),
                );
            }
        };
        if self.credentials.ensure_accepting().is_err() {
            return unavailable_response(
                request_id,
                observed_at,
                remote_enrollment_result_contract_v1(),
            );
        }
        let response = RemoteEnrollmentProtocolAdapterV1::new(registered.storage)
            .execute_enrollment(request, grant_credential, enrollment_credential);
        if self
            .credentials
            .refresh_storage(&registered.node_id)
            .is_err()
        {
            return unavailable_response(
                request_id,
                observed_at,
                remote_enrollment_result_contract_v1(),
            );
        }
        response
    }
}

/// Request-scoped spool keyring derived from the presented enrollment
/// credential. Spool frames stay encrypted at rest; the key exists only while
/// the authenticated request executes.
fn presented_spool_keyring(
    credential: &OpaqueRemoteCredential,
    enrollment_revision: u64,
) -> Option<Arc<dyn RemoteSpoolKeyringV1>> {
    let bytes = credential.derive_spool_key_bytes().ok()?;
    let keyring =
        CredentialDerivedSpoolKeyringV1::from_secret_bytes(enrollment_revision, bytes).ok()?;
    Some(Arc::new(keyring))
}

struct DaemonRemoteCaptureProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
}

impl RemoteProtocolPortV1<RemoteCaptureRequestV1> for DaemonRemoteCaptureProtocolPortV1 {
    type Output = RemoteCaptureReceiptV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteCaptureRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<Self::Output> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
        {
            Ok(registered) => registered,
            Err(_) => {
                return unavailable_response(
                    request_id,
                    observed_at,
                    remote_capture_result_contract_v1(),
                );
            }
        };
        let Some(keyring) = presented_spool_keyring(&credential, request.enrollment_revision)
        else {
            return unavailable_response(
                request_id,
                observed_at,
                remote_capture_result_contract_v1(),
            );
        };
        let storage = registered.storage.with_keyring(keyring);
        let shared = Arc::new(storage.clone());
        RemoteOfflineCaptureProtocolAdapterV1::new(RemoteOfflineCaptureProtocolServiceV1::new(
            shared.clone(),
            shared,
            storage,
            tracedecay_application::clock::now_micros,
        ))
        .execute(request, credential)
    }
}

struct DaemonRemoteReplayProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    transaction: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
}

impl RemoteProtocolPortV1<RemoteReplayRequestV1> for DaemonRemoteReplayProtocolPortV1 {
    type Output = RemoteReplayOutcomeV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteReplayRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<Self::Output> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
        {
            Ok(registered) => registered,
            Err(_) => {
                return unavailable_response(
                    request_id,
                    observed_at,
                    remote_replay_result_contract_v1(),
                );
            }
        };
        let Some(keyring) = presented_spool_keyring(&credential, request.enrollment_revision)
        else {
            return unavailable_response(
                request_id,
                observed_at,
                remote_replay_result_contract_v1(),
            );
        };
        let storage = Arc::new(registered.storage.with_keyring(keyring));
        RemoteReplayProtocolAdapterV1::new(RemoteReplayServiceV1::new(
            storage.clone(),
            storage.clone(),
            storage.clone(),
            storage.clone(),
            storage.clone(),
            storage.clone(),
            self.transaction.clone(),
            storage,
        ))
        .execute(request, credential)
    }
}

struct DaemonRemoteQueryProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    targets: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
}

impl RemoteProtocolPortV1<RemoteQueryRequestV1> for DaemonRemoteQueryProtocolPortV1 {
    type Output = RemoteQueryResultV1;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<RemoteQueryRequestV1>,
        credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<Self::Output> {
        let request_id = request.request_id.clone();
        let observed_at = request.sent_at;
        let registered = match self
            .credentials
            .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
        {
            Ok(registered) => registered,
            Err(_) => {
                return unavailable_response(
                    request_id,
                    observed_at,
                    tracedecay_application::remote::query::
                        remote_exact_observation_query_result_contract_v1(),
                );
            }
        };
        let storage = Arc::new(registered.storage);
        RemoteExactObservationQueryProtocolAdapterV1::new(
            RemoteExactObservationQueryServiceV1::new(
                storage.clone(),
                storage.clone(),
                Arc::new(DaemonRemoteExactObservationQueryPortV1::new(
                    storage,
                    Arc::clone(&self.targets),
                )),
            ),
        )
        .execute(request, credential)
    }
}

struct DaemonRemoteRecoveryControlV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    cancellation: CancellationSignal,
    deadline: UtcMicros,
    clock: fn() -> UtcMicros,
    interruption: AtomicU8,
}

impl RemoteRecoveryControlPortV1 for DaemonRemoteRecoveryControlV1 {
    fn interruption(&self, _request_id: &RequestId) -> Option<RemoteRecoveryInterruptionV1> {
        let observed = self.interruption.load(Ordering::Acquire);
        if observed == 1 {
            return Some(RemoteRecoveryInterruptionV1::Cancelled);
        }
        if observed == 2 {
            return Some(RemoteRecoveryInterruptionV1::DeadlineExceeded);
        }
        let next =
            if self.cancellation.is_cancelled() || self.credentials.ensure_accepting().is_err() {
                1
            } else if (self.clock)() >= self.deadline {
                2
            } else {
                return None;
            };
        let preserved =
            match self
                .interruption
                .compare_exchange(0, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => next,
                Err(existing) => existing,
            };
        match preserved {
            1 => Some(RemoteRecoveryInterruptionV1::Cancelled),
            2 => Some(RemoteRecoveryInterruptionV1::DeadlineExceeded),
            _ => None,
        }
    }

    fn effective_deadline(&self, _request_id: &RequestId) -> Option<UtcMicros> {
        Some(self.deadline)
    }
}

struct DaemonRemoteRecoveryProtocolPortV1 {
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    backup_contract: ResultContractRef,
    restore_contract: ResultContractRef,
    promotion_contract: ResultContractRef,
}

macro_rules! impl_daemon_remote_recovery_protocol {
    ($request:ty, $output:ty, $contract:ident) => {
        impl RemoteProtocolPortV1<$request> for DaemonRemoteRecoveryProtocolPortV1 {
            type Output = $output;

            fn execute(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                credential: OpaqueRemoteCredential,
            ) -> RemoteProtocolResponseV1<Self::Output> {
                let contract = self.$contract.clone();
                let Some(deadline) = request.body.execution_expires_at() else {
                    return unavailable_response(request.request_id, request.sent_at, contract);
                };
                let cancellation = match CancellationSignal::active(format!(
                    "cancel.remote.direct.{}",
                    request.request_id.as_str()
                )) {
                    Ok(cancellation) => cancellation,
                    Err(_) => {
                        return unavailable_response(request.request_id, request.sent_at, contract);
                    }
                };
                self.execute_controlled(
                    request,
                    credential,
                    RemoteProtocolExecutionControlV1 {
                        deadline,
                        cancellation,
                    },
                )
            }

            fn execute_controlled(
                &self,
                request: RemoteProtocolRequestV1<$request>,
                credential: OpaqueRemoteCredential,
                control: RemoteProtocolExecutionControlV1,
            ) -> RemoteProtocolResponseV1<Self::Output> {
                let contract = self.$contract.clone();
                let registered = match self
                    .credentials
                    .storage_for_presented(RemoteCredentialClassV1::Enrollment, &credential)
                {
                    Ok(registered) => registered,
                    Err(_) => {
                        return unavailable_response(request.request_id, request.sent_at, contract);
                    }
                };
                let Some(recovery) = registered.recovery else {
                    return unavailable_response(request.request_id, request.sent_at, contract);
                };
                let admission = Arc::new(RemoteCredentialAdmissionServiceV1::new(
                    DaemonRemoteCredentialLookupV1::new(Arc::clone(&self.credentials)),
                ));
                let owner = RemoteRecoveryProtocolOwnerV1::new(
                    admission,
                    recovery,
                    Arc::new(DaemonRemoteRecoveryControlV1 {
                        credentials: Arc::clone(&self.credentials),
                        cancellation: control.cancellation,
                        deadline: control.deadline,
                        clock: tracedecay_application::clock::now_micros,
                        interruption: AtomicU8::new(0),
                    }),
                    tracedecay_application::clock::now_micros,
                );
                owner.execute(request, credential)
            }
        }
    };
}

pub(crate) fn build_daemon_remote_protocol_router(
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
    transaction: Arc<DaemonRemoteReplayTransactionAuthorityV1>,
) -> Result<Router> {
    let recovery = Arc::new(DaemonRemoteRecoveryProtocolPortV1 {
        credentials: Arc::clone(&credentials),
        backup_contract: remote_result_contract("remote.backup.result")?,
        restore_contract: remote_result_contract("remote.restore.result")?,
        promotion_contract: remote_result_contract("remote.promotion.result")?,
    });
    let owner = RemoteProtocolOwnerV1::new(
        Arc::new(DaemonRemoteEnrollmentProtocolPortV1 {
            credentials: Arc::clone(&credentials),
        }),
        Arc::new(DaemonRemoteCaptureProtocolPortV1 {
            credentials: Arc::clone(&credentials),
        }),
        Arc::new(DaemonRemoteReplayProtocolPortV1 {
            credentials: Arc::clone(&credentials),
            transaction: Arc::clone(&transaction),
        }),
        Arc::new(DaemonRemoteQueryProtocolPortV1 {
            credentials: Arc::clone(&credentials),
            targets: transaction,
        }),
        recovery.clone(),
        recovery.clone(),
        recovery,
    );
    let admission = Arc::new(RemoteCredentialAdmissionServiceV1::new(
        DaemonRemoteCredentialLookupV1::new(credentials),
    ));
    Ok(tracedecay_api::remote::remote_protocol_router(
        owner,
        admission,
        tracedecay_application::clock::now_micros,
    ))
}

impl_daemon_remote_recovery_protocol!(BackupRequestV1, BackupOperationStateV1, backup_contract);
impl_daemon_remote_recovery_protocol!(
    StagedRestoreConfirmationV1,
    StagedRestoreProgressV1,
    restore_contract
);
impl_daemon_remote_recovery_protocol!(
    PromotionConfirmationV1,
    PromotionCasReceiptV1,
    promotion_contract
);

fn remote_result_contract(schema_id: &str) -> Result<ResultContractRef> {
    let schema_id = SchemaId::new(schema_id).map_err(|error| TraceDecayError::Config {
        message: format!("remote protocol result schema identity is invalid: {error}"),
    })?;
    ResultContractRef::new(schema_id, 1).map_err(|error| TraceDecayError::Config {
        message: format!("remote protocol result contract is invalid: {error}"),
    })
}

fn unavailable_response<T>(
    request_id: RequestId,
    observed_at: UtcMicros,
    contract: ResultContractRef,
) -> RemoteProtocolResponseV1<T> {
    RemoteProtocolResponseV1 {
        protocol_version: REMOTE_PROTOCOL_VERSION_V1,
        request_id: request_id.clone(),
        authority: CurrentRemoteAuthorityStateV1::Unavailable {
            reason: RemoteAuthorityUnavailableReasonV1::PlacementUnknown,
            observed_at,
        },
        result: Err(remote_protocol_problem(
            contract,
            request_id,
            RemoteProtocolFailureV1::AuthorityUnavailable,
        )),
    }
}

#[cfg(test)]
mod recovery_control_tests {
    use super::*;

    fn before_deadline() -> UtcMicros {
        UtcMicros(10)
    }

    fn at_deadline() -> UtcMicros {
        UtcMicros(20)
    }

    fn credentials() -> Arc<DaemonRemoteCredentialAuthorityV1> {
        Arc::new(DaemonRemoteCredentialAuthorityV1::new(
            BrainId::new("brain.recovery-control").unwrap(),
            UserProfileId::new("profile.recovery-control").unwrap(),
        ))
    }

    #[test]
    fn recovery_control_carries_deadline_and_stable_daemon_cancellation() {
        let request_id = RequestId::new("request.recovery-control").unwrap();
        let deadline_credentials = credentials();
        let deadline = DaemonRemoteRecoveryControlV1 {
            credentials: Arc::clone(&deadline_credentials),
            cancellation: CancellationSignal::active("cancel.recovery.deadline").unwrap(),
            deadline: UtcMicros(20),
            clock: at_deadline,
            interruption: AtomicU8::new(0),
        };
        assert_eq!(
            deadline.interruption(&request_id),
            Some(RemoteRecoveryInterruptionV1::DeadlineExceeded)
        );
        deadline_credentials.cancel();
        assert_eq!(
            deadline.interruption(&request_id),
            Some(RemoteRecoveryInterruptionV1::DeadlineExceeded)
        );

        let cancellation_credentials = credentials();
        let cancellation = DaemonRemoteRecoveryControlV1 {
            credentials: Arc::clone(&cancellation_credentials),
            cancellation: CancellationSignal::active("cancel.recovery.client").unwrap(),
            deadline: UtcMicros(20),
            clock: before_deadline,
            interruption: AtomicU8::new(0),
        };
        cancellation.cancellation.cancel(UtcMicros(11));
        assert_eq!(
            cancellation.interruption(&request_id),
            Some(RemoteRecoveryInterruptionV1::Cancelled)
        );
    }
}
