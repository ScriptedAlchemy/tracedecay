use serde_json::Value;
use tempfile::TempDir;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEvidenceV1, CanonicalObservationFactV1,
    CanonicalObservationRelationsV1, ComponentVersion, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectionGenerationId, ProviderId, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1,
    SanitizerDispositionV1, SensitivityV1, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationStore, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use super::*;
use crate::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_usecases::host_admission::HostAdmissionScope;

fn id<T: TryFrom<String>>(value: &str) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn complete_native_observation() -> CanonicalObservationEnvelopeV1 {
    let relations = CanonicalObservationRelationsV1::new(id::<SessionId>("session.native.codex"))
        .with_thread_id(id::<ObservationId>("thread.native.codex"))
        .with_turn_id(id::<ObservationId>("turn.native.codex"))
        .with_agent_id(id::<ObservationId>("agent.native.codex"))
        .with_message_id(id::<ObservationId>("message.native.codex"));
    CanonicalObservationEnvelopeV1::new(
        id::<ProviderId>("codex"),
        "event_msg",
        id::<ObservationId>("record.native.codex"),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Assistant,
            content: Value::String("saved".to_owned()),
            model: None,
            timestamp: None,
        }],
        CanonicalObservationEvidenceV1::new(
            ObservationOrderingDomainV1::FileBytes,
            ObservationSourceRangeV1::new(0, 1).unwrap(),
        ),
    )
    .unwrap()
}

