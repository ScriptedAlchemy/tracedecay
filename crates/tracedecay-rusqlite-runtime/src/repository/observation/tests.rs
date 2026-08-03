use rusqlite::Connection;
use serde_json::json;
use tracedecay_domain::{
    AuthorityEpoch, BrainId, BrainNodeId, ComponentVersion, CurrentRemoteAuthorityV1, EntityId,
    ManifestDigest, ObservationId, ObservationIdentityMaterialV1, ObservationOrderingDomainV1,
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, PayloadReferenceV1, ProjectId,
    ProjectionGenerationId, ProviderId, RefId, RemotePlacementRevisionV1, RemoteRepositoryScopeV1,
    RemoteWriterFenceV1, RepositoryId, RepositoryStateSnapshotId, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, ShardId, UtcMicros, WorktreeId,
};
use tracedecay_store::{
    AnchoredObservationWrite, ObservationCoverageReason, ObservationCursorAdvance,
    ObservationReadOperationV1, ObservationReadResultV1, ObservationWrite,
    RemoteObservationReplayPartsV1, RemoteObservationReplayWriteV1,
    SESSION_MESSAGE_PROJECTOR_VERSION, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use super::ObservationExecutor;

fn observation_write(body: &str, receipt_id: &str) -> ObservationWrite {
    observation_write_at(body, receipt_id, 1, 0, 1, None)
}

fn observation_write_at(
    body: &str,
    receipt_id: &str,
    generation: u64,
    start: u64,
    end: u64,
    expected_cursor: Option<ObservationSourceCursorV1>,
) -> ObservationWrite {
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("provider.fixture").unwrap(),
        SessionId::new("session.fixture").unwrap(),
    )
    .unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: ProjectId::new("project.fixture").unwrap(),
    };
    let generation = ObservationSourceGenerationV1::new(generation).unwrap();
    let range = ObservationSourceRangeV1::new(start, end).unwrap();
    let payload = json!({"kind": "assistant_message", "body": body});
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.fixture.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let observation = tracedecay_domain::DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            scope.clone(),
            generation,
            range,
            ObservationOrderingDomainV1::SqliteRowId,
            ObservationId::new("record.fixture").unwrap(),
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.fixture").unwrap(),
        payload,
    )
    .unwrap();
    let next_cursor = ObservationSourceCursorV1::for_ordering(
        source,
        scope,
        generation,
        ObservationOrderingDomainV1::SqliteRowId,
        range.end(),
    )
    .unwrap();
    ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap()
}

fn anchored_observation_write(body: &str, receipt_id: &str) -> AnchoredObservationWrite {
    let write = observation_write(body, receipt_id);
    anchored(write)
}

