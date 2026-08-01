use super::*;
use tracedecay_domain::{
    ComponentVersion, Confidence, EvidenceClass, FactAssertionId, FactAssertionKindV1,
    FactAssertionV1, FactCategoryV1, FactEventId, FactEvidenceRefV1, FactEvidenceRelationV1,
    FactId, FactIdentityMaterialV1, FactIdentitySourceV1, FactLineageEventKindV1,
    FactLineageEventV1, FactPayloadV1, PayloadAccessState, PayloadReferenceV1, ProvenanceId,
    RetentionClass, RetrievalAnchorId, SanitizationReceiptId, SanitizationReceiptRefV1,
    SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1, UtcMicros,
};
use tracedecay_store::{FactCurrentQuery, FactLineageQuery};

/// Every table `insert_assertion` writes or compares against, so the write
/// path is exercised with the real column set rather than a stub.
fn assertion_schema(connection: &rusqlite::Connection) {
    connection
        .execute_batch(
            "CREATE TABLE memory_v2_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_access TEXT NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_assertions (
                    assertion_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    assertion_header_json TEXT NOT NULL,
                    kind_json TEXT NOT NULL,
                    payload_reference_json TEXT NOT NULL,
                    receipt_json TEXT NOT NULL,
                    asserted_at INTEGER NOT NULL,
                    actor_id TEXT,
                    PRIMARY KEY (assertion_id, fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_assertion_supersession (
                    assertion_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    superseded_assertion_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    PRIMARY KEY (assertion_id, fact_id, owner_kind, project_id, ordinal)
                 );
                 CREATE TABLE memory_v2_assertion_payloads (
                    assertion_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    content TEXT NOT NULL,
                    PRIMARY KEY (assertion_id, fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_evidence (
                    evidence_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    anchor_id TEXT NOT NULL,
                    evidence_json TEXT NOT NULL,
                    PRIMARY KEY (evidence_id, fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_assertion_evidence (
                    assertion_id TEXT NOT NULL,
                    evidence_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    ordinal INTEGER NOT NULL,
                    PRIMARY KEY (assertion_id, fact_id, owner_kind, project_id, ordinal)
                 );",
        )
        .unwrap();
}

fn payload(content: &str) -> FactPayloadV1 {
    let material = serde_json::json!({
        "content": content,
        "category": "project",
        "tags": ["fact-executor"],
        "entities": ["TraceDecay"],
        "metadata": {},
    });
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new("receipt.fact-executor").unwrap(),
            ComponentVersion::new("sanitizer.fact-executor.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&material).unwrap()),
    )
    .unwrap();
    FactPayloadV1::new(
        content.to_owned(),
        FactCategoryV1::Project,
        vec!["fact-executor".to_owned()],
        vec!["TraceDecay".to_owned()],
        serde_json::json!({}),
        receipt,
        RetentionClass::new("durable.fact-executor").unwrap(),
    )
    .unwrap()
}

fn evidence_ref(fact_id: &FactId, anchor: &str) -> FactEvidenceRefV1 {
    FactEvidenceRefV1::new(
        fact_id.clone(),
        RetrievalAnchorId::new(anchor).unwrap(),
        FactEvidenceRelationV1::Supports,
        EvidenceClass::Observed,
        Confidence::new(1.0).unwrap(),
    )
    .unwrap()
}

fn assertion(fact_id: &FactId, content: &str, evidence: Vec<FactEvidenceRefV1>) -> FactAssertionV1 {
    FactAssertionV1::new(
        fact_id.clone(),
        FactOwnerV1::Profile,
        FactAssertionKindV1::Initial,
        payload(content),
        evidence,
        UtcMicros(5),
        None,
    )
    .unwrap()
}

#[test]
fn assertion_replay_is_idempotent_and_reuse_is_a_collision() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    assertion_schema(&connection);
    let owner = OwnerColumns::new(&FactOwnerV1::Profile).unwrap();
    let fact_id = profile_fact_id("operation.assertion-replay");
    let anchor = "retrieval.fact-executor.alpha";
    let assertion = assertion(
        &fact_id,
        "assertion replay",
        vec![evidence_ref(&fact_id, anchor)],
    );
    let savepoint = connection.savepoint().unwrap();

    insert_assertion(&savepoint, &owner, &assertion).unwrap();
    // Exact replay of a stored assertion is a no-op, not a primary-key
    // violation surfaced from the driver.
    insert_assertion(&savepoint, &owner, &assertion).unwrap();
    let assertions = savepoint
        .query_row(
            "SELECT COUNT(*) FROM memory_v2_assertions WHERE assertion_id = ?1",
            [assertion.assertion_id().as_str()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(assertions, 1, "replay must not append a second assertion");

    // The same assertion id bound to different stored content is a
    // collision, classified exactly as the root commit engine classifies it.
    savepoint
        .execute(
            "UPDATE memory_v2_assertions SET asserted_at = asserted_at + 1
                 WHERE assertion_id = ?1",
            [assertion.assertion_id().as_str()],
        )
        .unwrap();
    let error = insert_assertion(&savepoint, &owner, &assertion).unwrap_err();
    assert!(
        error.to_string().contains("assertion identity collision"),
        "unexpected error: {error}"
    );
}

#[test]
fn evidence_rebound_to_another_anchor_is_a_collision() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    assertion_schema(&connection);
    let owner = OwnerColumns::new(&FactOwnerV1::Profile).unwrap();
    let fact_id = profile_fact_id("operation.evidence-rebound");
    let evidence = evidence_ref(&fact_id, "retrieval.fact-executor.alpha");
    let assertion = assertion(&fact_id, "evidence rebound", vec![evidence.clone()]);
    let savepoint = connection.savepoint().unwrap();
    // A stored evidence row that reuses the evidence id against a different
    // anchor must not be silently adopted by `INSERT OR IGNORE`.
    savepoint
        .execute(
            "INSERT INTO memory_v2_evidence (
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                evidence.evidence_id().as_str(),
                fact_id.as_str(),
                owner.kind,
                owner.project_id,
                owner.json,
                "retrieval.fact-executor.beta",
                encode(&evidence).unwrap(),
            ],
        )
        .unwrap();

    let error = insert_assertion(&savepoint, &owner, &assertion).unwrap_err();
    assert!(
        error.to_string().contains("evidence identity collision"),
        "unexpected error: {error}"
    );
}

#[test]
fn evidence_exact_replay_is_accepted() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    assertion_schema(&connection);
    let owner = OwnerColumns::new(&FactOwnerV1::Profile).unwrap();
    let fact_id = profile_fact_id("operation.evidence-replay");
    let anchor = "retrieval.fact-executor.alpha";
    let evidence = evidence_ref(&fact_id, anchor);
    let assertion = assertion(&fact_id, "evidence replay", vec![evidence.clone()]);
    let savepoint = connection.savepoint().unwrap();
    savepoint
        .execute(
            "INSERT INTO memory_v2_evidence (
                    evidence_id, fact_id, owner_kind, project_id,
                    owner_json, anchor_id, evidence_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                evidence.evidence_id().as_str(),
                fact_id.as_str(),
                owner.kind,
                owner.project_id,
                owner.json,
                anchor,
                encode(&evidence).unwrap(),
            ],
        )
        .unwrap();

    insert_assertion(&savepoint, &owner, &assertion).unwrap();
    let linked = savepoint
        .query_row(
            "SELECT COUNT(*) FROM memory_v2_assertion_evidence
                 WHERE assertion_id = ?1 AND evidence_id = ?2",
            params![
                assertion.assertion_id().as_str(),
                evidence.evidence_id().as_str(),
            ],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(
        linked, 1,
        "identical evidence must still link the assertion"
    );
}

fn profile_fact_id(operation: &str) -> FactId {
    FactId::derive(
        &FactIdentityMaterialV1::new(
            FactOwnerV1::Profile,
            FactIdentitySourceV1::Application {
                operation_id: ProvenanceId::new(operation).unwrap(),
            },
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn fact_write_rejects_stored_identity_mismatch() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    last_event_id TEXT NOT NULL
                 );
                 CREATE TABLE memory_v2_facts (
                    fact_id TEXT PRIMARY KEY,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    identity_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                 );",
        )
        .unwrap();
    let owner = FactOwnerV1::Profile;
    let requested_identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: ProvenanceId::new("operation.requested").unwrap(),
        },
    )
    .unwrap();
    let requested_fact_id = FactId::derive(&requested_identity).unwrap();
    let stored_identity = FactIdentityMaterialV1::new(
        owner.clone(),
        FactIdentitySourceV1::Application {
            operation_id: ProvenanceId::new("operation.other").unwrap(),
        },
    )
    .unwrap();
    connection
        .execute(
            "INSERT INTO memory_v2_facts (
                    fact_id, owner_kind, project_id, owner_json, identity_json, created_at
                 ) VALUES (?1, 'profile', '', ?2, ?3, 1)",
            params![
                requested_fact_id.as_str(),
                serde_json::to_string(&owner).unwrap(),
                serde_json::to_string(&stored_identity).unwrap(),
            ],
        )
        .unwrap();
    let event = FactLineageEventV1::new(
        requested_fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(2),
        None,
    )
    .unwrap();
    let batch = FactWriteBatch::new(
        requested_fact_id,
        owner,
        None,
        vec![event],
        vec![],
        vec![],
        None,
        None,
    )
    .unwrap()
    .with_identity_material(requested_identity)
    .unwrap();
    let savepoint = connection.savepoint().unwrap();

    let error = FactExecutor.execute_write(&savepoint, &batch).unwrap_err();
    assert!(error.to_string().contains("fact identity collision"));
}

