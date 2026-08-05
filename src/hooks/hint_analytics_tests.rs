use super::tool_hints::{HintCategory, MAX_HINTS_PER_SESSION};
use super::{
    HintAgent, Path, PathBuf, ToolHint, Value, deduped_project_hint_with_id, mint_hint_id,
    record_hint_emitted, record_hook_invoked,
};
use crate::config::USER_DATA_DIR_ENV;
use std::collections::HashSet;

/// Terminal event kinds a single `hint_candidate` may resolve to. Every
/// candidate must be followed by exactly one of these.
const TERMINAL_EVENTS: &[&str] = &[
    "hint_emitted",
    "hint_escalated",
    "suppressed_duplicate",
    "suppressed_budget",
    "dropped_no_root",
    "missing_session",
];

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn test_hint() -> ToolHint {
    ToolHint {
        category: HintCategory::Impact,
        message: "use tracedecay_impact".to_string(),
        context: "context".to_string(),
        nonblocking: true,
    }
}

/// Enrolls `project_root` in the profile store and materializes its data dir
/// so `deduped_project_hint` reaches the on-disk dedupe branch.
fn enroll_project(project_root: &Path, project_id: &str) -> PathBuf {
    crate::storage::write_enrollment_marker(
        project_root,
        &crate::storage::EnrollmentMarker {
            project_id: project_id.to_string(),
            storage_mode: crate::storage::StorageMode::ProfileSharded,
        },
    )
    .unwrap();
    let layout = crate::storage::resolve_layout_for_current_profile(project_root).unwrap();
    std::fs::create_dir_all(&layout.data_root).unwrap();
    // Hook telemetry is fail-closed: the timing span only records a
    // `hook_completed` row once a runtime configuration snapshot is
    // published. Bootstrap the default snapshot so duration telemetry and
    // hint dedupe rows are observable in these tests.
    crate::config::bootstrap_runtime_configuration(project_root, &layout)
        .expect("publish hook test runtime configuration");
    layout.data_root
}

/// Reads every recorded analytics row visible to a project: its own store
/// file plus the user-level fallback file.
fn recorded_rows(data_root: &Path, profile_root: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    for path in [
        data_root.join(super::HOOK_ANALYTICS_FILENAME),
        profile_root.join(super::HOOK_ANALYTICS_FILENAME),
    ] {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                if let Ok(row) = serde_json::from_str::<Value>(line) {
                    rows.push(row);
                }
            }
        }
    }
    rows
}

fn recorded_file_rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path).map_or_else(
        |_| Vec::new(),
        |text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect()
        },
    )
}

fn event_kind(row: &Value) -> &str {
    row.get("event").and_then(Value::as_str).unwrap_or_default()
}

fn hint_id(row: &Value) -> &str {
    row.get("hint_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// Rows carrying a specific `hint_id`, in insertion order.
fn events_for<'a>(rows: &'a [Value], id: &str) -> Vec<&'a Value> {
    rows.iter().filter(|row| hint_id(row) == id).collect()
}

#[test]
fn mint_hint_id_is_unique_across_calls() {
    let ids: HashSet<String> = (0..256).map(|_| mint_hint_id()).collect();
    assert_eq!(ids.len(), 256, "hint ids must be unique");
}

#[test]
fn hook_invocation_rows_include_duration_telemetry() {
    let _lock = super::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_hook_duration");

    {
        let _hook_telemetry = record_hook_invoked(
            Some(&project_root),
            HintAgent::Codex,
            "PostToolUse",
            r#"{"session_id":"s1","tool_name":"Bash","cwd":"/tmp"}"#,
        );
    }

    let rows = recorded_rows(&data_root, &profile_root);
    let row = rows
        .iter()
        .find(|row| event_kind(row) == "hook_completed")
        .expect("hook_completed row");
    assert_eq!(row["hook_name"].as_str(), Some("PostToolUse"));
    for forbidden in [
        "tool_name",
        "session_id",
        "project_root",
        "event_cwd",
        "command",
    ] {
        assert!(
            row.get(forbidden).is_none(),
            "hook telemetry must omit {forbidden}"
        );
    }
    assert!(row["duration_us"].as_u64().is_some());
    assert!(row["duration_ms"].as_u64().is_some());
    assert!(row["hook_wall_time_us"].as_u64().is_some());
    assert_eq!(row["coverage"], "host_measured");
}