fn anchored(write: ObservationWrite) -> AnchoredObservationWrite {
    let projection_generation = ProjectionGenerationId::new("projection.fixture.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "runtime.fixture.v1")
            .unwrap();
    let anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    AnchoredObservationWrite::new(write, anchor, projection_generation).unwrap()
}

fn connection() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sanitization_receipts (
                    receipt_id TEXT PRIMARY KEY,
                    sanitizer_version TEXT NOT NULL,
                    payload_digest TEXT NOT NULL,
                    receipt_json TEXT NOT NULL
                 );
                 CREATE TABLE observations (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                    observation_id TEXT NOT NULL UNIQUE,
                    payload_digest TEXT NOT NULL,
                    receipt_id TEXT NOT NULL,
                    content_digest TEXT NOT NULL,
                    committed_cursor_json TEXT NOT NULL
                 );
                 CREATE TABLE session_content_objects (
                    content_digest TEXT PRIMARY KEY,
                    inline_bytes BLOB,
                    durable_file_locator TEXT,
                    byte_count INTEGER NOT NULL,
                    char_count INTEGER NOT NULL
                 );
                 CREATE TABLE session_content_references (
                    owner_kind TEXT NOT NULL,
                    owner_id TEXT NOT NULL,
                    content_kind TEXT NOT NULL,
                    content_digest TEXT NOT NULL,
                    sanitization_receipt_id TEXT,
                    retrieval_anchor_id TEXT,
                    PRIMARY KEY(owner_kind, owner_id)
                 );
                 CREATE TABLE source_cursors (
                    source_json TEXT NOT NULL,
                    scope_json TEXT NOT NULL,
                    cursor_json TEXT NOT NULL,
                    PRIMARY KEY (source_json, scope_json)
                 );
                 CREATE TABLE source_cursor_advances (
                    source_json TEXT NOT NULL,
                    scope_json TEXT NOT NULL,
                    coverage_json TEXT NOT NULL,
                    reason TEXT NOT NULL,
                    receipt_id TEXT,
                    PRIMARY KEY(source_json, scope_json, coverage_json)
                 );
                 CREATE TABLE projection_queue (
                    observation_id TEXT PRIMARY KEY,
                    observation_sequence INTEGER NOT NULL UNIQUE
                 );
                 CREATE TABLE retrieval_anchors (
                    anchor_id TEXT PRIMARY KEY,
                    anchor_json TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    projection_generation TEXT NOT NULL,
                    UNIQUE(anchor_id, owner_json)
                 );
                 CREATE TABLE retrieval_anchor_aliases (
                    owner_json TEXT NOT NULL,
                    alias_kind TEXT NOT NULL,
                    locator_digest TEXT NOT NULL,
                    anchor_id TEXT NOT NULL,
                    PRIMARY KEY(owner_json, alias_kind, locator_digest)
                 );
                 CREATE TABLE observation_retrieval_anchors (
                    observation_id TEXT PRIMARY KEY,
                    anchor_id TEXT NOT NULL UNIQUE
                 );
                 CREATE TABLE observation_repository_provenance (
                    observation_id TEXT PRIMARY KEY,
                    availability_json TEXT NOT NULL,
                    capture_json TEXT,
                    retrieval_anchor_id TEXT UNIQUE,
                    owner_json TEXT
                 );
                 CREATE TABLE observation_projection_checkpoints (
                    projector_version TEXT PRIMARY KEY,
                    last_sequence INTEGER NOT NULL
                 );
                 CREATE TABLE observation_projection_rebuilds (
                    projector_version TEXT PRIMARY KEY,
                    generation TEXT NOT NULL,
                    frontier_sequence INTEGER NOT NULL,
                    aliases_staged_through INTEGER NOT NULL,
                    staged_through INTEGER NOT NULL,
                    projected_rows INTEGER NOT NULL,
                    skipped_observations INTEGER NOT NULL,
                    state TEXT NOT NULL
                 );",
        )
        .unwrap();
    connection
        .execute_batch(crate::remote::REMOTE_OBSERVATION_EVENTS_SCHEMA)
        .unwrap();
    connection
}

fn execute(connection: &mut Connection, write: &AnchoredObservationWrite) -> rusqlite::Result<()> {
    let mut transaction = connection.transaction()?;
    let savepoint = transaction.savepoint()?;
    ObservationExecutor.execute_write(&savepoint, write)?;
    savepoint.commit()?;
    transaction.commit()
}

