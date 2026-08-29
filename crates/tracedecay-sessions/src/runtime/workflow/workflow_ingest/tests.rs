use super::*;

fn sample_meta() -> Value {
    serde_json::json!({
        "runId": "wf_d0bf6fa4-48f",
        "workflowName": "tracedecay-triggering-evals",
        "summary": "Mine real transcripts into a broad eval corpus",
        "status": "completed",
        "startTime": 1_783_142_254_914_i64,
        "durationMs": 983_890_i64,
        "agentCount": 2,
        "defaultModel": "claude-fable-5",
        "phases": [
            {"title": "Mine", "detail": "harvest scenarios"},
            {"title": "Run", "detail": "run it", "model": "fable"}
        ],
        "result": {"scored": 45, "scenarios": 36},
        "workflowProgress": [
            {"type": "workflow_phase", "phaseTitle": "Mine"},
            {
                "type": "workflow_agent",
                "label": "mine:claude-transcripts",
                "phaseTitle": "Mine",
                "phaseIndex": 1,
                "agentId": "a17141dbe5a308242",
                "model": "claude-fable-5",
                "state": "done",
                "startedAt": 1_783_142_254_936_i64,
                "lastProgressAt": 1_783_142_255_936_i64
            },
            {
                "type": "workflow_agent",
                "label": "",
                "phaseTitle": "Run",
                "agentId": "aa09ec4d07fccc915",
                "state": "in_progress",
                "startedAt": 1_783_142_260_000_i64
            }
        ]
    })
}

#[test]
fn parse_run_from_meta_maps_fields_and_folds_status() {
    let (run, agents) = parse_run_from_meta("wf_fallback", "sess-parent", &sample_meta());

    assert_eq!(run.run_id, "wf_d0bf6fa4-48f"); // runId wins over the dir name
    assert_eq!(run.parent_session_id, "sess-parent");
    assert_eq!(run.name.as_deref(), Some("tracedecay-triggering-evals"));
    assert_eq!(run.status, WorkflowStatus::Completed);
    // startTime ms -> secs.
    assert_eq!(run.started_ts, Some(1_783_142_254));
    // started + durationMs/1000.
    assert_eq!(run.ended_ts, Some(1_783_142_254 + 983));
    // agentCount from meta, not roster length.
    assert_eq!(run.agent_count, 2);
    // `summary` present -> used verbatim (one-lined).
    assert_eq!(
        run.result_summary.as_deref(),
        Some("Mine real transcripts into a broad eval corpus")
    );

    // phase_json round-trips as a JSON array of the phases.
    let phases: Value = serde_json::from_str(run.phase_json.as_deref().unwrap()).unwrap();
    assert!(phases.is_array());
    assert_eq!(phases.as_array().unwrap().len(), 2);
    assert_eq!(phases[0]["title"], "Mine");

    // Only the two workflow_agent rows, in order; workflow_phase is dropped.
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].agent_label, "mine:claude-transcripts");
    assert_eq!(agents[0].phase.as_deref(), Some("Mine"));
    assert_eq!(agents[0].status, WorkflowStatus::Completed);
    assert_eq!(agents[0].model.as_deref(), Some("claude-fable-5"));
    assert_eq!(agents[0].started_ts, Some(1_783_142_254));
    assert_eq!(agents[0].ended_ts, Some(1_783_142_255));
    // Empty label falls back to the agent id; missing model backfills from
    // defaultModel; state folds `in_progress` -> Running.
    assert_eq!(agents[1].agent_label, "aa09ec4d07fccc915");
    assert_eq!(agents[1].model.as_deref(), Some("claude-fable-5"));
    assert_eq!(agents[1].status, WorkflowStatus::Running);
}

#[test]
fn result_summary_truncates_json_result_when_no_summary() {
    let mut meta = sample_meta();
    meta.as_object_mut().unwrap().remove("summary");
    let long = "x ".repeat(2000);
    meta.as_object_mut()
        .unwrap()
        .insert("result".to_string(), Value::String(long));
    let summary = run_result_summary(&meta).unwrap();
    // Single-char ellipsis convention: at most `CAP` content chars + `…`.
    assert!(summary.chars().count() <= RESULT_SUMMARY_CAP + 1);
    assert!(summary.ends_with('…'));
    // Whitespace collapsed to single spaces.
    assert!(!summary.contains("  "));
}

#[test]
fn result_summary_prefers_summary_over_result() {
    let meta = sample_meta();
    // Even though `result` is a dict, the `summary` string wins.
    assert_eq!(
        run_result_summary(&meta).as_deref(),
        Some("Mine real transcripts into a broad eval corpus")
    );
}

