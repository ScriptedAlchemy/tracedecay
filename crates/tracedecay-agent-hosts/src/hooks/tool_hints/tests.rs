use super::*;

#[test]
fn benign_git_narration_does_not_fire_the_unexpected_change_hint() {
    let benign = ToolHintInput {
        prompt: Some("I committed the slice and confirmed the working tree is clean.".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    };
    assert_ne!(
        classify_hint(&benign),
        Some(HintCategory::UnexpectedChanges),
        "ordinary commit narration must not trigger the unexpected-change hint"
    );
}

#[test]
fn classifier_priority_handles_overlapping_signals() {
    let recall = ToolHintInput {
        prompt: Some("remember when we traced setup_project last time?".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    };
    assert_eq!(classify_hint(&recall), Some(HintCategory::SessionRecall));

    let file_list = ToolHintInput {
        tool_name: Some("Bash".to_string()),
        command: Some("rg --files src/hooks".to_string()),
        prompt: Some("find files, do not search contents".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    };
    assert_eq!(classify_hint(&file_list), Some(HintCategory::FileLookup));

    let call_vs_impact = ToolHintInput {
        prompt: Some("who calls setup_project and which tests depend on it?".to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    };
    assert_eq!(
        classify_hint(&call_vs_impact),
        Some(HintCategory::CallGraph)
    );
}

#[test]
fn every_category_has_compact_skill_backed_rendering() {
    let categories = [
        HintCategory::Search,
        HintCategory::SemanticSearch,
        HintCategory::FileRead,
        HintCategory::ToolDescriptorRead,
        HintCategory::BroadRead,
        HintCategory::CallGraph,
        HintCategory::Impact,
        HintCategory::SymbolLookup,
        HintCategory::FileLookup,
        HintCategory::ProjectContext,
        HintCategory::SessionRecall,
        HintCategory::AtomicEdit,
        HintCategory::TypeOrientation,
        HintCategory::ExploreSubagent,
        HintCategory::SubagentStartContext,
        HintCategory::BuildDiagnostics,
        HintCategory::ReviewChanges,
        HintCategory::MemoryStore,
        HintCategory::EditRedundancy,
        HintCategory::UnexpectedChanges,
    ];

    for category in categories {
        let hint = hint_for_category(category);
        let visible = format!("{}\n{}", hint.message, hint.context);
        assert_eq!(hint.category, category);
        assert!(!hint.message.is_empty(), "{category:?}");
        assert!(!hint.context.is_empty(), "{category:?}");
        assert!(
            visible.len() <= 850,
            "{category:?} hint is too verbose: {} chars\n{}",
            visible.len(),
            visible
        );
        let skill = category_skill(category);
        assert!(
            visible.contains(&format!("Skill: tracedecay:{skill}.")),
            "{category:?} missing skill trigger"
        );
    }
}

#[test]
fn dedupe_emits_each_category_once_per_session() {
    let mut dedupe = ToolHintDedupe::default();
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDeliveryDecisionV1::Deliver
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::ToolDescriptorRead),
        HintDeliveryDecisionV1::Deliver
    );
    // Fresh session gets its own budget.
    assert_eq!(
        dedupe.decide("s2", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );
}

#[test]
fn escalation_fires_exactly_once_after_repeated_triggers() {
    let mut dedupe = ToolHintDedupe::default();
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );
    // Repeat fires below the threshold stay silent.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressDuplicate
    );
    // Third post-hint fire unlocks the single escalation.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::DeliverEscalation
    );
    // Everything after escalation is permanently silent.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDeliveryDecisionV1::Deliver
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Impact),
        HintDeliveryDecisionV1::SuppressBudget
    );
}

#[test]
fn escalation_respects_the_total_session_budget() {
    let mut dedupe = ToolHintDedupe::default();
    // Exhaust the budget with three categories, then escalate the first.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDeliveryDecisionV1::Deliver
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::Impact),
        HintDeliveryDecisionV1::Deliver
    );
    for _ in 0..(ESCALATION_TRIGGER_THRESHOLD - 1) {
        assert_eq!(
            dedupe.decide("s1", HintCategory::Search),
            HintDeliveryDecisionV1::SuppressDuplicate
        );
    }
    // Escalation is another emitted hint, so the spent budget suppresses it.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressBudget
    );
}

#[test]
fn dedupe_round_trips_through_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/tool_hints_seen.json");

    let mut dedupe = ToolHintDedupe::load_or_default(&path);
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );
    dedupe.save(&path).unwrap();

    let mut reloaded = ToolHintDedupe::load_or_default(&path);
    assert_eq!(
        reloaded.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressDuplicate,
        "persisted (session, category) pairs must suppress re-emission"
    );
    assert_eq!(
        reloaded.decide("s1", HintCategory::FileRead),
        HintDeliveryDecisionV1::Deliver
    );
}