fn remote_write(
    anchored_write: AnchoredObservationWrite,
    digest_byte: char,
    capture_sequence: u64,
    previous_event_id: Option<String>,
) -> RemoteObservationReplayWriteV1 {
    let digest_hex = std::iter::repeat_n(digest_byte, 64).collect::<String>();
    RemoteObservationReplayWriteV1::new(
        RemoteObservationReplayPartsV1 {
            event_id: format!("remote.event.{digest_hex}"),
            frame_digest: ManifestDigest::new(format!("sha256:{digest_hex}")).unwrap(),
            enrollment_id: EntityId::new("enrollment.fixture").unwrap(),
            enrollment_revision: 1,
            node_id: BrainNodeId::new("node.fixture").unwrap(),
            policy_revision: 1,
            capture_sequence,
            previous_event_id,
            writer_project_id: ProjectId::new("project.fixture").unwrap(),
            writer_scope: RemoteRepositoryScopeV1 {
                project_id: ProjectId::new("project.fixture").unwrap(),
                repository_id: RepositoryId::new("repository.fixture").unwrap(),
                worktree_id: WorktreeId::new("worktree.fixture").unwrap(),
                reference: Some(RefId::new("refs/heads/main").unwrap()),
                snapshot_id: RepositoryStateSnapshotId::new("snapshot.fixture").unwrap(),
            },
            current_writer: CurrentRemoteAuthorityV1 {
                fence: RemoteWriterFenceV1 {
                    brain_id: BrainId::new("brain.fixture").unwrap(),
                    shard_id: ShardId::new("shard.fixture").unwrap(),
                    generation_id: ProjectionGenerationId::new("generation.fixture").unwrap(),
                    placement_revision: RemotePlacementRevisionV1::new(1).unwrap(),
                    authority_epoch: AuthorityEpoch(1),
                    authority_node_id: BrainNodeId::new("node.authority").unwrap(),
                },
                credential_revision: 1,
                observed_at: UtcMicros(1),
            },
            captured_at: UtcMicros(2),
        },
        anchored_write,
    )
    .unwrap()
}

fn execute_remote(
    connection: &mut Connection,
    write: &RemoteObservationReplayWriteV1,
) -> rusqlite::Result<()> {
    let mut transaction = connection.transaction()?;
    let savepoint = transaction.savepoint()?;
    ObservationExecutor.execute_remote_write(&savepoint, write)?;
    savepoint.commit()?;
    transaction.commit()
}

fn read(
    connection: &mut Connection,
    operation: &ObservationReadOperationV1,
) -> rusqlite::Result<ObservationReadResultV1> {
    let transaction = connection.transaction()?;
    ObservationExecutor.execute_read(&transaction, operation)
}

fn execute_cursor_advance(
    connection: &mut Connection,
    advance: &ObservationCursorAdvance,
) -> rusqlite::Result<()> {
    let mut transaction = connection.transaction()?;
    let savepoint = transaction.savepoint()?;
    ObservationExecutor.execute_cursor_advance(&savepoint, advance)?;
    savepoint.commit()?;
    transaction.commit()
}

#[test]
fn anchored_write_persists_all_authority_rows_atomically() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");

    execute(&mut connection, &write).unwrap();

    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_xinfo('observations')
                 WHERE name = 'observation_json'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    for table in [
        "observations",
        "sanitization_receipts",
        "retrieval_anchors",
        "retrieval_anchor_aliases",
        "observation_retrieval_anchors",
        "observation_repository_provenance",
        "source_cursors",
        "projection_queue",
        "session_content_objects",
        "session_content_references",
    ] {
        let count = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert!(count > 0, "{table} was not persisted");
    }
}

#[test]
fn remote_write_commits_canonical_effect_and_event_atomically() {
    let mut connection = connection();
    let write = remote_write(
        anchored_observation_write("remote fixture", "receipt.remote"),
        'a',
        1,
        None,
    );

    execute_remote(&mut connection, &write).unwrap();
    execute_remote(&mut connection, &write).unwrap();

    for table in [
        "observations",
        "sanitization_receipts",
        "retrieval_anchors",
        "observation_retrieval_anchors",
        "observation_repository_provenance",
        "source_cursors",
        "projection_queue",
        "remote_observation_events",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1,
            "{table} was not committed exactly once"
        );
    }
}

#[test]
fn remote_event_identity_conflict_rolls_back_without_partial_effect() {
    let mut connection = connection();
    let anchored = anchored_observation_write("remote fixture", "receipt.remote");
    let first = remote_write(anchored.clone(), 'a', 1, None);
    let conflict = remote_write(anchored, 'b', 1, None);
    execute_remote(&mut connection, &first).unwrap();

    assert!(execute_remote(&mut connection, &conflict).is_err());
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM remote_observation_events",
                [],
                |row| { row.get::<_, i64>(0) }
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM observations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
}

