use tempfile::TempDir;
use tokensave::db::Database;
use tokensave::memory::encoding::HolographicEncoder;
use tokensave::memory::entities::{extract_entities, normalize_entity};
use tokensave::memory::retrieval::FactRetriever;
use tokensave::memory::store::MemoryStore;
use tokensave::memory::trust::{
    apply_feedback, clamp_trust, temporal_decay, trust_bucket, trust_distribution, DEFAULT_TRUST,
};
use tokensave::memory::types::{
    AddFactRequest, FactRecord, FeedbackAction, FeedbackRequest, MemoryCategory,
    SearchFactsRequest, UpdateFactRequest,
};
use tokensave::tokensave::TokenSave;

async fn make_project() -> (TempDir, TokenSave) {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "pub fn hello() {}").unwrap();
    let cg = TokenSave::init(tmp.path()).await.unwrap();
    (tmp, cg)
}

async fn make_memory_store() -> (Database, TempDir) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("tokensave.db");
    let (db, _) = Database::initialize(&db_path).await.unwrap();
    (db, tmp)
}

fn fact_request(content: &str, category: MemoryCategory, trust: f64) -> AddFactRequest {
    AddFactRequest {
        content: content.to_string(),
        category,
        source: Some("test".to_string()),
        tags: Vec::new(),
        entities: Vec::new(),
        trust: Some(trust),
        metadata: serde_json::json!({}),
    }
}

