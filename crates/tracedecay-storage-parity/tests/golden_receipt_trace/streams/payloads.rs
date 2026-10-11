//! Every repository payload variant, on the registered shard that owns it.

use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
use tracedecay_runtime_core::params;
use tracedecay_store::RepositoryWritePayloadV1 as Payload;

use super::super::fixtures::{observation, project, project_id};
use super::registered::Registered;

/// `Configuration` on the profile shard, over a seeded root revision.
pub async fn profile() -> String {
    let mut store = Registered::publish("profile.db", TestDatabaseRuntimeScope::Profile).await;
    let root = project::empty_configuration_snapshot();
    store
        .seed(
            "seed_configuration_root_revision",
            "INSERT INTO configuration_revisions (revision_id, parent_revision_id, snapshot_id,
                effective_behavior_digest, resolution_provenance_digest, actor_id,
                operation_kind, created_at)
             VALUES (?1, NULL, ?2, ?3, ?4, 'actor.configuration.parity',
                'canonical_initialization', 1)",
            params![
                project::CONFIG_ROOT_REVISION,
                root.snapshot_id.as_str(),
                root.effective_behavior_digest.as_str(),
                root.resolution_provenance_digest.as_str()
            ],
        )
        .await;
    let commit = || {
        Payload::Configuration(Box::new(project::configuration(
            "configuration.revision.child",
            'a',
        )))
    };
    store
        .submit("configuration_commit", "key.configuration", 'a', commit())
        .await;
    store
        .submit(
            "configuration_exact_replay",
            "key.configuration",
            'a',
            commit(),
        )
        .await;
    store.finish()
}

/// Fact, Diagnostics, both retrieval-anchor variants and the external-source
/// family on the project shard.
pub async fn project() -> String {
    let mut store = Registered::publish(
        "project.db",
        TestDatabaseRuntimeScope::Project {
            project_id: project_id(),
        },
    )
    .await;
    let fact = |content| Payload::Fact(Box::new(project::fact(content)));
    store
        .submit("fact_fresh", "key.fact", 'a', fact("parity fact"))
        .await;
    store
        .submit("fact_exact_replay", "key.fact", 'a', fact("parity fact"))
        .await;
    store
        .submit(
            "fact_idempotency_conflict",
            "key.fact",
            'b',
            fact("parity fact changed"),
        )
        .await;
    store
        .submit(
            "fact_lineage_conflict",
            "key.fact.lineage",
            'c',
            fact("parity fact changed"),
        )
        .await;

    let diagnostics = |generation, message| {
        Payload::Diagnostics(Box::new(project::diagnostics(generation, message)))
    };
    store
        .submit(
            "diagnostics_fresh",
            "key.diagnostics",
            'a',
            diagnostics("generation.parity", "mismatched types"),
        )
        .await;
    store
        .submit(
            "diagnostics_republished",
            "key.diagnostics.changed",
            'b',
            diagnostics("generation.parity", "changed message"),
        )
        .await;
    store
        .submit(
            "diagnostics_anchor_collision",
            "key.diagnostics.collision",
            'c',
            diagnostics("generation.other", "mismatched types"),
        )
        .await;

    let owner_json = serde_json::to_string(&project::project_owner()).unwrap();
    store
        .seed(
            "seed_retrieval_anchor",
            "INSERT INTO retrieval_anchors (anchor_id, anchor_json, owner_json, projection_generation)
             VALUES (?1, '{}', ?2, 'projection.parity')",
            params![project::SOURCE_ANCHOR, owner_json],
        )
        .await;
    store
        .submit(
            "anchor_disposition_fresh",
            "key.anchor.disposition",
            'a',
            Payload::RetrievalAnchorDisposition(Box::new(project::anchor_disposition(true))),
        )
        .await;
    store
        .submit(
            "anchor_disposition_replay_conflict",
            "key.anchor.disposition.conflict",
            'b',
            Payload::RetrievalAnchorDisposition(Box::new(project::anchor_disposition(false))),
        )
        .await;
    store
        .submit(
            "anchor_derivative_fresh",
            "key.anchor.derivative",
            'a',
            Payload::RetrievalAnchorDerivative(Box::new(project::anchor_derivative(true))),
        )
        .await;
    store
        .submit(
            "anchor_derivative_replay_conflict",
            "key.anchor.derivative.conflict",
            'b',
            Payload::RetrievalAnchorDerivative(Box::new(project::anchor_derivative(false))),
        )
        .await;

    store
        .submit(
            "external_source_fresh",
            "key.external_source",
            'a',
            Payload::ExternalSource(Box::new(project::external_source())),
        )
        .await;
    store
        .submit(
            "external_source_projection",
            "key.external_source.projection",
            'a',
            Payload::ExternalSourceProjection(Box::new(project::external_source_projection())),
        )
        .await;
    store
        .submit(
            "external_source_batch",
            "key.external_source.batch",
            'a',
            Payload::ExternalSourceBatch(project::external_source_batch()),
        )
        .await;
    store
        .submit(
            "external_source_acquisition",
            "key.external_source.acquisition",
            'a',
            Payload::ExternalSourceAcquisition(Box::new(project::external_source_acquisition())),
        )
        .await;
    store
        .submit(
            "external_source_acquisition_cas_conflict",
            "key.external_source.acquisition.again",
            'b',
            Payload::ExternalSourceAcquisition(Box::new(project::external_source_acquisition())),
        )
        .await;
    store.finish()
}

