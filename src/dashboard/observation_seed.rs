//! Canonical observation-capture seeding for dashboard integration fixtures.
//!
//! The daemon session-temporal projection discovers sessions exclusively from
//! `session_temporal_observation_effects`; raw `sessions`/`session_messages`
//! upserts never reach it. Fixtures that want their sessions readable through
//! the daemon LCM/explorer authorities must therefore seed messages through
//! the same durable-observation persist/project route production ingest uses,
//! then materialize the pending session refresh. This root-owned bridge keeps
//! those store authorities out of the integration crates.

use serde_json::json;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1, CanonicalObservationEvidenceV1,
    CanonicalObservationFactV1, CanonicalObservationRelationsV1, ComponentVersion,
    DurableObservationV1, ObservationId, ObservationIdentityMaterialV1,
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceCursorV1,
    ObservationSourceGenerationV1, ObservationSourceIdentityV1, ObservationSourceRangeV1,
    PayloadReferenceV1, ProjectId, ProjectionGenerationId, ProviderId, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationProjectionStore, ObservationStore, ObservationWrite,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;

/// One canonical message observation to seed for a dashboard fixture session.
pub struct DashboardSessionMessageSeedV1<'a> {
    pub project_id: &'a str,
    pub provider: &'a str,
    pub session_id: &'a str,
    pub message_id: &'a str,
    pub role: &'a str,
    pub content: &'a str,
    pub model: Option<&'a str>,
    pub timestamp: i64,
    pub ordinal: u64,
}

fn fixture_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("dashboard observation fixture {operation}: {error}"),
    }
}

fn seed_role(role: &str) -> Result<CanonicalMessageRoleV1> {
    match role {
        "user" => Ok(CanonicalMessageRoleV1::User),
        "assistant" => Ok(CanonicalMessageRoleV1::Assistant),
        "system" => Ok(CanonicalMessageRoleV1::System),
        "tool" => Ok(CanonicalMessageRoleV1::Tool),
        other => Err(TraceDecayError::Config {
            message: format!("dashboard observation fixture role '{other}' is not canonical"),
        }),
    }
}

fn build_observation(seed: &DashboardSessionMessageSeedV1<'_>) -> Result<DurableObservationV1> {
    let session_id =
        SessionId::new(seed.session_id).map_err(|error| fixture_error("session id", error))?;
    let provider =
        ProviderId::new(seed.provider).map_err(|error| fixture_error("provider id", error))?;
    let source = ObservationSourceIdentityV1::for_provider(provider.clone(), session_id.clone())
        .map_err(|error| fixture_error("source identity", error))?;
    let range = ObservationSourceRangeV1::new(seed.ordinal, seed.ordinal + 1)
        .map_err(|error| fixture_error("source range", error))?;
    let message_id =
        ObservationId::new(seed.message_id).map_err(|error| fixture_error("message id", error))?;
    let relations =
        CanonicalObservationRelationsV1::new(session_id).with_message_id(message_id.clone());
    let envelope = CanonicalObservationEnvelopeV1::new(
        provider,
        "message",
        message_id.clone(),
        relations,
        vec![CanonicalObservationFactV1::Message {
            role: seed_role(seed.role)?,
            content: json!({ "text": seed.content }),
            model: seed.model.map(ToOwned::to_owned),
            timestamp: Some(seed.timestamp),
        }],
        CanonicalObservationEvidenceV1::new(ObservationOrderingDomainV1::SnapshotOrder, range)
            .with_native_timestamp(seed.timestamp),
    )
    .map_err(|error| fixture_error("canonical envelope", error))?;
    let payload =
        serde_json::to_value(envelope).map_err(|error| fixture_error("payload encode", error))?;
    let scope = ObservationScopeV1::Project {
        project_id: ProjectId::new(seed.project_id)
            .map_err(|error| fixture_error("project id", error))?,
    };
    let identity = ObservationIdentityMaterialV1::for_native_record(
        source,
        scope,
        ObservationSourceGenerationV1::new(1)
            .map_err(|error| fixture_error("source generation", error))?,
        range,
        ObservationOrderingDomainV1::SnapshotOrder,
        message_id.clone(),
    )
    .map_err(|error| fixture_error("identity material", error))?;
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt-dashboard-{}", seed.message_id))
                .map_err(|error| fixture_error("receipt id", error))?,
            ComponentVersion::new("sanitizer.dashboard-fixture.v1")
                .map_err(|error| fixture_error("sanitizer version", error))?,
        )
        .map_err(|error| fixture_error("receipt reference", error))?,
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(
            PayloadReferenceV1::for_payload(&payload)
                .map_err(|error| fixture_error("payload reference", error))?,
        ),
    )
    .map_err(|error| fixture_error("sanitization receipt", error))?;
    DurableObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.dashboard-fixture")
            .map_err(|error| fixture_error("retention class", error))?,
        payload,
    )
    .map_err(|error| fixture_error("durable observation", error))
}

