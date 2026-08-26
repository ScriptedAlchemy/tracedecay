use super::*;

fn grep_request(query: &str) -> LcmGrepRequest {
    LcmGrepRequest {
        provider: "cursor".into(),
        query: query.into(),
        scope: LcmScope::All,
        session_id: None,
        include_summaries: false,
        limit: 10,
        sort: LcmGrepSort::Recency,
        source: None,
        role: None,
        start_time: None,
        end_time: None,
        git_filter: Default::default(),
    }
}

#[tokio::test]
async fn grep_all_provider_searches_raw_messages_across_providers() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "cursor", "cursor-session").await;
    insert_session(&db, "codex", "codex-session").await;
    assert!(
        db.upsert_session_message(&raw_message(
            "cursor",
            "cursor-cross-provider",
            "cursor-session",
            1,
            "Cross provider grep search should find cursor."
        ))
        .await
    );
    assert!(
        db.upsert_session_message(&raw_message(
            "codex",
            "codex-cross-provider",
            "codex-session",
            1,
            "Cross provider grep search should find codex."
        ))
        .await
    );

    let mut request = grep_request("cross provider grep search");
    request.provider = "all".into();
    let hits = db
        .lcm_grep_for_test(request)
        .await
        .expect("all-provider grep should succeed")
        .hits;
    let providers = hits
        .iter()
        .map(|hit| hit.provider.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(providers.contains("cursor"));
    assert!(providers.contains("codex"));
}

// Hermes scopes message FTS matches to the content column only
// (store.py:173-204 `build_message_fts_spec` indexes nothing but `content`).
// Role and metadata text must therefore never satisfy an unqualified grep.
#[tokio::test]
async fn grep_does_not_match_role_or_metadata_text() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "cursor", "session-fts-scope").await;
    let message = raw_message_with_role_source_timestamp(
        "cursor",
        "fts-scope-message",
        "session-fts-scope",
        1,
        "deploy pipeline ready",
        RawMessageContext {
            role: "assistant",
            source: "zephyrsource",
            timestamp: 1_715_000_001,
        },
    );
    assert!(db.upsert_session_message(&message).await);

    // Positive control: content terms still match through the FTS index.
    let content_hits = db
        .lcm_grep_for_test(grep_request("pipeline"))
        .await
        .expect("content grep should succeed")
        .hits;
    assert_eq!(content_hits.len(), 1);
    assert_eq!(
        content_hits[0].message_id.as_deref(),
        Some("fts-scope-message")
    );

    // Role text ("assistant") must not over-match the row.
    let role_hits = db
        .lcm_grep_for_test(grep_request("assistant"))
        .await
        .expect("role grep should succeed")
        .hits;
    assert!(
        role_hits.is_empty(),
        "role column text must not satisfy an unqualified grep: {role_hits:?}"
    );

    // Metadata text (the source marker) must not over-match the row either.
    let metadata_hits = db
        .lcm_grep_for_test(grep_request("zephyrsource"))
        .await
        .expect("metadata grep should succeed")
        .hits;
    assert!(
        metadata_hits.is_empty(),
        "metadata_json text must not satisfy an unqualified grep: {metadata_hits:?}"
    );
}

#[tokio::test]
async fn grep_downranks_transcript_inventory_below_substantive_hits() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    // Genuine implementation message in one session.
    insert_raw_messages(
        &db,
        "cursor",
        "impl-session",
        &["implemented branch redundancy scoring in the ranker".to_string()],
    )
    .await;
    // Inventory tool call in another session: it also matches the query but is
    // just a glob listing over transcript directories.
    insert_raw_messages(
        &db,
        "cursor",
        "review-session",
        &["Glob **/*.jsonl over .claude sessions for branch redundancy".to_string()],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "branch redundancy".into(),
            scope: LcmScope::All,
            session_id: None,
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Relevance,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
            git_filter: Default::default(),
        })
        .await
        .expect("grep should succeed")
        .hits;

    let impl_idx = hits.iter().position(|h| h.session_id == "impl-session");
    let inv_idx = hits.iter().position(|h| h.session_id == "review-session");
    assert!(
        impl_idx.is_some(),
        "substantive hit should be present: {hits:?}"
    );
    assert!(
        inv_idx.is_some(),
        "inventory hit should still be present: {hits:?}"
    );
    assert!(
        impl_idx < inv_idx,
        "substantive hit must rank above transcript inventory: {hits:?}"
    );
}

#[tokio::test]
async fn grep_caps_hits_per_session_in_cross_session_scope() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let flood: Vec<String> = (0..5)
        .map(|i| format!("apricot ledger note number {i}"))
        .collect();
    insert_raw_messages(&db, "cursor", "flood-session", &flood).await;
    insert_raw_messages(
        &db,
        "cursor",
        "other-session",
        &["apricot ledger summary".to_string()],
    )
    .await;

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "cursor".into(),
            query: "apricot ledger".into(),
            scope: LcmScope::All,
            session_id: None,
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Relevance,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
            git_filter: Default::default(),
        })
        .await
        .expect("grep should succeed")
        .hits;

    let flood_hits = hits
        .iter()
        .filter(|h| h.session_id == "flood-session")
        .count();
    assert!(
        flood_hits <= 3,
        "one session must not flood a cross-session page: {flood_hits}"
    );
    assert!(
        hits.iter().any(|h| h.session_id == "other-session"),
        "the distinct session must still appear: {hits:?}"
    );
}

