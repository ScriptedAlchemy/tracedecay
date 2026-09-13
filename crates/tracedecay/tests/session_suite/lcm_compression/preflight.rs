use super::*;

// Preflight is a read-only decision surface under the daemon-owned compaction
// authority: host-supplied messages are never ingested and the replay comes
// from the stored transcript only (ingest-protection rewriting moved to the
// compress/ingest path, retiring the old ingest_protection_changed_replay
// preflight reason).
#[tokio::test]
async fn preflight_is_read_only_and_never_ingests_active_messages() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_session(&db, "cursor", "session-1").await;

    let response = db
        .lcm_preflight(LcmPreflightRequest {
            provider: "cursor".into(),
            session_id: "session-1".into(),
            messages: vec![json!({
                "id": "protected-1",
                "role": "assistant",
                "content": format!("data:image/png;base64,{}", "A".repeat(100_000))
            })],
            current_tokens: Some(100),
            threshold_tokens: None,
            max_assembly_tokens: None,
            leaf_chunk_tokens: None,
            max_source_messages: None,
            summary_fan_in: None,
            incremental_max_depth: None,
            fresh_tail_count: None,
            dynamic_leaf_chunk_enabled: None,
            dynamic_leaf_chunk_max: None,
            context_length: None,
            reserve_tokens_floor: None,
            ignore_session_patterns: Vec::new(),
            stateless_session_patterns: Vec::new(),
        })
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert!(!response.should_compress);
    assert_eq!(response.reason, "no_compression_needed");
    assert!(response.replay_messages.is_empty());
    assert!(
        db.lcm_load_raw_message("cursor", "protected-1")
            .await
            .is_none()
    );
    assert_eq!(
        db.lcm_status("cursor", Some("session-1"))
            .await
            .unwrap()
            .raw_message_count,
        0
    );
}

/// How a decision case seeds its stored transcript.
#[derive(Clone, Copy)]
enum Seed {
    /// Assistant-role contents, seeded through `insert_raw_messages`.
    Contents(&'static [&'static str]),
    /// Explicit `(role, content)` pairs, seeded through
    /// `insert_raw_messages_with_roles`.
    Roles(&'static [(&'static str, &'static str)]),
}

/// One preflight decision: a seeded transcript plus the request overrides that
/// distinguish it, and the `(should_compress, reason)` the daemon must answer.
struct Case {
    name: &'static str,
    seed: Seed,
    current_tokens: i64,
    threshold_tokens: Option<i64>,
    leaf_chunk_tokens: Option<i64>,
    max_source_messages: Option<usize>,
    max_assembly_tokens: Option<i64>,
    context_length: Option<i64>,
    reserve_tokens_floor: Option<i64>,
    should_compress: bool,
    reason: &'static str,
}

/// Base case with every optional override cleared; each table row fills in only
/// the overrides that case is actually about.
const fn case(
    name: &'static str,
    seed: Seed,
    current_tokens: i64,
    should_compress: bool,
    reason: &'static str,
) -> Case {
    Case {
        name,
        seed,
        current_tokens,
        threshold_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        max_assembly_tokens: None,
        context_length: None,
        reserve_tokens_floor: None,
        should_compress,
        reason,
    }
}

/// The forced-overflow cases all share this two-message transcript: a system
/// anchor that cannot be evicted plus one fresh user turn.
const SYSTEM_ANCHOR_AND_FRESH_USER: &[(&str, &str)] =
    &[("system", "system anchor"), ("user", "fresh user")];