#[test]
fn fact_executor_does_not_claim_replay_without_writer_ledger() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    last_event_id TEXT NOT NULL
                 );
                 CREATE TABLE memory_v2_lineage_events (
                    event_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    event_json TEXT NOT NULL,
                    occurred_at INTEGER NOT NULL
                 );",
        )
        .unwrap();
    let owner = FactOwnerV1::Profile;
    let fact_id = profile_fact_id("operation.writer-ledger");
    let event = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(2),
        None,
    )
    .unwrap();
    connection
        .execute(
            "INSERT INTO memory_v2_current_facts
                    (fact_id, owner_kind, project_id, last_event_id)
                 VALUES (?1, 'profile', '', ?2)",
            params![fact_id.as_str(), event.event_id().as_str()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO memory_v2_lineage_events
                    (event_id, fact_id, owner_kind, project_id, event_json, occurred_at)
                 VALUES (?1, ?2, 'profile', '', ?3, ?4)",
            params![
                event.event_id().as_str(),
                fact_id.as_str(),
                serde_json::to_string(&event).unwrap(),
                event.occurred_at().0,
            ],
        )
        .unwrap();
    let batch = FactWriteBatch::new(
        fact_id,
        owner,
        None,
        vec![event],
        vec![],
        vec![],
        None,
        None,
    )
    .unwrap();
    let savepoint = connection.savepoint().unwrap();

    let error = FactExecutor.execute_write(&savepoint, &batch).unwrap_err();
    assert!(error.to_string().contains("last-event conflict"));
}