#[tokio::test]
async fn grep_collapses_parent_prompt_copies_from_eight_subagents() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    let prompt = "Open pull requests to fix any issues.";
    insert_session(&db, "codex", "parent").await;
    assert!(
        db.upsert_session_message(&raw_message_with_role_source_timestamp(
            "codex",
            "parent-prompt",
            "parent",
            1,
            prompt,
            RawMessageContext {
                role: "user",
                source: "codex_rollout",
                timestamp: 1_715_000_001,
            },
        ))
        .await
    );

    for index in 0..8 {
        let session_id = format!("agent-worker-{index}");
        let child = SessionRecord {
            session_id: session_id.clone(),
            parent_session_id: Some("parent".to_string()),
            is_subagent: true,
            agent_id: Some(format!("worker-{index}")),
            ..sample_session("codex", &session_id)
        };
        assert!(db.upsert_session(&child).await);
        assert!(
            db.upsert_session_message(&raw_message_with_role_source_timestamp(
                "codex",
                &format!("child-prompt-{index}"),
                &session_id,
                index + 2,
                prompt,
                RawMessageContext {
                    role: "user",
                    source: "codex_rollout",
                    timestamp: 1_715_000_002 + index,
                },
            ))
            .await
        );
    }

    let hits = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "codex".into(),
            query: "open pull requests".into(),
            scope: LcmScope::All,
            session_id: None,
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Relevance,
            source: None,
            role: Some("user".into()),
            start_time: None,
            end_time: None,
            git_filter: Default::default(),
        })
        .await
        .expect("grep should succeed")
        .hits;

    assert_eq!(hits.len(), 1, "copied child prompts must collapse");
    assert_eq!(hits[0].session_id, "parent");
}

#[tokio::test]
async fn grep_disclosed_cap_reserves_a_tool_slot_for_capped_sessions() {
    let tmp = TempDir::new().unwrap();
    let db = registered_lcm_runtime(&tmp).await;
    insert_session(&db, "codex", "busy-session").await;
    insert_session(&db, "codex", "quiet-session").await;
    // Four narration rows plus one exact-action tool row, all matching. The
    // role penalty ranks the tool row below every narration row, so a naive
    // cap keeps narration only and "what did it actually do" is unanswerable.
    for i in 0..4 {
        assert!(
            db.upsert_session_message(&raw_message(
                "codex",
                &format!("busy-narration-{i}"),
                "busy-session",
                i + 1,
                &format!("quokka merge narration recap number {i}"),
            ))
            .await
        );
    }
    assert!(
        db.upsert_session_message(&raw_message_with_role_source_timestamp(
            "codex",
            "busy-tool-call",
            "busy-session",
            9,
            "gh pr merge 366 quokka merge exact command",
            RawMessageContext {
                role: "tool",
                source: "codex_rollout",
                timestamp: 1_715_000_009,
            },
        ))
        .await
    );
    assert!(
        db.upsert_session_message(&raw_message(
            "codex",
            "quiet-only",
            "quiet-session",
            1,
            "quokka merge summary elsewhere",
        ))
        .await
    );

    let outcome = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "codex".into(),
            query: "quokka merge".into(),
            scope: LcmScope::All,
            session_id: None,
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Relevance,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
            git_filter: Default::default(),
        })
        .await
        .expect("grep should succeed");

    let busy_kept: Vec<_> = outcome
        .hits
        .iter()
        .filter(|h| h.session_id == "busy-session")
        .collect();
    assert_eq!(busy_kept.len(), 3, "cap must hold: {busy_kept:?}");
    assert!(
        busy_kept.iter().any(|h| h.role.as_deref() == Some("tool")),
        "one capped slot must be reserved for the top tool-role hit so exact \
         actions are not fully shadowed by narration: {busy_kept:?}"
    );
    assert_eq!(
        outcome.capped_sessions.get("busy-session").copied(),
        Some(2),
        "capping must be disclosed, never silent: {:?}",
        outcome.capped_sessions
    );
    assert!(
        !outcome.capped_sessions.contains_key("quiet-session"),
        "uncapped sessions must not be reported"
    );

    // Session scope is uncapped and undisclosed: the full set comes back.
    let scoped = db
        .lcm_grep_for_test(LcmGrepRequest {
            provider: "codex".into(),
            query: "quokka merge".into(),
            scope: LcmScope::Session,
            session_id: Some("busy-session".into()),
            include_summaries: false,
            limit: 10,
            sort: LcmGrepSort::Relevance,
            source: None,
            role: None,
            start_time: None,
            end_time: None,
            git_filter: Default::default(),
        })
        .await
        .expect("session-scoped grep should succeed");
    assert_eq!(scoped.hits.len(), 5);
    assert!(scoped.capped_sessions.is_empty());
}