#[test]
fn record_hint_emitted_missing_session_is_single_terminal() {
    let _lock = super::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_missing_session");
    let hint = test_hint();
    let id = mint_hint_id();

    record_hint_emitted(Some(&project_root), HintAgent::Cursor, None, &id, &hint);

    let rows = recorded_rows(&data_root, &profile_root);
    let seq: Vec<&str> = events_for(&rows, &id)
        .iter()
        .map(|row| event_kind(row))
        .collect();
    // Exactly one terminal, and it is `missing_session` (never also
    // `hint_emitted`) so the per-candidate outcome count stays 1.
    assert_eq!(seq, vec!["missing_session"], "single terminal expected");
}

/// Walks each terminal branch of the hint pipeline and asserts that the
/// candidate resolves to exactly one terminal event carrying the same
/// `hint_id`, and that the row is attributed to the project when a root is
/// known.
#[test]
fn every_hint_branch_yields_exactly_one_terminal_with_hint_id() {
    let _lock = super::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_terminal_invariant");

    // Branch: root known, session known → on-disk dedupe emits once.
    let emit_id = mint_hint_id();
    assert!(
        deduped_project_hint_with_id(
            Some(&project_root),
            HintAgent::Cursor,
            Some("session-emit".to_string()),
            &emit_id,
            test_hint(),
        )
        .is_some()
    );

    // Branch: same (session, category) again → suppressed as duplicate.
    let dup_id = mint_hint_id();
    assert!(
        deduped_project_hint_with_id(
            Some(&project_root),
            HintAgent::Cursor,
            Some("session-emit".to_string()),
            &dup_id,
            test_hint(),
        )
        .is_none()
    );

    // Branch: root known, session missing → single `missing_session` terminal.
    let no_session_id = mint_hint_id();
    assert!(
        deduped_project_hint_with_id(
            Some(&project_root),
            HintAgent::Cursor,
            None,
            &no_session_id,
            test_hint(),
        )
        .is_some()
    );

    // Branch: no root at all → emits with no attribution.
    let no_root_id = mint_hint_id();
    assert!(
        deduped_project_hint_with_id(
            None,
            HintAgent::Cursor,
            Some("session-noroot".to_string()),
            &no_root_id,
            test_hint(),
        )
        .is_some()
    );

    let rows = recorded_rows(&data_root, &profile_root);
    let project_rows = recorded_file_rows(&data_root.join(super::HOOK_ANALYTICS_FILENAME));
    let profile_rows = recorded_file_rows(&profile_root.join(super::HOOK_ANALYTICS_FILENAME));

    let cases = [
        (&emit_id, "hint_emitted", true),
        (&dup_id, "suppressed_duplicate", true),
        (&no_session_id, "missing_session", true),
        (&no_root_id, "hint_emitted", false),
    ];
    for (id, expected_terminal, expect_attribution) in cases {
        let matched = events_for(&rows, id);
        let terminals: Vec<&str> = matched
            .iter()
            .map(|row| event_kind(row))
            .filter(|kind| TERMINAL_EVENTS.contains(kind))
            .collect();
        assert_eq!(
            terminals,
            vec![expected_terminal],
            "hint_id {id} must have exactly one terminal ({expected_terminal})"
        );
        for row in &matched {
            assert_eq!(hint_id(row), id.as_str(), "hint_id must be carried");
            assert!(
                row.get("project_root").is_none(),
                "content-free analytics must not persist a workspace path"
            );
        }
        let routed_rows = if expect_attribution {
            &project_rows
        } else {
            &profile_rows
        };
        assert!(
            !events_for(routed_rows, id).is_empty(),
            "row for {id} must be routed to its authoritative analytics file"
        );
        if expect_attribution {
            assert!(
                events_for(&profile_rows, id).is_empty(),
                "project rows must not leak into the profile fallback file"
            );
        }
    }
}