#[test]
fn purge_access_transition_clears_active_assertion() {
    for current in [PayloadAccessState::Quarantined, PayloadAccessState::Deleted] {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_v2_current_facts (
                        fact_id TEXT NOT NULL,
                        owner_kind TEXT NOT NULL,
                        project_id TEXT NOT NULL,
                        payload_access TEXT NOT NULL,
                        trust_score REAL,
                        active_assertion_id TEXT,
                        last_event_id TEXT NOT NULL,
                        updated_at INTEGER NOT NULL,
                        PRIMARY KEY (fact_id, owner_kind, project_id)
                    );",
            )
            .unwrap();
        let owner = FactOwnerV1::Profile;
        let owner_columns = OwnerColumns::new(&owner).unwrap();
        let fact_id = profile_fact_id("operation.purge-projection");
        connection
            .execute(
                "INSERT INTO memory_v2_current_facts (
                        fact_id, owner_kind, project_id, payload_access, trust_score,
                        active_assertion_id, last_event_id, updated_at
                     ) VALUES (?1, 'profile', '', 'eligible', 0.8, ?2, ?3, 1)",
                params![
                    fact_id.as_str(),
                    FactAssertionId::new("assertion.active").unwrap().as_str(),
                    FactEventId::new("event.previous").unwrap().as_str(),
                ],
            )
            .unwrap();
        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner.clone(),
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current,
            },
            UtcMicros(2),
            None,
        )
        .unwrap();
        let batch = FactWriteBatch::new(
            fact_id.clone(),
            owner,
            None,
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap();
        let savepoint = connection.savepoint().unwrap();

        publish_projection(&savepoint, &owner_columns, &batch).unwrap();
        let active = savepoint
            .query_row(
                "SELECT active_assertion_id FROM memory_v2_current_facts
                     WHERE fact_id = ?1 AND owner_kind = 'profile' AND project_id = ''",
                [fact_id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap();

        assert_eq!(active, None, "{current:?} must purge the active assertion");
    }
}

#[test]
fn stale_projection_transitions_are_rejected() {
    let mut connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_access TEXT NOT NULL,
                    trust_score REAL,
                    active_assertion_id TEXT,
                    last_event_id TEXT NOT NULL,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                );",
        )
        .unwrap();
    let owner = FactOwnerV1::Profile;
    let owner_columns = OwnerColumns::new(&owner).unwrap();
    let fact_id = profile_fact_id("operation.stale-projection");
    connection
        .execute(
            "INSERT INTO memory_v2_current_facts (
                    fact_id, owner_kind, project_id, payload_access, trust_score,
                    active_assertion_id, last_event_id, updated_at
                 ) VALUES (?1, 'profile', '', 'eligible', 0.8, ?2, ?3, 1)",
            params![
                fact_id.as_str(),
                FactAssertionId::new("assertion.active").unwrap().as_str(),
                FactEventId::new("event.previous").unwrap().as_str(),
            ],
        )
        .unwrap();
    let stale_trust = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::TrustChanged {
            previous: Confidence::new(0.7).unwrap(),
            current: Confidence::new(0.9).unwrap(),
            evidence_ids: vec![],
        },
        UtcMicros(2),
        None,
    )
    .unwrap();
    let stale_access = FactLineageEventV1::new(
        fact_id.clone(),
        owner.clone(),
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Redacted,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(3),
        None,
    )
    .unwrap();

    for event in [stale_trust, stale_access] {
        let batch = FactWriteBatch::new(
            fact_id.clone(),
            owner.clone(),
            None,
            vec![event],
            vec![],
            vec![],
            None,
            None,
        )
        .unwrap();
        let savepoint = connection.savepoint().unwrap();

        assert!(publish_projection(&savepoint, &owner_columns, &batch).is_err());
    }
}