fn durable_native_observation(project_id: &ProjectId) -> AnchoredObservationWrite {
    let envelope = complete_native_observation();
    let payload = serde_json::to_value(&envelope).unwrap();
    let range = ObservationSourceRangeV1::new(0, 1).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        envelope.provider().clone(),
        envelope.relations().session_id().clone(),
    )
    .unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let record_id = ObservationId::new("record.native.codex").unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.native.codex").unwrap(),
            ComponentVersion::new("sanitizer.native-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            scope.clone(),
            generation,
            range,
            ObservationOrderingDomainV1::FileBytes,
            record_id,
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.native-test").unwrap(),
        payload,
    )
    .unwrap();
    let cursor = ObservationSourceCursorV1::for_ordering(
        source,
        scope,
        generation,
        ObservationOrderingDomainV1::FileBytes,
        1,
    )
    .unwrap();
    let write = ObservationWrite::new(observation, None, cursor).unwrap();
    let projection_generation = ProjectionGenerationId::new("projection.native-test.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "native-test").unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

/// Writes durable evidence straight into the shard, bypassing the
/// runtime scope gate.
///
/// This is only ever correct for staging state the store contract
/// forbids — a row scoped to another project — so that the reader-side
/// guards can be exercised against it. Everything else must go through
/// [`ObservationStore::persist_observation`].
async fn stage_foreign_scoped_observation(
    sessions: &RegisteredGlobalDb,
    write: &AnchoredObservationWrite,
) {
    let observation = write.observation();
    let receipt = observation.receipt();
    let writer = sessions.writer_connection().unwrap();
    writer
        .execute(
            "INSERT INTO sanitization_receipts
                (receipt_id, sanitizer_version, payload_digest, receipt_json)
             VALUES (?1, ?2, ?3, ?4)",
            crate::db::engine::params![
                receipt.receipt().receipt_id().as_str(),
                receipt.receipt().sanitizer_version().as_str(),
                observation.payload_reference().digest().as_str(),
                serde_json::to_string(receipt).unwrap()
            ],
        )
        .await
        .unwrap();
    writer
        .execute(
            "INSERT INTO observations
                (observation_id, payload_digest, receipt_id, observation_json,
                 committed_cursor_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            crate::db::engine::params![
                observation.observation_id().as_str(),
                observation.payload_reference().digest().as_str(),
                receipt.receipt().receipt_id().as_str(),
                serde_json::to_string(observation).unwrap(),
                serde_json::to_string(write.next_cursor()).unwrap()
            ],
        )
        .await
        .unwrap();
}

fn authority_is_registered(hook_project_id: [u8; 16], hook_worktree_id: [u8; 16]) -> bool {
    registered_context_scout_lifecycle_authorities()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(&(hook_project_id, hook_worktree_id))
}

/// Every test below registers under hook ids unique to that test: the
/// authority registry is process-global, so overlapping keys would make
/// parallel tests observe each other's entries.
async fn project_runtime(
    temporary: &TempDir,
    project_id: &ProjectId,
) -> HostAdmissionTestRuntimeV1 {
    HostAdmissionTestRuntimeV1::project(
        temporary.path().join("profile"),
        temporary.path().join("project"),
        project_id.clone(),
    )
    .await
    .unwrap()
}

#[test]
fn complete_canonical_native_lifecycle_is_admitted_without_hash_identity() {
    let lifecycle = lifecycle_from_canonical(
        &id("profile.native"),
        &id("project.native"),
        &id("worktree.native"),
        &id("session.native.codex"),
        &complete_native_observation(),
    )
    .unwrap();
    assert_eq!(lifecycle.provider_id.as_str(), "codex");
    assert_eq!(lifecycle.session_id.as_str(), "session.native.codex");
    assert_eq!(lifecycle.thread_id.as_str(), "thread.native.codex");
    assert_eq!(lifecycle.turn_id.as_str(), "turn.native.codex");
    assert_eq!(lifecycle.agent_id.as_str(), "agent.native.codex");
    assert_eq!(
        lifecycle.logical_message_id.as_str(),
        "message.native.codex"
    );
}

#[test]
fn wrong_session_fails_closed() {
    assert!(
        lifecycle_from_canonical(
            &id("profile.native"),
            &id("project.native"),
            &id("worktree.native"),
            &id("session.other"),
            &complete_native_observation(),
        )
        .is_none()
    );
}

#[test]
fn checked_in_native_fixture_without_complete_lifecycle_fails_closed() {
    let fixture = include_str!(
        "../../../tests/fixtures/provider_normalization/codex/agent_message.expected_envelope.json"
    )
    .replace("$STABLE_RECORD_ID", "message.native.fixture");
    let observation = serde_json::from_str::<CanonicalObservationEnvelopeV1>(&fixture).unwrap();
    assert!(
        lifecycle_from_canonical(
            &id("profile.native"),
            &id("project.native"),
            &id("worktree.native"),
            observation.relations().session_id(),
            &observation,
        )
        .is_none()
    );
}

#[tokio::test]
async fn protected_replay_locator_re_resolves_the_authoritative_native_lifecycle() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.replay");
    let worktree_id = id::<WorktreeId>("worktree.native.replay");
    let runtime = project_runtime(&temporary, &project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    sessions
        .observation_store()
        .persist_observation(durable_native_observation(&project_id))
        .await
        .unwrap();
    let hook_project_id = [211; 16];
    let hook_worktree_id = [212; 16];
    assert_eq!(
        register_context_scout_lifecycle_authority(
            hook_project_id,
            hook_worktree_id,
            project_id,
            worktree_id,
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
    );

    let session_id = lookup_registered_context_scout_native_session(
        hook_project_id,
        hook_worktree_id,
        crate::hooks::protected_native_session_id("session.native.codex"),
    )
    .await
    .unwrap();

    assert_eq!(session_id.as_str(), "session.native.codex");
}

/// The cap is a fail-closed bound, not a tuning knob: it must stay small
/// enough to keep one hook lookup bounded, and `cap + 1` must remain a
/// valid `i64` SQL `LIMIT` (the `try_from` in the lookup returns `None`
/// otherwise, silently failing every lookup closed).
#[test]
fn session_observation_cap_stays_bounded_and_expressible_as_a_sql_limit() {
    assert_eq!(MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1, 64);
    assert!(i64::try_from(MAX_CONTEXT_SCOUT_SESSION_OBSERVATIONS_V1 + 1).is_ok());
}

#[tokio::test]
async fn zero_hook_identifiers_are_rejected_before_registration() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.zero-hook");
    let worktree_id = id::<WorktreeId>("worktree.native.zero-hook");
    let runtime = project_runtime(&temporary, &project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();

    // The two zero locators are distinguishable: neither is a conflict, and
    // each names the field that was never admissible.
    assert_eq!(
        register_context_scout_lifecycle_authority(
            [0; 16],
            [71; 16],
            project_id.clone(),
            worktree_id.clone(),
            &sessions,
        ),
        AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::ZeroHookProjectId)
    );
    assert_eq!(
        register_context_scout_lifecycle_authority(
            [71; 16],
            [0; 16],
            project_id,
            worktree_id,
            &sessions,
        ),
        AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::ZeroHookWorktreeId)
    );
    assert!(!authority_is_registered([0; 16], [71; 16]));
    assert!(!authority_is_registered([71; 16], [0; 16]));
}

#[tokio::test]
async fn profile_scoped_session_authority_is_rejected() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.profile-scope");
    let worktree_id = id::<WorktreeId>("worktree.native.profile-scope");
    let runtime = project_runtime(&temporary, &project_id).await;
    // A profile shard carries `StoreShardScopeV1::ProfileSessions`, which
    // can never authorize a project-scoped lifecycle lookup.
    let profile_sessions = runtime
        .registered_database_arc(HostAdmissionScope::Profile)
        .unwrap();

    assert_eq!(
        register_context_scout_lifecycle_authority(
            [41; 16],
            [42; 16],
            project_id,
            worktree_id,
            &profile_sessions,
        ),
        AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::NonProjectSessionScope)
    );
    assert!(!authority_is_registered([41; 16], [42; 16]));
}