#[test]
fn hints_without_project_root_dedupe_in_the_user_profile() {
    let _lock = super::lock_test_env();
    let profile = tempfile::tempdir().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let session = Some("session-without-project-root".to_string());

    assert!(
        deduped_project_hint_with_id(
            None,
            HintAgent::Codex,
            session.clone(),
            &mint_hint_id(),
            test_hint(),
        )
        .is_some()
    );
    assert!(
        deduped_project_hint_with_id(
            None,
            HintAgent::Codex,
            session,
            &mint_hint_id(),
            test_hint(),
        )
        .is_none(),
        "a missing project root must not turn every prompt into a fresh hint"
    );
}

/// A hint over the per-session budget resolves to a single `suppressed_budget`
/// terminal, and no hint is returned to the caller.
#[test]
fn budget_exhaustion_records_suppressed_budget_terminal() {
    let _lock = super::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_budget");

    let session = "session-budget".to_string();
    // Fill the budget with distinct categories.
    let categories = [
        HintCategory::Search,
        HintCategory::FileRead,
        HintCategory::Impact,
    ];
    assert_eq!(categories.len(), MAX_HINTS_PER_SESSION);
    for category in categories {
        let hint = ToolHint {
            category,
            message: "m".to_string(),
            context: "c".to_string(),
            nonblocking: true,
        };
        assert!(
            deduped_project_hint_with_id(
                Some(&project_root),
                HintAgent::Cursor,
                Some(session.clone()),
                &mint_hint_id(),
                hint,
            )
            .is_some()
        );
    }

    // A fourth, not-yet-seen category is over budget (test_hint's Impact is
    // already spent above, so use a distinct category to isolate the budget
    // branch from the duplicate branch).
    let over_id = mint_hint_id();
    let over = deduped_project_hint_with_id(
        Some(&project_root),
        HintAgent::Cursor,
        Some(session.clone()),
        &over_id,
        ToolHint {
            category: HintCategory::CallGraph,
            message: "m".to_string(),
            context: "c".to_string(),
            nonblocking: true,
        },
    );
    assert!(over.is_none(), "over-budget hint must be suppressed");

    let rows = recorded_rows(&data_root, &profile_root);
    let terminals: Vec<&str> = events_for(&rows, &over_id)
        .iter()
        .map(|row| event_kind(row))
        .filter(|kind| TERMINAL_EVENTS.contains(kind))
        .collect();
    assert_eq!(terminals, vec!["suppressed_budget"]);
}

/// Repeated native usage past the escalation threshold surfaces exactly one
/// stronger re-hint recorded as `hint_escalated`, with the escalation prefix.
#[test]
fn repeated_usage_records_hint_escalated_terminal() {
    let _lock = super::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let profile = tempfile::tempdir().unwrap();
    let project_root = project.path().canonicalize().unwrap();
    let profile_root = profile.path().canonicalize().unwrap();
    let _profile_env = EnvGuard::set_path(USER_DATA_DIR_ENV, &profile_root);
    let data_root = enroll_project(&project_root, "proj_escalate");

    let session = "session-escalate".to_string();
    let emit = |id: &str| {
        deduped_project_hint_with_id(
            Some(&project_root),
            HintAgent::Cursor,
            Some(session.clone()),
            id,
            test_hint(),
        )
    };

    // First fire emits; the next fires below the threshold are silent; the
    // threshold fire escalates.
    assert!(emit(&mint_hint_id()).is_some(), "first fire emits");
    assert!(
        emit(&mint_hint_id()).is_none(),
        "below-threshold fire silent"
    );
    assert!(
        emit(&mint_hint_id()).is_none(),
        "below-threshold fire silent"
    );

    let escalate_id = mint_hint_id();
    let escalated = emit(&escalate_id).expect("threshold fire escalates");
    assert!(
        escalated.message.starts_with("Repeated native"),
        "escalation must carry the stronger prefix: {}",
        escalated.message
    );

    // A further fire is permanently silent.
    assert!(
        emit(&mint_hint_id()).is_none(),
        "post-escalation fire silent"
    );

    let rows = recorded_rows(&data_root, &profile_root);
    let terminals: Vec<&str> = events_for(&rows, &escalate_id)
        .iter()
        .map(|row| event_kind(row))
        .filter(|kind| TERMINAL_EVENTS.contains(kind))
        .collect();
    assert_eq!(terminals, vec!["hint_escalated"]);
}