#[test]
fn current_read_omits_fact_without_active_assertion() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE memory_v2_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    owner_json TEXT NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_current_facts (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_access TEXT NOT NULL,
                    trust_score REAL,
                    active_assertion_id TEXT,
                    last_event_id TEXT NOT NULL,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY (fact_id, owner_kind, project_id)
                 );
                 CREATE TABLE memory_v2_assertion_payloads (
                    assertion_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    payload_json TEXT NOT NULL
                 );
                 CREATE TABLE memory_v2_legacy_map (
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    mapping_json TEXT NOT NULL
                 );",
        )
        .unwrap();
    let owner = FactOwnerV1::Profile;
    let fact_id = profile_fact_id("operation.current-after-purge");
    connection
        .execute(
            "INSERT INTO memory_v2_facts
                    (fact_id, owner_kind, project_id, owner_json)
                 VALUES (?1, 'profile', '', ?2)",
            params![fact_id.as_str(), encode(&owner).unwrap()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO memory_v2_current_facts (
                    fact_id, owner_kind, project_id, payload_access, trust_score,
                    active_assertion_id, last_event_id, updated_at
                 ) VALUES (?1, 'profile', '', 'deleted', 0.8, NULL, ?2, 2)",
            params![
                fact_id.as_str(),
                FactEventId::new("event.deleted").unwrap().as_str(),
            ],
        )
        .unwrap();
    let query = FactCurrentQuery::new(owner, fact_id).unwrap();

    assert_eq!(read_current(&connection, &query).unwrap(), None);
}

