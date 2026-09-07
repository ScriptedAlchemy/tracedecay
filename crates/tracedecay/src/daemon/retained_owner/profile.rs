//! Profile-scoped retained admission and direct typed memory/LCM execution.

use std::collections::BTreeSet;
use std::sync::Arc;

#[cfg(test)]
use tracedecay_application::retained_surfaces::RetainedSurfaceOperation;
use tracedecay_application::retained_surfaces::{
    RetainedSurfaceRequestV1, RetainedSurfaceResultV1,
};
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOperation, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationResult, CancellationSignal, CapabilityGrantId, CapabilityGrantSnapshot, Deadline,
    DisclosureClass, RequestContext, RequestId, RetainedSurfacePortsV1, RetainedSurfaceServiceV1,
    RetryDirective, SafeDiagnostic, now_micros,
};
use tracedecay_domain::{
    ActorId, BrainId, ManifestDigest, UserProfileId, UtcMicros, canonical_sha256,
};
use tracedecay_session_memory::context::{
    ProfileId, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
};
use tracedecay_session_runtime::session_retrieval::SessionRetrievalServingIdentityV1;
use tracedecay_store::StoreShardIdV1;

use super::lcm::DirectRetainedLcmPortV1;
use super::memory::DirectRetainedMemoryPortV1;
use super::session::DirectProfileRetainedSessionPortV1;
use tracedecay_domain::errors::TraceDecayError;

/// Exact mounted authorities for a profile-retained request.
#[derive(Clone)]
pub(crate) struct ProfileRetainedAuthoritiesV1<'a> {
    pub(crate) runtime_registry:
        Option<&'a tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
    pub(crate) session_identity: ResolvedSessionIdentity,
    pub(crate) configuration_digest: ManifestDigest,
    pub(crate) lcm_authority:
        Option<&'a dyn tracedecay_session_runtime::lcm_authority::MountedLcmAuthorityPort>,
}

const PROFILE_RETAINED_REQUEST_GRANT_REVISION_V1: u64 = 1;
const PROFILE_RETAINED_ACTOR_DOMAIN_V1: &str =
    "tracedecay.daemon.profile-retained.local-profile-actor.v1";
const PROFILE_RETAINED_REQUEST_GRANT_DOMAIN_V1: &str =
    "tracedecay.daemon.profile-retained.request-grant.v1";

pub(crate) fn profile_session_retrieval_serving_identity(
    identity: &dyn tracedecay_application::ProfileIdentityReadPort,
    expected_runtime_shard: &StoreShardIdV1,
    serving_db: &std::path::Path,
) -> Option<SessionRetrievalServingIdentityV1> {
    if &expected_runtime_shard.brain_id != identity.brain_id()
        || &expected_runtime_shard.profile_id != identity.profile_id()
    {
        return None;
    }
    let suffix = identity.profile_id().as_str().strip_prefix("profile.")?;
    if suffix.is_empty() {
        return None;
    }
    SessionRetrievalServingIdentityV1::profile(
        ProfileId::new(identity.profile_id().as_str().to_owned()).ok()?,
        SessionStoreId::new(format!("store.profile.{suffix}")).ok()?,
        SessionRootId::new(format!("root.profile.{suffix}")).ok()?,
        expected_runtime_shard,
        serving_db,
        identity.profile_root(),
    )
}

/// Durable identity authority retained for one authenticated local-profile
/// connection. Grants are deliberately absent: each request is admitted for
/// one operation under its exact controls.
#[derive(Clone)]
pub(crate) struct ProfileRetainedConnectionAuthorityV1 {
    brain_id: BrainId,
    user_profile_id: UserProfileId,
    actor: ActorId,
    session_identity: ResolvedSessionIdentity,
    configuration_digest: ManifestDigest,
}

impl ProfileRetainedConnectionAuthorityV1 {
    pub(crate) fn session_identity(&self) -> &ResolvedSessionIdentity {
        &self.session_identity
    }

    pub(crate) fn configuration_digest(&self) -> &ManifestDigest {
        &self.configuration_digest
    }
}