/// The observation family and remote replay on the project-sessions shard.
pub async fn sessions() -> String {
    let project_id = project_id();
    let mut store = Registered::publish(
        "sessions.db",
        TestDatabaseRuntimeScope::ProjectSessions {
            project_id: project_id.clone(),
        },
    )
    .await;
    let single = |body, receipt| {
        Payload::Observation(Box::new(observation::single(&project_id, body, receipt)))
    };
    store
        .submit(
            "observation_fresh",
            "key.observation",
            'a',
            single("parity", "receipt.parity"),
        )
        .await;
    store
        .submit(
            "observation_resubmitted",
            "key.observation",
            'a',
            single("parity", "receipt.parity"),
        )
        .await;
    store
        .submit(
            "observation_identity_collision",
            "key.observation.collision",
            'b',
            single("conflicting", "receipt.conflicting"),
        )
        .await;
    store
        .submit(
            "observation_batch",
            "key.observation.batch",
            'a',
            Payload::ObservationBatch(observation::batch(&project_id)),
        )
        .await;
    let advance =
        || Payload::ObservationCursorAdvance(Box::new(observation::cursor_advance(&project_id)));
    store
        .submit("cursor_advance_fresh", "key.cursor", 'a', advance())
        .await;
    store
        .submit("cursor_advance_exact_replay", "key.cursor", 'a', advance())
        .await;
    store
        .submit(
            "cursor_advance_idempotency_conflict",
            "key.cursor",
            'b',
            advance(),
        )
        .await;

    let binding = store.binding.clone();
    store
        .submit(
            "remote_writer_fence_install",
            "key.fence.install",
            'a',
            Payload::RemoteWriterFenceInstall(Box::new(observation::fence_install(
                &binding,
                &project_id,
            ))),
        )
        .await;
    store
        .submit(
            "remote_observation_replay",
            "key.remote.replay",
            'b',
            Payload::RemoteObservationReplay(Box::new(observation::remote_replay(&project_id))),
        )
        .await;
    store
        .submit(
            "remote_writer_fence_promotion",
            "key.fence.promotion",
            'c',
            Payload::RemoteWriterFenceInstall(Box::new(observation::fence_promotion(
                &binding,
                &project_id,
            ))),
        )
        .await;
    store
        .submit(
            "remote_observation_replay_stale_fence",
            "key.remote.replay.stale",
            'd',
            Payload::RemoteObservationReplay(Box::new(observation::remote_replay(&project_id))),
        )
        .await;
    store.finish()
}