#[test]
fn lineage_read_rejects_stored_event_identity_mismatch() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE memory_v2_lineage_events (
                    event_id TEXT NOT NULL,
                    fact_id TEXT NOT NULL,
                    owner_kind TEXT NOT NULL,
                    project_id TEXT NOT NULL,
                    event_json TEXT NOT NULL,
                    occurred_at INTEGER NOT NULL
                );",
        )
        .unwrap();
    let requested_fact_id = profile_fact_id("operation.requested");
    let stored_event = FactLineageEventV1::new(
        profile_fact_id("operation.other"),
        FactOwnerV1::Profile,
        FactLineageEventKindV1::PayloadAccessChanged {
            previous: PayloadAccessState::Eligible,
            current: PayloadAccessState::Deleted,
        },
        UtcMicros(7),
        None,
    )
    .unwrap();
    connection
        .execute(
            "INSERT INTO memory_v2_lineage_events (
                    event_id, fact_id, owner_kind, project_id, event_json, occurred_at
                 ) VALUES (?1, ?2, 'profile', '', ?3, ?4)",
            params![
                stored_event.event_id().as_str(),
                requested_fact_id.as_str(),
                serde_json::to_string(&stored_event).unwrap(),
                stored_event.occurred_at().0,
            ],
        )
        .unwrap();
    let query = FactLineageQuery::new(FactOwnerV1::Profile, requested_fact_id, None, 10).unwrap();

    assert!(read_lineage(&connection, &query).is_err());
}

#[test]
fn referenced_anchor_availability_matches_row_at_a_time() {
    // The row-at-a-time predicate the batched `anchor_id IN (...)` load replaces.
    fn old_path(
        connection: &rusqlite::Connection,
        owner: &OwnerColumns,
        anchor_ids: &[RetrievalAnchorId],
    ) -> rusqlite::Result<()> {
        for anchor_id in anchor_ids {
            let exists = connection.query_row(
                "SELECT EXISTS(
                        SELECT 1 FROM retrieval_anchors
                        WHERE anchor_id = ?1 AND owner_json = ?2
                     )",
                params![anchor_id.as_str(), owner.json],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(invalid("fact references an unavailable retrieval anchor"));
            }
        }
        Ok(())
    }

    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(tracedecay_store::RETRIEVAL_ANCHORS_SCHEMA_DDL)
        .unwrap();
    let owner = OwnerColumns::new(&FactOwnerV1::Profile).unwrap();
    let other_owner_json = encode(&FactOwnerV1::Project {
        project_id: tracedecay_domain::ProjectId::new("project.other").unwrap(),
    })
    .unwrap();

    let insert_anchor = |anchor_id: &str, owner_json: &str| {
        connection
            .execute(
                "INSERT INTO retrieval_anchors (
                        anchor_id, anchor_json, owner_json, projection_generation
                     ) VALUES (?1, '{}', ?2, 'projection.fact')",
                params![anchor_id, owner_json],
            )
            .unwrap();
    };
    insert_anchor("retrieval.fact-executor.alpha", &owner.json);
    insert_anchor("retrieval.fact-executor.beta", &owner.json);
    // The same anchor id filed under a different owner is not available here.
    insert_anchor("retrieval.fact-executor.gamma", &other_owner_json);

    let anchor = |id: &str| RetrievalAnchorId::new(id).unwrap();
    let scenarios: Vec<Vec<RetrievalAnchorId>> = vec![
        vec![],
        vec![anchor("retrieval.fact-executor.alpha")],
        vec![
            anchor("retrieval.fact-executor.alpha"),
            anchor("retrieval.fact-executor.beta"),
        ],
        vec![
            anchor("retrieval.fact-executor.alpha"),
            anchor("retrieval.fact-executor.missing"),
        ],
        vec![anchor("retrieval.fact-executor.gamma")],
    ];
    for anchor_ids in scenarios {
        let batched = require_referenced_anchors_available(&connection, &owner, &anchor_ids)
            .map_err(|error| error.to_string());
        let reference =
            old_path(&connection, &owner, &anchor_ids).map_err(|error| error.to_string());
        assert_eq!(batched, reference, "outcome diverged for {anchor_ids:?}");
    }
}
