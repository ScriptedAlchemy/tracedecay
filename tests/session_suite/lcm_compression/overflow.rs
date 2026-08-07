use super::*;

#[tokio::test]
async fn compress_forces_overflow_recovery_with_reserve_derived_cap() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "forced summary".into(),
        },
        Some(10),
        None,
        None,
    );
    request.context_length = Some(150);
    request.reserve_tokens_floor = Some(50);

    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.reason, "forced_overflow_recovery");
    assert!(response.summary_nodes_created >= 1);
}

#[tokio::test]
async fn forced_overflow_summary_replay_keeps_latest_message_when_nothing_fits() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let huge_tail = "tool-output ".repeat(5_000);
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("user", "start"),
            ("assistant", huge_tail.as_str()),
            ("tool", huge_tail.as_str()),
            ("assistant", huge_tail.as_str()),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "summary too large for cap".into(),
        },
        Some(1),
        None,
        Some(1),
    );
    request.current_tokens = Some(20_000);
    request.fresh_tail_count = Some(2);

    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.status, "best_effort");
    assert_eq!(
        response.reason,
        "forced_overflow_recovery_replay_over_budget"
    );
    assert!(!response.replay_messages.is_empty());
    let latest = response.replay_messages.last().unwrap();
    assert_eq!(latest["role"], "assistant");
    assert_eq!(latest["content"], huge_tail);
}

#[tokio::test]
async fn forced_overflow_compresses_sub_threshold_backlog() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &["old-1", "old-2", "fresh-1", "fresh-2"],
    )
    .await;

    let response = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "forced summary".into(),
            },
            Some(10),
            None,
            Some(100),
        ))
        .await
        .unwrap();

    assert_eq!(response.reason, "forced_overflow_recovery");
    assert!(response.summary_nodes_created >= 1);
}

#[tokio::test]
async fn forced_overflow_recovery_compacts_additional_backlog_and_reports_reason() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let store_ids = insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "system policy anchor"),
            ("user", "old user one"),
            ("assistant", "old assistant one"),
            ("user", "old user two"),
            ("assistant", "old assistant two"),
            ("user", "fresh user"),
            ("assistant", "fresh assistant"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "forced overflow summary".into(),
        },
        Some(2),
        Some(1),
        Some(20),
    );
    request.current_tokens = Some(200);
    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "forced_overflow_recovery");
    assert_eq!(response.summary_nodes_created, 4);
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[4])
    );
    assert!(response.frontier.maintenance_debt.is_empty());
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "system policy anchor",
            "forced overflow summary",
            "forced overflow summary",
            "forced overflow summary",
            "forced overflow summary",
            "fresh user",
            "fresh assistant",
        ]
    );
    let mut expanded_sources = 0;
    for node in &response.summary_nodes {
        expanded_sources += db
            .lcm_expand_summary_node("cursor", "session-1", &node.node_id)
            .await
            .unwrap()
            .sources
            .len();
    }
    assert_eq!(expanded_sources, 4);
}

#[tokio::test]
async fn forced_overflow_triggers_at_configured_cap_and_catches_up_in_passes() {
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

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "catchup summary".into(),
        },
        Some(4),
        Some(2),
        Some(40),
    );
    request.current_tokens = Some(40);
    let response = db.lcm_compress(request).await.unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "forced_overflow_recovery");
    assert_eq!(response.summary_nodes_created, 2);
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[3])
    );
    assert!(response.frontier.maintenance_debt.is_empty());
}

#[tokio::test]
async fn forced_overflow_without_backlog_reports_irreducible_best_effort() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "system anchor words"),
            ("user", "fresh tail words that cannot be compacted"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "unused summary".into(),
        },
        None,
        None,
        Some(3),
    );
    request.current_tokens = Some(3);
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "best_effort");
    assert_eq!(response.reason, "irreducible_overflow_no_backlog");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(response_json["replay_over_budget"], true);
    assert!(response_json["replay_token_estimate"].as_i64().unwrap() > 3);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "system anchor words".to_string(),
            "fresh tail words that cannot be compacted".to_string()
        ]
    );
}