#[tokio::test]
async fn registration_rejects_a_project_that_the_authority_does_not_own() {
    let temporary = TempDir::new().unwrap();
    let bound_project_id = id::<ProjectId>("project.native.bound");
    let worktree_id = id::<WorktreeId>("worktree.native.bound");
    let runtime = project_runtime(&temporary, &bound_project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();

    assert_eq!(
        register_context_scout_lifecycle_authority(
            [43; 16],
            [44; 16],
            id::<ProjectId>("project.native.unowned"),
            worktree_id,
            &sessions,
        ),
        AuthorityRegistrationV1::Rejected(AuthorityRejectionV1::ProjectNotOwnedByAuthority)
    );
    assert!(!authority_is_registered([43; 16], [44; 16]));
}

#[tokio::test]
async fn re_registration_is_idempotent_but_conflicting_identity_is_rejected() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.rereg");
    let worktree_id = id::<WorktreeId>("worktree.native.rereg");
    let runtime = project_runtime(&temporary, &project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();

    assert_eq!(
        register_context_scout_lifecycle_authority(
            [31; 16],
            [32; 16],
            project_id.clone(),
            worktree_id.clone(),
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
    );
    // An exactly-matching re-registration is a no-op, and says so instead
    // of being indistinguishable from the fresh install above.
    assert_eq!(
        register_context_scout_lifecycle_authority(
            [31; 16],
            [32; 16],
            project_id.clone(),
            worktree_id,
            &sessions,
        ),
        AuthorityRegistrationV1::AlreadyRegistered
    );
    // Same hook key, different native worktree: the live entry wins, and
    // the caller learns this was a conflict rather than bad input.
    assert_eq!(
        register_context_scout_lifecycle_authority(
            [31; 16],
            [32; 16],
            project_id,
            id::<WorktreeId>("worktree.native.rereg-conflict"),
            &sessions,
        ),
        AuthorityRegistrationV1::Conflict
    );
    assert!(authority_is_registered([31; 16], [32; 16]));
}

#[tokio::test]
async fn unregistration_removes_only_the_exact_session_client() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.unregister");
    let worktree_id = id::<WorktreeId>("worktree.native.unregister");
    let runtime = project_runtime(&temporary, &project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    assert_eq!(
        register_context_scout_lifecycle_authority(
            [11; 16],
            [12; 16],
            project_id.clone(),
            worktree_id,
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
    );

    // A different client under the same locator pair must not unregister
    // the incumbent: a rolled-back setup can never remove its successor.
    let foreign_temporary = TempDir::new().unwrap();
    let foreign_runtime = project_runtime(&foreign_temporary, &project_id).await;
    let foreign_sessions = foreign_runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    assert!(!unregister_context_scout_lifecycle_authority(
        [11; 16],
        [12; 16],
        &foreign_sessions,
    ));
    assert!(authority_is_registered([11; 16], [12; 16]));

    assert!(unregister_context_scout_lifecycle_authority(
        [11; 16], [12; 16], &sessions,
    ));
    assert!(!authority_is_registered([11; 16], [12; 16]));
}

#[tokio::test]
async fn registered_lifecycle_lookup_resolves_the_authoritative_tuple() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.registered-lookup");
    let worktree_id = id::<WorktreeId>("worktree.native.registered-lookup");
    let runtime = project_runtime(&temporary, &project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    sessions
        .observation_store()
        .persist_observation(durable_native_observation(&project_id))
        .await
        .unwrap();
    assert_eq!(
        register_context_scout_lifecycle_authority(
            [21; 16],
            [22; 16],
            project_id,
            worktree_id,
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
    );
    let session_id = id::<SessionId>("session.native.codex");

    let lifecycle = lookup_registered_context_scout_lifecycle([21; 16], [22; 16], &session_id)
        .await
        .unwrap();

    assert_eq!(
        lifecycle.project_id.as_str(),
        "project.native.registered-lookup"
    );
    assert_eq!(
        lifecycle.worktree_id.as_str(),
        "worktree.native.registered-lookup"
    );
    assert_eq!(lifecycle.provider_id.as_str(), "codex");
    assert_eq!(lifecycle.session_id.as_str(), "session.native.codex");
    assert_eq!(lifecycle.thread_id.as_str(), "thread.native.codex");
    assert_eq!(lifecycle.turn_id.as_str(), "turn.native.codex");
    assert_eq!(lifecycle.agent_id.as_str(), "agent.native.codex");
    assert_eq!(
        lifecycle.logical_message_id.as_str(),
        "message.native.codex"
    );
    assert_eq!(
        lifecycle.profile_id.as_str(),
        sessions.binding().shard_id.profile_id.as_str()
    );

    // An unregistered hook pair resolves nothing, even though the same
    // observation is durable and the same session id is requested.
    assert!(
        lookup_registered_context_scout_lifecycle([51; 16], [52; 16], &session_id)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn lookup_rejects_a_binding_that_does_not_match_the_requested_identity() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.binding");
    let worktree_id = id::<WorktreeId>("worktree.native.binding");
    let runtime = project_runtime(&temporary, &project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    sessions
        .observation_store()
        .persist_observation(durable_native_observation(&project_id))
        .await
        .unwrap();
    let bound_profile_id = sessions.binding().shard_id.profile_id.clone();
    let session_id = id::<SessionId>("session.native.codex");

    // Control: the matching identity does resolve, so the rejections
    // below are attributable to the binding checks and nothing else.
    assert!(
        lookup_context_scout_lifecycle(
            &bound_profile_id,
            &project_id,
            &worktree_id,
            &session_id,
            &sessions,
        )
        .await
        .is_resolved()
    );
    // Both rejections still fail closed, and both name the binding check
    // rather than the absence of evidence.
    assert_eq!(
        lookup_context_scout_lifecycle(
            &UserProfileId::new("profile.native.not-bound").unwrap(),
            &project_id,
            &worktree_id,
            &session_id,
            &sessions,
        )
        .await,
        ContextScoutLifecycleLookupV1::Unresolved(
            ContextScoutLifecycleLookupFailureV1::UnauthorizedBinding
        ),
        "a profile that does not own the shard must fail closed"
    );
    assert_eq!(
        lookup_context_scout_lifecycle(
            &bound_profile_id,
            &id::<ProjectId>("project.native.not-bound"),
            &worktree_id,
            &session_id,
            &sessions,
        )
        .await,
        ContextScoutLifecycleLookupV1::Unresolved(
            ContextScoutLifecycleLookupFailureV1::UnauthorizedBinding
        ),
        "a project the shard is not scoped to must fail closed"
    );
}

/// Every lookup failure reason must have a distinct, stable tracing
/// label: the reasons only earn their keep if a log line can tell an
/// unauthorized binding apart from exhausted evidence.
#[test]
fn lookup_failure_reasons_have_distinct_tracing_labels() {
    use ContextScoutLifecycleLookupFailureV1 as Failure;
    let reasons = [
        Failure::InvalidProfileId,
        Failure::InvalidProjectId,
        Failure::InvalidWorktreeId,
        Failure::InvalidSessionId,
        Failure::UnauthorizedBinding,
        Failure::SnapshotUnavailable,
        Failure::ObservationQueryFailed,
        Failure::ObservationRowUnreadable,
        Failure::ObservationBudgetExceeded,
        Failure::MalformedDurableObservation,
        Failure::DurableScopeMismatch,
        Failure::MalformedCanonicalEnvelope,
        Failure::CanonicalEnvelopeMismatch,
        Failure::NoCompleteLifecycle,
    ];
    let labels = reasons
        .iter()
        .map(|reason| reason.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(labels.len(), reasons.len());
    // Every reason still fails closed for the `Option`-shaped callers.
    for reason in reasons {
        assert!(
            ContextScoutLifecycleLookupV1::Unresolved(reason)
                .into_address()
                .is_none()
        );
    }
}

/// Every registration rejection must keep a distinct, stable label too.
#[test]
fn registration_rejections_have_distinct_labels() {
    let rejections = [
        AuthorityRejectionV1::ZeroHookProjectId,
        AuthorityRejectionV1::ZeroHookWorktreeId,
        AuthorityRejectionV1::InvalidProjectId,
        AuthorityRejectionV1::InvalidWorktreeId,
        AuthorityRejectionV1::NonProjectSessionScope,
        AuthorityRejectionV1::ProjectNotOwnedByAuthority,
    ];
    let labels = rejections
        .iter()
        .map(|rejection| rejection.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(labels.len(), rejections.len());
}

#[tokio::test]
async fn durable_scope_and_session_mismatches_fail_closed() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.durable-scope");
    let worktree_id = id::<WorktreeId>("worktree.native.durable-scope");
    let runtime = project_runtime(&temporary, &project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    // Durable evidence carrying a foreign project scope, stored in the
    // shard that the lookup is otherwise authorized against.
    let foreign = durable_native_observation(&id::<ProjectId>("project.native.foreign-scope"));
    // The store contract refuses to create this state: every observation
    // operation is gated on the observation scope matching the bound
    // shard family, so the runtime rejects the write before it reaches
    // the shard. The row can therefore only arrive by corruption or a
    // pre-contract import, which is exactly what the lookup's durable
    // scope check has to survive.
    assert!(
        sessions
            .observation_store()
            .persist_observation(foreign.clone())
            .await
            .is_err(),
        "the runtime must refuse a foreign-scoped write against this shard"
    );
    stage_foreign_scoped_observation(&sessions, &foreign).await;
    let bound_profile_id = sessions.binding().shard_id.profile_id.clone();

    // Foreign-scoped evidence and no evidence at all both fail closed,
    // but they are not the same observable outcome.
    assert_eq!(
        lookup_context_scout_lifecycle(
            &bound_profile_id,
            &project_id,
            &worktree_id,
            &id::<SessionId>("session.native.codex"),
            &sessions,
        )
        .await,
        ContextScoutLifecycleLookupV1::Unresolved(
            ContextScoutLifecycleLookupFailureV1::DurableScopeMismatch
        ),
        "an observation scoped to another project must never satisfy this lookup"
    );
    assert_eq!(
        lookup_context_scout_lifecycle(
            &bound_profile_id,
            &project_id,
            &worktree_id,
            &id::<SessionId>("session.native.absent"),
            &sessions,
        )
        .await,
        ContextScoutLifecycleLookupV1::Unresolved(
            ContextScoutLifecycleLookupFailureV1::NoCompleteLifecycle
        ),
        "a session with no durable evidence must fail closed"
    );
}

#[tokio::test]
async fn zero_protected_session_id_is_rejected_even_with_a_live_authority() {
    let temporary = TempDir::new().unwrap();
    let project_id = id::<ProjectId>("project.native.zero-protected");
    let runtime = project_runtime(&temporary, &project_id).await;
    let sessions = runtime
        .registered_database_arc(HostAdmissionScope::Project)
        .unwrap();
    sessions
        .observation_store()
        .persist_observation(durable_native_observation(&project_id))
        .await
        .unwrap();
    assert_eq!(
        register_context_scout_lifecycle_authority(
            [61; 16],
            [62; 16],
            project_id,
            id::<WorktreeId>("worktree.native.zero-protected"),
            &sessions,
        ),
        AuthorityRegistrationV1::Registered
    );

    assert!(
        lookup_registered_context_scout_native_session([61; 16], [62; 16], [0; 32])
            .await
            .is_none(),
        "an all-zero protected locator is never a resolvable session"
    );
}