/// Threshold-backlog and forced-overflow decisions. The table is a fixed-size
/// array, so it can never iterate empty. Each case gets its own session id, so
/// the cases stay isolated the way the per-test databases used to isolate them,
/// while sharing one `TempDir` + `open_lcm_db` setup.
#[tokio::test]
async fn preflight_decides_compression_from_threshold_and_overflow_pressure() {
    let cases: [Case; 7] = [
        Case {
            threshold_tokens: Some(100),
            ..case(
                "over-threshold eligible backlog",
                Seed::Contents(&["old-1 token", "old-2 token", "fresh-1", "fresh-2"]),
                120,
                true,
                "threshold_backlog_ready",
            )
        },
        Case {
            threshold_tokens: Some(100),
            leaf_chunk_tokens: Some(10),
            max_source_messages: Some(2),
            ..case(
                "backlog below the leaf-chunk threshold skips compression",
                Seed::Contents(&["tiny", "fresh-1", "fresh-2"]),
                120,
                false,
                "threshold_no_eligible_backlog",
            )
        },
        Case {
            threshold_tokens: Some(100),
            leaf_chunk_tokens: Some(5),
            max_source_messages: Some(2),
            ..case(
                "threshold eligibility uses the full backlog despite the source-message cap",
                Seed::Contents(&["m1", "m2", "m3", "m4", "m5", "m6", "fresh-1", "fresh-2"]),
                120,
                true,
                "threshold_backlog_ready",
            )
        },
        Case {
            max_assembly_tokens: Some(50),
            ..case(
                "forced overflow from an explicit assembly cap, without replay change",
                Seed::Roles(SYSTEM_ANCHOR_AND_FRESH_USER),
                50,
                true,
                "forced_overflow_pressure",
            )
        },
        // Mirrors hermes-lcm `_effective_assembly_token_cap`: with no explicit
        // max_assembly_tokens, the assembly cap derives from
        // context_length - reserve_tokens_floor when both are positive.
        Case {
            context_length: Some(80),
            reserve_tokens_floor: Some(30),
            ..case(
                "forced-overflow cap derived from the context window reserve floor",
                Seed::Roles(SYSTEM_ANCHOR_AND_FRESH_USER),
                50,
                true,
                "forced_overflow_pressure",
            )
        },
        // Mirrors hermes-lcm: a reserve floor that consumes the whole context
        // window disables the reserve-based cap instead of clamping it to zero.
        Case {
            context_length: Some(30),
            reserve_tokens_floor: Some(30),
            ..case(
                "reserve floor without headroom disables the derived cap",
                Seed::Roles(SYSTEM_ANCHOR_AND_FRESH_USER),
                50,
                false,
                "no_compression_needed",
            )
        },
        // Mirrors hermes-lcm: when both an explicit max_assembly_tokens and a
        // reserve-derived cap apply, the effective cap is the minimum of the two.
        Case {
            max_assembly_tokens: Some(200),
            context_length: Some(80),
            reserve_tokens_floor: Some(30),
            ..case(
                "effective cap is the minimum of explicit and reserve-derived",
                Seed::Roles(SYSTEM_ANCHOR_AND_FRESH_USER),
                50,
                true,
                "forced_overflow_pressure",
            )
        },
    ];

    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;

    for (index, case) in cases.iter().enumerate() {
        let session_id = format!("session-{}", index + 1);
        match case.seed {
            Seed::Contents(contents) => {
                insert_raw_messages(&db, "cursor", &session_id, contents).await;
            }
            Seed::Roles(messages) => {
                insert_raw_messages_with_roles(&db, "cursor", &session_id, messages).await;
            }
        }

        let mut request =
            preflight_request("cursor", &session_id, Vec::new(), Some(case.current_tokens));
        request.threshold_tokens = case.threshold_tokens;
        request.leaf_chunk_tokens = case.leaf_chunk_tokens;
        request.max_source_messages = case.max_source_messages;
        request.max_assembly_tokens = case.max_assembly_tokens;
        request.context_length = case.context_length;
        request.reserve_tokens_floor = case.reserve_tokens_floor;

        let response = db.lcm_preflight(request).await.unwrap();

        assert_eq!(response.status, "ok", "{}", case.name);
        assert_eq!(
            response.should_compress, case.should_compress,
            "{}",
            case.name
        );
        assert_eq!(response.reason, case.reason, "{}", case.name);
    }
}

#[tokio::test]
async fn preflight_requests_compression_for_maintenance_debt() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "old-1 token",
            "old-2 token",
            "old-3 token",
            "old-4 token",
            "fresh-1",
            "fresh-2",
        ],
    )
    .await;
    let first = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "first chunk summary".into(),
            },
            Some(4),
            Some(2),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(
        first.frontier.maintenance_debt,
        vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[2],
            to_store_id: store_ids[3],
        }]
    );

    let response = db
        .lcm_preflight(preflight_request(
            "cursor",
            "session-1",
            Vec::new(),
            Some(10),
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert!(response.should_compress);
    assert_eq!(response.reason, "maintenance_debt_ready");
}