// Mirrors hermes-lcm `_assemble_context` budget enforcement: tail messages
// that do not fit under the assembly cap are dropped (newest kept first) and
// the summary block is budgeted, instead of returning over-cap replay.
#[tokio::test]
async fn forced_overflow_trims_replay_to_assembly_cap() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "system anchor words words"),
            ("user", "old backlog one"),
            ("assistant", "old backlog two"),
            ("user", "fresh tail words words"),
            ("assistant", "fresh assistant words words"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "small summary".into(),
        },
        None,
        None,
        Some(6),
    );
    request.current_tokens = Some(20);
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "forced_overflow_recovery");
    assert_eq!(response.summary_nodes_created, 1);
    assert_eq!(response_json["replay_over_budget"], false);
    assert!(response_json["replay_token_estimate"].as_i64().unwrap() <= 6);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![
            "system anchor words words".to_string(),
            "small summary".to_string(),
        ]
    );
}

#[tokio::test]
async fn forced_overflow_replay_budget_accounts_for_prompt_overhead_delta() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "policy anchor words"),
            ("user", "fresh tail words"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "unused summary".into(),
        },
        None,
        None,
        Some(12),
    );
    // Host-observed prompt tokens include local overhead beyond the message
    // token estimate; overflow recovery should tighten the assembly cap.
    request.current_tokens = Some(20);
    request.messages = vec![
        json!({ "role": "system", "content": "policy anchor words" }),
        json!({ "role": "user", "content": "fresh tail words" }),
    ];
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "best_effort");
    assert_eq!(
        response.reason,
        "forced_overflow_recovery_replay_over_budget"
    );
    assert_eq!(response_json["replay_over_budget"], true);
}

#[tokio::test]
async fn oversized_authoritative_summary_is_preserved_exactly() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "alpha beta gamma delta epsilon zeta eta theta",
            "iota kappa lambda mu nu xi omicron pi",
            "fresh-1",
            "fresh-2",
        ],
    )
    .await;

    let exact_summary = "oversized ".repeat(100);
    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: exact_summary.clone(),
            },
        ))
        .await
        .unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "compressed_backlog");
    let summary = &response.summary_nodes[0];
    assert_eq!(summary.summary_text, exact_summary);
    assert!(!response.fallback_used);
}

#[tokio::test]
async fn authoritative_summary_reports_no_fallback_attempt_state() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages(
        &db,
        "cursor",
        "session-1",
        &[
            "alpha beta gamma delta epsilon zeta eta theta",
            "iota kappa lambda mu nu xi omicron pi",
            "fresh-1",
            "fresh-2",
        ],
    )
    .await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "oversized ".repeat(100),
            },
        ))
        .await
        .unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "compressed_backlog");
    assert_eq!(response_json["compression_attempts"], 1);
    assert_eq!(response_json["fallback_used"], false);
    assert!(response_json["retry_status"].is_null());
    assert!(response.frontier.maintenance_debt.is_empty());
    assert_eq!(response_json["replay_over_budget"], false);
}

#[tokio::test]
async fn critical_pressure_catch_up_reports_attempts_debt_and_budget_state() {
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
            "old-5 token",
            "old-6 token",
            "fresh-1",
            "fresh-2",
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "catchup summary".into(),
        },
        Some(2),
        Some(1),
        Some(3),
    );
    request.current_tokens = Some(40);
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "forced_overflow_recovery");
    assert_eq!(response.summary_nodes_created, 4);
    assert_eq!(response_json["compression_attempts"], 4);
    assert_eq!(response_json["fallback_used"], false);
    assert_eq!(
        response_json["retry_status"].as_str(),
        Some("critical_pressure_catch_up")
    );
    assert_eq!(
        response.frontier.current_frontier_store_id,
        Some(store_ids[3])
    );
    assert_eq!(
        response.frontier.maintenance_debt,
        vec![LcmMaintenanceDebt::RawBacklog {
            from_store_id: store_ids[4],
            to_store_id: store_ids[5],
        }]
    );
    // Budget enforcement keeps the freshest tail and drops over-cap summary
    // blocks and deferred backlog from active replay; they stay recoverable
    // through the DAG and maintenance debt.
    assert_eq!(response_json["replay_over_budget"], false);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["fresh-1".to_string(), "fresh-2".to_string()]
    );
}

