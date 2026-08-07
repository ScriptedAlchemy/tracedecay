//! Legacy fact-store, retrieval, grooming, and memory-status tests (moved verbatim from `memory_test`).

use super::*;

#[tokio::test]
async fn fact_relations_rewire_and_deduplicate_when_facts_merge() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let a = store
        .add_fact(
            fact_request("alpha relation fact", MemoryCategory::Project, 0.9),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    let b = store
        .add_fact(
            fact_request("beta relation fact", MemoryCategory::Project, 0.9),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    let c = store
        .add_fact(
            fact_request("gamma relation fact", MemoryCategory::Project, 0.9),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();

    store
        .upsert_fact_relation(
            a.fact_id,
            c.fact_id,
            FactRelationKind::Supports,
            0.6,
            "test",
            serde_json::json!({"evidence": "a"}),
        )
        .await
        .unwrap();
    store
        .upsert_fact_relation(
            b.fact_id,
            c.fact_id,
            FactRelationKind::Supports,
            0.8,
            "test",
            serde_json::json!({"evidence": "b"}),
        )
        .await
        .unwrap();

    store
        .merge_facts(a.fact_id, vec![b.fact_id], None)
        .await
        .unwrap();

    let relations = store.list_fact_relations(None).await.unwrap();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].source_fact_id, a.fact_id);
    assert_eq!(relations[0].target_fact_id, c.fact_id);
    assert_eq!(relations[0].relation, FactRelationKind::Supports);
    assert_eq!(relations[0].confidence, 0.8);
}