impl ProfileRetainedConnectionAuthorityV1 {
    #[hotpath::measure(label = "daemon.retained.profile.admit")]
    fn admit_request(
        &self,
        operation: &ApplicationOperation,
        request_id: RequestId,
        deadline: Deadline,
        cancellation: &CancellationSignal,
        observed_at: UtcMicros,
    ) -> Result<RequestContext, TraceDecayError> {
        if deadline.is_elapsed_at(observed_at) {
            return Err(TraceDecayError::Config {
                message: "profile retained request deadline elapsed before admission".to_owned(),
            });
        }
        let scope = self
            .session_identity
            .session_request_scope()
            .map_err(|error| TraceDecayError::Config {
                message: format!("profile retained request scope is invalid: {error}"),
            })?;
        let cancellation_context = cancellation.context();
        let capabilities = BTreeSet::from([operation.capability_id().clone()]);
        let use_cases = BTreeSet::from([operation.use_case_id().clone()]);
        let grant_digest = canonical_sha256(&(
            PROFILE_RETAINED_REQUEST_GRANT_DOMAIN_V1,
            &self.brain_id,
            &self.user_profile_id,
            self.session_identity.store_id().as_str(),
            self.session_identity.root_id().as_str(),
            &scope,
            &self.actor,
            &self.configuration_digest,
            operation.capability_id(),
            operation.use_case_id(),
            &request_id,
            &deadline,
            &cancellation_context,
            observed_at,
        ))
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained request grant digest failed: {error}"),
        })?;
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new(format!(
                "grant.tracedecay-daemon.profile-retained.request.{}",
                grant_digest.as_str().trim_start_matches("sha256:")
            ))
            .map_err(|error| TraceDecayError::Config {
                message: format!("profile retained request grant identity is invalid: {error}"),
            })?,
            PROFILE_RETAINED_REQUEST_GRANT_REVISION_V1,
            grant_digest,
            self.actor.clone(),
            observed_at,
            deadline.expires_at,
            scope.clone(),
            capabilities,
            use_cases,
            DisclosureClass::Sensitive,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained request grant is invalid: {error}"),
        })?;
        RequestContext::new(
            self.actor.clone(),
            scope,
            grant,
            request_id,
            deadline,
            cancellation_context,
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained request context is invalid: {error}"),
        })
    }
}

fn profile_retained_configuration_digest(
    brain_id: &BrainId,
    user_profile_id: &UserProfileId,
    session_identity: &ResolvedSessionIdentity,
) -> Result<ManifestDigest, TraceDecayError> {
    canonical_sha256(&(
        "tracedecay.daemon.profile-retained.configuration.v1",
        brain_id,
        user_profile_id,
        session_identity.store_id().as_str(),
        session_identity.root_id().as_str(),
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("profile retained configuration digest failed: {error}"),
    })
}

pub(crate) fn profile_retained_connection_authority(
    identity: &dyn tracedecay_application::ProfileIdentityReadPort,
    session_identity: &ResolvedSessionIdentity,
) -> Result<ProfileRetainedConnectionAuthorityV1, TraceDecayError> {
    profile_retained_connection_authority_from_persisted_identity(
        identity.brain_id(),
        identity.profile_id(),
        session_identity,
    )
}

fn profile_retained_connection_authority_from_persisted_identity(
    brain_id: &BrainId,
    user_profile_id: &UserProfileId,
    session_identity: &ResolvedSessionIdentity,
) -> Result<ProfileRetainedConnectionAuthorityV1, TraceDecayError> {
    if session_identity.project_id().is_some()
        || session_identity.profile_id().as_str() != user_profile_id.as_str()
    {
        return Err(TraceDecayError::Config {
            message: "profile retained connection requires the exact profile session identity"
                .to_owned(),
        });
    }
    session_identity
        .session_request_scope()
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained connection scope is invalid: {error}"),
        })?;
    let configuration_digest =
        profile_retained_configuration_digest(brain_id, user_profile_id, session_identity)?;
    let actor_digest =
        canonical_sha256(&(PROFILE_RETAINED_ACTOR_DOMAIN_V1, brain_id, user_profile_id)).map_err(
            |error| TraceDecayError::Config {
                message: format!("profile retained actor digest failed: {error}"),
            },
        )?;
    let actor = ActorId::new(format!(
        "actor.tracedecay-daemon.profile-retained.{}",
        actor_digest.as_str().trim_start_matches("sha256:")
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("profile retained policy actor is invalid: {error}"),
    })?;
    Ok(ProfileRetainedConnectionAuthorityV1 {
        brain_id: brain_id.clone(),
        user_profile_id: user_profile_id.clone(),
        actor,
        session_identity: session_identity.clone(),
        configuration_digest,
    })
}