#[test]
fn remote_sequence_gap_rolls_back_every_canonical_effect() {
    let mut connection = connection();
    let missing_predecessor =
        "remote.event.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let write = remote_write(
        anchored_observation_write("remote gap", "receipt.remote-gap"),
        'b',
        2,
        Some(missing_predecessor.to_owned()),
    );

    assert!(execute_remote(&mut connection, &write).is_err());
    for table in [
        "observations",
        "sanitization_receipts",
        "retrieval_anchors",
        "source_cursors",
        "projection_queue",
        "remote_observation_events",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0,
            "{table} changed after sequence rejection"
        );
    }
}

#[test]
fn relocated_native_duplicate_advances_coverage_without_reinserting() {
    let mut connection = connection();
    let original = anchored(observation_write_at(
        "stable payload",
        "receipt.original",
        1,
        41,
        42,
        None,
    ));
    execute(&mut connection, &original).unwrap();
    let relocated = anchored(observation_write_at(
        "stable payload",
        "receipt.relocated",
        2,
        71,
        72,
        Some(original.next_cursor().clone()),
    ));
    assert_eq!(
        original.observation().observation_id(),
        relocated.observation().observation_id()
    );

    execute(&mut connection, &relocated).unwrap();
    execute(&mut connection, &relocated).unwrap();

    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM observations", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM source_cursor_advances", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sanitization_receipts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        2
    );
    let source_json = super::encode(relocated.observation().source()).unwrap();
    let scope_json = super::encode(relocated.observation().scope()).unwrap();
    assert_eq!(
        super::read_cursor(&connection, &source_json, &scope_json).unwrap(),
        Some(relocated.next_cursor().clone())
    );
}

#[test]
fn exact_replay_is_a_no_op_after_the_source_cursor_advanced() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    let replay_write = ObservationWrite::new(
        write.observation().clone(),
        None,
        write.next_cursor().clone().with_resume_checkpoint(7, 11),
    )
    .unwrap();
    let replay = AnchoredObservationWrite::new(
        replay_write,
        write.retrieval_anchor().clone(),
        write.projection_generation().clone(),
    )
    .unwrap();

    execute(&mut connection, &write).unwrap();
    execute(&mut connection, &replay).unwrap();

    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM projection_queue", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM session_content_objects", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM session_content_references
                 WHERE owner_kind = 'projection'
                   AND content_kind = 'observation_json'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

#[test]
fn replay_with_different_anchor_fails_without_mutating_authority_rows() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    execute(&mut connection, &write).unwrap();
    let conflicting_generation = ProjectionGenerationId::new("projection.conflicting.v1").unwrap();
    let authorization =
        build_observation_resolution_authorization_v1(write.observation(), "runtime.fixture.v1")
            .unwrap();
    let conflicting_anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        conflicting_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    let conflicting = AnchoredObservationWrite::new(
        ObservationWrite::new(
            write.observation().clone(),
            None,
            write.next_cursor().clone(),
        )
        .unwrap(),
        conflicting_anchor,
        conflicting_generation,
    )
    .unwrap();

    let error = execute(&mut connection, &conflicting).unwrap_err();

    assert!(error.to_string().contains("retrieval anchor"));
    for table in [
        "observations",
        "retrieval_anchors",
        "observation_retrieval_anchors",
        "observation_repository_provenance",
        "source_cursors",
        "projection_queue",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            1,
            "{table} changed after rejected replay"
        );
    }
}

#[test]
fn replay_does_not_repair_missing_anchor_authority() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    execute(&mut connection, &write).unwrap();
    connection
        .execute("DELETE FROM retrieval_anchor_aliases", [])
        .unwrap();

    let error = execute(&mut connection, &write).unwrap_err();

    assert!(error.to_string().contains("retrieval anchor alias"));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM retrieval_anchor_aliases", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}