#[test]
fn roster_extracts_only_workflow_agents() {
    let meta = sample_meta();
    let roster = parse_roster("wf_x", &meta, Some("fallback-model"));
    assert_eq!(roster.len(), 2);
    assert!(roster.iter().all(|agent| agent.run_id == "wf_x"));
    let labels: Vec<&str> = roster.iter().map(|a| a.agent_label.as_str()).collect();
    assert_eq!(labels, vec!["mine:claude-transcripts", "aa09ec4d07fccc915"]);
}

#[test]
fn transcript_tokens_sum_input_and_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-test.jsonl");
    let body = concat!(
        r#"{"type":"user","sessionId":"agent-sess","timestamp":"2026-07-04T05:17:34.967Z","message":{"role":"user","content":"hi"}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-04T05:18:00.000Z","message":{"role":"assistant","usage":{"input_tokens":100,"output_tokens":40}}}"#,
        "\n",
        "   \n",
        r#"not json"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-07-04T05:25:32.232Z","message":{"role":"assistant","usage":{"input_tokens":10,"output_tokens":8,"cache_read_input_tokens":999}}}"#,
        "\n",
    );
    std::fs::write(&path, body).unwrap();
    let summary = summarize_transcript_file(&path);
    // 100+40 + 10+8 (cache_* excluded).
    assert_eq!(summary.tokens, 158);
    assert_eq!(summary.session_id.as_deref(), Some("agent-sess"));
    assert_eq!(
        summary.first_ts,
        parse_timestamp("2026-07-04T05:17:34.967Z").map(|s| s as i64)
    );
    assert_eq!(
        summary.last_ts,
        parse_timestamp("2026-07-04T05:25:32.232Z").map(|s| s as i64)
    );
}

#[test]
fn dir_only_run_is_running_with_journal_roster() {
    let dir = tempfile::tempdir().unwrap();
    let journal = concat!(
        r#"{"type":"started","agentId":"a1"}"#,
        "\n",
        r#"{"type":"started","agentId":"a2"}"#,
        "\n",
        r#"{"type":"result","agentId":"a1"}"#,
        "\n",
        r#"{"type":"started","agentId":""}"#,
        "\n",
    );
    std::fs::write(dir.path().join("journal.jsonl"), journal).unwrap();
    let events = read_journal(dir.path());
    // Empty agentId dropped; three valid events remain.
    assert_eq!(events.len(), 3);
    // a1 has a terminal result -> Completed; a2 only started -> Running.
    assert_eq!(
        journal_agent_status(&events, "a1"),
        WorkflowStatus::Completed
    );
    assert_eq!(journal_agent_status(&events, "a2"), WorkflowStatus::Running);
    assert_eq!(
        journal_agent_status(&events, "absent"),
        WorkflowStatus::Unknown
    );
}

#[test]
fn dir_only_run_from_disk_yields_running_and_roster() {
    let dir = tempfile::tempdir().unwrap();
    let agents_dir = dir.path();
    // Two agent transcripts + a journal naming a3 that has no file yet.
    std::fs::write(
        agents_dir.join("agent-a1.jsonl"),
        format!(
            "{}\n",
            r#"{"sessionId":"s","timestamp":"2026-07-04T05:00:00.000Z","message":{"usage":{"input_tokens":5,"output_tokens":5}}}"#
        ),
    )
    .unwrap();
    std::fs::write(agents_dir.join("agent-a1.meta.json"), "{}").unwrap();
    std::fs::write(agents_dir.join("agent-a2.jsonl"), "\n").unwrap();
    std::fs::write(
        agents_dir.join("journal.jsonl"),
        concat!(
            r#"{"type":"started","agentId":"a1"}"#,
            "\n",
            r#"{"type":"started","agentId":"a3"}"#,
            "\n"
        ),
    )
    .unwrap();

    let (run, mut agents) = parse_run_from_dir("wf_dir", "sess", agents_dir);
    assert_eq!(run.status, WorkflowStatus::Running);
    assert_eq!(run.run_id, "wf_dir");
    // a1, a2 (from files) then a3 (journal-only).
    let mut ids: Vec<String> = agents.iter().map(|a| a.agent_id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["a1", "a2", "a3"]);
    assert_eq!(run.agent_count, 3);

    // Enrichment attaches the transcript path + tokens for a1.
    for agent in &mut agents {
        enrich_agent_from_transcript(agent, agents_dir);
    }
    let a1 = agents.iter().find(|a| a.agent_id == "a1").unwrap();
    assert_eq!(a1.tokens, 10);
    assert!(
        a1.transcript_path
            .as_deref()
            .unwrap()
            .ends_with("agent-a1.jsonl")
    );
    assert_eq!(a1.agent_session_id.as_deref(), Some("s"));
    // a3 has no file: no transcript path, zero tokens.
    let a3 = agents.iter().find(|a| a.agent_id == "a3").unwrap();
    assert!(a3.transcript_path.is_none());
    assert_eq!(a3.tokens, 0);
}
