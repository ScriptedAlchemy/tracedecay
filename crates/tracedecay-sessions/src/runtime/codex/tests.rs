//! Codex rollout parser and observation-normalization tests.

use serde_json::Value;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationFactV1, CanonicalUnknownStateV1,
    CanonicalWorkflowSemanticKindV1, ProjectId,
};

use super::CodexSource;
use super::goals::{codex_goal_event_from_line, goal_event_message};
use super::meta::{CodexMeta, session_meta_with_provenance};
use super::observation::{
    CodexObservationAdmission, codex_native_record_id, normalize_codex_observation,
};
use super::records::response_item_tool_metadata;
use crate::runtime::shared::StoredCursor;
use crate::runtime::source::TranscriptSource;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod goal_event_tests {
    use super::*;
    use serde_json::json;

    fn goal_event_line(objective: &str, status: &str) -> Value {
        json!({
            "timestamp": "2026-07-08T08:49:29.711Z",
            "type": "event_msg",
            "payload": {
                "type": "thread_goal_updated",
                "threadId": "thread-1",
                "goal": {
                    "threadId": "thread-1",
                    "objective": objective,
                    "status": status,
                    "tokensUsed": 42,
                    "timeUsedSeconds": 7,
                    "createdAt": 1_783_500_569i64,
                    "updatedAt": 1_783_500_600i64
                }
            }
        })
    }

    #[test]
    fn parses_goal_event_into_row_with_metadata() {
        let event =
            codex_goal_event_from_line(&goal_event_line("ship the parser", "active")).unwrap();
        let meta = CodexMeta {
            cwd: std::path::PathBuf::from("/tmp/project"),
            session_id: "sess-1".to_string(),
            model: None,
            git: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            agent_nickname: None,
            agent_role: None,
            thread_source: None,
        };
        let message = goal_event_message(
            &meta,
            Some("gpt-5.5"),
            std::path::Path::new("/tmp/rollout.jsonl"),
            128,
            Some(1_783_500_600),
            &event,
        );
        assert_eq!(message.role, "system");
        assert_eq!(message.kind.as_deref(), Some("goal"));
        assert_eq!(message.text, "ship the parser");
        assert_eq!(message.ordinal, 128);
        let metadata: Value =
            serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["source"], "codex_thread_goal");
        assert_eq!(metadata["source_event"], "thread_goal_updated");
        assert_eq!(metadata["status"], "active");
        assert_eq!(metadata["thread_id"], "thread-1");
        assert_eq!(metadata["tokens_used"], 42);
        assert_eq!(metadata["time_used_seconds"], 7);
        assert_eq!(metadata["created_at"], 1_783_500_569i64);
        assert_eq!(metadata["updated_at"], 1_783_500_600i64);
    }

    #[test]
    fn consecutive_identical_states_share_a_dedup_key() {
        let a = codex_goal_event_from_line(&goal_event_line("same goal", "active")).unwrap();
        // Same objective+status, only token/time drift -> same dedup key (skipped).
        let mut drift = goal_event_line("same goal", "active");
        drift["payload"]["goal"]["tokensUsed"] = json!(9999);
        drift["payload"]["goal"]["timeUsedSeconds"] = json!(321);
        let b = codex_goal_event_from_line(&drift).unwrap();
        assert_eq!(a.dedup_key(), b.dedup_key());
        // A status transition is a distinct key (new row).
        let c = codex_goal_event_from_line(&goal_event_line("same goal", "paused")).unwrap();
        assert_ne!(a.dedup_key(), c.dedup_key());
    }

    #[test]
    fn unknown_status_is_carried_through_verbatim() {
        let event =
            codex_goal_event_from_line(&goal_event_line("do the thing", "completed")).unwrap();
        assert_eq!(event.status.as_deref(), Some("completed"));
        let metadata = event.metadata();
        assert_eq!(metadata["status"], "completed");
    }

    #[test]
    fn missing_status_and_objective_are_handled_gracefully() {
        // No status key at all -> status None, still a valid goal row.
        let mut no_status = goal_event_line("objective only", "active");
        no_status["payload"]["goal"]
            .as_object_mut()
            .unwrap()
            .remove("status");
        let event = codex_goal_event_from_line(&no_status).unwrap();
        assert!(event.status.is_none());
        assert!(!event.metadata().as_object().unwrap().contains_key("status"));
        // Empty objective -> no goal row (nothing to catalog).
        let empty = goal_event_line("   ", "active");
        assert!(codex_goal_event_from_line(&empty).is_none());
    }

    #[test]
    fn non_goal_event_lines_are_ignored() {
        let token_count = json!({
            "type": "event_msg",
            "payload": {"type": "token_count", "info": {}}
        });
        assert!(codex_goal_event_from_line(&token_count).is_none());
        let user = json!({
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "hi"}
        });
        assert!(codex_goal_event_from_line(&user).is_none());
    }

    #[test]
    fn exposed_reasoning_carries_visibility_without_claiming_hidden_content() {
        let payload = json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "visible summary"}],
        });
        let metadata = response_item_tool_metadata("reasoning", &payload, None, None);
        assert_eq!(metadata["reasoning_visibility"], "provider_exposed");
        assert_eq!(metadata["reasoning_retention"], "provider_exposed");
        assert!(metadata.get("encrypted_content").is_none());
    }

    #[test]
    fn observation_admission_routes_project_and_profile_records_by_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let project_root = temp.path().join("project");
        let project_src = project_root.join("src");
        let other = temp.path().join("other");
        std::fs::create_dir_all(&project_src).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let status = std::process::Command::new(crate::git::git_program())
            .args(["init", "--quiet"])
            .current_dir(&project_root)
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");

        let project_id = ProjectId::new("project-id").unwrap();
        let project = CodexObservationAdmission::Project {
            root: &project_root,
            project_id: project_id.clone(),
        };
        let linked_root = temp.path().join("linked-worktree");
        std::fs::create_dir_all(&linked_root).unwrap();
        let linked = CodexObservationAdmission::Project {
            root: &linked_root,
            project_id,
        };
        assert_eq!(project.scope(), linked.scope());
        assert!(project.accepts(Some(&project_src)));
        assert!(!project.accepts(Some(&other)));

        let registered = vec![project_root];
        let profile = CodexObservationAdmission::Profile {
            session_id: Some("session-1"),
            registered_roots: &registered,
        };
        assert!(!profile.accepts(Some(&project_src)));
        assert!(profile.accepts(Some(&other)));
        assert!(profile.accepts(None));
        assert!(profile.accepts_session("session-1"));
        assert!(!profile.accepts_session("session-2"));
    }

    #[test]
    fn native_record_identity_is_stable_across_json_formatting() {
        let compact: Value = serde_json::from_str(
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"redacted"}}"#,
        )
        .unwrap();
        let spaced: Value = serde_json::from_str(
            r#"{ "payload": { "message": "redacted", "type": "agent_message" }, "type": "event_msg" }"#,
        )
        .unwrap();
        assert_eq!(
            codex_native_record_id("session-redacted", &compact)
                .unwrap()
                .as_str(),
            codex_native_record_id("session-redacted", &spaced)
                .unwrap()
                .as_str()
        );
    }

    #[test]
    fn canonical_codex_record_is_typed_and_redacts_provider_bags() {
        let native = json!({
            "timestamp": "2026-07-08T08:49:29Z",
            "type": "response_item",
            "cwd": "/secret/project",
            "payload": {
                "type": "function_call",
                "name": "shell",
                "call_id": "call-redacted",
                "arguments": {"path": "/secret/project", "token": "credential-redacted"}
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(40, 80).unwrap();
        let record_id = codex_native_record_id("session-redacted", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "session-redacted",
            Some("session-redacted"),
            record_id.clone(),
            range,
        )
        .unwrap();
        let rendered = format!("{envelope:?}");
        assert!(rendered.contains("ToolInvocation"));
        assert!(rendered.contains("FileBytes"));
        assert!(rendered.contains(record_id.as_str()));
        assert!(!rendered.contains("/secret/project"));
        assert!(!rendered.contains("credential-redacted"));
    }

    #[test]
    fn codex_turn_context_sets_native_turn_and_thread_relations() {
        let native = json!({
            "type": "turn_context",
            "payload": {
                "turn_id": "turn-native-1",
                "cwd": "/secret/project",
                "model": "gpt-5.5"
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 40).unwrap();
        let record_id = codex_native_record_id("thread-redacted", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "thread-redacted",
            Some("thread-redacted"),
            record_id,
            range,
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["session_id"], "thread-redacted");
        assert_eq!(relations["thread_id"], "thread-redacted");
        assert_eq!(relations["turn_id"], "turn-native-1");
        assert!(relations.get("message_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert!(relations.get("parent_agent_id").is_none());
        assert!(envelope.facts().iter().any(|fact| matches!(
            fact,
            CanonicalObservationFactV1::Unknown {
                native_kind,
                state: CanonicalUnknownStateV1::Unsupported,
            } if native_kind == "turn_context"
        )));
    }

    #[test]
    fn codex_session_meta_subagent_sets_native_agent_lineage() {
        let native = json!({
            "type": "session_meta",
            "payload": {
                "id": "child-thread",
                "cwd": "/secret/project",
                "thread_source": "subagent",
                "agent_nickname": "worker-a",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": "parent-thread",
                            "agent_nickname": "worker-a"
                        }
                    }
                }
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 50).unwrap();
        let record_id = codex_native_record_id("child-thread", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "child-thread",
            Some("child-thread"),
            record_id,
            range,
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["thread_id"], "child-thread");
        assert_eq!(relations["agent_id"], "child-thread");
        assert_eq!(relations["parent_agent_id"], "parent-thread");
        assert!(relations.get("turn_id").is_none());
        assert!(relations.get("message_id").is_none());
    }

    #[test]
    fn codex_response_without_turn_id_leaves_turn_unset() {
        let native = json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "hello"}]
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 20).unwrap();
        let record_id = codex_native_record_id("thread-redacted", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "thread-redacted",
            Some("thread-redacted"),
            record_id.clone(),
            range,
        )
        .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert_eq!(relations["thread_id"], "thread-redacted");
        assert_eq!(relations["message_id"], record_id.as_str());
        assert!(relations.get("turn_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert!(relations.get("parent_agent_id").is_none());
    }

    #[test]
    fn codex_canonical_agent_identity_ignores_nickname_and_role() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let native = |nickname: &str, role: &str| {
            json!({
                "type": "session_meta",
                "payload": {
                    "id": "child-thread",
                    "cwd": project,
                    "thread_source": "subagent",
                    "agent_nickname": nickname,
                    "agent_role": role,
                    "source": {
                        "subagent": {
                            "thread_spawn": {
                                "parent_thread_id": "parent-thread",
                                "agent_nickname": nickname,
                                "agent_role": role
                            }
                        }
                    }
                }
            })
        };
        let write_rollout = |name: &str, record: &Value| {
            let path = temp.path().join(name);
            let contents = format!(
                "{}\n{}\n",
                serde_json::to_string(record).unwrap(),
                json!({
                    "type": "event_msg",
                    "payload": {"type": "agent_message", "message": "done"}
                })
            );
            std::fs::write(&path, contents).unwrap();
            path
        };

        let first_native = native("Euler", "explorer");
        let renamed_native = native("Gauss", "reviewer");
        let first_path = write_rollout("first.jsonl", &first_native);
        let renamed_path = write_rollout("renamed.jsonl", &renamed_native);
        let first_meta = session_meta_with_provenance(&first_path).unwrap();
        let renamed_meta = session_meta_with_provenance(&renamed_path).unwrap();
        assert_eq!(first_meta.native_thread_id.as_deref(), Some("child-thread"));
        assert_eq!(
            renamed_meta.native_thread_id.as_deref(),
            Some("child-thread")
        );
        assert_eq!(first_meta.meta.agent_id.as_deref(), Some("Euler"));
        assert_eq!(renamed_meta.meta.agent_id.as_deref(), Some("Gauss"));

        let source = CodexSource::with_home(temp.path());
        let first_parsed = source
            .parse_new(&first_path, StoredCursor::default(), &project, None)
            .unwrap();
        let renamed_parsed = source
            .parse_new(&renamed_path, StoredCursor::default(), &project, None)
            .unwrap();
        assert_eq!(first_parsed.draft.agent_id.as_deref(), Some("Euler"));
        assert_eq!(renamed_parsed.draft.agent_id.as_deref(), Some("Gauss"));

        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let first_record_id = codex_native_record_id("child-thread", &first_native).unwrap();
        let renamed_record_id = codex_native_record_id("child-thread", &renamed_native).unwrap();
        assert_ne!(first_record_id, renamed_record_id);
        let first_envelope = normalize_codex_observation(
            &first_native,
            "child-thread",
            first_meta.native_thread_id.as_deref(),
            first_record_id,
            range,
        )
        .unwrap();
        let renamed_envelope = normalize_codex_observation(
            &renamed_native,
            "child-thread",
            renamed_meta.native_thread_id.as_deref(),
            renamed_record_id,
            range,
        )
        .unwrap();
        let first_relations = serde_json::to_value(first_envelope.relations()).unwrap();
        let renamed_relations = serde_json::to_value(renamed_envelope.relations()).unwrap();
        assert_eq!(first_relations["agent_id"], "child-thread");
        assert_eq!(renamed_relations["agent_id"], "child-thread");
        assert_eq!(first_relations["parent_agent_id"], "parent-thread");
        assert_eq!(renamed_relations["parent_agent_id"], "parent-thread");
    }

    #[test]
    fn codex_subagent_filename_fallback_does_not_invent_canonical_identity() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let native = json!({
            "type": "session_meta",
            "payload": {
                "cwd": project,
                "thread_source": "subagent",
                "agent_nickname": "mutable-label",
                "agent_role": "mutable-role",
                "forked_from_id": "parent-thread"
            }
        });
        let path = temp.path().join("rollout-filename.jsonl");
        let contents = format!(
            "{}\n{}\n",
            native,
            json!({
                "type": "event_msg",
                "payload": {"type": "agent_message", "message": "legacy rollout"}
            })
        );
        std::fs::write(&path, contents).unwrap();

        let meta = session_meta_with_provenance(&path).unwrap();
        assert_eq!(meta.meta.session_id, "rollout-filename");
        assert!(meta.native_thread_id.is_none());
        assert_eq!(meta.meta.agent_id.as_deref(), Some("mutable-label"));
        let parsed = CodexSource::with_home(temp.path())
            .parse_new(&path, StoredCursor::default(), &project, None)
            .unwrap();
        assert_eq!(parsed.draft.agent_id.as_deref(), Some("mutable-label"));

        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let record_id = codex_native_record_id("rollout-filename", &native).unwrap();
        let envelope =
            normalize_codex_observation(&native, "rollout-filename", None, record_id, range)
                .unwrap();
        let relations = serde_json::to_value(envelope.relations()).unwrap();
        assert!(relations.get("thread_id").is_none());
        assert!(relations.get("agent_id").is_none());
        assert_eq!(relations["parent_agent_id"], "parent-thread");
    }

    fn load_codex_golden_input(name: &str) -> Value {
        let path = format!(
            "{}/tests/fixtures/provider_normalization/codex/{name}.input.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn load_codex_golden_expected(name: &str, stable_record_id: &str) -> Value {
        let path = format!(
            "{}/tests/fixtures/provider_normalization/codex/{name}.expected_envelope.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read_to_string(path).unwrap().replace(
            "\"$STABLE_RECORD_ID\"",
            &serde_json::to_string(stable_record_id).unwrap(),
        );
        serde_json::from_str(&raw).unwrap()
    }

    fn assert_codex_golden_envelope(
        name: &str,
        session_id: &str,
        native_thread_id: Option<&str>,
        range: tracedecay_domain::ObservationSourceRangeV1,
    ) {
        let native = load_codex_golden_input(name);
        // Stable record id is content-addressed provider-parser evidence
        // (`codex_native_record_id`), not a hand-built canonical identity.
        let record_id = codex_native_record_id(session_id, &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            session_id,
            native_thread_id,
            record_id.clone(),
            range,
        )
        .unwrap();
        let actual = serde_json::to_value(&envelope).unwrap();
        let expected = load_codex_golden_expected(name, record_id.as_str());
        assert_eq!(
            actual, expected,
            "Codex golden envelope mismatch for {name}: parser projection must match checked-in expected envelope"
        );
        // Hostile lookalike / provider bags must not survive normalization.
        let rendered = actual.to_string();
        assert!(!rendered.contains("/secret/project"));
        assert!(!rendered.contains("credential-redacted"));
        assert!(!rendered.contains("/redacted/project"));
    }

    #[test]
    fn codex_checked_in_agent_message_golden_matches_parser_envelope() {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        assert_codex_golden_envelope(
            "agent_message",
            "codex-golden-session",
            Some("codex-golden-session"),
            range,
        );
    }

    #[test]
    fn codex_checked_in_session_meta_golden_matches_parser_envelope() {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        assert_codex_golden_envelope(
            "session_meta",
            "codex-golden-session",
            Some("codex-golden-session"),
            range,
        );
    }

    #[test]
    fn codex_checked_in_function_call_golden_matches_parser_envelope() {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(40, 80).unwrap();
        assert_codex_golden_envelope(
            "function_call",
            "codex-golden-session",
            Some("codex-golden-session"),
            range,
        );
    }

    #[test]
    fn codex_checked_in_thread_goal_updated_golden_matches_parser_envelope() {
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        assert_codex_golden_envelope(
            "thread_goal_updated",
            "codex-golden-session",
            Some("codex-golden-session"),
            range,
        );
    }

    #[test]
    fn thread_goal_updated_maps_nested_goal_to_workflow_lifecycle() {
        // Binding shape: goal_event_line / write_codex_rollout_with_goal_events.
        let native = goal_event_line("phlogiston pipeline overhaul", "active");
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let record_id = codex_native_record_id("thread-1", &native).unwrap();
        let envelope =
            normalize_codex_observation(&native, "thread-1", Some("thread-1"), record_id, range)
                .unwrap();
        let facts = envelope.facts();
        assert_eq!(facts.len(), 1);
        match &facts[0] {
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Goal,
                provider_reference,
                item_id,
                parent_reference,
                list_reference,
                state,
                status,
                item_order,
                revision,
                event_sequence,
                content,
            } => {
                assert_eq!(provider_reference.as_deref(), Some("thread-1"));
                assert_eq!(status.as_deref(), Some("active"));
                assert!(item_id.is_none());
                assert!(parent_reference.is_none());
                assert!(list_reference.is_none());
                assert!(state.is_none());
                assert!(item_order.is_none());
                assert!(revision.is_none());
                assert!(event_sequence.is_none());
                let content = content.as_ref().expect("native goal object");
                assert_eq!(content["objective"], "phlogiston pipeline overhaul");
                assert_eq!(content["status"], "active");
                assert_eq!(content["threadId"], "thread-1");
                assert_eq!(content["tokensUsed"], 42);
                assert!(content.get("revision").is_none());
            }
            other => panic!("expected WorkflowLifecycle Goal, got {other:?}"),
        }
    }

    #[test]
    fn update_plan_preserves_arguments_as_workflow_lifecycle_plan() {
        // Binding shape: write_codex_rollout_with_structured_events / update_plan_row.
        let native = json!({
            "timestamp": "2026-01-03T00:00:03.000Z",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "update_plan",
                "call_id": "call-plan-1",
                "arguments": "{\"explanation\":\"why\",\"plan\":[{\"step\":\"sweep telemetry\",\"status\":\"in_progress\"},{\"step\":\"ship\",\"status\":\"pending\"}]}"
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(10, 20).unwrap();
        let record_id = codex_native_record_id("codex-structured", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "codex-structured",
            Some("codex-structured"),
            record_id,
            range,
        )
        .unwrap();
        let facts = envelope.facts();
        assert_eq!(facts.len(), 2);
        assert!(matches!(
            &facts[0],
            CanonicalObservationFactV1::ToolInvocation { name, arguments, .. }
                if name == "update_plan" && arguments.is_null()
        ));
        match &facts[1] {
            CanonicalObservationFactV1::WorkflowLifecycle {
                semantic_kind: CanonicalWorkflowSemanticKindV1::Plan,
                provider_reference,
                item_id,
                revision,
                status,
                item_order,
                content,
                ..
            } => {
                assert_eq!(provider_reference.as_deref(), Some("call-plan-1"));
                assert!(item_id.is_none());
                assert!(revision.is_none());
                assert!(status.is_none());
                assert!(item_order.is_none());
                let content = content.as_ref().expect("preserved update_plan arguments");
                assert_eq!(content["explanation"], "why");
                assert_eq!(content["plan"][0]["step"], "sweep telemetry");
                assert_eq!(content["plan"][0]["status"], "in_progress");
                assert_eq!(content["plan"][1]["step"], "ship");
                assert_eq!(content["plan"][1]["status"], "pending");
            }
            other => panic!("expected WorkflowLifecycle Plan, got {other:?}"),
        }
    }

    #[test]
    fn task_complete_and_turn_events_map_exactly_without_lookalikes() {
        // Binding shape: write_codex_rollout_with_structured_events /
        // task_events_become_turn_boundary_rows — singular task_complete only.
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        for (payload, expected_state, expected_status) in [
            (
                json!({
                    "type": "task_started",
                    "turn_id": "turn-1",
                    "started_at": 1_782_000_000i64,
                    "model_context_window": 258_400
                }),
                "task_started",
                None,
            ),
            (
                json!({
                    "type": "task_complete",
                    "turn_id": "turn-1",
                    "duration_ms": 8000,
                    "time_to_first_token_ms": 900,
                    "last_agent_message": "must not become content"
                }),
                "task_complete",
                None,
            ),
            (
                json!({
                    "type": "turn_aborted",
                    "turn_id": "turn-2",
                    "reason": "interrupted",
                    "duration_ms": 5626
                }),
                "turn_aborted",
                None,
            ),
        ] {
            let expected_turn_id = payload
                .get("turn_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let native = json!({
                "timestamp": "2026-01-03T00:00:09.000Z",
                "type": "event_msg",
                "payload": payload
            });
            let record_id = codex_native_record_id("turn-session", &native).unwrap();
            let envelope = normalize_codex_observation(
                &native,
                "turn-session",
                Some("turn-session"),
                record_id,
                range,
            )
            .unwrap();
            match &envelope.facts()[0] {
                CanonicalObservationFactV1::WorkflowLifecycle {
                    semantic_kind: CanonicalWorkflowSemanticKindV1::Task,
                    provider_reference,
                    state,
                    status,
                    revision,
                    content,
                    ..
                } => {
                    assert_eq!(state.as_deref(), Some(expected_state));
                    assert_eq!(status.as_deref(), expected_status);
                    assert!(revision.is_none());
                    assert_eq!(provider_reference.as_deref(), expected_turn_id.as_deref());
                    let content = content.as_ref().expect("turn content");
                    assert_eq!(content["type"], expected_state);
                    assert!(content.get("last_agent_message").is_none());
                    if expected_state == "turn_aborted" {
                        assert_eq!(content["reason"], "interrupted");
                    }
                }
                other => {
                    panic!("expected WorkflowLifecycle Task for {expected_state}, got {other:?}")
                }
            }
        }

        // Lookalikes must not become Task lifecycle.
        for lookalike in ["task_completed", "task_failed"] {
            let native = json!({
                "timestamp": "2026-01-03T00:00:09.000Z",
                "type": "event_msg",
                "payload": {"type": lookalike, "turn_id": "turn-x"}
            });
            let record_id = codex_native_record_id("turn-session", &native).unwrap();
            let envelope = normalize_codex_observation(
                &native,
                "turn-session",
                Some("turn-session"),
                record_id,
                range,
            )
            .unwrap();
            assert!(
                matches!(
                    &envelope.facts()[0],
                    CanonicalObservationFactV1::Unknown {
                        native_kind,
                        state: CanonicalUnknownStateV1::Unsupported,
                    } if native_kind == lookalike
                ),
                "lookalike {lookalike} must stay Unknown, got {:?}",
                envelope.facts()
            );
        }
    }

    #[test]
    fn goal_context_response_item_remains_message_only() {
        // Goal-context prose is not a fixture-backed native lifecycle record.
        // It must remain a Message rather than becoming inferred Goal state.
        let native = json!({
            "timestamp": "2026-01-01T00:00:15.100Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "<codex_internal_context source=\"goal\"><objective>ensure all provider session messages are ingested</objective>\nToken budget: 12000\nTokens remaining: 11000</codex_internal_context>"
                }]
            }
        });
        let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 1).unwrap();
        let record_id = codex_native_record_id("codex-goal-context", &native).unwrap();
        let envelope = normalize_codex_observation(
            &native,
            "codex-goal-context",
            Some("codex-goal-context"),
            record_id,
            range,
        )
        .unwrap();
        let facts = envelope.facts();
        assert_eq!(facts.len(), 1);
        assert!(matches!(
            &facts[0],
            CanonicalObservationFactV1::Message {
                role: CanonicalMessageRoleV1::User,
                ..
            }
        ));
    }
}