#[test]
fn core_memory_types_use_stable_json_strings() {
    assert_eq!(MemoryCategory::UserPref.to_string(), "user_pref");
    assert_eq!(
        "code_area".parse::<MemoryCategory>().unwrap(),
        MemoryCategory::CodeArea
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
        helpful_count: 1,
        unhelpful_count: 0,
        created_at: 1,
        updated_at: 2,
        last_retrieved_at: Some(3),
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
    assert!(temporal_decay(0.9, 30.0) < 0.9);
    assert!(temporal_decay(0.1, 30.0) > 0.1);
}

#[test]
fn entity_extraction_finds_expected_patterns_and_dedupes() {
    let entities = extract_entities(
        r#"Project Phoenix uses "holographic memory" aka Amari Memory, also known as Fact Lens in src/memory/types.rs via HolographicEncoder::encode_fact and tokensave_search. Project Phoenix keeps RustNative::Memory nearby."#,
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
            "tokensave_search",
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
        r#"Implement Project Phoenix AKA Firebird via src\memory\mod.rs and /etc/config. Then use TOKENSAVE-SEARCH with .gitignore. Project Phoenix appears again."#,
    );

    assert!(entities.contains(&"Project Phoenix".to_string()));
    assert!(entities.contains(&"Firebird".to_string()));
    assert!(entities.contains(&"src\\memory\\mod.rs".to_string()));
    assert!(entities.contains(&"/etc/config".to_string()));
    assert!(entities.contains(&".gitignore".to_string()));
    assert!(entities.contains(&"tokensave_search".to_string()));
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
    let decoded = HolographicEncoder::deserialize(&bytes).unwrap();
    assert_eq!(decoded, first);
    assert!(HolographicEncoder::deserialize(b"not bincode").is_err());
}

#[tokio::test]
async fn memory_store_add_list_get_and_deduplicates_by_content() {
    let (db, _tmp) = make_memory_store().await;
    let store = MemoryStore::new(db.conn());

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
        .unwrap();
    let duplicate = store.add_fact(request, DEFAULT_TRUST).await.unwrap();

    assert_eq!(duplicate.fact_id, first.fact_id);
    assert_eq!(first.tags, vec!["storage"]);
    assert_eq!(first.entities, vec!["SQLite"]);

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
async fn memory_store_links_explicit_and_extracted_entities_and_updates_fields() {
    let (db, _tmp) = make_memory_store().await;
    let store = MemoryStore::new(db.conn());

    let mut request = fact_request(
        r#"Project Phoenix stores facts in src/memory/store.rs via HolographicEncoder::encode_fact"#,
        MemoryCategory::Project,
        0.6,
    );
    request.entities = vec!["Manual Entity".to_string(), "Project Phoenix".to_string()];

    let fact = store.add_fact(request, DEFAULT_TRUST).await.unwrap();
    assert!(fact.entities.contains(&"Manual Entity".to_string()));
    assert!(fact.entities.contains(&"Project Phoenix".to_string()));
    assert!(fact.entities.contains(&"src/memory/store.rs".to_string()));
    assert!(fact
        .entities
        .contains(&"HolographicEncoder::encode_fact".to_string()));

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
async fn memory_store_persists_vectors_and_rebuilds_missing_vectors_and_banks() {
    let (db, _tmp) = make_memory_store().await;
    let store = MemoryStore::new(db.conn());

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
        .unwrap();
    let fact_without_vector = store
        .add_fact(
            fact_request(
                "Bank rebuild still counts facts while skipping missing vectors",
                MemoryCategory::Project,
                0.8,
            ),
            DEFAULT_TRUST,
        )
        .await
        .unwrap();

    let mut rows = db
        .conn()
        .query(
            "SELECT length(hrr_vector) FROM memory_facts WHERE fact_id = ?1",
            libsql::params![fact.fact_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let vector_len: i64 = row.get(0).unwrap();
    assert!(vector_len > 0);

    db.conn()
        .execute(
            "UPDATE memory_facts SET hrr_vector = NULL, hrr_dim = 8 WHERE fact_id = ?1",
            libsql::params![fact.fact_id],
        )
        .await
        .unwrap();

    assert_eq!(store.compute_missing_vectors(10).await.unwrap(), 1);
    assert_eq!(store.compute_missing_vectors(10).await.unwrap(), 0);
    let mut rows = db
        .conn()
        .query(
            "SELECT hrr_dim FROM memory_facts WHERE fact_id = ?1",
            libsql::params![fact.fact_id],
        )
        .await
        .unwrap();
    let hrr_dim: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(hrr_dim, HolographicEncoder::DIMENSIONS as i64);

    db.conn()
        .execute(
            "UPDATE memory_facts SET hrr_vector = NULL WHERE fact_id = ?1",
            libsql::params![fact_without_vector.fact_id],
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .rebuild_bank("project", Some(MemoryCategory::Project))
            .await
            .unwrap(),
        2
    );
    assert!(store.rebuild_all_banks().await.unwrap() >= 1);
    store.remove_fact(fact.fact_id).await.unwrap();
    store
        .remove_fact(fact_without_vector.fact_id)
        .await
        .unwrap();
    assert_eq!(
        store
            .rebuild_bank("project", Some(MemoryCategory::Project))
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn memory_store_records_feedback_audit_and_retrieval_counts() {
    let (db, _tmp) = make_memory_store().await;
    let store = MemoryStore::new(db.conn());
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
}

#[tokio::test]
async fn memory_status_handles_empty_fact_store() {
    let (_tmp, cg) = make_project().await;
    let status = cg.memory_status().await.unwrap();
    assert_eq!(status.fact_count, 0);
    assert_eq!(status.missing_vector_count, 0);
}

#[tokio::test]
async fn fact_retriever_search_sanitizes_fts_chars_and_trust_weights_ordering() {
    let (db, _tmp) = make_memory_store().await;
    let store = MemoryStore::new(db.conn());
    let retriever = FactRetriever::new(db.conn());

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
async fn fact_retriever_probe_related_reason_and_contradiction() {
    let (db, _tmp) = make_memory_store().await;
    let store = MemoryStore::new(db.conn());
    let retriever = FactRetriever::new(db.conn());

    let mut first = fact_request(
        "Project Phoenix uses SQLite memory",
        MemoryCategory::Decision,
        0.8,
    );
    first.entities = vec!["Project Phoenix".to_string(), "SQLite".to_string()];
    store.add_fact(first, DEFAULT_TRUST).await.unwrap();

    let mut second = fact_request(
        "Project Phoenix uses HRR banks",
        MemoryCategory::Decision,
        0.8,
    );
    second.entities = vec!["Project Phoenix".to_string(), "HRR banks".to_string()];
    store.add_fact(second, DEFAULT_TRUST).await.unwrap();

    let mut third = fact_request(
        "Do not use SQLite memory for Project Phoenix",
        MemoryCategory::Decision,
        0.8,
    );
    third.entities = vec!["Project Phoenix".to_string(), "SQLite".to_string()];
    store.add_fact(third, DEFAULT_TRUST).await.unwrap();

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
    assert!(contradictions.iter().any(|result| result
        .existing_fact
        .content
        .contains("uses SQLite")
        && result.new_content.contains("Do not use SQLite")));
}

#[tokio::test]
async fn fact_retriever_reason_applies_entity_predicates_before_limit() {
    let (db, _tmp) = make_memory_store().await;
    let store = MemoryStore::new(db.conn());
    let retriever = FactRetriever::new(db.conn());

    let mut matching = fact_request(
        "Older fact links Project Phoenix and SQLite",
        MemoryCategory::Decision,
        0.9,
    );
    matching.entities = vec!["Project Phoenix".to_string(), "SQLite".to_string()];
    store.add_fact(matching, DEFAULT_TRUST).await.unwrap();

    for i in 0..125 {
        let mut unrelated = fact_request(
            &format!("Newer unrelated fact {i}"),
            MemoryCategory::Decision,
            0.9,
        );
        unrelated.entities = vec![format!("Unrelated {i}")];
        store.add_fact(unrelated, DEFAULT_TRUST).await.unwrap();
    }

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