/// Execute one profile-scoped retained request through canonical admission and
/// render the typed application result only after execution has completed.
#[hotpath::measure(label = "daemon.retained.profile.execute", future = true)]
pub(crate) async fn execute_profile_retained_application(
    authorities: ProfileRetainedAuthoritiesV1<'_>,
    connection: &ProfileRetainedConnectionAuthorityV1,
    request: RetainedSurfaceRequestV1,
    request_id: RequestId,
    deadline: Deadline,
    cancellation: CancellationSignal,
) -> Result<ApplicationResult<RetainedSurfaceResultV1>, TraceDecayError> {
    let observed_at = now_micros();
    let operation =
        tracedecay_application::retained_surface_application_operation(request.operation())
            .map_err(|error| TraceDecayError::Config {
                message: error.to_string(),
            })?;
    let scope = authorities
        .session_identity
        .session_request_scope()
        .map_err(|error| TraceDecayError::Config {
            message: error.to_string(),
        })?;
    if deadline.is_elapsed_at(observed_at) {
        return Ok(Err(application_problem_envelope(
            operation.result_contract().clone(),
            request_id,
            ApplicationProblem::timed_out_before_admission(),
        )?));
    }
    if connection.user_profile_id.as_str() != authorities.session_identity.profile_id().as_str()
        || connection.session_identity != authorities.session_identity
    {
        return Ok(Err(application_problem_envelope(
            operation.result_contract().clone(),
            request_id,
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        )?));
    }
    if connection.configuration_digest != authorities.configuration_digest {
        return Ok(Err(application_problem_envelope(
            operation.result_contract().clone(),
            request_id,
            ApplicationProblem::stale(SafeDiagnostic {
                code: "application.retained.profile-configuration-stale".to_owned(),
                message: "The retained profile authority changed after connection admission."
                    .to_owned(),
            }),
        )?));
    }
    let context = connection.admit_request(
        &operation,
        request_id.clone(),
        deadline,
        &cancellation,
        observed_at,
    )?;
    let ports = profile_retained_surface_ports(&authorities)?;
    let service = RetainedSurfaceServiceV1::new(ports);
    Ok(
        match hotpath::future!(
            service.execute(&context, &cancellation, observed_at, &request),
            label = "daemon.retained.profile.serve"
        )
        .await
        {
            Ok(outcome) => Ok(ApplicationEnvelope {
                contract: operation.result_contract().clone(),
                request_id,
                scope,
                outcome,
            }),
            Err(problem) => Err(application_problem_envelope(
                operation.result_contract().clone(),
                request_id,
                problem,
            )?),
        },
    )
}

fn application_problem_envelope(
    contract: tracedecay_application::ResultContractRef,
    request_id: RequestId,
    problem: ApplicationProblem,
) -> Result<ApplicationProblemEnvelope, TraceDecayError> {
    ApplicationProblemEnvelope::new(contract, request_id, problem).map_err(|error| {
        TraceDecayError::Config {
            message: format!("profile retained problem envelope is invalid: {error}"),
        }
    })
}

