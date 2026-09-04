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
    use tracedecay_domain::{CanonicalObservationEnvelopeV1, ObservationScopeV1};

    use crate::admission::HostAdmission;
    use crate::admission::test_support::MemoryHostAdmission;
    use crate::observation::ObservationCancellation;
    use crate::runtime::codex::try_admit_codex_jsonl_observations_for_project_with_admission;

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
        let status = std::process::Command::new(
            tracedecay_runtime_core::git::try_git_program()
                .expect("absolute git executable should resolve"),
        )
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
        assert!(project.scope_matcher().accepts(Some(&project_src)));
        assert!(!project.scope_matcher().accepts(Some(&other)));

        let registered = vec![project_root];
        let profile = CodexObservationAdmission::Profile {
            session_id: Some("session-1"),
            registered_roots: &registered,
        };
        assert!(!profile.scope_matcher().accepts(Some(&project_src)));
        assert!(profile.scope_matcher().accepts(Some(&other)));
        assert!(profile.scope_matcher().accepts(None));
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
            "{}/../../tests/fixtures/provider_normalization/codex/{name}.input.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn load_codex_golden_expected(name: &str, stable_record_id: &str) -> Value {
        let path = format!(
            "{}/../../tests/fixtures/provider_normalization/codex/{name}.expected_envelope.json",
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
    fn checked_in_codex_goldens_match_parser_envelopes() {
        for (name, start, end) in [
            ("agent_message", 0, 1),
            ("session_meta", 0, 1),
            ("function_call", 40, 80),
            ("thread_goal_updated", 0, 1),
        ] {
            let range = tracedecay_domain::ObservationSourceRangeV1::new(start, end).unwrap();
            assert_codex_golden_envelope(
                name,
                "codex-golden-session",
                Some("codex-golden-session"),
                range,
            );
        }
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
    fn response_only_goal_context_keeps_legacy_stable_identity() {
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
        assert_eq!(
            envelope.relations().message_id(),
            Some(envelope.stable_record_id())
        );
        assert!(matches!(
            &envelope.facts()[0],
            CanonicalObservationFactV1::Message {
                role: tracedecay_domain::CanonicalMessageRoleV1::User,
                content,
                ..
            } if tracedecay_store::codex_message_visible_text(content).contains(
                "ensure all provider session messages are ingested"
            )
        ));
    }

    #[tokio::test]
    async fn current_user_message_reaches_project_admission_and_projection() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let transcript = temp.path().join("rollout.jsonl");
        let lines = [
            json!({
                "timestamp": "2026-09-04T12:00:00.000Z",
                "type": "session_meta",
                "payload": {"id": "session-current-user", "cwd": project}
            }),
            json!({
                "timestamp": "2026-09-04T12:00:01.004Z",
                "type": "event_msg",
                "payload": {
                    "type": "item_completed",
                    "thread_id": "thread-1",
                    "turn_id": "turn-1",
                    "item": {
                        "type": "UserMessage",
                        "id": "user-item-1",
                        "content": [{
                            "type": "text",
                            "text": "Find the callers of publish_generation."
                        }]
                    }
                }
            }),
        ];
        std::fs::write(
            &transcript,
            lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let project_id = ProjectId::new("project-current-user").unwrap();
        let admission = MemoryHostAdmission::default();

        let progress = try_admit_codex_jsonl_observations_for_project_with_admission(
            &transcript,
            &project,
            project_id.clone(),
            &admission,
            None,
        )
        .await
        .unwrap();

        assert_eq!(progress.frames_persisted, 2);
        let observations = admission.observations();
        let message = observations.iter().find_map(|stored| {
            serde_json::from_value::<CanonicalObservationEnvelopeV1>(
                stored.observation().payload().clone(),
            )
            .ok()
            .filter(|envelope| {
                envelope.facts().iter().any(|fact| {
                    matches!(
                        fact,
                        CanonicalObservationFactV1::Message {
                            role: CanonicalMessageRoleV1::User,
                            content,
                            ..
                        } if content.to_string().contains("publish_generation")
                    )
                })
            })
        });
        assert!(
            message.is_some(),
            "current UserMessage must survive admission"
        );
        let scope = ObservationScopeV1::Project { project_id };
        let projected = admission
            .drain_projection_queue("codex", &scope, &ObservationCancellation::default(), 8)
            .await
            .unwrap();
        assert_eq!(projected.projected, 2);
        assert_eq!(admission.pending_projection_count(), 0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod message_record_tests {
    use std::path::Path;

    use serde_json::{Value, json};

    use super::super::records::message_from_line;
    use super::{CodexMeta, CodexSource, StoredCursor, TranscriptSource};

    fn meta() -> CodexMeta {
        CodexMeta {
            cwd: "/tmp/project".into(),
            session_id: "session-1".to_string(),
            model: None,
            git: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            agent_nickname: None,
            agent_role: None,
            thread_source: None,
        }
    }

    #[test]
    fn current_user_message_item_becomes_one_canonical_user_row() {
        let record = json!({
            "timestamp": "2026-09-04T12:00:01.004Z",
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "thread_id": "thread-1",
                "turn_id": "turn-1",
                "item": {
                    "type": "UserMessage",
                    "id": "user-item-1",
                    "client_id": "client-1",
                    "content": [{
                        "type": "text",
                        "text": "Find the callers of publish_generation.",
                        "text_elements": []
                    }]
                }
            }
        });

        let message = message_from_line(
            &record,
            &meta(),
            Some("gpt-5.6-sol"),
            Path::new("/tmp/rollout.jsonl"),
            42,
        )
        .unwrap();

        assert_eq!(message.role, "user");
        assert_eq!(message.message_id, "session-1:user-item-1");
        assert!(
            message
                .text
                .contains("Find the callers of publish_generation.")
        );
        let metadata: Value =
            serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(metadata["source"], "codex_rollout");
        assert_eq!(metadata["source_event"], "item_completed");
    }

    #[test]
    fn malformed_duplicate_source_and_non_user_items_are_not_messages() {
        let current_response_duplicate = json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "same prompt"}]
            }
        });
        let missing_item_id = json!({
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "item": {
                    "type": "UserMessage",
                    "content": [{"type": "text", "text": "no stable identity"}]
                }
            }
        });
        let agent_item = json!({
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "item": {
                    "type": "AgentMessage",
                    "id": "agent-item-1",
                    "content": [{"type": "Text", "text": "assistant reply"}]
                }
            }
        });
        let no_visible_text = json!({
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "item": {
                    "type": "UserMessage",
                    "id": "image-only-item",
                    "content": [{"type": "image", "image_url": "redacted"}]
                }
            }
        });

        for record in [
            current_response_duplicate,
            missing_item_id,
            agent_item,
            no_visible_text,
        ] {
            assert!(
                message_from_line(&record, &meta(), None, Path::new("/tmp/rollout.jsonl"), 42,)
                    .is_none()
            );
        }
    }

    #[test]
    fn current_goal_context_keeps_item_identity_and_collapses_paired_shapes() {
        let goal = concat!(
            "<codex_internal_context source=\"goal\">",
            "<objective>finish the canonical admission fix</objective>\n",
            "Token budget: 12000\nTokens remaining: 11000",
            "</codex_internal_context>"
        );
        let current = json!({
            "timestamp": "2026-09-04T12:00:01.004Z",
            "type": "event_msg",
            "payload": {
                "type": "item_completed",
                "item": {
                    "type": "UserMessage",
                    "id": "goal-user-item-1",
                    "content": [{"type": "text", "text": goal}]
                }
            }
        });
        let message =
            message_from_line(&current, &meta(), None, Path::new("/tmp/rollout.jsonl"), 42)
                .unwrap();
        assert_eq!(message.kind.as_deref(), Some("goal_context"));
        assert_eq!(message.message_id, "session-1:goal-user-item-1");

        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let transcript = temp.path().join("rollout.jsonl");
        let response = json!({
            "timestamp": "2026-09-04T12:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "msg-goal-1",
                "role": "user",
                "content": [{"type": "input_text", "text": goal}]
            }
        });
        let lines = [
            json!({
                "timestamp": "2026-09-04T12:00:00.000Z",
                "type": "session_meta",
                "payload": {"id": "session-1", "cwd": project}
            }),
            response,
            current.clone(),
            current,
        ];
        std::fs::write(
            &transcript,
            lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();

        let parsed = CodexSource::with_home(temp.path())
            .parse_new(&transcript, StoredCursor::default(), &project, None)
            .unwrap();
        let goal_rows = parsed
            .messages
            .iter()
            .filter(|message| message.kind.as_deref() == Some("goal_context"))
            .collect::<Vec<_>>();
        assert_eq!(goal_rows.len(), 1);
        assert_eq!(goal_rows[0].message_id, "session-1:goal-user-item-1");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod source_matcher_cache_tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use tempfile::TempDir;

    use super::CodexSource;
    use crate::runtime::shared::{ProjectRootMatcherCache, StoredCursor};
    use crate::runtime::source::TranscriptSource;
    use tracedecay_runtime_core::git_discovery::{
        GitDiscoveryUnknown, GitRepositoryIdentity, GitRepositoryIdentityOutcome,
    };

    static UNKNOWN_PATH_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

    fn retrying_identity(path: &Path) -> GitRepositoryIdentityOutcome {
        let root = path
            .ancestors()
            .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "repo"))
            .unwrap_or(path);
        if UNKNOWN_PATH_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 1 {
            return GitRepositoryIdentityOutcome::Unknown(GitDiscoveryUnknown::DeadlineExceeded);
        }
        GitRepositoryIdentityOutcome::Resolved(GitRepositoryIdentity {
            worktree_root: root.to_path_buf(),
            git_dir: root.join(".git"),
            common_dir: root.join(".git"),
        })
    }

    fn write_rollout(path: &Path, session_id: &str, cwd: &Path) {
        let lines = [
            json!({
                "timestamp": "2026-01-01T00:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "cwd": cwd,
                    "model": "gpt-5.5"
                }
            }),
            json!({
                "timestamp": "2026-01-01T00:00:01.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "user_message",
                    "message": format!("message from {session_id}")
                }
            }),
        ];
        std::fs::write(
            path,
            lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
    }

    #[test]
    fn codex_source_reuses_project_matcher_across_parse_calls() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("repo");
        let nested_cwd = project_root.join("packages/app");
        std::fs::create_dir_all(&nested_cwd).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&project_root)
            .status()
            .unwrap();
        assert!(status.success());

        let first_path = temp.path().join("first.jsonl");
        let second_path = temp.path().join("second.jsonl");
        write_rollout(&first_path, "first-session", &nested_cwd);
        write_rollout(&second_path, "second-session", &nested_cwd);
        let source = CodexSource::with_home(temp.path());

        let first = source
            .parse_new(&first_path, StoredCursor::default(), &project_root, None)
            .unwrap();
        assert_eq!(first.messages.len(), 1);
        let first_metadata: serde_json::Value =
            serde_json::from_str(first.messages[0].metadata_json.as_deref().unwrap()).unwrap();
        let first_worktree = first_metadata["codex_turn_worktree"].clone();
        assert!(first_worktree.is_string());

        std::fs::rename(project_root.join(".git"), project_root.join(".git.hidden")).unwrap();
        let second = source
            .parse_new(&second_path, StoredCursor::default(), &project_root, None)
            .unwrap();
        assert_eq!(second.messages.len(), 1);
        let second_metadata: serde_json::Value =
            serde_json::from_str(second.messages[0].metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(second_metadata["codex_turn_worktree"], first_worktree);
    }

    #[test]
    fn codex_unknown_membership_retries_without_advancing_cursor() {
        UNKNOWN_PATH_ATTEMPTS.store(0, Ordering::SeqCst);
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("repo");
        let nested_cwd = project_root.join("packages/app");
        std::fs::create_dir_all(&nested_cwd).unwrap();
        let transcript = temp.path().join("retry.jsonl");
        write_rollout(&transcript, "retry-session", &nested_cwd);
        let mut source = CodexSource::with_home(temp.path());
        source.project_matchers =
            ProjectRootMatcherCache::with_identity_resolver(retrying_identity);

        let previous = StoredCursor::default();
        assert!(
            source
                .parse_new(&transcript, previous, &project_root, None)
                .is_none(),
            "unknown membership must abort before a new cursor can be persisted"
        );

        let retried = source
            .parse_new(&transcript, previous, &project_root, None)
            .expect("unknown membership must be resolved again on retry");
        assert_eq!(retried.messages.len(), 1);
        assert!(retried.new_cursor.position > previous.position);
        assert_eq!(UNKNOWN_PATH_ATTEMPTS.load(Ordering::SeqCst), 3);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod recent_first_discovery_tests {
    use std::collections::BTreeSet;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use tempfile::TempDir;

    use super::CodexSource;
    use crate::admission::test_support::MemoryHostAdmission;
    use crate::runtime::codex::{
        CodexCorpusEpoch, CodexDiscoveryDelivery, CodexDiscoveryFrontier, CodexDiscoveryHub,
        CodexDiscoverySourceKey, CodexDiscoveryState, CodexExactSessionPathAuthority,
        CodexIndexedPath, CodexReplayIndex, EXACT_HOOK_DISCOVERY_UNITS_PER_CALL,
        IDLE_FULL_VALIDATION_CYCLES, MAX_EXACT_HOOK_SESSION_REQUESTS,
        MAX_EXACT_HOOK_SOURCE_AUTHORITIES, MAX_SCAN_DEPTH, indexed_replay_pass,
        replay_index_entries_visited_for_test, reset_replay_index_entries_visited_for_test,
    };
    use crate::runtime::source::{
        HostProviderCoverage, TranscriptDiscoveryBounds, TranscriptIngestError,
        persist_codex_history_frontier, persist_host_provider_coverage,
        read_codex_history_frontier, read_host_provider_coverage,
    };

    /// Creates `sessions/YYYY/MM/DD/rollout-<name>.jsonl` under the Codex home.
    fn write_dated_rollout(home: &Path, date: (&str, &str, &str), name: &str) -> PathBuf {
        let dir = home
            .join(".codex/sessions")
            .join(date.0)
            .join(date.1)
            .join(date.2);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-{name}.jsonl"));
        std::fs::write(&path, "{}\n").unwrap();
        path
    }

    fn retained_pass(
        source: &CodexSource,
        state: &mut CodexDiscoveryState,
        bounds: TranscriptDiscoveryBounds,
        frontier: CodexDiscoveryFrontier,
    ) -> super::super::CodexDiscoveryPass {
        let pass = source
            .discover_transcript_paths_with_state(bounds, frontier, state)
            .unwrap();
        state.acknowledge();
        pass
    }

    async fn drain_hub_consumer(
        hub: &CodexDiscoveryHub,
        consumer: &str,
        source: &CodexSource,
        bounds: TranscriptDiscoveryBounds,
        mut frontier: CodexDiscoveryFrontier,
    ) -> (Vec<PathBuf>, CodexDiscoveryFrontier) {
        let mut paths = Vec::new();
        for _ in 0..4096 {
            match hub
                .discover(consumer, source, bounds, frontier)
                .await
                .expect("bounded hub discovery")
            {
                CodexDiscoveryDelivery::Waiting => continue,
                CodexDiscoveryDelivery::Ready(pass) => {
                    paths.extend(pass.report.paths.iter().cloned());
                    frontier = pass.next_frontier;
                    hub.acknowledge(consumer);
                    if frontier.is_complete() {
                        return (paths, frontier);
                    }
                }
            }
        }
        panic!("bounded hub discovery did not converge");
    }

    #[test]
    fn indexed_replay_starts_at_the_acknowledged_btree_position() {
        let mut index = CodexReplayIndex {
            complete: true,
            frontier: CodexDiscoveryFrontier::complete(CodexCorpusEpoch {
                high: 1,
                low: 2,
                files: 8192,
            }),
            ..Default::default()
        };
        for value in 0..8192 {
            index.paths.insert(CodexIndexedPath {
                root_order: 0,
                path: PathBuf::from(format!("/sessions/rollout-{value:05}.jsonl")),
            });
        }
        let position = CodexIndexedPath {
            root_order: 0,
            path: PathBuf::from("/sessions/rollout-07999.jsonl"),
        };
        reset_replay_index_entries_visited_for_test();

        let (pass, _) = indexed_replay_pass(
            &index,
            TranscriptDiscoveryBounds::from_discovered_units(8),
            CodexDiscoveryFrontier::initial(),
            Some(&position),
        )
        .expect("indexed replay page");

        assert_eq!(pass.report.paths.len(), 8);
        assert!(
            replay_index_entries_visited_for_test() <= 9,
            "an acknowledged tail position must not rescan the B-tree prefix"
        );
    }

    #[test]
    fn indexed_replay_preserves_recent_sessions_before_archive() {
        let mut index = CodexReplayIndex {
            complete: true,
            frontier: CodexDiscoveryFrontier::complete(CodexCorpusEpoch {
                high: 1,
                low: 2,
                files: 3,
            }),
            ..Default::default()
        };
        let newest = CodexIndexedPath {
            root_order: 0,
            path: PathBuf::from("/sessions/2026/08/24/rollout-new.jsonl"),
        };
        let oldest = CodexIndexedPath {
            root_order: 0,
            path: PathBuf::from("/sessions/2025/01/01/rollout-old.jsonl"),
        };
        let archived = CodexIndexedPath {
            root_order: 1,
            path: PathBuf::from("/archive/rollout-archived.jsonl"),
        };
        index
            .paths
            .extend([oldest.clone(), archived.clone(), newest.clone()]);

        let (pass, _) = indexed_replay_pass(
            &index,
            TranscriptDiscoveryBounds::from_discovered_units(8),
            CodexDiscoveryFrontier::initial(),
            None,
        )
        .expect("ordered indexed replay");

        assert_eq!(
            pass.report.paths,
            vec![newest.path, oldest.path, archived.path]
        );
    }

    #[test]
    fn exact_hook_source_capacity_never_evicts_an_incomplete_scan() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let mut authority = CodexExactSessionPathAuthority::default();
        for index in 0..MAX_EXACT_HOOK_SOURCE_AUTHORITIES {
            let source = CodexDiscoverySourceKey {
                sessions_dir: PathBuf::from(format!("/sessions/{index}")),
                archived_sessions_dir: PathBuf::from(format!("/archive/{index}")),
            };
            authority
                .source_index_or_admit(source)
                .expect("bounded pending source authority");
        }

        let error = authority
            .source_index_or_admit(CodexDiscoverySourceKey {
                sessions_dir: PathBuf::from("/sessions/excess"),
                archived_sessions_dir: PathBuf::from("/archive/excess"),
            })
            .expect_err("an excess source must receive typed backpressure");
        assert!(matches!(
            error,
            TranscriptIngestError::BackgroundResourceUnavailable {
                provider: "codex",
                resource: "exact-session source lookup capacity",
            }
        ));
    }

    #[test]
    fn exact_hook_request_capacity_never_evicts_an_incomplete_lookup() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let mut authority = CodexExactSessionPathAuthority::default();
        let source = authority
            .source_index_or_admit(CodexDiscoverySourceKey {
                sessions_dir: PathBuf::from("/sessions"),
                archived_sessions_dir: PathBuf::from("/archive"),
            })
            .unwrap();
        for index in 0..MAX_EXACT_HOOK_SESSION_REQUESTS {
            authority
                .request_index_or_admit(source, format!("session-{index}"))
                .expect("bounded pending request authority");
        }

        let error = authority
            .request_index_or_admit(source, "session-excess".to_owned())
            .expect_err("an excess request must receive typed backpressure");
        assert!(matches!(
            error,
            TranscriptIngestError::BackgroundResourceUnavailable {
                provider: "codex",
                resource: "exact-session request lookup capacity",
            }
        ));
    }

    #[tokio::test]
    async fn shared_hub_fans_one_immutable_generation_to_profile_and_projects() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let expected = write_dated_rollout(home, ("2026", "08", "23"), "shared");
        let hub = CodexDiscoveryHub::default();
        for consumer in ["profile", "project-a", "project-b"] {
            hub.register(consumer, Some(home));
        }
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(128);
        let first = match hub
            .discover(
                "profile",
                &source,
                bounds,
                CodexDiscoveryFrontier::initial(),
            )
            .await
            .unwrap()
        {
            CodexDiscoveryDelivery::Ready(pass) => pass,
            CodexDiscoveryDelivery::Waiting => panic!("first scanner unexpectedly waited"),
        };
        assert_eq!(first.report.paths, vec![expected]);
        for consumer in ["project-a", "project-b"] {
            let delivered = match hub
                .discover(consumer, &source, bounds, CodexDiscoveryFrontier::initial())
                .await
                .unwrap()
            {
                CodexDiscoveryDelivery::Ready(pass) => pass,
                CodexDiscoveryDelivery::Waiting => panic!("queued consumer unexpectedly waited"),
            };
            assert!(Arc::ptr_eq(&first, &delivered));
        }
    }

    #[tokio::test]
    async fn unacknowledged_budget_delivery_reuses_the_exact_immutable_generation() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let expected = write_dated_rollout(home, ("2026", "08", "23"), "budget-retry");
        let hub = CodexDiscoveryHub::default();
        hub.register("profile", Some(home));
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(128);
        let frontier = CodexDiscoveryFrontier::initial();

        let first = match hub
            .discover("profile", &source, bounds, frontier)
            .await
            .unwrap()
        {
            CodexDiscoveryDelivery::Ready(pass) => pass,
            CodexDiscoveryDelivery::Waiting => panic!("initial delivery unexpectedly waited"),
        };
        assert_eq!(first.report.paths, vec![expected]);
        let retry = match hub
            .discover("profile", &source, bounds, frontier)
            .await
            .unwrap()
        {
            CodexDiscoveryDelivery::Ready(pass) => pass,
            CodexDiscoveryDelivery::Waiting => panic!("budget retry unexpectedly waited"),
        };

        assert!(
            Arc::ptr_eq(&first, &retry),
            "ordinary byte deferral must retain the immutable pass instead of rescanning"
        );
        assert_eq!(retry.report.files_considered, first.report.files_considered);
    }

    #[tokio::test]
    async fn shared_hub_never_fans_paths_across_source_homes() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let first_home = TempDir::new().unwrap();
        let second_home = TempDir::new().unwrap();
        let first_path = write_dated_rollout(first_home.path(), ("2026", "08", "23"), "first-home");
        let second_path =
            write_dated_rollout(second_home.path(), ("2026", "08", "23"), "second-home");
        let hub = CodexDiscoveryHub::default();
        hub.register("first", Some(first_home.path()));
        hub.register("second", Some(second_home.path()));
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(128);
        let first_source = CodexSource::with_home(first_home.path());
        let second_source = CodexSource::with_home(second_home.path());

        let first = hub
            .discover(
                "first",
                &first_source,
                bounds,
                CodexDiscoveryFrontier::initial(),
            )
            .await
            .unwrap();
        assert!(matches!(first, CodexDiscoveryDelivery::Ready(_)));
        let (second, second_frontier) = drain_hub_consumer(
            &hub,
            "second",
            &second_source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;
        assert_eq!(second, vec![second_path]);
        assert!(second_frontier.is_complete());
        assert!(!second.contains(&first_path));
    }

    #[tokio::test]
    async fn slow_shared_consumer_falls_to_replay_without_blocking_healthy_progress() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let file_count =
            crate::runtime::jsonl_observation_admission::shared_jsonl_preparation_workers() + 1;
        for index in 0..file_count {
            write_dated_rollout(home, ("2026", "08", "23"), &format!("slow-{index:02}"));
        }
        let hub = CodexDiscoveryHub::default();
        hub.register("healthy", Some(home));
        hub.register("slow", Some(home));
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(128);
        let mut frontier = CodexDiscoveryFrontier::initial();
        let mut delivered = 0usize;
        for _ in 0..128 {
            let pass = match hub
                .discover("healthy", &source, bounds, frontier)
                .await
                .unwrap()
            {
                CodexDiscoveryDelivery::Ready(pass) => pass,
                CodexDiscoveryDelivery::Waiting => continue,
            };
            delivered = delivered.saturating_add(pass.report.paths.len());
            frontier = pass.next_frontier;
            hub.acknowledge("healthy");
            if frontier.is_complete() {
                break;
            }
        }
        assert_eq!(delivered, file_count);
        assert!(frontier.is_complete());

        let (replay, replay_frontier) = drain_hub_consumer(
            &hub,
            "slow",
            &source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;
        assert_eq!(replay.len(), file_count);
        assert!(replay_frontier.is_complete());
    }

    #[tokio::test]
    async fn two_laggers_share_one_memory_bounded_replay_enumeration() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let expected = (0..73)
            .map(|index| {
                write_dated_rollout(home, ("2026", "08", "23"), &format!("lag-{index:03}"))
            })
            .collect::<BTreeSet<_>>();
        let hub = CodexDiscoveryHub::default();
        hub.register("healthy", Some(home));
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let (_, healthy_frontier) = drain_hub_consumer(
            &hub,
            "healthy",
            &source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;
        assert!(healthy_frontier.is_complete());
        hub.register("lagger-a", Some(home));
        hub.register("lagger-b", Some(home));

        let (first, first_frontier) = drain_hub_consumer(
            &hub,
            "lagger-a",
            &source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;
        let healthy_probe = hub
            .discover("healthy", &source, bounds, healthy_frontier)
            .await
            .expect("healthy consumer remains responsive during replay");
        assert!(matches!(healthy_probe, CodexDiscoveryDelivery::Ready(_)));
        hub.acknowledge("healthy");
        let (second, second_frontier) = drain_hub_consumer(
            &hub,
            "lagger-b",
            &source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;

        assert_eq!(first.into_iter().collect::<BTreeSet<_>>(), expected);
        assert_eq!(second.into_iter().collect::<BTreeSet<_>>(), expected);
        assert!(first_frontier.is_complete());
        assert!(second_frontier.is_complete());
        let inner = hub.inner.lock().unwrap();
        let index = inner
            .replay_indexes
            .get(&source.discovery_key())
            .expect("one source replay index");
        assert_eq!(index.completed_enumerations, 1);
        assert_eq!(index.files_considered, 73);
        assert!(!index._memory.is_empty());
    }

    #[tokio::test]
    async fn completed_secondary_source_rebuilds_after_add_and_delete() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let primary = TempDir::new().unwrap();
        let secondary = TempDir::new().unwrap();
        write_dated_rollout(primary.path(), ("2026", "08", "23"), "primary");
        let removed = write_dated_rollout(secondary.path(), ("2026", "08", "23"), "secondary-old");
        let hub = CodexDiscoveryHub::default();
        hub.register("primary", Some(primary.path()));
        let primary_source = CodexSource::with_home(primary.path());
        let secondary_source = CodexSource::with_home(secondary.path());
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let (_, primary_frontier) = drain_hub_consumer(
            &hub,
            "primary",
            &primary_source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;
        assert!(primary_frontier.is_complete());
        hub.register("secondary", Some(secondary.path()));
        let (initial, secondary_frontier) = drain_hub_consumer(
            &hub,
            "secondary",
            &secondary_source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;
        assert_eq!(initial, vec![removed.clone()]);
        let (unchanged, unchanged_frontier) = drain_hub_consumer(
            &hub,
            "secondary",
            &secondary_source,
            bounds,
            secondary_frontier,
        )
        .await;
        assert!(unchanged.is_empty());
        assert_eq!(unchanged_frontier, secondary_frontier);
        assert_eq!(
            hub.inner
                .lock()
                .unwrap()
                .replay_indexes
                .get(&secondary_source.discovery_key())
                .expect("secondary replay index")
                .completed_enumerations,
            1,
            "an unchanged retained probe must not enumerate the corpus again"
        );
        std::fs::remove_file(&removed).unwrap();
        let added = write_dated_rollout(secondary.path(), ("2026", "08", "24"), "secondary-new");

        let (rebuilt, rebuilt_frontier) = drain_hub_consumer(
            &hub,
            "secondary",
            &secondary_source,
            bounds,
            secondary_frontier,
        )
        .await;

        assert_eq!(rebuilt, vec![added]);
        assert!(rebuilt_frontier.is_complete());
        assert_ne!(rebuilt_frontier, secondary_frontier);
        let inner = hub.inner.lock().unwrap();
        let index = inner
            .replay_indexes
            .get(&secondary_source.discovery_key())
            .expect("secondary replay index");
        assert_eq!(index.completed_enumerations, 2);
        assert!(!index.paths.iter().any(|entry| entry.path == removed));
    }

    #[tokio::test]
    async fn replay_index_retires_after_the_last_source_consumer() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let primary = TempDir::new().unwrap();
        let secondary = TempDir::new().unwrap();
        write_dated_rollout(primary.path(), ("2026", "08", "23"), "primary");
        write_dated_rollout(secondary.path(), ("2026", "08", "23"), "secondary");
        let hub = CodexDiscoveryHub::default();
        let primary_source = CodexSource::with_home(primary.path());
        let secondary_source = CodexSource::with_home(secondary.path());
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        hub.register("primary", Some(primary.path()));
        let (_, primary_frontier) = drain_hub_consumer(
            &hub,
            "primary",
            &primary_source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;
        assert!(primary_frontier.is_complete());
        hub.register("secondary-a", Some(secondary.path()));
        hub.register("secondary-b", Some(secondary.path()));
        let (_, secondary_frontier) = drain_hub_consumer(
            &hub,
            "secondary-a",
            &secondary_source,
            bounds,
            CodexDiscoveryFrontier::initial(),
        )
        .await;
        assert!(secondary_frontier.is_complete());
        let source_key = secondary_source.discovery_key();
        assert!(
            hub.inner
                .lock()
                .unwrap()
                .replay_indexes
                .contains_key(&source_key)
        );

        hub.deregister("secondary-a");
        assert!(
            hub.inner
                .lock()
                .unwrap()
                .replay_indexes
                .contains_key(&source_key)
        );
        hub.deregister("secondary-b");
        assert!(
            !hub.inner
                .lock()
                .unwrap()
                .replay_indexes
                .contains_key(&source_key),
            "the last source consumer releases retained paths and scanner memory"
        );
    }

    #[tokio::test]
    async fn duplicate_registration_release_keeps_surviving_consumer_live() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        write_dated_rollout(home, ("2026", "08", "23"), "lease");
        let hub = CodexDiscoveryHub::default();
        hub.register("same", Some(home));
        hub.register("same", Some(home));
        hub.deregister("same");

        let delivery = hub
            .discover(
                "same",
                &CodexSource::with_home(home),
                TranscriptDiscoveryBounds::from_discovered_units(8),
                CodexDiscoveryFrontier::initial(),
            )
            .await
            .unwrap();
        assert!(matches!(delivery, CodexDiscoveryDelivery::Ready(_)));
    }

    #[tokio::test]
    async fn deregistered_consumer_is_never_implicitly_resurrected() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let hub = CodexDiscoveryHub::default();
        hub.register("retired", Some(home));
        hub.deregister("retired");

        let result = hub
            .discover(
                "retired",
                &CodexSource::with_home(home),
                TranscriptDiscoveryBounds::from_discovered_units(8),
                CodexDiscoveryFrontier::initial(),
            )
            .await;
        assert!(matches!(
            result,
            Err(TranscriptIngestError::InvalidCodexDiscoveryFrontier { .. })
        ));
    }

    #[test]
    fn exact_hook_session_lookup_converges_in_bounded_retained_slices() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        // Discovery reports resolved paths, so the fixture must be built on a
        // resolved home: macOS hands out `/var/folders/...` for a tempdir the
        // filesystem itself names `/private/var/folders/...`.
        let resolved_home = temp.path().canonicalize().unwrap();
        let home = resolved_home.as_path();
        let directory = home.join(".codex/sessions/2026/08/23");
        std::fs::create_dir_all(&directory).unwrap();
        for index in 0..4_100 {
            std::fs::write(
                directory.join(format!("rollout-distractor-{index:04}.jsonl")),
                b"{}\n",
            )
            .unwrap();
        }
        let session_id = "0198-session-beyond-default-budget";
        let expected = directory.join(format!("rollout-2026-08-23-{session_id}.jsonl"));
        std::fs::write(&expected, b"{}\n").unwrap();

        let source = CodexSource::with_home(home);
        let mut calls = 0_u64;
        let paths = loop {
            calls += 1;
            let lookup = source
                .find_session_transcript_paths_bounded(session_id)
                .unwrap();
            assert!(
                lookup.files_considered <= EXACT_HOOK_DISCOVERY_UNITS_PER_CALL as u64,
                "one hook call exceeded its filesystem work budget"
            );
            if !lookup.paths.is_empty() {
                break lookup.paths;
            }
            assert!(lookup.source_deferred);
            assert!(calls < 100, "retained lookup did not converge");
        };

        assert!(calls > 1, "fixture did not exercise retained continuation");
        assert_eq!(paths, vec![expected]);
        let cached = source
            .find_session_transcript_paths_bounded(session_id)
            .unwrap();
        assert_eq!(cached.files_considered, 0);
        assert_eq!(cached.paths, paths);
    }

    #[cfg(unix)]
    #[test]
    fn exact_hook_session_lookup_follows_and_deduplicates_file_symlinks() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        // Discovery reports resolved paths, so the fixture must be built on a
        // resolved home: macOS hands out `/var/folders/...` for a tempdir the
        // filesystem itself names `/private/var/folders/...`.
        let resolved_home = temp.path().canonicalize().unwrap();
        let home = resolved_home.as_path();
        let directory = home.join(".codex/sessions/2026/08/23");
        std::fs::create_dir_all(&directory).unwrap();
        let session_id = "0198-symlink-session";
        let target = directory.join(format!("rollout-{session_id}.jsonl"));
        std::fs::write(&target, b"{}\n").unwrap();
        symlink(
            &target,
            directory.join(format!("rollout-copy-{session_id}.jsonl")),
        )
        .unwrap();

        let lookup = CodexSource::with_home(home)
            .find_session_transcript_paths_bounded(session_id)
            .unwrap();

        assert_eq!(lookup.paths, vec![target]);
    }

    #[test]
    fn distinct_exact_hook_ids_share_one_monotonic_source_index() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        // Discovery reports resolved paths, so the fixture must be built on a
        // resolved home: macOS hands out `/var/folders/...` for a tempdir the
        // filesystem itself names `/private/var/folders/...`.
        let resolved_home = temp.path().canonicalize().unwrap();
        let home = resolved_home.as_path();
        let directory = home.join(".codex/sessions/2026/08/23");
        std::fs::create_dir_all(&directory).unwrap();
        let target_id = "indexed-before-late-request";
        let expected = directory.join(format!("rollout-{target_id}.jsonl"));
        std::fs::write(&expected, b"{}\n").unwrap();
        for index in 0..192 {
            std::fs::write(
                directory.join(format!("rollout-index-{index:04}.jsonl")),
                b"{}\n",
            )
            .unwrap();
        }
        let source = CodexSource::with_home(home);

        let mut completed = false;
        for index in 0..MAX_EXACT_HOOK_SESSION_REQUESTS {
            let lookup = source
                .find_session_transcript_paths_bounded(&format!("missing-drive-{index:03}"))
                .expect("distinct lookup must advance the shared source sweep");
            if !lookup.source_deferred {
                completed = true;
                break;
            }
        }
        assert!(
            completed,
            "distinct IDs did not converge the retained source sweep"
        );
        for index in 0..80 {
            source
                .find_session_transcript_paths_bounded(&format!("missing-after-{index:03}"))
                .expect("completed requests may rotate without resetting source discovery");
        }

        let target = source
            .find_session_transcript_paths_bounded(target_id)
            .expect("late target must resolve from the retained source index");
        assert_eq!(target.files_considered, 0);
        assert_eq!(target.paths, vec![expected]);
    }

    #[test]
    fn stale_exact_lookup_lease_cannot_reinsert_an_evicted_source() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let source = CodexSource::with_home(temp.path());
        let key = source.discovery_key();
        let mut authority = CodexExactSessionPathAuthority::default();
        let stale_index = authority.source_index_or_admit(key.clone()).unwrap();
        authority
            .request_index_or_admit(stale_index, "stale".to_owned())
            .unwrap();
        let stale_lease = authority.sources[stale_index].lease;
        authority.sources.remove(stale_index);

        let replacement_index = authority.source_index_or_admit(key.clone()).unwrap();
        let replacement_lease = authority.sources[replacement_index].lease;

        assert_ne!(stale_lease, replacement_lease);
        assert!(matches!(
            authority.source_for_lease_mut(&key, stale_lease, "stale exact lookup lease"),
            Err(TranscriptIngestError::InvalidCodexDiscoveryFrontier { .. })
        ));
        assert!(authority.sources[replacement_index].discovery.is_some());
    }

    /// The starvation regression: with a historical backlog far larger than the
    /// discovery file cap, one pass must still discover TODAY's session first
    /// instead of exhausting the cap on the oldest days.
    #[test]
    fn codex_discovery_serves_newest_sessions_before_backlog() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        // 30 historical days x 4 rollouts = 120 backlog files.
        for day in 1..=30 {
            for item in 0..4 {
                write_dated_rollout(
                    home,
                    ("2025", "11", &format!("{day:02}")),
                    &format!("old-{day:02}-{item}"),
                );
            }
        }
        let today = write_dated_rollout(home, ("2026", "08", "17"), "today");

        // Cap far below the backlog so oldest-first discovery could never
        // reach the newest file.
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(16);
        let source = CodexSource::with_home(home);
        let mut state = CodexDiscoveryState::default();
        let mut pass = retained_pass(
            &source,
            &mut state,
            bounds,
            CodexDiscoveryFrontier::initial(),
        );
        while pass.report.paths.is_empty() {
            pass = retained_pass(
                &source,
                &mut state,
                bounds,
                CodexDiscoveryFrontier::initial(),
            );
        }

        assert_eq!(
            pass.report.paths.first(),
            Some(&today),
            "the newest session must be discovered first, before any backlog"
        );
        assert!(
            pass.report.is_truncated(),
            "an over-cap backlog must report truncation so catch-up stays scheduled"
        );
        assert!(pass.report.paths.len() <= bounds.max_files);
    }

    /// An unchanged completed corpus is truly idle: it must not hand the same
    /// recent slice back to the JSONL scanner on every background poll.
    #[test]
    fn codex_complete_frontier_idles_without_selecting_files() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let active = write_dated_rollout(home, ("2026", "08", "17"), "today");
        let source = CodexSource::with_home(home);
        let mut state = CodexDiscoveryState::default();
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(16);

        let mut completed = retained_pass(
            &source,
            &mut state,
            bounds,
            CodexDiscoveryFrontier::initial(),
        );
        while !completed.next_frontier.is_complete() {
            completed = retained_pass(
                &source,
                &mut state,
                bounds,
                CodexDiscoveryFrontier::initial(),
            );
        }
        assert!(completed.next_frontier.is_complete());

        let idle = retained_pass(&source, &mut state, bounds, completed.next_frontier);
        assert!(idle.next_frontier.is_complete());
        assert!(idle.report.paths.is_empty());
        assert!(!idle.report.is_truncated());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&active)
            .unwrap();
        file.write_all(b"{}\n").unwrap();
        let awakened = retained_pass(&source, &mut state, bounds, completed.next_frontier);
        assert!(awakened.report.is_truncated());
        assert!(!awakened.next_frontier.is_complete());
    }

    /// A file-count watermark cannot distinguish deletion plus addition. The
    /// corpus epoch must invalidate completion even when cardinality is fixed.
    #[test]
    fn codex_constant_cardinality_replacement_invalidates_completion() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let removed = write_dated_rollout(home, ("2026", "08", "17"), "removed");
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(16);

        let completed = source
            .discover_transcript_paths_with_frontier(bounds, CodexDiscoveryFrontier::initial())
            .unwrap()
            .next_frontier;
        assert!(completed.is_complete());

        std::fs::remove_file(removed).unwrap();
        let added = write_dated_rollout(home, ("2026", "08", "17"), "added");
        let changed = source
            .discover_transcript_paths_with_frontier(bounds, completed)
            .unwrap();

        assert!(changed.report.paths.contains(&added));
        assert_ne!(changed.next_frontier.epoch, completed.epoch);
    }

    #[test]
    #[cfg(unix)]
    fn codex_same_path_same_size_preserved_mtime_replacement_changes_epoch() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let path = write_dated_rollout(home, ("2026", "08", "17"), "replaced");
        let original_mtime =
            filetime::FileTime::from_last_modification_time(&std::fs::metadata(&path).unwrap());
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(16);
        let completed = source
            .discover_transcript_paths_with_frontier(bounds, CodexDiscoveryFrontier::initial())
            .unwrap()
            .next_frontier;

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "{}\n").unwrap();
        filetime::set_file_mtime(&path, original_mtime).unwrap();
        let replaced = source
            .discover_transcript_paths_with_frontier(bounds, completed)
            .unwrap();

        assert_ne!(replaced.next_frontier.epoch, completed.epoch);
        assert_eq!(replaced.report.paths, vec![path]);
    }

    #[test]
    fn codex_recent_selection_keeps_dated_sessions_before_archive() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let dated = write_dated_rollout(home, ("2026", "08", "17"), "dated");
        let archive_dir = home.join(".codex/archived_sessions");
        std::fs::create_dir_all(&archive_dir).unwrap();
        let archived = archive_dir.join("rollout-zzzz.jsonl");
        std::fs::write(&archived, "{}\n").unwrap();

        let pass = CodexSource::with_home(home)
            .discover_transcript_paths_with_frontier(
                TranscriptDiscoveryBounds::from_discovered_units(8),
                CodexDiscoveryFrontier::initial(),
            )
            .unwrap();

        assert_eq!(pass.report.paths.first(), Some(&dated));
        assert!(pass.report.paths.contains(&archived));
    }

    /// Coverage: retained traversal across passes must visit every historical
    /// file — tracked pending work, never a skipped range.
    #[test]
    fn codex_history_frontier_covers_every_backlog_file_across_passes() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let mut all: BTreeSet<PathBuf> = BTreeSet::new();
        for day in 1..=12 {
            for item in 0..2 {
                all.insert(write_dated_rollout(
                    home,
                    ("2025", "11", &format!("{day:02}")),
                    &format!("old-{day:02}-{item}"),
                ));
            }
        }
        all.insert(write_dated_rollout(home, ("2026", "08", "17"), "today"));

        // 8 units/pass: 7 recent + 1 history slice against 25 files.
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let source = CodexSource::with_home(home);
        let mut state = CodexDiscoveryState::default();

        let mut frontier = CodexDiscoveryFrontier::initial();
        let mut covered: BTreeSet<PathBuf> = BTreeSet::new();
        for _pass in 0..64 {
            let pass = retained_pass(&source, &mut state, bounds, frontier);
            covered.extend(pass.report.paths.iter().cloned());
            frontier = pass.next_frontier;
            if covered.len() == all.len() && frontier.is_complete() {
                break;
            }
        }
        assert_eq!(
            covered, all,
            "rotating passes must cover the entire backlog, no skipped-and-forgotten range"
        );
        assert!(
            frontier.is_complete(),
            "covering every backlog file must persist the sweep-complete watermark on the frontier"
        );
        let settled = retained_pass(&source, &mut state, bounds, frontier);
        assert!(
            !settled.report.is_truncated(),
            "after the history sweep visits every file, idle polls must report complete"
        );
        assert_eq!(settled.report.files_considered, 0);
        assert_eq!(
            settled.next_frontier, frontier,
            "an idle complete pass must keep the durable watermark, not restart from zero"
        );
        assert!(settled.report.paths.is_empty());

        write_dated_rollout(home, ("2026", "08", "18"), "newer");
        let grown = retained_pass(&source, &mut state, bounds, frontier);
        assert!(
            grown.report.is_truncated(),
            "new files must clear the complete watermark so history is walked again"
        );
        assert!(
            !grown.next_frontier.is_complete(),
            "growth must invalidate completion until the new tree is covered"
        );
    }

    #[test]
    fn codex_history_frontier_converges_beyond_discovery_byte_budget() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let mut all = BTreeSet::new();
        for item in 0..24 {
            all.insert(write_dated_rollout(
                home,
                ("2025", "11", "01"),
                &format!("byte-budget-{item:02}"),
            ));
        }
        let per_candidate = all
            .iter()
            .map(|path| {
                u64::try_from(crate::runtime::source::path_byte_len(path)).unwrap()
                    + u64::try_from(std::mem::size_of::<std::fs::Metadata>()).unwrap()
            })
            .max()
            .unwrap();
        let mut bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        bounds.max_discovery_bytes = per_candidate * 2;
        let source = CodexSource::with_home(home);
        let mut state = CodexDiscoveryState::default();
        let mut frontier = CodexDiscoveryFrontier::initial();
        let mut covered = BTreeSet::new();

        for _pass in 0..64 {
            let pass = retained_pass(&source, &mut state, bounds, frontier);
            assert!(pass.report.bytes_charged <= bounds.max_discovery_bytes);
            covered.extend(pass.report.paths);
            frontier = pass.next_frontier;
            if frontier.is_complete() {
                break;
            }
        }

        assert_eq!(covered, all);
        assert!(frontier.is_complete());
        let restarted = retained_pass(&source, &mut state, bounds, frontier);
        assert!(restarted.report.paths.is_empty());
        assert!(!restarted.report.is_truncated());
    }

    #[test]
    fn codex_changing_recent_file_does_not_pin_history_cursor() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let mut all = BTreeSet::new();
        for item in 0..24 {
            all.insert(write_dated_rollout(
                home,
                ("2025", "11", "01"),
                &format!("moving-{item:02}"),
            ));
        }
        let changing = write_dated_rollout(home, ("2026", "08", "17"), "changing");
        all.insert(changing.clone());
        let source = CodexSource::with_home(home);
        let mut state = CodexDiscoveryState::default();
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let mut frontier = CodexDiscoveryFrontier::initial();
        let mut covered = BTreeSet::new();

        for pass_index in 0..64 {
            std::fs::write(&changing, format!("{{\"pass\":{pass_index}}}\n")).unwrap();
            let pass = retained_pass(&source, &mut state, bounds, frontier);
            covered.extend(pass.report.paths);
            frontier = pass.next_frontier;
            if covered == all {
                break;
            }
        }

        assert_eq!(covered, all, "epoch churn must not restart at cursor zero");
    }

    #[test]
    fn codex_idle_validation_eventually_rediscovers_append_outside_active_window() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let mut files = BTreeSet::new();
        for item in 0..60 {
            files.insert(write_dated_rollout(
                home,
                ("2025", "11", "01"),
                &format!("idle-{item:03}"),
            ));
        }
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let mut state = CodexDiscoveryState::default();
        let mut frontier = CodexDiscoveryFrontier::initial();
        for _ in 0..1_000 {
            let pass = retained_pass(&source, &mut state, bounds, frontier);
            frontier = pass.next_frontier;
            if frontier.is_complete() {
                break;
            }
        }
        assert!(frontier.is_complete());

        let oldest = files.first().unwrap().clone();
        let idle = state.idle.as_mut().expect("completed idle authority");
        assert!(
            !idle.active_files.iter().any(|file| file.path == oldest),
            "fixture must mutate a file outside the bounded active window"
        );
        idle.completed_probe_cycles = IDLE_FULL_VALIDATION_CYCLES - 1;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&oldest)
            .unwrap()
            .write_all(b"{}\n")
            .unwrap();

        let mut rediscovered = false;
        for _ in 0..2_000 {
            let pass = retained_pass(&source, &mut state, bounds, frontier);
            rediscovered |= pass.report.paths.contains(&oldest);
            frontier = pass.next_frontier;
            if rediscovered {
                break;
            }
        }
        assert!(
            rediscovered,
            "bounded idle authority must eventually validate files outside its active window"
        );
    }

    #[test]
    fn codex_validation_retains_its_cursor_without_scanning_the_corpus_in_one_call() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        for item in 0..64 {
            write_dated_rollout(home, ("2026", "08", "28"), &format!("validation-{item:02}"));
        }
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let mut state = CodexDiscoveryState::default();
        state.reset_for(&source, true);

        let pass = retained_pass(
            &source,
            &mut state,
            bounds,
            CodexDiscoveryFrontier::complete(CodexCorpusEpoch::initial()),
        );
        let scan = state
            .scan
            .as_ref()
            .expect("validation cursor remains active");

        assert!(pass.report.is_truncated());
        assert!(
            scan.validation,
            "the validation sweep must remain in progress"
        );
        assert!(
            scan.files_considered <= bounds.max_files as u64,
            "one call must not stat the entire file corpus"
        );

        let directory_temp = TempDir::new().unwrap();
        let directory_home = directory_temp.path();
        for year in 2000..2032 {
            let year = year.to_string();
            write_dated_rollout(directory_home, (&year, "01", "01"), "validation");
        }
        let directory_source = CodexSource::with_home(directory_home);
        let mut directory_state = CodexDiscoveryState::default();
        directory_state.reset_for(&directory_source, true);

        retained_pass(
            &directory_source,
            &mut directory_state,
            bounds,
            CodexDiscoveryFrontier::complete(CodexCorpusEpoch::initial()),
        );
        let directory_scan = directory_state
            .scan
            .as_ref()
            .expect("directory validation cursor remains active");
        assert!(directory_scan.validation);
        let structural_work_limit = bounds
            .max_files
            .saturating_mul(usize::from(MAX_SCAN_DEPTH).saturating_add(2));
        assert!(
            directory_scan.directories.len() <= structural_work_limit,
            "one call must not traverse the entire directory corpus"
        );
    }

    /// A backlog under the cap needs no catch-up: one pass discovers everything
    /// and reports no truncation, so no catch-up is scheduled.
    #[test]
    fn codex_discovery_under_cap_is_complete_in_one_pass() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let a = write_dated_rollout(home, ("2026", "08", "16"), "yesterday");
        let b = write_dated_rollout(home, ("2026", "08", "17"), "today");

        let bounds = TranscriptDiscoveryBounds::from_discovered_units(16);
        let pass = CodexSource::with_home(home)
            .discover_transcript_paths_with_frontier(bounds, CodexDiscoveryFrontier::initial())
            .unwrap();

        assert_eq!(pass.report.paths, vec![b, a], "newest-first ordering");
        assert_eq!(pass.report.files_considered, 2);
        assert!(!pass.report.is_truncated());
        assert!(pass.next_frontier.is_complete());
    }

    /// A single historical bucket larger than the whole per-pass budget must
    /// converge file-by-file through the retained directory iterator instead
    /// of starving its tail or pinning forever.
    ///
    /// The intra-sweep cursor is the retained `CodexDiscoveryState`, not the
    /// durable frontier: the frontier stays pinned at the incoming epoch for
    /// every unfinished pass and only advances once one sweep has observed the
    /// whole corpus. So progress is asserted on the retained cursor's own
    /// output — each unfinished pass must emit files it has not emitted before,
    /// and must not claim completion while it is still reporting truncation.
    #[test]
    fn codex_oversized_bucket_converges_through_retained_traversal() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let mut all: BTreeSet<PathBuf> = BTreeSet::new();
        for item in 0..40 {
            all.insert(write_dated_rollout(
                home,
                ("2025", "11", "01"),
                &format!("old-{item:02}"),
            ));
        }
        all.insert(write_dated_rollout(home, ("2026", "08", "17"), "today"));

        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let source = CodexSource::with_home(home);

        let mut state = CodexDiscoveryState::default();
        let mut frontier = CodexDiscoveryFrontier::initial();
        let mut covered: BTreeSet<PathBuf> = BTreeSet::new();
        let mut completed = false;
        for _pass in 0..64 {
            let pass = retained_pass(&source, &mut state, bounds, frontier);
            let before = covered.len();
            covered.extend(pass.report.paths.iter().cloned());
            if pass.next_frontier.is_complete() {
                assert!(
                    !pass.report.is_truncated(),
                    "a sweep may not claim completion while it still reports truncation"
                );
                completed = true;
                frontier = pass.next_frontier;
                break;
            }
            assert!(
                pass.report.is_truncated(),
                "an unfinished sweep must keep catch-up scheduled"
            );
            assert!(
                covered.len() > before,
                "an unfinished oversized bucket must still advance its retained cursor"
            );
            frontier = pass.next_frontier;
        }
        assert!(
            completed,
            "the retained sweep must reach a durable completion claim"
        );
        assert_eq!(
            covered, all,
            "an oversized bucket's tail must be reached across passes"
        );
        assert_eq!(
            frontier.epoch.files,
            u64::try_from(all.len()).unwrap(),
            "the completed frontier's epoch must count the whole corpus"
        );
    }

    /// Sweep-complete is store-durable: a fresh source reading only admission
    /// parse offsets (the production persist path) idles instead of restarting
    /// truncated-from-zero. MemoryHostAdmission is the same offset table a
    /// process restart would reopen — not a process-local memo.
    #[tokio::test]
    async fn codex_sweep_complete_watermark_survives_admission_restart() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let mut all: BTreeSet<PathBuf> = BTreeSet::new();
        for day in 1..=12 {
            for item in 0..2 {
                all.insert(write_dated_rollout(
                    home,
                    ("2025", "11", &format!("{day:02}")),
                    &format!("old-{day:02}-{item}"),
                ));
            }
        }
        all.insert(write_dated_rollout(home, ("2026", "08", "17"), "today"));

        let bounds = TranscriptDiscoveryBounds::from_discovered_units(8);
        let source = CodexSource::with_home(home);
        let admission = MemoryHostAdmission::default();
        let scope = tracedecay_domain::ObservationScopeV1::Profile;

        let mut frontier = CodexDiscoveryFrontier::initial();
        let mut covered: BTreeSet<PathBuf> = BTreeSet::new();
        // Reaching the watermark is a long-lived scheduler's job, so this
        // setup drives the retained traversal the scheduler uses. The
        // standalone helper documents that it does not converge past one pass:
        // its cursor lives in `CodexDiscoveryState`, not in the durable
        // frontier. The restart the test is actually about is still a fresh
        // `CodexSource` with fresh state below, reading only the persisted
        // admission offsets.
        let mut state = CodexDiscoveryState::default();
        for _pass in 0..64 {
            let pass = source
                .discover_transcript_paths_with_state(bounds, frontier, &mut state)
                .unwrap();
            state.acknowledge();
            covered.extend(pass.report.paths.iter().cloned());
            persist_codex_history_frontier(&admission, &scope, frontier, pass.next_frontier)
                .await
                .unwrap();
            let coverage = if pass.report.is_truncated() {
                HostProviderCoverage::Partial
            } else {
                HostProviderCoverage::Complete
            };
            persist_host_provider_coverage(
                &admission,
                &scope,
                "codex",
                coverage,
                u64::from(pass.report.is_truncated()),
            )
            .await
            .unwrap();
            frontier = pass.next_frontier;
            if covered.len() == all.len() && !pass.report.is_truncated() {
                break;
            }
        }
        assert_eq!(covered, all);
        assert!(frontier.is_complete());

        let stored = read_codex_history_frontier(&admission, &scope)
            .await
            .unwrap();
        let coverage = read_host_provider_coverage(&admission, &scope, "codex")
            .await
            .unwrap();
        assert_eq!(coverage, Some(HostProviderCoverage::Complete));
        assert_eq!(stored, frontier);
        assert!(stored.is_complete());

        let restarted_source = CodexSource::with_home(home);
        let mut restarted_state = CodexDiscoveryState::default();
        let first_restart = retained_pass(
            &restarted_source,
            &mut restarted_state,
            bounds,
            stored.for_coverage(true),
        );
        assert!(
            first_restart.report.is_truncated(),
            "a fresh process must validate a large persisted corpus in bounded slices"
        );
        assert!(first_restart.report.paths.is_empty());

        let mut restarted_frontier = first_restart.next_frontier;
        for _ in 0..64 {
            let pass = retained_pass(
                &restarted_source,
                &mut restarted_state,
                bounds,
                restarted_frontier,
            );
            assert!(
                pass.report.paths.is_empty(),
                "unchanged restart validation must not re-emit transcripts"
            );
            restarted_frontier = pass.next_frontier;
            if restarted_frontier.is_complete() {
                break;
            }
        }
        assert_eq!(restarted_frontier, stored);

        let added = write_dated_rollout(home, ("2026", "08", "18"), "after-restart");
        let mut rediscovered = false;
        for _ in 0..64 {
            let pass = retained_pass(
                &restarted_source,
                &mut restarted_state,
                bounds,
                restarted_frontier,
            );
            rediscovered |= pass.report.paths.contains(&added);
            restarted_frontier = pass.next_frontier;
            if rediscovered {
                break;
            }
        }
        assert!(
            rediscovered,
            "bounded restart validation must find new files"
        );
        assert!(!restarted_frontier.is_complete());
    }

    /// Symlink-to-file candidates belong to the same ordered snapshot as
    /// regular files; otherwise the epoch and selected population diverge.
    #[test]
    #[cfg(unix)]
    fn jsonl_counts_and_discovery_selection_describe_the_same_files() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let real = write_dated_rollout(home, ("2026", "08", "17"), "real");
        let bucket = real.parent().unwrap();
        std::os::unix::fs::symlink(&real, bucket.join("rollout-link.jsonl")).unwrap();

        let selected = CodexSource::with_home(home)
            .discover_transcript_paths_with_frontier(
                TranscriptDiscoveryBounds::from_discovered_units(64),
                CodexDiscoveryFrontier::initial(),
            )
            .unwrap()
            .report
            .paths
            .len();

        assert_eq!(selected, 2, "discovery retains symlink-to-file candidates");
    }

    /// A non-directory source root is an I/O failure, not an empty Complete
    /// corpus. Providers can therefore retry without persisting discovery or
    /// coverage state.
    #[test]
    fn codex_non_directory_root_is_a_typed_discovery_error() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/sessions"), b"not a directory").unwrap();

        let error = CodexSource::with_home(home)
            .discover_transcript_paths_with_frontier(
                TranscriptDiscoveryBounds::from_discovered_units(8),
                CodexDiscoveryFrontier::initial(),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            crate::runtime::source::TranscriptIngestError::ScanIo { .. }
        ));
    }

    /// The durable epoch stores both digest halves directly. This catches a
    /// regression back to bit packing, masking, saturation, or clamping.
    #[test]
    fn codex_frontier_round_trip_preserves_full_width_epoch() {
        let frontier = CodexDiscoveryFrontier::in_progress(CodexCorpusEpoch {
            high: u64::MAX,
            low: u64::MAX - 1,
            files: u64::MAX - 2,
        });
        let (stored_frontier, stored_epoch) = frontier.into_parse_offsets();

        let reloaded =
            CodexDiscoveryFrontier::from_parse_offsets(stored_frontier, stored_epoch).unwrap();

        assert_eq!(reloaded, frontier);
    }

    #[tokio::test]
    async fn project_frontier_cas_persists_non_monotonic_epoch_fields_exactly() {
        let admission = MemoryHostAdmission::default();
        let scope = tracedecay_domain::ObservationScopeV1::Profile;
        let high = CodexDiscoveryFrontier::complete(CodexCorpusEpoch {
            high: u64::MAX,
            low: u64::MAX,
            files: 7,
        });
        let lower = CodexDiscoveryFrontier::in_progress(CodexCorpusEpoch {
            high: 1,
            low: 2,
            files: 7,
        });

        persist_codex_history_frontier(&admission, &scope, CodexDiscoveryFrontier::initial(), high)
            .await
            .unwrap();
        persist_codex_history_frontier(&admission, &scope, high, lower)
            .await
            .unwrap();

        assert_eq!(
            read_codex_history_frontier(&admission, &scope)
                .await
                .unwrap(),
            lower
        );
    }
}