#[tokio::test]
async fn maintenance_debt_clears_when_retry_compacts_remaining_backlog() {
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
                summary_text: "first retry summary".into(),
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

    let second = db
        .lcm_compress(limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "second retry summary".into(),
            },
            Some(4),
            Some(2),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(second.status, "ok");
    assert_eq!(second.reason, "compressed_backlog");
    assert_eq!(
        second.frontier.current_frontier_store_id,
        Some(store_ids[3])
    );
    assert!(second.frontier.maintenance_debt.is_empty());
}

#[tokio::test]
async fn compression_reinjects_latest_user_objective_when_tail_is_tool_heavy() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "policy anchor"),
            ("user", "Ship OAuth login and preserve this objective."),
            ("assistant", "acknowledged"),
            ("tool", "first tool result payload"),
            ("assistant", "working on intermediate steps"),
            ("tool", "latest tool result payload"),
        ],
    )
    .await;

    let response = db
        .lcm_compress(compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "historical summary".into(),
            },
        ))
        .await
        .unwrap();

    let replay_contents = response
        .replay_messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>();
    assert!(replay_contents.iter().any(|content| {
        content.contains("[Current user objective preserved from compacted history]")
    }));
    assert!(
        replay_contents
            .iter()
            .any(|content| content.contains("Ship OAuth login and preserve this objective."))
    );
}

#[tokio::test]
async fn overflow_recovery_keeps_preserved_objective_scaffold_when_evicting_tail() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "policy anchor stays"),
            (
                "assistant",
                "bulky derived assistant turn with many filler words that should be evicted",
            ),
            (
                "assistant",
                "[Current user objective preserved from compacted history]\nShip OAuth login now",
            ),
            ("user", "keep me"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "unused summary".into(),
        },
        None,
        None,
        Some(18),
    );
    request.current_tokens = Some(50);
    let response = db.lcm_compress(request).await.unwrap();
    let replay = response
        .replay_messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>();
    assert!(replay.iter().any(|content| {
        content.contains("[Current user objective preserved from compacted history]")
    }));
    assert!(replay.contains(&"keep me"));
    assert!(!replay.iter().any(|content| {
        content.contains("bulky derived assistant turn with many filler words")
    }));
}

// Mirrors hermes-lcm `_assemble_overflow_recovery_context`: with no backlog
// to compact, forced overflow evicts droppable assistant/tool tail turns that
// do not fit under the cap while keeping anchors and budgetable user intent.
#[tokio::test]
async fn overflow_recovery_without_backlog_evicts_droppable_tail() {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    insert_raw_messages_with_roles(
        &db,
        "cursor",
        "session-1",
        &[
            ("system", "policy anchor stays"),
            (
                "assistant",
                "bulky derived assistant turn with many filler words here",
            ),
            ("user", "keep me"),
        ],
    )
    .await;

    let mut request = limited_compress_request(
        "cursor",
        "session-1",
        LcmSummarizerMode::Fake {
            summary_text: "unused summary".into(),
        },
        None,
        None,
        Some(5),
    );
    request.current_tokens = Some(50);
    let response = db.lcm_compress(request).await.unwrap();
    let response_json = serde_json::to_value(&response).unwrap();

    assert_eq!(response.status, "ok");
    assert_eq!(response.reason, "overflow_recovery_no_backlog");
    assert_eq!(response.summary_nodes_created, 0);
    assert_eq!(response_json["replay_over_budget"], false);
    assert_eq!(
        response
            .replay_messages
            .iter()
            .map(|message| message["content"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec!["policy anchor stays".to_string(), "keep me".to_string()]
    );
}
