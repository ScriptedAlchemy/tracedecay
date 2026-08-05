//! Daemon-wide Remote Brain credential routing and protocol composition.
//!
//! Credential bytes are fingerprinted before lookup and never retained. The
//! only routing entries come from exact registered Remote-node runtimes; no
//! path, request body, or caller-supplied node identity can select a store.

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use thiserror::Error;
use tracedecay_application::remote::auth::{
    OpaqueRemoteCredential, RemoteEnrollmentProtocolAdapterV1,
};
use tracedecay_application::remote::credential_admission::{
    RemoteCredentialAdmissionServiceV1, RemoteCredentialAuthorityRecordV1, RemoteCredentialClassV1,
    RemoteCredentialLookupErrorV1, RemoteCredentialLookupPortV1,
};
use tracedecay_application::remote::protocol::{
    EnrollmentRequestV1, REMOTE_PROTOCOL_VERSION_V1, RemoteEnrollmentProtocolPortV1,
    RemoteProtocolFailureV1, RemoteProtocolPortV1, RemoteProtocolRequestV1,
    RemoteProtocolResponseV1, remote_enrollment_result_contract_v1, remote_protocol_problem,
    remote_replay_result_contract_v1,
};
use tracedecay_application::remote::protocol_owner::RemoteProtocolOwnerV1;
use tracedecay_application::remote::query::{
    RemoteQueryRequestV1, RemoteQueryResultV1, remote_exact_observation_query_result_contract_v1,
};
use tracedecay_application::remote::recovery::{
    BackupOperationStateV1, BackupRequestV1, PromotionCasReceiptV1, PromotionConfirmationV1,
    StagedRestoreConfirmationV1, StagedRestoreProgressV1,
};
use tracedecay_application::remote::replay::{RemoteReplayOutcomeV1, RemoteReplayRequestV1};
use tracedecay_application::{RequestId, ResultContractRef};
use tracedecay_domain::{
    BrainId, BrainNodeId, CurrentRemoteAuthorityStateV1, EnrollmentCredentialRecordV1,
    RemoteAuthorityUnavailableReasonV1, RemoteCredentialFingerprintV1, UserProfileId, UtcMicros,
};
use tracedecay_rusqlite_runtime::remote::{
    RemoteCredentialInventoryErrorV1, RemoteCredentialRegistrationV1, RemoteSqliteStorageV1,
};
use tracedecay_store::{StoreRuntimeBindingV1, StoreShardScopeV1};
use tracedecay_tool_catalog::SchemaId;

use crate::errors::{Result, TraceDecayError};

const MAX_REGISTERED_REMOTE_NODES: usize = 128;
const MAX_REGISTERED_REMOTE_CREDENTIALS: usize = 8_192;

#[derive(Clone)]
struct RegisteredRemoteNodeStoreV1 {
    node_id: BrainNodeId,
    binding: StoreRuntimeBindingV1,
    storage: RemoteSqliteStorageV1,
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
        state.nodes.insert(
            node_id.clone(),
            RegisteredRemoteNodeStoreV1 {
                node_id,
                binding: storage.binding().clone(),
                storage,
            },
        );
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

struct UnavailableRemoteProtocolPortV1<Request, Output> {
    contract: ResultContractRef,
    _request: PhantomData<fn(Request) -> Output>,
}

impl<Request, Output> UnavailableRemoteProtocolPortV1<Request, Output> {
    fn new(contract: ResultContractRef) -> Self {
        Self {
            contract,
            _request: PhantomData,
        }
    }
}

impl<Request, Output> RemoteProtocolPortV1<Request>
    for UnavailableRemoteProtocolPortV1<Request, Output>
{
    type Output = Output;

    fn execute(
        &self,
        request: RemoteProtocolRequestV1<Request>,
        _credential: OpaqueRemoteCredential,
    ) -> RemoteProtocolResponseV1<Self::Output> {
        unavailable_response(request.request_id, request.sent_at, self.contract.clone())
    }
}

pub(crate) fn build_daemon_remote_protocol_router(
    credentials: Arc<DaemonRemoteCredentialAuthorityV1>,
) -> Result<Router> {
    let backup_contract = remote_result_contract("remote.backup.result")?;
    let restore_contract = remote_result_contract("remote.restore.result")?;
    let promotion_contract = remote_result_contract("remote.promotion.result")?;
    let owner = RemoteProtocolOwnerV1::new(
        Arc::new(DaemonRemoteEnrollmentProtocolPortV1 {
            credentials: Arc::clone(&credentials),
        }),
        Arc::new(UnavailableRemoteProtocolPortV1::<
            RemoteReplayRequestV1,
            RemoteReplayOutcomeV1,
        >::new(remote_replay_result_contract_v1())),
        Arc::new(UnavailableRemoteProtocolPortV1::<
            RemoteQueryRequestV1,
            RemoteQueryResultV1,
        >::new(
            remote_exact_observation_query_result_contract_v1()
        )),
        Arc::new(UnavailableRemoteProtocolPortV1::<
            BackupRequestV1,
            BackupOperationStateV1,
        >::new(backup_contract)),
        Arc::new(UnavailableRemoteProtocolPortV1::<
            StagedRestoreConfirmationV1,
            StagedRestoreProgressV1,
        >::new(restore_contract)),
        Arc::new(UnavailableRemoteProtocolPortV1::<
            PromotionConfirmationV1,
            PromotionCasReceiptV1,
        >::new(promotion_contract)),
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
