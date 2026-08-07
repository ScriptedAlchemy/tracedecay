use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_domain::{
    CodeGenerationId, CommitId, CrossReferenceLocatorV1, CrossReferenceRelationV1,
    CrossReferenceTargetV1, ManifestDigest, ProjectionGenerationId, RepositoryId,
    RetrievalAnchorId, SessionId, SymbolOccurrenceId, TaskId,
};
use tracedecay_graph_db::{
    GraphNamespace, GraphProjectorRevision, GraphTraversalDirection, NeverCancelled,
    VerifiedGraphSnapshot,
};
use tracedecay_runtime_core::cross_reference_graph::{
    CROSS_REFERENCE_PROJECTOR_REVISION_V1, CrossReferenceGraphError, CrossReferenceProjectionV1,
    CrossReferenceStore, build_cross_reference_manifest_checked,
    cross_reference_projection_identity,
};

fn digest(label: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", label.to_string().repeat(64))).expect("digest")
}

fn code() -> CrossReferenceTargetV1 {
    CrossReferenceTargetV1::CodeSymbol {
        generation_id: CodeGenerationId::new("generation.code.fixture").expect("generation"),
        occurrence_id: SymbolOccurrenceId::new("symbol.cross-reference.fixture")
            .expect("occurrence"),
    }
}

fn commit() -> CrossReferenceTargetV1 {
    CrossReferenceTargetV1::GitCommit {
        repository_id: RepositoryId::new("repository.cross-reference").expect("repository"),
        commit_id: CommitId::new("a".repeat(40)).expect("commit"),
    }
}

fn session() -> CrossReferenceTargetV1 {
    CrossReferenceTargetV1::Session {
        session_id: SessionId::new("session.cross-reference").expect("session"),
    }
}

fn work() -> CrossReferenceTargetV1 {
    CrossReferenceTargetV1::WorkTask {
        generation_id: ProjectionGenerationId::try_from("generation.work.fixture".to_owned())
            .expect("generation"),
        task_id: TaskId::new("task.cross-reference").expect("task"),
    }
}

fn locator(
    sequence: usize,
    relation: CrossReferenceRelationV1,
    source: CrossReferenceTargetV1,
    target: CrossReferenceTargetV1,
) -> CrossReferenceLocatorV1 {
    CrossReferenceLocatorV1::new(
        digest('1'),
        digest(char::from_digit(u32::try_from(sequence).expect("digit"), 16).expect("label")),
        RetrievalAnchorId::new(format!("retrieval.cross-reference.{sequence}")).expect("anchor"),
        relation,
        source,
        target,
    )
    .expect("locator")
}

fn projection() -> CrossReferenceProjectionV1 {
    let mut locators = vec![
        locator(
            2,
            CrossReferenceRelationV1::SessionProducedCommit,
            commit(),
            session(),
        ),
        locator(
            1,
            CrossReferenceRelationV1::CodeObservedAtCommit,
            code(),
            commit(),
        ),
        locator(
            3,
            CrossReferenceRelationV1::WorkSupportedBy,
            session(),
            work(),
        ),
    ];
    locators.sort_by(|left, right| left.locator_digest().cmp(right.locator_digest()));
    CrossReferenceProjectionV1 {
        scope_digest: digest('1'),
        source_watermark: digest('f'),
        locators,
    }
}

fn store(projection: &CrossReferenceProjectionV1) -> CrossReferenceStore {
    let identity = cross_reference_projection_identity(
        GraphNamespace::new("cross-reference-test").expect("namespace"),
    )
    .expect("identity");
    let revision =
        GraphProjectorRevision::try_from(CROSS_REFERENCE_PROJECTOR_REVISION_V1.to_owned())
            .expect("revision");
    let manifest =
        build_cross_reference_manifest_checked(identity, projection, &revision, &|| Ok(()))
            .expect("manifest");
    let snapshot = VerifiedGraphSnapshot::memory(manifest, Arc::new(NeverCancelled))
        .expect("verified snapshot");
    CrossReferenceStore::from_verified_snapshot(snapshot, projection).expect("store")
}

#[test]
fn cross_reference_traversal_is_scope_bound_and_payload_free() {
    let projection = projection();
    let store = store(&projection);
    let relations = BTreeSet::from([
        CrossReferenceRelationV1::CodeObservedAtCommit,
        CrossReferenceRelationV1::SessionProducedCommit,
        CrossReferenceRelationV1::WorkSupportedBy,
    ]);
    let cancellation = Arc::new(NeverCancelled);

    let mut related = store
        .related(
            &code(),
            &digest('1'),
            &relations,
            GraphTraversalDirection::Outgoing,
            8,
            16,
            cancellation.clone(),
        )
        .expect("authorized traversal");
    related.sort();
    assert_eq!(related, vec![commit(), work(), session()]);
    assert_eq!(
        store.related(
            &code(),
            &digest('2'),
            &relations,
            GraphTraversalDirection::Outgoing,
            8,
            16,
            cancellation,
        ),
        Err(CrossReferenceGraphError::Denied)
    );
}

#[test]
fn cross_reference_projection_rejects_mixed_scope_and_replays_exactly() {
    let projection = projection();
    let mut mixed = projection.clone();
    mixed.locators[0] = CrossReferenceLocatorV1::new(
        digest('2'),
        digest('e'),
        RetrievalAnchorId::new("retrieval.cross-reference.foreign").expect("anchor"),
        CrossReferenceRelationV1::Related,
        code(),
        work(),
    )
    .expect("foreign locator");
    mixed
        .locators
        .sort_by(|left, right| left.locator_digest().cmp(right.locator_digest()));
    assert_eq!(mixed.validate(), Err(CrossReferenceGraphError::MixedScope));

    let identity = cross_reference_projection_identity(
        GraphNamespace::new("cross-reference-test").expect("namespace"),
    )
    .expect("identity");
    let revision =
        GraphProjectorRevision::try_from(CROSS_REFERENCE_PROJECTOR_REVISION_V1.to_owned())
            .expect("revision");
    let original = build_cross_reference_manifest_checked(
        identity.clone(),
        &projection,
        &revision,
        &|| Ok(()),
    )
    .expect("original");
    let replayed =
        build_cross_reference_manifest_checked(identity, &projection, &revision, &|| Ok(()))
            .expect("replayed");
    assert_eq!(original.generation, replayed.generation);
    assert_eq!(
        original
            .expected_recovered_digest(&|| Ok(()))
            .expect("original digest"),
        replayed
            .expected_recovered_digest(&|| Ok(()))
            .expect("replayed digest")
    );
}