#[test]
fn replay_rejects_extra_anchor_alias_authority() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    execute(&mut connection, &write).unwrap();
    connection
        .execute(
            "INSERT INTO retrieval_anchor_aliases (
                    owner_json, alias_kind, locator_digest, anchor_id
                 )
                 SELECT owner_json, 'corrupt-extra', locator_digest, anchor_id
                 FROM retrieval_anchor_aliases LIMIT 1",
            [],
        )
        .unwrap();

    let error = execute(&mut connection, &write).unwrap_err();

    assert!(error.to_string().contains("retrieval anchor alias"));
}

#[test]
fn identity_collision_fails_without_advancing_the_source_cursor() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    execute(&mut connection, &write).unwrap();
    let cursor_before: String = connection
        .query_row("SELECT cursor_json FROM source_cursors", [], |row| {
            row.get(0)
        })
        .unwrap();

    let error = execute(
        &mut connection,
        &anchored_observation_write("conflicting", "receipt.conflicting"),
    )
    .unwrap_err();

    assert!(error.to_string().contains("observation identity collision"));
    assert_eq!(
        connection
            .query_row("SELECT cursor_json FROM source_cursors", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
        cursor_before
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM projection_queue", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn source_cursor_advance_replays_exactly_and_rejects_reason_collision() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    execute(&mut connection, &write).unwrap();
    let advance = ObservationCursorAdvance::for_ordering(
        write.observation().source().clone(),
        write.observation().scope().clone(),
        write.observation().identity().generation(),
        write.observation().identity().ordering_domain(),
        Some(write.next_cursor().clone()),
        ObservationSourceRangeV1::new(1, 2).unwrap(),
        ObservationCoverageReason::BlankFrame,
    )
    .unwrap();

    execute_cursor_advance(&mut connection, &advance).unwrap();
    execute_cursor_advance(&mut connection, &advance).unwrap();

    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM source_cursor_advances", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        1
    );
    let conflicting = ObservationCursorAdvance::for_ordering(
        write.observation().source().clone(),
        write.observation().scope().clone(),
        write.observation().identity().generation(),
        write.observation().identity().ordering_domain(),
        Some(write.next_cursor().clone()),
        ObservationSourceRangeV1::new(1, 2).unwrap(),
        ObservationCoverageReason::OutOfScope,
    )
    .unwrap();
    let error = execute_cursor_advance(&mut connection, &conflicting).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source cursor advance identity collision")
    );
}

#[test]
fn retrieval_anchor_alias_reads_are_owner_bound() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    let alias = write.retrieval_anchor().aliases()[0].clone();
    execute(&mut connection, &write).unwrap();

    let resolved = read(
        &mut connection,
        &ObservationReadOperationV1::RetrievalAnchorByAlias {
            scope: write.observation().scope().clone(),
            alias: alias.clone(),
        },
    )
    .unwrap();
    assert_eq!(
        resolved,
        ObservationReadResultV1::RetrievalAnchorByAlias(Some(write.retrieval_anchor_id().clone()))
    );

    let foreign = read(
        &mut connection,
        &ObservationReadOperationV1::RetrievalAnchorByAlias {
            scope: ObservationScopeV1::Profile,
            alias,
        },
    )
    .unwrap();
    assert_eq!(
        foreign,
        ObservationReadResultV1::RetrievalAnchorByAlias(None)
    );
}