#[tokio::test]
async fn grooming_batch_prevalidates_before_mutating_and_rejects_conflicting_links() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let fact = store
        .add_fact(
            fact_request("batch validation fact", MemoryCategory::Project, 0.9),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    let operations = vec![
        MemoryGroomingOperation::NormalizeTags {
            fact_id: fact.fact_id,
            tags: vec![" Needs Cleanup ".to_string()],
            evidence_fact_ids: vec![fact.fact_id],
            confidence: 0.9,
        },
        MemoryGroomingOperation::LinkFacts {
            source_fact_id: fact.fact_id,
            target_fact_id: fact.fact_id,
            relation: FactRelationKind::Supports,
            evidence_fact_ids: vec![fact.fact_id],
            confidence: 0.9,
            source: "test".to_string(),
            metadata: serde_json::json!({}),
        },
    ];

    assert!(store.apply_grooming_batch(&operations, 0.5).await.is_err());
    assert!(
        store
            .get_fact(fact.fact_id)
            .await
            .unwrap()
            .unwrap()
            .tags
            .is_empty()
    );
    assert!(store.list_fact_relations(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn entity_grooming_rewires_links_supports_alias_retrieval_and_repairs_vectors() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let mut first_request =
        fact_request("TraceDecay owns graph memory", MemoryCategory::Project, 0.9);
    first_request.entities = vec!["TraceDecay".to_string()];
    let first = store
        .add_fact(first_request, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();
    let mut second_request =
        fact_request("MemoryGraph stores relations", MemoryCategory::Project, 0.9);
    second_request.entities = vec!["MemoryGraph".to_string()];
    let second = store
        .add_fact(second_request, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();
    let winner = entity_id(&db, "tracedecay").await;
    let loser = entity_id(&db, "memorygraph").await;
    drop(writer);
    execute_sql(
        &db,
        "UPDATE memory_facts SET hrr_vector = X'00', hrr_algebra = 'wrong', hrr_dim = 1,
                    hrr_precision = 'f64' WHERE fact_id = ?1",
        rusqlite::params![first.fact_id],
    );
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let report = store
        .apply_grooming_batch(
            &[
                MemoryGroomingOperation::MergeEntities {
                    winner_entity_id: winner,
                    loser_entity_ids: vec![loser],
                    evidence_fact_ids: vec![first.fact_id, second.fact_id],
                    confidence: 0.95,
                },
                MemoryGroomingOperation::AddAlias {
                    entity_id: winner,
                    alias: "TD".to_string(),
                    evidence_fact_ids: vec![first.fact_id],
                    confidence: 0.95,
                },
                MemoryGroomingOperation::RepairVector {
                    fact_id: first.fact_id,
                    evidence_fact_ids: vec![first.fact_id],
                    confidence: 1.0,
                },
            ],
            0.5,
        )
        .await
        .unwrap();

    assert_eq!(report.merged_entities, 1);
    assert_eq!(report.aliases_added, 1);
    assert_eq!(report.derived_repair.banks_rebuilt, 0);
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM memory_entities WHERE entity_id = 0"
        )
        .await,
        0
    );
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM memory_entities WHERE normalized_name = 'memorygraph'"
        )
        .await,
        0
    );
    let retriever = writer.retriever();
    let alias_hits = retriever.probe("TD", None, Some(0.0), 10).await.unwrap();
    assert!(
        alias_hits
            .iter()
            .any(|hit| hit.fact.fact_id == first.fact_id)
    );
    assert!(
        alias_hits
            .iter()
            .any(|hit| hit.fact.fact_id == second.fact_id)
    );
    let (algebra, dimension, precision, bytes): (String, i64, String, i64) =
        rusqlite::Connection::open_with_flags(
            db.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT hrr_algebra, hrr_dim, hrr_precision, length(hrr_vector)
             FROM memory_facts WHERE fact_id = ?1",
            rusqlite::params![first.fact_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(algebra, "amari_fhrr");
    assert_eq!(dimension, HolographicEncoder::DIMENSIONS as i64);
    assert_eq!(precision, HolographicEncoder::HRR_PRECISION);
    assert_eq!(bytes, HolographicEncoder::SERIALIZED_F32_BYTES as i64);
}

#[test]
fn core_memory_types_use_stable_json_strings() {
    assert_eq!(MemoryCategory::UserPref.to_string(), "user_pref");
    assert_eq!(
        "code_area".parse::<MemoryCategory>().unwrap(),
        MemoryCategory::CodeArea
    );
    assert!("tool_guidance".parse::<MemoryCategory>().is_err());
    assert_eq!(
        MemoryCategory::from_proposal_label("tool_guidance").unwrap(),
        MemoryCategory::Tool
    );
    assert_eq!(
        MemoryCategory::from_proposal_label("workflow preference").unwrap(),
        MemoryCategory::UserPref
    );

    let fact = FactRecord {
        fact_id: 42,
        content: "Prefer Rust-native memory".to_string(),
        category: MemoryCategory::Decision,
        tags: vec!["memory".to_string()],
        entities: vec!["Rust-native memory".to_string()],
        trust_score: 0.7,
        source: Some("test".to_string()),
        retrieval_count: 3,
        access_count: 2,
        helpful_count: 1,
        unhelpful_count: 0,
        created_at: 1,
        updated_at: 2,
        last_retrieved_at: Some(3),
        last_recalled_at: Some(5),
        last_feedback_at: Some(4),
        metadata: serde_json::json!({"scope": "core"}),
    };

    let json = serde_json::to_string(&fact).unwrap();
    assert!(json.contains(r#""fact_id":42"#));
    assert!(json.contains(r#""trust_score":0.7"#));
    assert!(!json.contains(r#""id":"#));
    assert!(!json.contains(r#""trust":"#));
    assert!(json.contains(r#""category":"decision""#));
    let round_trip: FactRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(round_trip, fact);
}

#[test]
fn memory_request_types_round_trip_through_json() {
    let add = AddFactRequest {
        content: "Use amari-holographic for fact vectors".to_string(),
        category: MemoryCategory::Project,
        source: Some("plan".to_string()),
        tags: vec!["hrr".to_string()],
        entities: vec!["amari-holographic".to_string()],
        trust: Some(0.8),
        metadata: serde_json::json!({"phase": "core"}),
    };
    let search = SearchFactsRequest {
        query: "fact vectors".to_string(),
        category: Some(MemoryCategory::Project),
        limit: Some(5),
        min_trust: Some(0.4),
        include_why: true,
    };
    let update = UpdateFactRequest {
        fact_id: 7,
        content: Some("Use deterministic fact vectors".to_string()),
        category: Some(MemoryCategory::Decision),
        tags: Some(vec!["reviewed".to_string()]),
        entities: Some(vec!["deterministic fact vectors".to_string()]),
        trust: Some(0.9),
        source: Some("review".to_string()),
        metadata: Some(serde_json::json!({"reviewed": true})),
    };
    let feedback = FeedbackRequest {
        fact_id: 7,
        action: FeedbackAction::Helpful,
        source: Some("test".to_string()),
        note: Some("matched project context".to_string()),
    };

    assert_eq!(
        serde_json::from_value::<AddFactRequest>(serde_json::to_value(add.clone()).unwrap())
            .unwrap(),
        add
    );
    assert_eq!(
        serde_json::from_value::<SearchFactsRequest>(serde_json::to_value(search.clone()).unwrap())
            .unwrap(),
        search
    );
    assert_eq!(
        serde_json::from_value::<UpdateFactRequest>(serde_json::to_value(update.clone()).unwrap())
            .unwrap(),
        update
    );
    assert_eq!(
        serde_json::from_value::<FeedbackRequest>(serde_json::to_value(feedback.clone()).unwrap())
            .unwrap(),
        feedback
    );
}

#[test]
fn trust_feedback_clamps_buckets_and_decays() {
    assert_eq!(clamp_trust(-0.2), 0.0);
    assert_eq!(clamp_trust(1.2), 1.0);
    assert!((apply_feedback(DEFAULT_TRUST, FeedbackAction::Helpful) - 0.55).abs() < f64::EPSILON);
    assert!((apply_feedback(DEFAULT_TRUST, FeedbackAction::Unhelpful) - 0.4).abs() < f64::EPSILON);
    assert_eq!(trust_bucket(0.2), "low");
    assert_eq!(trust_bucket(0.5), "medium");
    assert_eq!(trust_bucket(0.8), "high");
    assert_eq!(trust_distribution(&[0.2, 0.31, 0.6, 0.8]), (1, 2, 1));
}

#[test]
fn entity_extraction_finds_expected_patterns_and_dedupes() {
    let entities = extract_entities(
        r#"Project Phoenix uses "holographic memory" aka Amari Memory, also known as Fact Lens in src/memory/types.rs via HolographicEncoder::encode_fact and tracedecay_search. Project Phoenix keeps RustNative::Memory nearby."#,
    );

    assert_eq!(
        entities,
        vec![
            "Project Phoenix",
            "holographic memory",
            "Amari Memory",
            "Fact Lens",
            "src/memory/types.rs",
            "HolographicEncoder::encode_fact",
            "tracedecay_search",
            "RustNative::Memory",
        ]
    );
}

#[test]
fn entity_extraction_handles_alias_paths_tools_and_whitespace_edges() {
    assert_eq!(
        normalize_entity("  Project\tPhoenix\nCore  "),
        "Project Phoenix Core"
    );

    let entities = extract_entities(
        r#"Implement Project Phoenix AKA Firebird via src\memory\mod.rs and /etc/config. Then use TRACEDECAY-SEARCH with .gitignore. Project Phoenix appears again."#,
    );

    assert!(entities.contains(&"Project Phoenix".to_string()));
    assert!(entities.contains(&"Firebird".to_string()));
    assert!(entities.contains(&"src\\memory\\mod.rs".to_string()));
    assert!(entities.contains(&"/etc/config".to_string()));
    assert!(entities.contains(&".gitignore".to_string()));
    assert!(entities.contains(&"tracedecay_search".to_string()));
    assert_eq!(
        entities
            .iter()
            .filter(|entity| entity.eq_ignore_ascii_case("Project Phoenix"))
            .count(),
        1
    );
}

#[test]
fn holographic_encoding_is_deterministic_and_round_trips() {
    let encoder = HolographicEncoder;
    assert_eq!(HolographicEncoder::ROLE_CONTENT, "__hrr_role_content__");
    assert_eq!(HolographicEncoder::ROLE_ENTITY, "__hrr_role_entity__");
    assert_eq!(
        encoder.encode_text("Prefer Rust-native memory"),
        encoder.encode_text("Prefer Rust-native memory")
    );
    let first = encoder.encode_fact(
        "Prefer Rust-native memory",
        &["Project Phoenix".to_string()],
    );
    let same = encoder.encode_fact(
        "Prefer Rust-native memory",
        &["Project Phoenix".to_string()],
    );
    let different = encoder.encode_fact("Prefer Python memory", &["Project Phoenix".to_string()]);
    let reordered = encoder.encode_fact(
        "Prefer Rust-native memory",
        &["SQLite".to_string(), "Project Phoenix".to_string()],
    );
    let reordered_same = encoder.encode_fact(
        "Prefer Rust-native memory",
        &["Project Phoenix".to_string(), "SQLite".to_string()],
    );

    assert_eq!(first, same);
    assert_eq!(reordered, reordered_same);
    assert_eq!(first.len(), HolographicEncoder::DIMENSIONS);
    assert!(first.iter().all(|value| (-1.0..=1.0).contains(value)));
    assert!(encoder.similarity(&first, &same) > 0.999_999);
    assert!(encoder.similarity(&first, &different) < 0.95);
    assert_ne!(
        encoder.encode_text("Prefer Rust-native memory"),
        encoder.encode_fact("Prefer Rust-native memory", &[])
    );
    assert_eq!(
        encoder.encode_fact("Prefer Rust-native memory", &["SQLite".to_string()]),
        encoder.encode_fact("Prefer Rust-native memory", &["sqlite".to_string()])
    );
    assert_eq!(encoder.similarity(&[], &first), 0.0);

    let bytes = HolographicEncoder::serialize(&first).unwrap();
    assert_eq!(
        bytes.len(),
        8200,
        "2048-dimensional FHRR vectors must serialize as bincode Vec<f32> (8-byte length + 2048×4 bytes)"
    );
    let decoded = HolographicEncoder::deserialize(&bytes).unwrap();
    let max_abs_error = decoded
        .iter()
        .zip(&first)
        .map(|(decoded, baseline)| (decoded - baseline).abs())
        .fold(0.0_f64, f64::max);
    assert!(
        max_abs_error <= 3.0e-8,
        "f32 round-trip max_abs_error={max_abs_error:e} exceeded measured tolerance"
    );
    assert!(
        encoder.similarity(&decoded, &first) > 0.999_999_999,
        "holographic similarity should be preserved across f32 serialization"
    );
    assert!(HolographicEncoder::deserialize(b"not bincode").is_err());
}

#[test]
fn write_time_vector_similarity_uses_real_cosine_for_normalized_vectors() {
    let mut left = vec![0.0; HolographicEncoder::DIMENSIONS];
    let mut right = vec![0.0; HolographicEncoder::DIMENSIONS];
    left[0] = 1.0;
    right[1] = 1.0;

    let sim = vector_similarity(&left, &right);

    assert!(
        sim.abs() < f64::EPSILON,
        "expected orthogonal vectors to score near 0, got {sim}"
    );
}

#[test]
fn vector_deserialize_accepts_legacy_f64_blobs_for_forward_compatibility_and_backfill() {
    let encoder = HolographicEncoder;
    let baseline = encoder.encode_fact(
        "Legacy f64 vectors remain readable during precision backfill",
        &["ForwardCompatibility".to_string()],
    );
    let legacy_bytes = bincode::serialize(&baseline).unwrap();
    assert_eq!(legacy_bytes.len(), 16_392);

    let decoded = HolographicEncoder::deserialize(&legacy_bytes).unwrap();
    assert_eq!(decoded, baseline);
}

#[tokio::test]
async fn memory_store_add_list_get_and_deduplicates_by_content() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let mut request = fact_request(
        "Use SQLite-backed holographic memory",
        MemoryCategory::Decision,
        0.72,
    );
    request.tags = vec!["storage".to_string()];
    request.entities = vec!["SQLite".to_string()];

    let first = store
        .add_fact(request.clone(), DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();
    let duplicate = store
        .add_fact(request, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();

    assert_eq!(duplicate.fact_id, first.fact_id);
    assert_eq!(first.tags, vec!["storage"]);
    // "SQLite-backed" is auto-extracted from the content by the same verb-led
    // remainder rule that recovers "Tokio" from "Prefers Tokio" ("Use" is a
    // leading verb, so the remainder noun is kept as an entity).
    assert_eq!(first.entities, vec!["SQLite", "SQLite-backed"]);

    let fetched = store.get_fact(first.fact_id).await.unwrap().unwrap();
    assert_eq!(fetched, first);

    let listed = store
        .list_facts(Some(MemoryCategory::Decision), Some(0.7), 10)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].fact_id, first.fact_id);

    assert!(store.remove_fact(first.fact_id).await.unwrap());
    assert!(store.get_fact(first.fact_id).await.unwrap().is_none());
    assert!(!store.remove_fact(first.fact_id).await.unwrap());
}

#[tokio::test]
async fn memory_store_refreshes_vector_when_duplicate_add_merges_entities() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let encoder = HolographicEncoder;
    let content = "persist duplicate vector content";

    let mut first_request = fact_request(content, MemoryCategory::Project, 0.8);
    first_request.entities = vec!["FirstEntity".to_string()];
    let first = store
        .add_fact(first_request, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();
    assert_vector_matches_with_f32_tolerance(
        &fact_hrr_vector(&db, first.fact_id).await,
        &encoder.encode_fact(content, &["FirstEntity".to_string()]),
    );

    let mut duplicate_request = fact_request(content, MemoryCategory::Project, 0.8);
    duplicate_request.entities = vec!["SecondEntity".to_string()];
    let duplicate = store
        .add_fact(duplicate_request, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();

    assert_eq!(duplicate.fact_id, first.fact_id);
    assert!(duplicate.entities.contains(&"FirstEntity".to_string()));
    assert!(duplicate.entities.contains(&"SecondEntity".to_string()));
    assert_vector_matches_with_f32_tolerance(
        &fact_hrr_vector(&db, first.fact_id).await,
        &encoder.encode_fact(
            content,
            &["FirstEntity".to_string(), "SecondEntity".to_string()],
        ),
    );
}

#[tokio::test]
async fn memory_store_links_explicit_and_extracted_entities_and_updates_fields() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let mut request = fact_request(
        r#"Project Phoenix stores facts in src/memory/store.rs via HolographicEncoder::encode_fact"#,
        MemoryCategory::Project,
        0.6,
    );
    request.entities = vec!["Manual Entity".to_string(), "Project Phoenix".to_string()];

    let fact = store
        .add_fact(request, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();
    assert!(fact.entities.contains(&"Manual Entity".to_string()));
    assert!(fact.entities.contains(&"Project Phoenix".to_string()));
    assert!(fact.entities.contains(&"src/memory/store.rs".to_string()));
    assert!(
        fact.entities
            .contains(&"HolographicEncoder::encode_fact".to_string())
    );

    let updated = store
        .update_fact(UpdateFactRequest {
            fact_id: fact.fact_id,
            content: Some("Use deterministic HRR banks for Project Phoenix".to_string()),
            category: Some(MemoryCategory::Decision),
            tags: Some(vec!["updated".to_string()]),
            entities: Some(vec!["Project Phoenix".to_string(), "HRR banks".to_string()]),
            trust: Some(0.88),
            source: Some("review".to_string()),
            metadata: Some(serde_json::json!({"reviewed": true})),
        })
        .await
        .unwrap();

    assert_eq!(updated.category, MemoryCategory::Decision);
    assert_eq!(updated.tags, vec!["updated"]);
    assert_eq!(updated.source.as_deref(), Some("review"));
    assert!((updated.trust_score - 0.88).abs() < f64::EPSILON);
    assert_eq!(updated.metadata, serde_json::json!({"reviewed": true}));
    assert!(updated.entities.contains(&"HRR banks".to_string()));
}

#[tokio::test]
async fn memory_store_rejects_secret_like_fact_updates_without_mutating() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let fact = store
        .add_fact(
            fact_request(
                "Store only non-secret project preferences",
                MemoryCategory::Project,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();

    let attempted =
        "Do not persist this api_key=sk-test-742913 value in project memory".to_string();
    let err = store
        .update_fact(UpdateFactRequest {
            fact_id: fact.fact_id,
            content: Some(attempted.clone()),
            category: None,
            tags: None,
            entities: None,
            trust: None,
            source: None,
            metadata: None,
        })
        .await
        .expect_err("secret-like update should be rejected");
    let err = err.to_string();
    assert!(err.contains("rejected_secret_like"), "{err}");

    let unchanged = store.get_fact(fact.fact_id).await.unwrap().unwrap();
    assert_eq!(unchanged.content, fact.content);
    assert!(!unchanged.content.contains("sk-test-742913"));

    let (op, row_fact_id, detail): (String, i64, String) = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT op, fact_id, detail_json FROM memory_oplog ORDER BY id DESC LIMIT 1",
        (),
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap();
    assert_eq!(op, "reject_secret_like");
    assert_eq!(row_fact_id, fact.fact_id);
    assert!(detail.contains("content_hash"), "{detail}");
    assert!(detail.contains("reason"), "{detail}");
    assert!(
        !detail.contains("sk-test-742913") && !detail.contains("api_key"),
        "reject oplog must not leak attempted secret content: {detail}"
    );
}

#[tokio::test]
async fn memory_store_persists_vectors_and_repairs_missing_vectors() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let fact = store
        .add_fact(
            fact_request(
                "Persist an HRR vector for each fact",
                MemoryCategory::Project,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    let fact_without_vector = store
        .add_fact(
            fact_request(
                "A second fact keeps the recompute count falsifiable",
                MemoryCategory::Project,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();

    let vector_len: i64 = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT length(hrr_vector) FROM memory_facts WHERE fact_id = ?1",
        rusqlite::params![fact.fact_id],
        |row| row.get(0),
    )
    .unwrap();
    assert_eq!(vector_len, 8200);

    drop(writer);
    execute_sql(
        &db,
        "UPDATE memory_facts SET hrr_vector = NULL, hrr_dim = 8 WHERE fact_id = ?1",
        rusqlite::params![fact.fact_id],
    );
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    assert_eq!(store.compute_missing_vectors(10).await.unwrap(), 1);
    assert_eq!(store.compute_missing_vectors(10).await.unwrap(), 0);
    let hrr_dim: i64 = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT hrr_dim FROM memory_facts WHERE fact_id = ?1",
        rusqlite::params![fact.fact_id],
        |row| row.get(0),
    )
    .unwrap();
    assert_eq!(hrr_dim, HolographicEncoder::DIMENSIONS as i64);
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM memory_facts WHERE hrr_precision = 'f32' AND length(hrr_vector) = 8200",
        )
        .await,
        2,
        "fresh and recomputed vectors should be marked as f32 precision with compact blobs"
    );

}

#[tokio::test]
async fn compute_missing_vectors_backfills_legacy_f64_precision_to_f32() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let fact = store
        .add_fact(
            fact_request(
                "Legacy vector precision should be repaired without changing recall ordering",
                MemoryCategory::Project,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    let baseline_vector = fact_hrr_vector(&db, fact.fact_id).await;
    let legacy_bytes = bincode::serialize(&baseline_vector).unwrap();
    assert_eq!(legacy_bytes.len(), 16_392);

    drop(writer);
    execute_sql(
        &db,
        "UPDATE memory_facts
             SET hrr_vector = ?1, hrr_precision = 'f64'
             WHERE fact_id = ?2",
        rusqlite::params![legacy_bytes, fact.fact_id],
    );
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    assert_eq!(store.compute_missing_vectors(10).await.unwrap(), 1);
    assert_eq!(store.compute_missing_vectors(10).await.unwrap(), 0);
    assert_eq!(fact_hrr_blob(&db, fact.fact_id).await.len(), 8200);
    assert_eq!(
        scalar_i64(
            &db,
            "SELECT COUNT(*) FROM memory_facts WHERE hrr_precision = 'f32' AND length(hrr_vector) = 8200",
        )
        .await,
        1
    );

    let compact_vector = fact_hrr_vector(&db, fact.fact_id).await;
    let similarity = HolographicEncoder.similarity(&baseline_vector, &compact_vector);
    assert!(
        similarity > 0.999_999_999,
        "legacy f64→f32 backfill should preserve phase-cosine ordering; similarity={similarity}"
    );
}

#[tokio::test]
async fn remove_fact_defers_vacuum_while_peer_connections_are_live() {
    let (db, tmp) = make_memory_store().await;
    let db_path = tmp.path().join("tracedecay.db");
    let mut fact_ids = Vec::new();
    let vector = HolographicEncoder::serialize(&vec![0.0; HolographicEncoder::DIMENSIONS])
        .expect("serialize compact HRR vector");
    assert_eq!(vector.len(), HolographicEncoder::SERIALIZED_F32_BYTES);

    for idx in 0..48 {
        let fact_id = 50_000 + idx;
        execute_sql(
            &db,
            "INSERT INTO memory_facts (
                    fact_id, content, category, tags, trust_score, created_at,
                    updated_at, source, metadata, hrr_vector, hrr_algebra, hrr_dim, hrr_precision
                 )
                 VALUES (?1, ?2, ?3, '[]', ?4, ?5, ?5, ?6, '{}', ?7, ?8, ?9, ?10)",
            rusqlite::params![
                fact_id,
                format!("incremental vacuum blob reclamation fixture {idx}"),
                MemoryCategory::Project.as_str(),
                0.8_f64,
                1_900_000_000_i64 + idx,
                "test",
                vector.clone(),
                "amari_fhrr",
                HolographicEncoder::DIMENSIONS as i64,
                HolographicEncoder::HRR_PRECISION,
            ],
        );
        fact_ids.push(fact_id);
    }
    db.checkpoint().await.unwrap();
    let size_after_insert = std::fs::metadata(&db_path).unwrap().len();
    let (peer, _) = crate::common::open_test_database(&db_path).await.unwrap();
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    for fact_id in fact_ids {
        assert!(store.remove_fact(fact_id).await.unwrap());
    }
    drop(writer);
    db.checkpoint().await.unwrap();
    let size_after_delete = std::fs::metadata(&db_path).unwrap().len();

    assert!(
        scalar_i64(&db, "PRAGMA freelist_count").await > 0,
        "fact deletion must leave page reclamation for exclusive maintenance"
    );
    assert_eq!(
        scalar_i64(&peer, "SELECT COUNT(*) FROM memory_facts").await,
        0,
        "a peer opened before deletion must remain usable"
    );
    let (fresh, _) = crate::common::open_test_database(&db_path).await.unwrap();
    assert_eq!(
        scalar_i64(&fresh, "SELECT COUNT(*) FROM memory_facts").await,
        0,
        "a fresh peer must open after repeated fact deletion"
    );
    assert!(
        size_after_delete >= size_after_insert,
        "online deletion must not compact a live peer store; before={size_after_insert}, after={size_after_delete}"
    );
}

#[tokio::test]
async fn memory_store_records_feedback_audit_and_retrieval_counts() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let fact = store
        .add_fact(
            fact_request(
                "Feedback adjusts trust with an audit trail",
                MemoryCategory::General,
                0.5,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    let other_fact = store
        .add_fact(
            fact_request(
                "Batch retrieval count updates preserve duplicate IDs",
                MemoryCategory::General,
                0.5,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();

    store
        .increment_retrieval_counts(&[fact.fact_id, other_fact.fact_id, fact.fact_id])
        .await
        .unwrap();
    let retrieved = store.get_fact(fact.fact_id).await.unwrap().unwrap();
    assert_eq!(retrieved.retrieval_count, 2);
    assert!(retrieved.last_retrieved_at.is_some());
    assert_eq!(
        retrieved.updated_at, fact.updated_at,
        "retrieval is a read event and must not change updated_at ordering"
    );
    let other_retrieved = store.get_fact(other_fact.fact_id).await.unwrap().unwrap();
    assert_eq!(other_retrieved.retrieval_count, 1);
    assert!(other_retrieved.last_retrieved_at.is_some());

    let helpful = store
        .record_feedback_event(FeedbackRequest {
            fact_id: fact.fact_id,
            action: FeedbackAction::Helpful,
            source: Some("test".to_string()),
            note: Some("useful".to_string()),
        })
        .await
        .unwrap();
    assert!(helpful.event_id > 0);
    assert_eq!(helpful.fact_id, fact.fact_id);
    assert_eq!(helpful.action, FeedbackAction::Helpful);
    assert!((helpful.old_trust - 0.5).abs() < f64::EPSILON);
    assert!((helpful.new_trust - 0.55).abs() < f64::EPSILON);
    assert!((helpful.trust_delta - 0.05).abs() < f64::EPSILON);
    assert_eq!(helpful.helpful_count, 1);
    assert_eq!(helpful.unhelpful_count, 0);

    let unhelpful = store
        .record_feedback_event(FeedbackRequest {
            fact_id: fact.fact_id,
            action: FeedbackAction::Unhelpful,
            source: None,
            note: None,
        })
        .await
        .unwrap();
    assert!((unhelpful.old_trust - 0.55).abs() < f64::EPSILON);
    assert!((unhelpful.new_trust - 0.45).abs() < f64::EPSILON);
    assert_eq!(unhelpful.helpful_count, 1);
    assert_eq!(unhelpful.unhelpful_count, 1);

    let updated = store.get_fact(fact.fact_id).await.unwrap().unwrap();
    assert_eq!(updated.helpful_count, 1);
    assert_eq!(updated.unhelpful_count, 1);
    assert!(updated.last_feedback_at.is_some());

    let trust_history = store.fact_trust_history(fact.fact_id).await.unwrap();
    assert_eq!(trust_history.len(), 2);
    assert_eq!(trust_history[0].action, FeedbackAction::Helpful);
    assert!((trust_history[0].old_trust - 0.5).abs() < f64::EPSILON);
    assert!((trust_history[0].new_trust - 0.55).abs() < f64::EPSILON);
    assert!((trust_history[0].delta - 0.05).abs() < f64::EPSILON);
    assert_eq!(trust_history[0].source, "test");
    assert_eq!(trust_history[0].note.as_deref(), Some("useful"));
    assert_eq!(trust_history[1].action, FeedbackAction::Unhelpful);
    assert!((trust_history[1].old_trust - 0.55).abs() < f64::EPSILON);
    assert!((trust_history[1].new_trust - 0.45).abs() < f64::EPSILON);
    assert!((trust_history[1].delta + 0.10).abs() < f64::EPSILON);
    assert_eq!(trust_history[1].source, "mcp");
    assert_eq!(trust_history[1].note, None);

    let empty_history = store.fact_trust_history(other_fact.fact_id).await.unwrap();
    assert!(empty_history.is_empty());
}

#[tokio::test]
async fn memory_status_reports_exact_bucket_and_feedback_counts() {
    let (_tmp, cg) = make_project().await;
    let trusts = [0.24, 0.25, 0.50, 0.75];
    let mut fact_ids = Vec::new();
    for trust in trusts {
        let fact = cg
            .add_fact(AddFactRequest {
                content: format!("bucket fact {trust}"),
                category: MemoryCategory::General,
                source: Some("test".to_string()),
                tags: Vec::new(),
                entities: Vec::new(),
                trust: Some(trust),
                metadata: serde_json::json!({}),
            })
            .await
            .unwrap()
            .fact
            .unwrap();
        fact_ids.push(fact.fact_id);
    }

    cg.record_fact_feedback(FeedbackRequest {
        fact_id: fact_ids[1],
        action: FeedbackAction::Helpful,
        source: Some("test".to_string()),
        note: None,
    })
    .await
    .unwrap();
    cg.record_fact_feedback(FeedbackRequest {
        fact_id: fact_ids[2],
        action: FeedbackAction::Unhelpful,
        source: Some("test".to_string()),
        note: None,
    })
    .await
    .unwrap();

    let status = cg.memory_status().await.unwrap();
    assert_eq!(status.fact_count, 4);
    assert_eq!(status.trust_0_025_count, 1);
    assert_eq!(status.trust_025_050_count, 2);
    assert_eq!(status.trust_050_075_count, 0);
    assert_eq!(status.trust_075_100_count, 1);
    assert_eq!(status.below_default_recall_threshold_count, 1);
    assert_eq!(status.helpful_count, 1);
    assert_eq!(status.unhelpful_count, 1);
    assert_eq!(status.missing_vector_count, 0);

    // Feedback funnel: two facts were rated (helpful + unhelpful), none were
    // ever retrieved via search/probe in this test, so the funnel reports
    // rated facts with no "seen" activity behind them.
    assert_eq!(status.feedback_funnel.rated_fact_count, 2);
    assert_eq!(status.feedback_funnel.feedback_total, 2);
    assert_eq!(status.feedback_funnel.retrieval_count_total, 0);
    assert_eq!(status.feedback_funnel.access_count_total, 0);
    assert_eq!(status.feedback_funnel.retrieved_fact_count, 0);
    assert_eq!(status.feedback_funnel.seen_to_feedback_ratio, Some(0));
}

#[tokio::test]
async fn memory_status_feedback_funnel_tracks_retrieval_and_ratio() {
    let (_tmp, cg) = make_project().await;
    let fact = cg
        .add_fact(AddFactRequest {
            content: "Funnel fact retrieved via recall search".to_string(),
            category: MemoryCategory::General,
            source: Some("test".to_string()),
            tags: Vec::new(),
            entities: Vec::new(),
            trust: Some(0.6),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap()
        .fact
        .unwrap();

    // A tracked search bumps retrieval_count (search_facts) without any
    // feedback being recorded yet: the funnel should show activity "seen"
    // with a dead (None) seen:feedback ratio — nothing has been rated.
    cg.search_facts(SearchFactsRequest {
        query: "funnel fact retrieved".to_string(),
        category: None,
        limit: Some(5),
        min_trust: Some(0.0),
        include_why: false,
    })
    .await
    .unwrap();

    let before_feedback = cg.memory_status().await.unwrap();
    assert!(before_feedback.feedback_funnel.retrieval_count_total >= 1);
    assert_eq!(before_feedback.feedback_funnel.retrieved_fact_count, 1);
    assert_eq!(before_feedback.feedback_funnel.rated_fact_count, 0);
    assert_eq!(before_feedback.feedback_funnel.feedback_total, 0);
    assert_eq!(before_feedback.feedback_funnel.seen_to_feedback_ratio, None);

    cg.record_fact_feedback(FeedbackRequest {
        fact_id: fact.fact_id,
        action: FeedbackAction::Helpful,
        source: Some("test".to_string()),
        note: None,
    })
    .await
    .unwrap();

    let after_feedback = cg.memory_status().await.unwrap();
    assert_eq!(after_feedback.feedback_funnel.rated_fact_count, 1);
    assert_eq!(after_feedback.feedback_funnel.feedback_total, 1);
    assert_eq!(
        after_feedback.feedback_funnel.seen_to_feedback_ratio,
        Some(
            after_feedback.feedback_funnel.retrieval_count_total
                + after_feedback.feedback_funnel.access_count_total
        )
    );
}

#[tokio::test]
async fn memory_status_handles_empty_fact_store() {
    let (_tmp, cg) = make_project().await;
    let status = cg.memory_status().await.unwrap();
    assert_eq!(status.fact_count, 0);
    assert_eq!(status.missing_vector_count, 0);
    assert_eq!(status.feedback_funnel.retrieval_count_total, 0);
    assert_eq!(status.feedback_funnel.rated_fact_count, 0);
    assert_eq!(status.feedback_funnel.feedback_total, 0);
    assert_eq!(status.feedback_funnel.seen_to_feedback_ratio, None);
}

#[tokio::test]
async fn list_facts_paginates_past_the_page_limit() {
    let (_tmp, cg) = make_project().await;
    for n in 0..3 {
        cg.add_fact(AddFactRequest {
            content: format!("Pagination fixture fact number {n} with distinct content"),
            category: MemoryCategory::Project,
            source: Some("test".to_string()),
            tags: Vec::new(),
            entities: Vec::new(),
            trust: Some(0.8),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    }

    // A page smaller than the fact count carries a resume cursor equal to its
    // last fact id (resume is exclusive-start). This exact shape previously
    // failed page-contract validation with "compatibility fact page cursor is
    // not canonical" on any store larger than one page.
    let page = cg.list_facts(None, None, 2).await.unwrap();
    assert_eq!(page.len(), 2);

    let all = cg.list_facts(None, None, 10).await.unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn memory_status_reports_backlog_and_explicit_repair_converges_it() {
    let (_tmp, cg) = make_project().await;
    let fact = cg
        .add_fact(AddFactRequest {
            content: "Repair missing derived vector before status reports".to_string(),
            category: MemoryCategory::Project,
            source: Some("test".to_string()),
            tags: Vec::new(),
            entities: Vec::new(),
            trust: Some(0.8),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap()
        .fact
        .unwrap();
    clear_fact_vector(&cg, fact.fact_id).await;

    // A status read is pure: it reports the live backlog, performs no repair,
    // and its repair counters (repairs performed by this request) stay zero.
    let status = cg.memory_status().await.unwrap();
    assert_eq!(status.missing_vector_count, 1);
    assert_eq!(status.repair.missing_vectors_repaired, 0);
    assert_eq!(status.repair.banks_rebuilt, 0);

    // Repair is owned by the explicit entry point, which returns its own
    // batch stats; the next status read reflects the converged backlog.
    let repair = cg.repair_project_memory_once().await.unwrap();
    assert_eq!(repair.missing_vectors_repaired(), 1);
    assert_eq!(repair.banks_rebuilt(), 0);

    let status = cg.memory_status().await.unwrap();
    assert_eq!(status.missing_vector_count, 0);
    assert_eq!(status.repair.missing_vectors_repaired, 0);
}

#[tokio::test]
async fn memory_status_repair_preserves_fact_updated_at() {
    let (_tmp, cg) = make_project().await;
    let fact = cg
        .add_fact(AddFactRequest {
            content: "Status repair must not bump fact updated_at".to_string(),
            category: MemoryCategory::Project,
            source: Some("test".to_string()),
            tags: Vec::new(),
            entities: Vec::new(),
            trust: Some(0.8),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap()
        .fact
        .unwrap();
    clear_fact_vector(&cg, fact.fact_id).await;
    set_fact_updated_at(&cg, fact.fact_id, 1000).await;

    let repair = cg.repair_project_memory_once().await.unwrap();
    assert_eq!(repair.missing_vectors_repaired(), 1);

    let status = cg.memory_status().await.unwrap();
    assert_eq!(status.missing_vector_count, 0);

    assert_eq!(
        fact_updated_at(&cg, fact.fact_id).await,
        1000,
        "derived-vector repair must not change fact updated_at"
    );
}

#[tokio::test]
async fn fact_retriever_search_sanitizes_fts_chars_and_trust_weights_ordering() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let retriever = writer.retriever();

    store
        .add_fact(
            fact_request(
                "Rust HRR auth memory is preferred",
                MemoryCategory::Decision,
                0.9,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    store
        .add_fact(
            fact_request(
                "Rust HRR auth memory is experimental",
                MemoryCategory::Decision,
                0.2,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();

    let results = retriever
        .search(
            "Rust (HRR) + auth?",
            Some(MemoryCategory::Decision),
            None,
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(results[0].score > 0.0);
    assert!(results[0].fts_score >= 0.0);
    assert!(results[0].jaccard_score > 0.0);
    assert!(results[0].holographic_score >= 0.0);
    assert_eq!(results[0].trust_score, results[0].fact.trust_score);
    assert!(results[0].why.as_deref().unwrap_or("").contains("trust"));
    assert_eq!(results[0].fact.content, "Rust HRR auth memory is preferred");
}

#[tokio::test]
async fn fact_retriever_search_includes_old_entity_only_matches() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let mut matching = fact_request(
        "Older durable fact without the query words",
        MemoryCategory::Project,
        0.9,
    );
    matching.entities = vec!["EntityNeedle".to_string()];
    store
        .add_fact(matching, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();
    drop(writer);

    seed_newer_unrelated_memory_facts(
        &db,
        MemoryCategory::Project,
        "Newer unrelated project fact",
        "UnrelatedEntity",
        125,
    )
    .await;

    let writer = db.memory_writer().await.unwrap();
    let retriever = writer.retriever();
    let results = retriever
        .search("EntityNeedle", Some(MemoryCategory::Project), Some(0.3), 5)
        .await
        .unwrap();

    assert!(
        results
            .iter()
            .any(|result| result.fact.content == "Older durable fact without the query words"),
        "search should include facts found only through stored entities"
    );
}

#[tokio::test]
async fn fact_retriever_probe_related_reason_and_contradiction() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let retriever = writer.retriever();

    let mut first = fact_request(
        "Project Phoenix uses SQLite memory",
        MemoryCategory::Decision,
        0.8,
    );
    first.entities = vec!["Project Phoenix".to_string(), "SQLite".to_string()];
    store
        .add_fact(first, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();

    let mut second = fact_request(
        "Project Phoenix uses HRR banks",
        MemoryCategory::Decision,
        0.8,
    );
    second.entities = vec!["Project Phoenix".to_string(), "HRR banks".to_string()];
    store
        .add_fact(second, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();

    let mut third = fact_request(
        "Do not use SQLite memory for Project Phoenix",
        MemoryCategory::Decision,
        0.8,
    );
    third.entities = vec!["Project Phoenix".to_string(), "SQLite".to_string()];
    store
        .add_fact(third, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();

    let probe = retriever
        .probe("Project Phoenix", None, Some(0.0), 10)
        .await
        .unwrap();
    assert_eq!(probe.len(), 3);

    let related = retriever.related("Project Phoenix", 10).await.unwrap();
    let related_names: Vec<_> = related.into_iter().map(|entity| entity.name).collect();
    assert!(related_names.contains(&"SQLite".to_string()));
    assert!(related_names.contains(&"HRR banks".to_string()));

    let reason = retriever
        .reason(
            &["Project Phoenix".to_string(), "SQLite".to_string()],
            None,
            Some(0.0),
            10,
        )
        .await
        .unwrap();
    assert_eq!(reason.len(), 2);

    let contradictions = retriever
        .contradict(MemoryCategory::Decision, 0.2, 10)
        .await
        .unwrap();
    assert!(contradictions.iter().any(
        |result| result.existing_fact.content.contains("uses SQLite")
            && result.new_content.contains("Do not use SQLite")
    ));
}

#[tokio::test]
async fn fact_retriever_reason_applies_entity_predicates_before_limit() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let mut matching = fact_request(
        "Older fact links Project Phoenix and SQLite",
        MemoryCategory::Decision,
        0.9,
    );
    matching.entities = vec!["Project Phoenix".to_string(), "SQLite".to_string()];
    store
        .add_fact(matching, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();
    drop(writer);

    seed_newer_unrelated_memory_facts(
        &db,
        MemoryCategory::Decision,
        "Newer unrelated fact",
        "Unrelated",
        125,
    )
    .await;

    let writer = db.memory_writer().await.unwrap();
    let retriever = writer.retriever();
    let results = retriever
        .reason(
            &["Project Phoenix".to_string(), "SQLite".to_string()],
            Some(MemoryCategory::Decision),
            Some(0.3),
            10,
        )
        .await
        .unwrap();
    assert!(
        results
            .iter()
            .any(|result| result.fact.content.contains("Older fact links")),
        "reason should find matching facts before applying the result cap"
    );
}

#[tokio::test]
async fn fact_retriever_reason_deduplicates_entity_predicates() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();
    let retriever = writer.retriever();

    let mut request = fact_request(
        "Project Phoenix uses SQLite for memory search",
        MemoryCategory::Decision,
        0.9,
    );
    request.entities = vec!["Project Phoenix".to_string(), "SQLite".to_string()];
    let fact = store
        .add_fact(request, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();

    let results = retriever
        .reason(
            &[
                "Project Phoenix".to_string(),
                "project phoenix".to_string(),
                "SQLite".to_string(),
            ],
            Some(MemoryCategory::Decision),
            Some(0.3),
            10,
        )
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].fact.fact_id, fact.fact_id);
}

/// Policy: deleted memories are permanently hard-deleted. `remove_fact` (the
/// path behind dashboard curation and the MCP `fact_remove` tool) must leave
/// no trace on the store's own connection: the fact row, its FTS mirror, its
/// entity links, and its feedback events must all be gone, with the fact's
/// banks marked dirty for rebuild.
#[tokio::test]
async fn remove_fact_hard_deletes_fts_entity_links_and_feedback_events() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let mut request = fact_request(
        "hard delete cascade fixture fact",
        MemoryCategory::Project,
        0.8,
    );
    request.entities = vec!["CascadeEntity".to_string()];
    let fact = store
        .add_fact(request, DEFAULT_TRUST)
        .await
        .unwrap()
        .fact
        .unwrap();
    store
        .record_feedback_event(FeedbackRequest {
            fact_id: fact.fact_id,
            action: FeedbackAction::Helpful,
            source: None,
            note: Some("cascade fixture".to_string()),
        })
        .await
        .unwrap();

    async fn count(db: &Database, sql: &str, fact_id: i64) -> i64 {
        rusqlite::Connection::open_with_flags(
            db.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(sql, rusqlite::params![fact_id], |row| row.get(0))
        .unwrap()
    }

    let fts_sql = "SELECT COUNT(*) FROM memory_facts_fts WHERE rowid = ?1";
    let links_sql = "SELECT COUNT(*) FROM memory_fact_entities WHERE fact_id = ?1";
    let feedback_sql = "SELECT COUNT(*) FROM memory_feedback_events WHERE fact_id = ?1";
    assert_eq!(count(&db, fts_sql, fact.fact_id).await, 1);
    assert_eq!(count(&db, links_sql, fact.fact_id).await, 1);
    assert_eq!(count(&db, feedback_sql, fact.fact_id).await, 1);

    assert!(store.remove_fact(fact.fact_id).await.unwrap());

    assert!(store.get_fact(fact.fact_id).await.unwrap().is_none());
    assert_eq!(
        count(&db, fts_sql, fact.fact_id).await,
        0,
        "FTS delete trigger must remove the mirror row"
    );
    assert_eq!(
        count(&db, links_sql, fact.fact_id).await,
        0,
        "entity links must FK-cascade on fact delete"
    );
    assert_eq!(
        count(&db, feedback_sql, fact.fact_id).await,
        0,
        "feedback events must FK-cascade on fact delete"
    );
}

async fn scalar_count(db: &Database, sql: &str) -> i64 {
    scalar_i64(db, sql).await
}

#[tokio::test]
async fn search_facts_bump_access_count_only_for_returned_results() {
    let (_tmp, cg) = make_project().await;
    let outcome = cg
        .add_fact(AddFactRequest {
            content: "Phoenix cache invalidation must be explicit".to_string(),
            category: MemoryCategory::Project,
            source: Some("test".to_string()),
            tags: Vec::new(),
            entities: vec!["PhoenixCache".to_string()],
            trust: Some(0.9),
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    let fact = outcome.fact.expect("fact should be stored");
    assert_eq!(fact.access_count, 0);
    assert!(fact.last_recalled_at.is_none());

    // Probe and list scans bump retrieval_count but never access_count.
    cg.probe_entity("PhoenixCache", None, Some(0.0), 10)
        .await
        .unwrap();
    cg.list_facts(None, Some(0.0), 10).await.unwrap();
    let after_probe = cg.get_fact(fact.fact_id).await.unwrap().unwrap();
    assert!(after_probe.retrieval_count >= 2);
    assert_eq!(
        after_probe.access_count, 0,
        "probe/list scans must not bump access_count"
    );
    assert!(after_probe.last_recalled_at.is_none());

    // A recall search that RETURNS the fact bumps both access fields.
    let results = cg
        .search_facts(SearchFactsRequest {
            query: "phoenix cache invalidation".to_string(),
            category: None,
            limit: Some(5),
            min_trust: Some(0.0),
            include_why: false,
        })
        .await
        .unwrap();
    assert!(
        results
            .iter()
            .any(|result| result.fact.fact_id == fact.fact_id),
        "search should return the seeded fact"
    );
    let after_search = cg.get_fact(fact.fact_id).await.unwrap().unwrap();
    assert_eq!(after_search.access_count, 1);
    assert!(after_search.last_recalled_at.is_some());

    // A search whose results do NOT include the fact leaves it untouched.
    cg.search_facts(SearchFactsRequest {
        query: "completely unrelated quasar telemetry".to_string(),
        category: None,
        limit: Some(5),
        min_trust: Some(0.0),
        include_why: false,
    })
    .await
    .unwrap();
    let after_miss = cg.get_fact(fact.fact_id).await.unwrap().unwrap();
    assert_eq!(
        after_miss.access_count, 1,
        "non-returning searches must not bump access_count"
    );
}

#[tokio::test]
async fn add_fact_reports_near_duplicates_and_skips_normalized_equivalents() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let encoder = HolographicEncoder;
    let retriever = writer.retriever();
    let mut original_request = fact_request(
        "Use pnpm for installs in this repository",
        MemoryCategory::Tool,
        0.8,
    );
    original_request.entities = vec!["OldPackageManager".to_string()];
    let original = store
        .add_fact(original_request, DEFAULT_TRUST)
        .await
        .unwrap();
    assert_eq!(original.diff.diff, AddFactDiffKind::Add);
    let original_fact = original.fact.expect("original fact should be stored");
    let original_vector = fact_hrr_vector(&db, original_fact.fact_id).await;

    // Case/whitespace variant: near-exact AND content-normalized equivalent,
    // so the insert is skipped and the existing fact is returned.
    let mut normalized_duplicate = fact_request(
        "USE  PNPM   for installs in this Repository",
        MemoryCategory::Tool,
        0.8,
    );
    normalized_duplicate.entities = vec!["NewPackageManager".to_string()];
    normalized_duplicate.metadata = serde_json::json!({"source": "normalized"});
    let skipped = store
        .add_fact(normalized_duplicate, DEFAULT_TRUST)
        .await
        .unwrap();
    assert_eq!(skipped.diff.diff, AddFactDiffKind::NearDuplicate);
    assert_eq!(skipped.diff.closest_fact_id, Some(original_fact.fact_id));
    let skipped_fact = skipped.fact.expect("existing fact returned");
    assert_eq!(skipped_fact.fact_id, original_fact.fact_id);
    assert!(
        skipped_fact
            .entities
            .contains(&"NewPackageManager".to_string())
    );
    assert!(
        skipped_fact
            .entities
            .contains(&"OldPackageManager".to_string())
    );
    assert_eq!(
        skipped_fact.metadata,
        serde_json::json!({"source": "normalized"})
    );
    assert_eq!(
        scalar_count(&db, "SELECT COUNT(*) FROM memory_facts").await,
        1,
        "normalized-equivalent add must not insert a second row"
    );
    let updated_vector = fact_hrr_vector(&db, original_fact.fact_id).await;
    assert_ne!(
        updated_vector, original_vector,
        "normalized-equivalent entity merge must refresh the stored vector"
    );
    assert_vector_matches_with_f32_tolerance(
        &updated_vector,
        &encoder.encode_fact(&skipped_fact.content, &skipped_fact.entities),
    );

    let probe_results = retriever
        .probe(
            "NewPackageManager",
            Some(MemoryCategory::Tool),
            Some(0.3),
            5,
        )
        .await
        .unwrap();
    assert!(
        probe_results
            .iter()
            .any(|result| result.fact.fact_id == original_fact.fact_id),
        "probe by newly merged entity must find the existing fact"
    );
    let search_results = retriever
        .search(
            "NewPackageManager",
            Some(MemoryCategory::Tool),
            Some(0.3),
            5,
        )
        .await
        .unwrap();
    assert!(
        search_results
            .iter()
            .any(|result| result.fact.fact_id == original_fact.fact_id),
        "search by newly merged entity must find the existing fact"
    );

    // Near-exact but NOT normalized-equivalent (trailing punctuation):
    // conservative path inserts anyway and only reports.
    let reported = store
        .add_fact(
            fact_request(
                "Use pnpm for installs in this repository.",
                MemoryCategory::Tool,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap();
    assert_eq!(reported.diff.diff, AddFactDiffKind::NearDuplicate);
    assert_eq!(reported.diff.closest_fact_id, Some(original_fact.fact_id));
    let reported_fact = reported.fact.expect("near-duplicate is still stored");
    assert_ne!(reported_fact.fact_id, original_fact.fact_id);
    assert_eq!(
        scalar_count(&db, "SELECT COUNT(*) FROM memory_facts").await,
        2
    );
}

#[tokio::test]
async fn add_fact_flags_possible_conflict_on_negation_cues() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let original = store
        .add_fact(
            fact_request(
                "The project uses Redis for caching sessions and tokens",
                MemoryCategory::Decision,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap();
    let original_fact = original.fact.expect("original fact should be stored");

    let conflicted = store
        .add_fact(
            fact_request(
                "The project no longer uses Redis for caching sessions and tokens",
                MemoryCategory::Decision,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap();
    assert_eq!(conflicted.diff.diff, AddFactDiffKind::PossibleConflict);
    assert_eq!(conflicted.diff.closest_fact_id, Some(original_fact.fact_id));
    assert!(
        conflicted
            .diff
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("supersession")
    );
    // Conflicts are reported, never auto-resolved: both facts remain stored.
    assert!(conflicted.fact.is_some());
    assert_eq!(
        scalar_count(&db, "SELECT COUNT(*) FROM memory_facts").await,
        2
    );
}

#[tokio::test]
async fn add_fact_rejects_secret_like_content_without_storing() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let rejected = store
        .add_fact(
            fact_request(
                "Staging deploy uses api_key=TEST_ONLY_INVALID_CANARY for auth",
                MemoryCategory::Tool,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap();
    assert_eq!(rejected.diff.diff, AddFactDiffKind::RejectedSecretLike);
    assert!(rejected.fact.is_none(), "secret-like adds must not store");
    assert_eq!(
        scalar_count(&db, "SELECT COUNT(*) FROM memory_facts").await,
        0
    );

    // The rejection is auditable via a content-hash-only oplog row.
    let (op, detail): (String, String) = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT op, detail_json FROM memory_oplog ORDER BY id DESC LIMIT 1",
        (),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap();
    assert_eq!(op, "reject_secret_like");
    assert!(detail.contains("content_hash"));
    assert!(
        !detail.contains("Zx9mQ4tR7wLp2NvK8sBd1FgH"),
        "the oplog must never carry the rejected secret content"
    );
}

#[tokio::test]
async fn memory_oplog_records_mutations_with_hashes_not_content() {
    let (db, _tmp) = make_memory_store().await;
    let writer = db.memory_writer().await.unwrap();
    let store = writer.store();

    let fact = store
        .add_fact(
            fact_request(
                "Durable decision: keep curation hard-delete only",
                MemoryCategory::Decision,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap()
        .fact
        .unwrap();
    store
        .update_fact(UpdateFactRequest {
            fact_id: fact.fact_id,
            content: Some("Durable decision: curation stays hard-delete only".to_string()),
            category: None,
            tags: None,
            entities: None,
            trust: None,
            source: None,
            metadata: None,
        })
        .await
        .unwrap();
    store
        .record_feedback_event(FeedbackRequest {
            fact_id: fact.fact_id,
            action: FeedbackAction::Helpful,
            source: None,
            note: None,
        })
        .await
        .unwrap();
    assert!(store.remove_fact(fact.fact_id).await.unwrap());

    let conn = rusqlite::Connection::open_with_flags(
        db.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut statement = conn
        .prepare("SELECT op, fact_id, detail_json FROM memory_oplog ORDER BY id")
        .unwrap();
    let mut ops = Vec::new();
    let mut remove_detail = String::new();
    let rows = statement
        .query_map((), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap();
    for row in rows {
        let (op, row_fact_id, detail) = row.unwrap();
        assert_eq!(row_fact_id, Some(fact.fact_id));
        if op == "remove" {
            remove_detail = detail;
        }
        ops.push(op);
    }
    assert_eq!(ops, vec!["add", "update", "feedback", "remove"]);
    assert!(
        remove_detail.contains("content_hash"),
        "remove rows must record a content hash: {remove_detail}"
    );
    assert!(
        !remove_detail.contains("hard-delete only"),
        "remove rows must not carry the deleted content: {remove_detail}"
    );
}