#[test]
fn save_writes_versioned_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool_hints_seen.json");
    let mut dedupe = ToolHintDedupe::default();
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );
    dedupe.save(&path).unwrap();

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["version"], 2);
    assert!(value["sessions"].is_array());
    assert!(value["categories"].is_array());
}

#[test]
fn legacy_store_migrates_to_versioned_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool_hints_seen.json");
    // Legacy v1 file: a bare array of {session_id, category}.
    std::fs::write(
        &path,
        r#"[{"session_id":"s1","category":"search"},{"session_id":"s1","category":"file_read"}]"#,
    )
    .unwrap();

    let mut dedupe = ToolHintDedupe::load_or_default(&path);
    // v1 categories load as already-hinted: they suppress, not re-emit.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressDuplicate
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::FileRead),
        HintDeliveryDecisionV1::SuppressDuplicate
    );
    // The two migrated hints already count against s1's budget, so only one
    // more distinct category can emit before the cap.
    assert_eq!(
        dedupe.decide("s1", HintCategory::Impact),
        HintDeliveryDecisionV1::Deliver
    );
    assert_eq!(
        dedupe.decide("s1", HintCategory::CallGraph),
        HintDeliveryDecisionV1::SuppressBudget
    );

    // Persisting rewrites the file in v2 shape.
    dedupe.save(&path).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["version"], 2);

    // Reload from v2 preserves the migrated suppression state.
    let mut reloaded = ToolHintDedupe::load_or_default(&path);
    assert_eq!(
        reloaded.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::SuppressDuplicate
    );
}

#[test]
fn oversized_store_resets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool_hints_seen.json");
    // A v1 array beyond the persisted bound must reset to an empty state.
    let entries: Vec<String> = (0..=MAX_PERSISTED_HINT_ENTRIES)
        .map(|i| format!(r#"{{"session_id":"s{i}","category":"search"}}"#))
        .collect();
    std::fs::write(&path, format!("[{}]", entries.join(","))).unwrap();

    let mut dedupe = ToolHintDedupe::load_or_default(&path);
    // Reset means s0's category is treated as never hinted.
    assert_eq!(
        dedupe.decide("s0", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );
}

#[test]
fn dedupe_load_tolerates_missing_and_corrupt_files() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.json");
    let mut dedupe = ToolHintDedupe::load_or_default(&missing);
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );

    let corrupt = dir.path().join("corrupt.json");
    std::fs::write(&corrupt, "not json").unwrap();
    let mut dedupe = ToolHintDedupe::load_or_default(&corrupt);
    assert_eq!(
        dedupe.decide("s1", HintCategory::Search),
        HintDeliveryDecisionV1::Deliver
    );
}

fn shell_input(command: &str) -> ToolHintInput {
    ToolHintInput {
        tool_name: Some("Bash".to_string()),
        command: Some(command.to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    }
}

#[test]
fn tracedecay_tool_invocations_do_not_recommend_the_same_tool_family() {
    for command in [
        "tracedecay tool diagnostics",
        "tracedecay tool grep --pattern needle",
        "tracedecay tool read --file src/lib.rs",
    ] {
        assert!(
            decide_hint(&shell_input(command)).is_none(),
            "{command} already selected TraceDecay and must stay silent"
        );
    }
}

fn edit_input(tool_name: &str, file_path: &str) -> ToolHintInput {
    ToolHintInput {
        tool_name: Some(tool_name.to_string()),
        file_path: Some(file_path.to_string()),
        session_id: Some("session-1".to_string()),
        ..ToolHintInput::default()
    }
}

#[test]
fn non_memory_edits_get_no_memory_store_hint() {
    // A regular source edit is not a memory location — and edit tools have no
    // other hint branch, so no hint at all.
    assert!(decide_hint(&edit_input("Write", "src/lib.rs")).is_none());
    // A markdown file in a non-`.claude` `memory` dir does not match.
    assert!(!is_harness_memory_path("/repo/docs/memory/notes.md"));
    // `.claude` present but the file is not directly under a `memory` dir.
    assert!(!is_harness_memory_path(
        "/home/zack/.claude/memory/sub/notes.md"
    ));
    // A non-markdown file under `.claude/**/memory/` does not match.
    assert!(!is_harness_memory_path(
        "/home/zack/.claude/projects/foo/memory/data.json"
    ));
    // Positive controls.
    assert!(is_harness_memory_path(
        "/home/zack/.claude/projects/foo/memory/notes.md"
    ));
    assert!(is_harness_memory_path("/anywhere/MEMORY.md"));
    assert!(is_harness_memory_path("/anywhere/CLAUDE.md"));
    // Windows-style separators normalize.
    assert!(is_harness_memory_path(
        "C:\\Users\\z\\.claude\\projects\\foo\\memory\\notes.md"
    ));
}