/// Persists one canonical message observation and projects it into the
/// session-temporal observation effects the refresh scheduler discovers from.
pub async fn seed_session_message_observation_for_test(
    project_database: &RegisteredGlobalDb,
    seed: DashboardSessionMessageSeedV1<'_>,
) -> Result<()> {
    let observation = build_observation(&seed)?;
    let identity = observation.identity();
    let observation_id = observation.observation_id().clone();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        observation.source().clone(),
        observation.scope().clone(),
        identity.generation(),
        identity.ordering_domain(),
        identity.position().end(),
    )
    .map_err(|error| fixture_error("source cursor", error))?;
    // Observation writes are cursor-fenced per source: the first message of a
    // fixture session (ordinal 1) starts the source, and every later message
    // must present the cursor its predecessor left behind.
    let expected_cursor = if seed.ordinal <= 1 {
        None
    } else {
        Some(
            ObservationSourceCursorV1::for_ordering(
                observation.source().clone(),
                observation.scope().clone(),
                identity.generation(),
                identity.ordering_domain(),
                seed.ordinal,
            )
            .map_err(|error| fixture_error("expected source cursor", error))?,
        )
    };
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor)
        .map_err(|error| fixture_error("observation write", error))?;
    let projection = ProjectionGenerationId::new("projection.dashboard-fixture.v1")
        .map_err(|error| fixture_error("projection generation", error))?;
    let authorization = build_observation_resolution_authorization_v1(
        write.observation(),
        tracedecay_store::OBSERVATION_CAPTURE_AUTHORITY_V1,
    )
    .map_err(|error| fixture_error("resolution authorization", error))?;
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection.clone(),
        UtcMicros(1),
        authorization,
    )
    .map_err(|error| fixture_error("retrieval anchor", error))?;
    let store = crate::store::GlobalDbObservationStore::with_runtime(
        project_database.runtime(),
        project_database.authority(),
    );
    store
        .persist_observation(
            AnchoredObservationWrite::new(write, anchor, projection)
                .map_err(|error| fixture_error("anchored write", error))?,
        )
        .await
        .map_err(|error| fixture_error("persist observation", error))?;
    store
        .project_observation(&observation_id)
        .await
        .map_err(|error| fixture_error("project observation", error))?;
    Ok(())
}

/// Materializes the pending session-temporal refresh so the seeded
/// observations become readable through the daemon retrieval authority, then
/// re-applies the session relation projection for the newly-active
/// generation — the step the daemon's LCM effects loop performs after every
/// production refresh, without which summary lineage reads go relation-blind.
pub async fn materialize_session_temporal_refresh_for_test(
    project_database: &RegisteredGlobalDb,
    session_id: &str,
) -> Result<()> {
    let session_id =
        SessionId::new(session_id).map_err(|error| fixture_error("session id", error))?;
    crate::store::GlobalDbSessionTemporalStore::new(project_database)
        .materialize_pending_session_refresh_for_test(&session_id)
        .await
        .map_err(|error| fixture_error("materialize session refresh", error))?;
    project_database
        .apply_active_session_relation_projection(
            &session_id,
            std::sync::Arc::new(DashboardFixtureGraphCancellation),
        )
        .await
        .map_err(|error| fixture_error("apply session relation projection", error))?;
    Ok(())
}

struct DashboardFixtureGraphCancellation;

impl tracedecay_graph_db::GraphCancellation for DashboardFixtureGraphCancellation {
    fn is_cancelled(&self) -> bool {
        false
    }
}