fn profile_retained_surface_ports<'a>(
    authorities: &'a ProfileRetainedAuthoritiesV1<'a>,
) -> Result<RetainedSurfacePortsV1<'a>, TraceDecayError> {
    authorities
        .session_identity
        .session_request_scope()
        .map_err(|error| TraceDecayError::Config {
            message: format!("profile retained runtime identity is invalid: {error}"),
        })?;
    let mut ports = RetainedSurfacePortsV1::default();
    if let Some(runtime_registry) = authorities.runtime_registry {
        ports = ports.with_memory(Arc::new(DirectRetainedMemoryPortV1::profile(
            runtime_registry,
            authorities.configuration_digest.clone(),
        )));
        ports = ports.with_lcm(Arc::new(DirectRetainedLcmPortV1::profile(
            runtime_registry,
            authorities.session_identity.clone(),
            authorities.lcm_authority,
        )));
        ports = ports.with_session(Arc::new(DirectProfileRetainedSessionPortV1::profile(
            runtime_registry,
            authorities.session_identity.clone(),
        )));
    }
    Ok(ports)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_application::retained_surfaces::{
        MemoryScopeV1, MemoryStatusRequestV1, MessageSearchRequestV1, RetainedOutcomeStatusV1,
        SessionGitRefV1, SessionsForRequestV1,
    };
    use tracedecay_application::{
        ApplicationOutcome, ApplicationProblemKind, CancellationSignal, Deadline, RequestId,
    };
    use tracedecay_daemon_identity::profile_identity;
    use tracedecay_domain::{
        BrainId, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
        CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
        CanonicalObservationRelationsV1, DurableObservationV1, ManifestDigest, ObservationId,
        ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
        ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
        ObservationSourceRangeV1, PayloadReferenceV1, ProjectId, ProjectionGenerationId,
        ProviderId, RetentionClass, SanitizationReceiptId, SanitizationReceiptRefV1,
        SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, SessionId, UserProfileId,
        UtcMicros,
    };
    use tracedecay_session_memory::context::{
        ProfileId, ResolvedSessionIdentity, SessionRootId, SessionStoreId,
    };
    use tracedecay_session_runtime::session_retrieval::DaemonSessionRetrievalRoot;
    use tracedecay_sessions::runtime::{SessionMessageRecord, SessionRecord};
    use tracedecay_store::{
        AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
        SessionTemporalSnapshotRequestV1, build_observation_resolution_authorization_v1,
        build_observation_retrieval_anchor_v2,
    };

    use super::*;

    fn identity(profile: &str) -> ResolvedSessionIdentity {
        ResolvedSessionIdentity::for_profile(
            ProfileId::new(profile).expect("profile id"),
            SessionStoreId::new(format!("store.{profile}")).expect("store id"),
            SessionRootId::new(format!("root.{profile}")).expect("root id"),
        )
    }

    fn profile_retrieval_root(
        profile_identity: &dyn tracedecay_application::ProfileIdentityReadPort,
    ) -> DaemonSessionRetrievalRoot {
        let shard = tracedecay_store::StoreShardIdV1::profile_sessions(
            profile_identity.brain_id().clone(),
            profile_identity.profile_id().clone(),
        );
        let serving_db =
            tracedecay_sessions::runtime::user_sessions_db_path(profile_identity.profile_root());
        let serving =
            profile_session_retrieval_serving_identity(profile_identity, &shard, &serving_db)
                .expect("profile serving identity");
        DaemonSessionRetrievalRoot::profile(serving).expect("profile retrieval root")
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("manifest digest")
    }

    fn session(provider: &str, session_id: &str, project_key: &str) -> SessionRecord {
        SessionRecord {
            provider: provider.to_owned(),
            session_id: session_id.to_owned(),
            project_key: project_key.to_owned(),
            project_path: format!("/fixture/{project_key}"),
            title: Some(format!("{project_key} retained session")),
            started_at: Some(1_715_000_000),
            ended_at: None,
            transcript_path: Some(format!("/fixture/{project_key}/{session_id}.jsonl")),
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        }
    }

    async fn seed_temporal_message(
        database: &tracedecay_global_db::RegisteredGlobalDb,
        scope: ObservationScopeV1,
        session_id: &str,
        message_id: &str,
        text: &str,
    ) {
        let provider = "codex";
        let project_key = match &scope {
            ObservationScopeV1::Profile => "user".to_owned(),
            ObservationScopeV1::Project { project_id } => project_id.as_str().to_owned(),
        };
        let owning_session = session(provider, session_id, &project_key);
        let owning_message = SessionMessageRecord {
            provider: provider.to_owned(),
            message_id: message_id.to_owned(),
            session_id: session_id.to_owned(),
            role: "assistant".to_owned(),
            timestamp: Some(1_715_000_030),
            ordinal: 1,
            text: text.to_owned(),
            kind: Some("message".to_owned()),
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        };
        assert!(
            database.upsert_session(&owning_session).await,
            "seed canonical owning session",
        );

        let provider_id = ProviderId::new(provider).expect("provider");
        let session = SessionId::new(session_id).expect("session");
        let source =
            ObservationSourceIdentityV1::for_provider(provider_id.clone(), session.clone())
                .expect("observation source");
        let range = ObservationSourceRangeV1::new(1, 2).expect("source range");
        let record_id = ObservationId::new(message_id).expect("observation record id");
        let envelope = CanonicalObservationEnvelopeV1::new(
            provider_id,
            "message",
            record_id.clone(),
            CanonicalObservationRelationsV1::new(session.clone())
                .with_message_id(ObservationId::new(message_id).expect("message observation id")),
            vec![CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::Assistant,
                content: json!({ "text": text }),
                model: None,
                timestamp: Some(1_715_000_030),
            }],
            CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range),
        )
        .expect("canonical message envelope");
        let payload = serde_json::to_value(envelope).expect("canonical observation payload");
        let observation = DurableObservationV1::new(
            ObservationIdentityMaterialV1::for_native_record(
                source,
                scope,
                ObservationSourceGenerationV1::new(1).expect("source generation"),
                range,
                ObservationOrderingDomainV1::SnapshotOrder,
                record_id,
            )
            .expect("observation identity"),
            SanitizationReceiptV1::new(
                SanitizationReceiptRefV1::new(
                    SanitizationReceiptId::new(format!("receipt.{message_id}"))
                        .expect("receipt id"),
                    tracedecay_domain::ComponentVersion::new(
                        "sanitizer.profile-retained-fixture.v1",
                    )
                    .expect("sanitizer version"),
                )
                .expect("receipt reference"),
                SanitizerDispositionV1::Accepted,
                SensitivityV1::NonSensitive,
                Some(PayloadReferenceV1::for_payload(&payload).expect("payload reference")),
            )
            .expect("sanitization receipt"),
            RetentionClass::new("retention.profile-retained-fixture").expect("retention class"),
            payload,
        )
        .expect("durable observation");
        let store = database.observation_store();
        let previous_cursor = store
            .get_source_cursor(observation.source(), observation.scope())
            .await
            .expect("source cursor");
        let next_cursor = ObservationSourceCursorV1::for_ordering(
            observation.source().clone(),
            observation.scope().clone(),
            observation.identity().generation(),
            observation.identity().ordering_domain(),
            observation.identity().position().end(),
        )
        .expect("next source cursor");
        let write = ObservationWrite::new(observation.clone(), previous_cursor, next_cursor)
            .expect("observation write");
        let projection_generation =
            ProjectionGenerationId::new(format!("projection.profile-retained.{message_id}.v1"))
                .expect("projection generation");
        let authorization = build_observation_resolution_authorization_v1(
            write.observation(),
            tracedecay_store::OBSERVATION_CAPTURE_AUTHORITY_V1,
        )
        .expect("resolution authorization");
        let anchor = build_observation_retrieval_anchor_v2(
            write.observation(),
            projection_generation.clone(),
            UtcMicros(1),
            authorization,
        )
        .expect("retrieval anchor");
        store
            .persist_observation(
                AnchoredObservationWrite::new(write, anchor, projection_generation)
                    .expect("anchored observation"),
            )
            .await
            .expect("persist canonical observation");
        store
            .project_observation(observation.observation_id())
            .await
            .expect("project canonical observation");
        assert!(
            database
                .upsert_transcript_batch(
                    &owning_session,
                    std::slice::from_ref(&owning_message),
                    &format!("profile-retained-{message_id}.jsonl"),
                    tracedecay_global_db::ParseOffset::default(),
                )
                .await,
            "seed canonical owning transcript",
        );
        database
            .lcm_protect_session_raw_messages(provider, session_id)
            .await
            .expect("protect canonical raw message");
        tracedecay_session_temporal_store::GlobalDbSessionTemporalStore::new(database)
            .materialize_pending_session_refresh_for_test(&session)
            .await
            .expect("materialize canonical temporal occurrence");
        let snapshot = database
            .freeze_session_temporal_snapshot_result(SessionTemporalSnapshotRequestV1::new(session))
            .await
            .expect("activate canonical temporal snapshot");
        assert!(
            snapshot.watermarks().active_generation().value() > 0,
            "canonical temporal generation must be active before retrieval",
        );
    }

    fn request() -> RetainedSurfaceRequestV1 {
        RetainedSurfaceRequestV1::MemoryStatus(MemoryStatusRequestV1 {
            memory_scope: Some(MemoryScopeV1::User),
            project_selector: None,
        })
    }

    fn connection(identity: &ResolvedSessionIdentity) -> ProfileRetainedConnectionAuthorityV1 {
        profile_retained_connection_authority_from_persisted_identity(
            &BrainId::new("brain.profile-retained-test").expect("brain id"),
            &UserProfileId::new(identity.profile_id().as_str()).expect("user profile id"),
            identity,
        )
        .expect("profile connection authority")
    }

    async fn denied_kind(
        authorities: ProfileRetainedAuthoritiesV1<'_>,
        connection: &ProfileRetainedConnectionAuthorityV1,
        request_id: &str,
    ) -> ApplicationProblemKind {
        let cancellation =
            CancellationSignal::active(format!("cancellation.{request_id}")).expect("cancellation");
        execute_profile_retained_application(
            authorities,
            connection,
            request(),
            RequestId::new(request_id).expect("request id"),
            Deadline::new(UtcMicros(now_micros().0.saturating_add(30_000_000))).expect("deadline"),
            cancellation,
        )
        .await
        .expect("transport result")
        .expect_err("request must be denied before port execution")
        .problem
        .kind
    }

    #[tokio::test]
    async fn connection_admission_denies_a_different_profile_scope() {
        let admitted_identity = identity("profile.retained-admitted");
        let requested_identity = identity("profile.retained-other");
        let connection = connection(&admitted_identity);
        let kind = denied_kind(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: None,
                session_identity: requested_identity,
                configuration_digest: connection.configuration_digest().clone(),
                lcm_authority: None,
            },
            &connection,
            "request.profile-retained-scope-denial",
        )
        .await;
        assert_eq!(kind, ApplicationProblemKind::NotFoundOrNotAuthorized);
    }

    #[tokio::test]
    async fn connection_admission_reports_changed_configuration_as_stale() {
        let session_identity = identity("profile.retained-stale");
        let connection = connection(&session_identity);
        let kind = denied_kind(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: None,
                session_identity,
                configuration_digest: digest('c'),
                lcm_authority: None,
            },
            &connection,
            "request.profile-retained-stale-configuration",
        )
        .await;
        assert_eq!(kind, ApplicationProblemKind::Stale);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn profile_retained_message_search_reads_the_profile_session_store() {
        let temporary = tempfile::tempdir().expect("temporary profile parent");
        let profile_root = temporary.path().join("profile");
        let profile_identity =
            profile_identity::load_or_create(&profile_root).expect("durable profile identity");
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            1,
            "profile retained message search",
        )
        .expect("daemon database scope");
        let runtime_registry = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(
            profile_identity.clone(),
        )
        .await
        .expect("profile session runtime registry");
        let profile_database = runtime_registry
            .profile_sessions()
            .await
            .expect("profile session database");
        seed_temporal_message(
            profile_database.as_ref(),
            ObservationScopeV1::Profile,
            "session.profile-retained",
            "message.profile-retained",
            "retained scope beacon from profile authority",
        )
        .await;

        let project_id = ProjectId::new("project.profile-retained-decoy").expect("project id");
        let project_root = temporary.path().join("project-decoy");
        std::fs::create_dir_all(&project_root).expect("project fixture root");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &project_root,
            project_id.as_str(),
        )
        .expect("project enrollment");
        let project_database = runtime_registry
            .project_sessions(project_id.clone(), [project_root])
            .await
            .expect("project session database");
        seed_temporal_message(
            project_database.as_ref(),
            ObservationScopeV1::Project { project_id },
            "session.project-decoy",
            "message.project-decoy",
            "retained scope beacon from project decoy",
        )
        .await;
        let session_root = profile_retrieval_root(&profile_identity);
        let session_identity = session_root.identity().clone();
        let connection =
            profile_retained_connection_authority(&profile_identity, &session_identity)
                .expect("profile retained connection authority");
        let request_id =
            RequestId::new("request.profile-retained-message-search").expect("request identity");
        let cancellation =
            CancellationSignal::active("cancellation.profile-retained-message-search")
                .expect("cancellation");

        let result = execute_profile_retained_application(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: Some(&runtime_registry),
                session_identity,
                configuration_digest: connection.configuration_digest().clone(),
                lcm_authority: None,
            },
            &connection,
            RetainedSurfaceRequestV1::MessageSearch(MessageSearchRequestV1 {
                query: Some("retained scope beacon".to_owned()),
                ..MessageSearchRequestV1::default()
            }),
            request_id,
            Deadline::new(UtcMicros(now_micros().0.saturating_add(30_000_000))).expect("deadline"),
            cancellation,
        )
        .await
        .expect("profile message-search transport")
        .expect("profile message search must be mounted");
        let ApplicationOutcome::Evidence(packet) = result.outcome else {
            panic!("message search must return evidence")
        };
        let Some(RetainedSurfaceResultV1::MessageSearch(result)) = packet.payload else {
            panic!("message search must return its typed payload")
        };

        assert_eq!(result.status, RetainedOutcomeStatusV1::Partial);
        assert_eq!(result.outcome, RetainedOutcomeStatusV1::Partial);
        assert_eq!(result.store_scope.as_deref(), Some("profile"));
        let hits = result.results.expect("profile search results");
        assert_eq!(hits.len(), 1, "project decoy must remain isolated");
        assert_eq!(hits[0].message.message_id, "message.profile-retained");
        assert_eq!(
            hits[0].message.text,
            "retained scope beacon from profile authority"
        );
        assert!(
            hits.iter()
                .all(|hit| hit.message.message_id != "message.project-decoy"),
            "profile search must not read project sessions",
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn profile_retained_message_search_rejects_project_selection() {
        let temporary = tempfile::tempdir().expect("temporary profile parent");
        let profile_root = temporary.path().join("profile");
        let profile_identity =
            profile_identity::load_or_create(&profile_root).expect("durable profile identity");
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            1,
            "profile retained project selection refusal",
        )
        .expect("daemon database scope");
        let runtime_registry = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(
            profile_identity.clone(),
        )
        .await
        .expect("profile session runtime registry");
        let session_root = profile_retrieval_root(&profile_identity);
        let session_identity = session_root.identity().clone();
        let connection =
            profile_retained_connection_authority(&profile_identity, &session_identity)
                .expect("profile retained connection authority");

        let problem = execute_profile_retained_application(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: Some(&runtime_registry),
                session_identity,
                configuration_digest: connection.configuration_digest().clone(),
                lcm_authority: None,
            },
            &connection,
            RetainedSurfaceRequestV1::MessageSearch(MessageSearchRequestV1 {
                query: Some("must not be misrouted".to_owned()),
                project_selector: Some(
                    tracedecay_application::retained_surfaces::RetainedProjectSelectorV1 {
                        project_id: ProjectId::new("project.foreign-selector")
                            .expect("selector project id"),
                    },
                ),
                ..MessageSearchRequestV1::default()
            }),
            RequestId::new("request.profile-retained-project-selection-refusal")
                .expect("request identity"),
            Deadline::new(UtcMicros(now_micros().0.saturating_add(30_000_000))).expect("deadline"),
            CancellationSignal::active("cancellation.profile-retained-project-selection-refusal")
                .expect("cancellation"),
        )
        .await
        .expect("profile selection refusal transport")
        .expect_err("project selection must not reach the profile store");

        assert_eq!(
            problem.problem.kind,
            ApplicationProblemKind::NotFoundOrNotAuthorized
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn profile_retained_sessions_for_remains_unsupported() {
        let temporary = tempfile::tempdir().expect("temporary profile parent");
        let profile_root = temporary.path().join("profile");
        let profile_identity =
            profile_identity::load_or_create(&profile_root).expect("durable profile identity");
        let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &profile_root,
            1,
            "profile retained unsupported sessions-for",
        )
        .expect("daemon database scope");
        let runtime_registry = tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1::open(
            profile_identity.clone(),
        )
        .await
        .expect("profile session runtime registry");
        let session_root = profile_retrieval_root(&profile_identity);
        let session_identity = session_root.identity().clone();
        let connection =
            profile_retained_connection_authority(&profile_identity, &session_identity)
                .expect("profile retained connection authority");

        let problem = execute_profile_retained_application(
            ProfileRetainedAuthoritiesV1 {
                runtime_registry: Some(&runtime_registry),
                session_identity,
                configuration_digest: connection.configuration_digest().clone(),
                lcm_authority: None,
            },
            &connection,
            RetainedSurfaceRequestV1::SessionsFor(SessionsForRequestV1 {
                git_ref: SessionGitRefV1::Branch,
                value: "main".to_owned(),
                since: None,
                until: None,
                relation: None,
                limit: None,
                format: None,
            }),
            RequestId::new("request.profile-retained-unsupported-sessions-for")
                .expect("request identity"),
            Deadline::new(UtcMicros(now_micros().0.saturating_add(30_000_000))).expect("deadline"),
            CancellationSignal::active("cancellation.profile-retained-unsupported-sessions-for")
                .expect("cancellation"),
        )
        .await
        .expect("profile sessions-for transport")
        .expect_err("profile sessions-for must remain unsupported");

        assert_eq!(problem.problem.kind, ApplicationProblemKind::Unsupported);
    }

    #[test]
    fn request_admission_binds_one_operation_and_exact_request_controls() {
        let session_identity = identity("profile.retained-request-bound");
        let connection = connection(&session_identity);
        let operation = tracedecay_application::retained_surface_application_operation(
            RetainedSurfaceOperation::MemoryStatus,
        )
        .expect("retained operation");
        let observed_at = UtcMicros(100);
        let deadline = Deadline::new(UtcMicros(200)).expect("deadline");
        let cancellation = CancellationSignal::active("cancellation.profile-retained-bound")
            .expect("cancellation");
        let request_id = RequestId::new("request.profile-retained-bound").expect("request id");

        let context = connection
            .admit_request(
                &operation,
                request_id.clone(),
                deadline.clone(),
                &cancellation,
                observed_at,
            )
            .expect("request admission");
        let other_request = connection
            .admit_request(
                &operation,
                RequestId::new("request.profile-retained-other").expect("other request id"),
                deadline.clone(),
                &cancellation,
                observed_at,
            )
            .expect("other request admission");
        let other_cancellation =
            CancellationSignal::active("cancellation.profile-retained-bound-other")
                .expect("other cancellation");
        let other_cancellation_context = connection
            .admit_request(
                &operation,
                request_id.clone(),
                deadline.clone(),
                &other_cancellation,
                observed_at,
            )
            .expect("other cancellation admission");
        let other_deadline_context = connection
            .admit_request(
                &operation,
                request_id.clone(),
                Deadline::new(UtcMicros(201)).expect("other deadline"),
                &cancellation,
                observed_at,
            )
            .expect("other deadline admission");

        assert_eq!(context.request_id(), &request_id);
        assert_eq!(context.deadline(), &deadline);
        assert_eq!(context.cancellation(), &cancellation.context());
        assert_eq!(context.grant().issued_at, observed_at);
        assert_eq!(context.grant().expires_at, deadline.expires_at);
        assert_eq!(
            context.grant().allowed_capabilities,
            BTreeSet::from([operation.capability_id().clone()])
        );
        assert_eq!(
            context.grant().allowed_use_cases,
            BTreeSet::from([operation.use_case_id().clone()])
        );
        assert_ne!(context.grant().grant_id, other_request.grant().grant_id);
        assert_ne!(context.grant().digest, other_request.grant().digest);
        assert_ne!(
            context.grant().digest,
            other_cancellation_context.grant().digest
        );
        assert_ne!(
            context.grant().digest,
            other_deadline_context.grant().digest
        );
    }
}