#[test]
fn replay_queue_and_checkpoint_reads_preserve_projection_ordering() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    execute(&mut connection, &write).unwrap();

    let point = read(
        &mut connection,
        &ObservationReadOperationV1::Observation {
            observation_id: write.observation().observation_id().clone(),
        },
    )
    .unwrap();
    let ObservationReadResultV1::Observation(point) = point else {
        panic!("unexpected point-read result");
    };
    let point = point.expect("persisted observation must be readable");
    assert_eq!(point.observation, *write.observation());
    assert_eq!(point.committed_cursor, *write.next_cursor());
    assert_eq!(point.retrieval_anchor, *write.retrieval_anchor());
    assert_eq!(point.projection_generation, *write.projection_generation());
    assert_eq!(
        point.repository_provenance,
        *write.repository_provenance_attachment()
    );
    assert!(point.projection_queued);

    let replay = read(
        &mut connection,
        &ObservationReadOperationV1::Replay {
            after_sequence: 0,
            limit: 10,
        },
    )
    .unwrap();
    let ObservationReadResultV1::Replay(rows) = replay else {
        panic!("unexpected replay result");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].observation.observation_id(),
        write.observation().observation_id()
    );
    assert_eq!(&rows[0].retrieval_anchor, write.retrieval_anchor());
    assert_eq!(
        &rows[0].projection_generation,
        write.projection_generation()
    );
    assert_eq!(
        &rows[0].repository_provenance,
        write.repository_provenance_attachment()
    );
    assert!(rows[0].projection_queued);

    assert_eq!(
        read(
            &mut connection,
            &ObservationReadOperationV1::NextQueuedProjection,
        )
        .unwrap(),
        ObservationReadResultV1::NextQueuedProjection(Some(
            write.observation().observation_id().clone()
        ))
    );
    assert_eq!(
        read(
            &mut connection,
            &ObservationReadOperationV1::ProjectionCheckpoint,
        )
        .unwrap(),
        ObservationReadResultV1::ProjectionCheckpoint(0)
    );

    connection
        .execute(
            "INSERT INTO observation_projection_checkpoints
                    (projector_version, last_sequence) VALUES (?1, 1)",
            [SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO observation_projection_rebuilds (
                    projector_version, generation, frontier_sequence, aliases_staged_through,
                    staged_through, projected_rows, skipped_observations, state
                 ) VALUES (?1, ?2, 1, 1, 1, 1, 0, 'ready')",
            [SESSION_MESSAGE_PROJECTOR_VERSION, "projection.fixture.v1"],
        )
        .unwrap();
    assert_eq!(
        read(
            &mut connection,
            &ObservationReadOperationV1::NextQueuedProjection,
        )
        .unwrap(),
        ObservationReadResultV1::NextQueuedProjection(None)
    );
    assert_eq!(
        read(
            &mut connection,
            &ObservationReadOperationV1::ProjectionCheckpoint,
        )
        .unwrap(),
        ObservationReadResultV1::ProjectionCheckpoint(1)
    );
    let progress = read(
        &mut connection,
        &ObservationReadOperationV1::ProjectionRebuildProgress,
    )
    .unwrap();
    let ObservationReadResultV1::ProjectionRebuildProgress(Some(progress)) = progress else {
        panic!("unexpected projection rebuild progress result");
    };
    assert_eq!(
        progress.generation,
        ProjectionGenerationId::new("projection.fixture.v1").unwrap()
    );
    assert_eq!(progress.frontier_sequence, 1);
    assert_eq!(progress.staged_through, 1);
    assert_eq!(progress.projected_rows, 1);
}

#[test]
fn point_and_replay_reads_reject_incomplete_observation_authority() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    execute(&mut connection, &write).unwrap();
    connection
        .execute("DELETE FROM observation_retrieval_anchors", [])
        .unwrap();

    for operation in [
        ObservationReadOperationV1::Observation {
            observation_id: write.observation().observation_id().clone(),
        },
        ObservationReadOperationV1::Replay {
            after_sequence: 0,
            limit: 10,
        },
    ] {
        let error = read(&mut connection, &operation).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("observation retrieval anchor is missing")
        );
    }
}

#[test]
fn point_read_rejects_missing_projection_content_authority() {
    let mut connection = connection();
    let write = anchored_observation_write("fixture", "receipt.fixture");
    execute(&mut connection, &write).unwrap();
    connection
        .execute(
            "DELETE FROM session_content_references
             WHERE owner_kind = 'projection'",
            [],
        )
        .unwrap();

    let error = read(
        &mut connection,
        &ObservationReadOperationV1::Observation {
            observation_id: write.observation().observation_id().clone(),
        },
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("observation content authorization is missing")
    );
}
