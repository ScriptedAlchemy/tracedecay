use super::*;

use tempfile::TempDir;

async fn assert_compress_baseline_case(case: CompressBaselineCase) {
    let tmp = TempDir::new().unwrap();
    let db = open_lcm_db(&tmp).await;
    let session_id = format!("baseline-{}", case.name());
    let case_name = case.name();

    match case {
        CompressBaselineCase::FrontierChanged => {
            let store_ids =
                insert_raw_messages(&db, "cursor", &session_id, &["one", "two", "three", "four"])
                    .await;
            db.lcm_update_lifecycle(LcmLifecycleUpdate {
                provider: "cursor".into(),
                conversation_id: session_id.clone(),
                current_session_id: session_id.clone(),
                current_frontier_store_id: Some(store_ids[0]),
                last_finalized_session_id: None,
                last_finalized_frontier_store_id: None,
                maintenance_debt: Vec::new(),
            })
            .await
            .unwrap();

            let mut request = compress_request(
                "cursor",
                &session_id,
                LcmSummarizerMode::Fake {
                    summary_text: "should not be written".into(),
                },
            );
            request.expected_current_frontier_store_id = Some(0);
            let response = db.lcm_compress(request).await.unwrap();

            assert_eq!(response.status, "ok", "{case_name}");
            assert_eq!(response.reason, "frontier_changed", "{case_name}");
            assert_eq!(response.summary_nodes_created, 0, "{case_name}");
            assert!(response.context_recovery_hint.is_none(), "{case_name}");
            assert!(response.summary_request.is_none(), "{case_name}");
            assert_eq!(
                response.frontier.current_frontier_store_id,
                Some(store_ids[0]),
                "{case_name}"
            );
            assert_eq!(
                response
                    .replay_messages
                    .iter()
                    .map(|message| message["content"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>(),
                vec!["three".to_string(), "four".to_string()],
                "{case_name}"
            );
            assert_eq!(
                db.lcm_status("cursor", Some(&session_id))
                    .await
                    .unwrap()
                    .summary_node_count,
                0,
                "{case_name}"
            );
        }
        CompressBaselineCase::BelowLeafThreshold => {
            insert_raw_messages(
                &db,
                "cursor",
                &session_id,
                &["old-1", "old-2", "fresh-1", "fresh-2"],
            )
            .await;
            let response = db
                .lcm_compress(limited_compress_request(
                    "cursor",
                    &session_id,
                    LcmSummarizerMode::Fake {
                        summary_text: "should not be written".into(),
                    },
                    Some(10),
                    None,
                    None,
                ))
                .await
                .unwrap();

            assert_eq!(response.status, "ok", "{case_name}");
            assert_eq!(
                response.reason, "backlog_below_leaf_chunk_threshold",
                "{case_name}"
            );
            assert_eq!(response.summary_nodes_created, 0, "{case_name}");
            assert!(response.context_recovery_hint.is_none(), "{case_name}");
            assert_eq!(
                response
                    .replay_messages
                    .iter()
                    .map(|message| message["content"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>(),
                vec![
                    "old-1".to_string(),
                    "old-2".to_string(),
                    "fresh-1".to_string(),
                    "fresh-2".to_string(),
                ],
                "{case_name}"
            );
            assert_eq!(
                response.frontier.current_frontier_store_id, None,
                "{case_name}"
            );
        }
        CompressBaselineCase::AuxiliarySummaryRequest => {
            let store_ids = insert_raw_messages(
                &db,
                "cursor",
                &session_id,
                &["old-1", "old-2", "fresh-1", "fresh-2"],
            )
            .await;
            let response = db
                .lcm_compress(compress_request(
                    "cursor",
                    &session_id,
                    LcmSummarizerMode::HermesAuxiliary,
                ))
                .await
                .unwrap();

            assert_eq!(response.status, "needs_summary", "{case_name}");
            assert_eq!(
                response.reason, "hermes_auxiliary_not_available",
                "{case_name}"
            );
            assert_eq!(response.summary_nodes_created, 0, "{case_name}");
            assert!(response.context_recovery_hint.is_none(), "{case_name}");
            let summary_request = response
                .summary_request
                .as_ref()
                .expect("auxiliary mode should return summary contract");
            assert_eq!(summary_request.source_range.from_store_id, store_ids[0]);
            assert_eq!(summary_request.source_range.to_store_id, store_ids[1]);
            assert_eq!(
                response
                    .replay_messages
                    .iter()
                    .map(|message| message["content"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>(),
                vec!["fresh-1".to_string(), "fresh-2".to_string()],
                "{case_name}"
            );
        }
        CompressBaselineCase::FakeSummaryWrite => {
            let store_ids = insert_raw_messages(
                &db,
                "cursor",
                &session_id,
                &["old-1", "old-2", "fresh-1", "fresh-2"],
            )
            .await;
            let response = db
                .lcm_compress(compress_request(
                    "cursor",
                    &session_id,
                    LcmSummarizerMode::Fake {
                        summary_text: "baseline summary".into(),
                    },
                ))
                .await
                .unwrap();

            assert_eq!(response.status, "ok", "{case_name}");
            assert_eq!(response.reason, "compressed_backlog", "{case_name}");
            assert_eq!(response.summary_nodes_created, 1, "{case_name}");
            let recovery_hint = response
                .context_recovery_hint
                .as_deref()
                .expect("compression should include a recovery hint");
            assert!(recovery_hint.contains("provider 'cursor'"), "{case_name}");
            assert!(
                recovery_hint.contains(&format!("session '{session_id}'")),
                "{case_name}"
            );
            assert!(
                recovery_hint.contains("tracedecay_lcm_expand_query"),
                "{case_name}"
            );
            assert_eq!(
                response.frontier.current_frontier_store_id,
                Some(store_ids[1]),
                "{case_name}"
            );
            assert_eq!(
                response
                    .replay_messages
                    .iter()
                    .map(|message| message["content"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>(),
                vec![
                    "baseline summary".to_string(),
                    "fresh-1".to_string(),
                    "fresh-2".to_string(),
                ],
                "{case_name}"
            );
            assert_eq!(
                db.lcm_status("cursor", Some(&session_id))
                    .await
                    .unwrap()
                    .summary_node_count,
                1,
                "{case_name}"
            );
            let expanded = db
                .lcm_expand_summary_node("cursor", &session_id, &response.summary_nodes[0].node_id)
                .await
                .unwrap();
            assert_eq!(
                expanded
                    .sources
                    .iter()
                    .map(|source| source.content.as_str())
                    .collect::<Vec<_>>(),
                vec!["old-1", "old-2"],
                "{case_name}"
            );
        }
    }
}

#[tokio::test]
async fn compress_frontier_changed_baseline_decision_fixture_preserves_contract() {
    assert_compress_baseline_case(CompressBaselineCase::FrontierChanged).await;
}

#[tokio::test]
async fn compress_below_leaf_threshold_baseline_decision_fixture_preserves_contract() {
    assert_compress_baseline_case(CompressBaselineCase::BelowLeafThreshold).await;
}

#[tokio::test]
async fn compress_auxiliary_summary_request_baseline_decision_fixture_preserves_contract() {
    assert_compress_baseline_case(CompressBaselineCase::AuxiliarySummaryRequest).await;
}

#[tokio::test]
async fn compress_fake_summary_write_baseline_decision_fixture_preserves_contract() {
    assert_compress_baseline_case(CompressBaselineCase::FakeSummaryWrite).await;
}

#[test]
fn compression_decision_seam_preserves_token_budget_contract() {
    assert_eq!(
        effective_assembly_token_cap(AssemblyCapInput {
            max_assembly_tokens: None,
            context_length: Some(32),
            reserve_tokens_floor: Some(40),
        }),
        None
    );

    let backlog = vec![lcm_raw_message(1, "assistant", "123456")];
    let frontier = lifecycle_state_with_debt(vec![LcmMaintenanceDebt::RawBacklog {
        from_store_id: 1,
        to_store_id: 1,
    }]);
    let mut forced_request = preflight_request(
        "cursor",
        "session-1",
        vec![json!({"role": "user", "content": "active"})],
        Some(12),
    );
    forced_request.max_assembly_tokens = Some(8);
    let decision = preflight_decision(PreflightDecisionInput {
        request: &forced_request,
        frontier: &frontier,
        backlog: &backlog,
    });
    assert!(decision.should_compress);
    assert_eq!(decision.reason, "forced_overflow_pressure");

    let plan = compression_plan(CompressionPlanInput {
        request: &limited_compress_request(
            "cursor",
            "session-1",
            LcmSummarizerMode::Fake {
                summary_text: "unused".into(),
            },
            Some(6),
            Some(1),
            Some(8),
        ),
        backlog: &[
            lcm_raw_message(1, "assistant", "1234"),
            lcm_raw_message(2, "assistant", "5678"),
        ],
    });
    assert!(plan.forced_overflow_recovery);
    assert_eq!(plan.leaf_chunk_tokens, Some(6));
    assert_eq!(plan.selected_backlog.len(), 1);
    assert_eq!(plan.selected_backlog[0].store_id, 1);

    assert_eq!(
        overflow_recovery_assembly_cap(OverflowRecoveryCapInput {
            current_tokens: Some(18),
            max_assembly_tokens: Some(10),
            messages: &[json!({"role": "user", "content": "tiny"})],
        }),
        Some(1)
    );
}
