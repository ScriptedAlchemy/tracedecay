//! Codex rollout discovery and observation-normalization tests.

use serde_json::Value;
use tracedecay_domain::{
    CanonicalMessageRoleV1, CanonicalObservationFactV1, CanonicalUnknownStateV1,
    CanonicalWorkflowSemanticKindV1, ProjectId,
};

use super::CodexSource;
use super::meta::session_meta_with_provenance;
use super::observation::{
    CodexObservationAdmission, codex_native_record_id, normalize_codex_observation,
};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod goal_event_tests {
    use std::collections::BTreeSet;

    use super::*;
    use serde_json::json;
    use tracedecay_domain::{CanonicalObservationEnvelopeV1, ObservationScopeV1};

    use crate::admission::HostAdmission;
    use crate::admission::test_support::MemoryHostAdmission;
    use crate::observation::ObservationCancellation;
    use crate::runtime::hosts::codex::{
        try_admit_codex_jsonl_observations_for_project_window,
        try_admit_codex_jsonl_observations_for_project_with_admission,
    };
    use crate::runtime::ingest::project_provider::ProjectProviderRun;
    use crate::runtime::source::{HostProviderCoverage, read_host_provider_coverage};
    use crate::runtime::{SessionProvider, with_transcript_source_profile};

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
                if name == "update_plan" && arguments["plan"][1]["step"] == "ship"
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
        // task_events_become_turn_boundary_rows, singular task_complete only.
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

    #[tokio::test]
    async fn project_provider_yields_after_one_window_and_resumes_without_loss() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let transcript = temp.path().join("rollout.jsonl");
        let session_id = "session-windowed-project";
        let mut lines = vec![json!({
            "timestamp": "2026-09-04T12:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session_id, "cwd": project}
        })];
        lines.extend((0..256).map(|ordinal| {
            json!({
                "timestamp": "2026-09-04T12:00:01.004Z",
                "type": "event_msg",
                "payload": {
                    "type": "item_completed",
                    "thread_id": session_id,
                    "turn_id": format!("turn-{ordinal}"),
                    "item": {
                        "type": "UserMessage",
                        "id": format!("user-item-{ordinal}"),
                        "content": [{"type": "text", "text": format!("message {ordinal}")}]
                    }
                }
            })
        }));
        let encoded = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&transcript, &encoded).unwrap();
        let project_id = ProjectId::new("project-windowed-provider").unwrap();
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();

        let first = try_admit_codex_jsonl_observations_for_project_window(
            &transcript,
            &project,
            project_id.clone(),
            &admission,
            u64::MAX,
            &cancellation,
        )
        .await
        .unwrap();

        assert_eq!(first.frames_persisted, 256);
        assert!(first.source_deferred);
        assert!(first.bytes_consumed < encoded.len() as u64);
        assert_eq!(admission.observations().len(), 256);

        let second = try_admit_codex_jsonl_observations_for_project_window(
            &transcript,
            &project,
            project_id,
            &admission,
            u64::MAX,
            &cancellation,
        )
        .await
        .unwrap();

        assert_eq!(second.frames_persisted, 1);
        assert!(!second.source_deferred);
        assert_eq!(admission.observations().len(), 257);
        assert_eq!(
            first.bytes_consumed + second.bytes_consumed,
            encoded.len() as u64
        );
    }

    /// A pass yields on the source the capture window left mid-file, not on
    /// every source it finishes.
    ///
    /// `MAX_CAPTURE_WINDOW` is the cooperative bound that owns a resumable
    /// cursor: it stops inside one rollout and the next pass resumes at that
    /// byte offset. Yielding once per *finished* rollout has no such cursor, so
    /// the discovery frontier can never commit and every later pass re-reads
    /// every rollout it already exhausted, which is quadratic in the rollouts a
    /// project has. The profile-scope loop has always been bounded this way.
    #[tokio::test]
    async fn project_provider_yields_on_a_deferred_rollout_and_converges_without_loss() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        // The newest rollout alone exceeds one capture window, so pass 0 must
        // stop inside it; the two behind it fit in the pass that finishes it.
        let deferring_messages =
            crate::runtime::jsonl_observation_admission::MAX_CAPTURE_WINDOW + 44;
        let rollouts = [
            (("2026", "09", "03"), "session-newest", deferring_messages),
            (("2026", "09", "02"), "session-middle", 3),
            (("2026", "09", "01"), "session-oldest", 3),
        ];
        for (date, session_id, messages_per_rollout) in rollouts {
            let directory = home
                .join(".codex/sessions")
                .join(date.0)
                .join(date.1)
                .join(date.2);
            std::fs::create_dir_all(&directory).unwrap();
            let mut lines = vec![json!({
                "timestamp": format!("{}-{}-{}T12:00:00.000Z", date.0, date.1, date.2),
                "type": "session_meta",
                "payload": {"id": session_id, "cwd": project}
            })];
            lines.extend((0..messages_per_rollout).map(|ordinal| {
                json!({
                    "timestamp": format!("{}-{}-{}T12:00:01.{:03}Z", date.0, date.1, date.2, ordinal),
                    "type": "event_msg",
                    "payload": {
                        "type": "user_message",
                        "message": format!("{session_id}-message-{ordinal}")
                    }
                })
            }));
            std::fs::write(
                directory.join(format!("rollout-{session_id}.jsonl")),
                lines
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n",
            )
            .unwrap();
        }

        let project_id = ProjectId::new("project-dated-rollouts").unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();
        // Pass 0 stops inside the newest rollout at the capture window; pass 1
        // resumes it from the stored byte offset and, still inside its byte
        // budget, finishes the two rollouts behind it.
        let expected_passes: [(&[&str], usize); 2] = [
            (
                &["session-newest"],
                crate::runtime::jsonl_observation_admission::MAX_CAPTURE_WINDOW,
            ),
            (
                &["session-newest", "session-middle", "session-oldest"],
                (deferring_messages + 1)
                    - crate::runtime::jsonl_observation_admission::MAX_CAPTURE_WINDOW
                    + 8,
            ),
        ];
        let failure_ceiling = expected_passes.len().saturating_add(1);
        let mut completed_after = None;

        for pass_index in 0..failure_ceiling {
            let before = admission.observations().len();
            let outcome = with_transcript_source_profile(
                tracedecay_runtime_core::config::ProfileRoot::under_home(home.clone()),
                ProjectProviderRun {
                    project_root: &project,
                    project_id: &project_id,
                    facade: &admission,
                    scope: &scope,
                    candidate: SessionProvider::Codex,
                    max_new_bytes: u64::MAX,
                    cancellation: &cancellation,
                    codex_discovery: None,
                }
                .run_codex(),
            )
            .await;
            assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);

            let after = admission.observations();
            let admitted_this_pass = &after[before..];
            assert!(
                pass_index < expected_passes.len(),
                "coverage did not complete within the fixture-derived ceiling"
            );
            let (expected_sessions, expected_admitted) = expected_passes[pass_index];
            assert_eq!(
                admitted_this_pass.len(),
                expected_admitted,
                "pass {pass_index} must admit one capture window of {expected_sessions:?}"
            );
            assert_eq!(
                admitted_this_pass
                    .iter()
                    .map(|stored| {
                        let envelope: CanonicalObservationEnvelopeV1 =
                            serde_json::from_value(stored.observation().payload().clone()).unwrap();
                        envelope.relations().session_id().as_str().to_owned()
                    })
                    .collect::<BTreeSet<_>>(),
                expected_sessions
                    .iter()
                    .map(|session_id| (*session_id).to_owned())
                    .collect::<BTreeSet<_>>(),
                "pass {pass_index} admitted the wrong rollouts"
            );

            let coverage = read_host_provider_coverage(&admission, &scope, "codex")
                .await
                .unwrap();
            if coverage == Some(HostProviderCoverage::Complete) {
                completed_after = Some(pass_index + 1);
                break;
            }
            assert_eq!(coverage, Some(HostProviderCoverage::Partial));
        }

        assert_eq!(completed_after, Some(expected_passes.len()));
        let observations = admission.observations();
        assert_eq!(
            observations.len(),
            rollouts
                .iter()
                .map(|(_, _, messages)| messages + 1)
                .sum::<usize>()
        );
        let envelopes = observations
            .iter()
            .map(|stored| {
                serde_json::from_value::<CanonicalObservationEnvelopeV1>(
                    stored.observation().payload().clone(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let admitted_sessions = envelopes
            .iter()
            .map(|envelope| envelope.relations().session_id().as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            admitted_sessions.iter().cloned().collect::<BTreeSet<_>>(),
            rollouts
                .iter()
                .map(|(_, session_id, _)| (*session_id).to_owned())
                .collect::<BTreeSet<_>>()
        );
        for (_, session_id, messages_per_rollout) in rollouts {
            assert_eq!(
                admitted_sessions
                    .iter()
                    .filter(|admitted| admitted.as_str() == session_id)
                    .count(),
                messages_per_rollout + 1,
                "{session_id} must be admitted exactly once"
            );
            for ordinal in 0..messages_per_rollout {
                let expected = format!("{session_id}-message-{ordinal}");
                assert_eq!(
                    envelopes
                        .iter()
                        .flat_map(|envelope| envelope.facts())
                        .filter(|fact| matches!(
                            fact,
                            CanonicalObservationFactV1::Message { content, .. }
                                if content.as_str() == Some(expected.as_str())
                        ))
                        .count(),
                    1,
                    "{expected} must be admitted exactly once"
                );
            }
        }
        assert_eq!(
            read_host_provider_coverage(&admission, &scope, "codex")
                .await
                .unwrap(),
            Some(HostProviderCoverage::Complete)
        );
    }

    /// The newest in-project day is admitted, and its message text is durable,
    /// before any older out-of-project day is opened.
    ///
    /// Search reads the published projection of those admitted messages. A pass
    /// that keeps walking older days writes their coverage cursors before that
    /// projection can run, so the newest day stays invisible until the older
    /// sweep finishes. The follow-up pass must still open the older day, or
    /// the yield would retry the same page forever.
    #[tokio::test]
    async fn newest_day_messages_are_durable_before_older_days_are_opened() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("project");
        let other = home.join("other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let marker = "NEWEST_DAY_MARKER cobalt orchard scheduler is ready";
        let newest = [
            ("2026", "08", "29", "newest-a"),
            ("2026", "08", "29", "newest-b"),
        ];
        let older = [
            ("2026", "08", "28", "older-a"),
            ("2026", "08", "28", "older-b"),
        ];
        for (year, month, day, session_id) in newest {
            write_scoped_rollout(&home, year, month, day, session_id, &project, marker);
        }
        for (year, month, day, session_id) in older {
            write_scoped_rollout(
                &home,
                year,
                month,
                day,
                session_id,
                &other,
                "OLDER_DAY_MARKER should stay unopened",
            );
        }

        let project_id = ProjectId::new("project-newest-before-older").unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();
        let run_pass = || {
            with_transcript_source_profile(
                tracedecay_runtime_core::config::ProfileRoot::under_home(home.clone()),
                ProjectProviderRun {
                    project_root: &project,
                    project_id: &project_id,
                    facade: &admission,
                    scope: &scope,
                    candidate: SessionProvider::Codex,
                    max_new_bytes: u64::MAX,
                    cancellation: &cancellation,
                    codex_discovery: None,
                }
                .run_codex(),
            )
        };

        let first = run_pass().await;
        assert!(first.failures.is_empty(), "{:?}", first.failures);
        let admitted = session_ids_of(&admission.observations());
        assert_eq!(
            admitted,
            ["newest-a".to_owned(), "newest-b".to_owned()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        assert!(
            admission.observations().iter().any(|stored| {
                let envelope: CanonicalObservationEnvelopeV1 =
                    serde_json::from_value(stored.observation().payload().clone()).unwrap();
                envelope.facts().iter().any(|fact| {
                    matches!(
                        fact,
                        CanonicalObservationFactV1::Message { content, .. }
                            if content.as_str() == Some(marker)
                    )
                })
            }),
            "the newest day's message text must be durable before older days are opened"
        );
        for session_id in ["older-a", "older-b"] {
            let source =
                crate::runtime::hosts::codex::codex_observation_source_v2(session_id).unwrap();
            assert!(
                admission
                    .get_source_cursor(&source, &scope)
                    .await
                    .unwrap()
                    .is_none(),
                "{session_id} was opened in the pass that admitted the newest day"
            );
        }
        assert_eq!(
            read_host_provider_coverage(&admission, &scope, "codex")
                .await
                .unwrap(),
            Some(HostProviderCoverage::Partial)
        );

        let mut older_opened = false;
        for _ in 0..3 {
            let outcome = run_pass().await;
            assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
            let source =
                crate::runtime::hosts::codex::codex_observation_source_v2("older-a").unwrap();
            if admission
                .get_source_cursor(&source, &scope)
                .await
                .unwrap()
                .is_some()
            {
                older_opened = true;
                break;
            }
        }
        assert!(
            older_opened,
            "deferring the older day must still open it on a later pass"
        );
        assert_eq!(
            session_ids_of(&admission.observations()),
            ["newest-a".to_owned(), "newest-b".to_owned()]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            "out-of-project days stay out of the project observation set"
        );
    }

    /// An out-of-project rollout inside the day being admitted does not end
    /// the pass. On a profile that interleaves projects, ending there admits a
    /// few rollouts per pass and rediscovers the same page each time.
    #[tokio::test]
    async fn a_mixed_day_is_admitted_in_one_pass_before_older_days_are_opened() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("project");
        let other = home.join("other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        for (session_id, cwd) in [
            ("mixed-a", &project),
            ("mixed-m", &other),
            ("mixed-z", &project),
        ] {
            write_scoped_rollout(&home, "2026", "08", "29", session_id, cwd, "mixed day");
        }
        write_scoped_rollout(&home, "2026", "08", "28", "older-x", &other, "older day");

        let project_id = ProjectId::new("project-mixed-day").unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let admission = MemoryHostAdmission::default();
        let outcome = with_transcript_source_profile(
            tracedecay_runtime_core::config::ProfileRoot::under_home(home.clone()),
            ProjectProviderRun {
                project_root: &project,
                project_id: &project_id,
                facade: &admission,
                scope: &scope,
                candidate: SessionProvider::Codex,
                max_new_bytes: u64::MAX,
                cancellation: &ObservationCancellation::default(),
                codex_discovery: None,
            }
            .run_codex(),
        )
        .await;

        assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        assert_eq!(
            session_ids_of(&admission.observations()),
            ["mixed-a".to_owned(), "mixed-z".to_owned()]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
        let older = crate::runtime::hosts::codex::codex_observation_source_v2("older-x").unwrap();
        assert!(
            admission
                .get_source_cursor(&older, &scope)
                .await
                .unwrap()
                .is_none(),
            "the older out-of-project day belongs to the next pass"
        );
    }

    /// A session that appends to today's rollout between passes must not keep
    /// the pass yielding at yesterday's out-of-project day. The yield exists
    /// for a rollout opened this pass; a resumed live tail already has its
    /// earlier window searchable, so the pass walks on, admits the older
    /// in-project day, and commits its frontier.
    #[tokio::test]
    async fn a_live_tail_does_not_starve_older_in_project_days() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("project");
        let other = home.join("other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        write_scoped_rollout(&home, "2026", "08", "29", "live", &project, "live day");
        write_scoped_rollout(&home, "2026", "08", "28", "other-a", &other, "other day");
        write_scoped_rollout(&home, "2026", "08", "27", "oldest", &project, "oldest day");
        let live_rollout = home.join(".codex/sessions/2026/08/29/rollout-live.jsonl");

        let project_id = ProjectId::new("project-live-tail").unwrap();
        let scope = ObservationScopeV1::Project {
            project_id: project_id.clone(),
        };
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();
        let run_pass = || {
            with_transcript_source_profile(
                tracedecay_runtime_core::config::ProfileRoot::under_home(home.clone()),
                ProjectProviderRun {
                    project_root: &project,
                    project_id: &project_id,
                    facade: &admission,
                    scope: &scope,
                    candidate: SessionProvider::Codex,
                    max_new_bytes: u64::MAX,
                    cancellation: &cancellation,
                    codex_discovery: None,
                }
                .run_codex(),
            )
        };

        let first = run_pass().await;
        assert!(first.failures.is_empty(), "{:?}", first.failures);
        assert_eq!(
            session_ids_of(&admission.observations()),
            BTreeSet::from(["live".to_owned()]),
            "the newest day yields before the older out-of-project day is opened"
        );

        let appends = 3;
        for ordinal in 0..appends {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&live_rollout)
                .unwrap();
            std::io::Write::write_all(
                &mut file,
                format!(
                    "{}\n",
                    json!({
                        "timestamp": format!("2026-08-29T12:00:{:02}.000Z", ordinal + 2),
                        "type": "event_msg",
                        "payload": {"type": "user_message", "message": format!("live append {ordinal}")}
                    })
                )
                .as_bytes(),
            )
            .unwrap();
            let outcome = run_pass().await;
            assert!(outcome.failures.is_empty(), "{:?}", outcome.failures);
        }

        assert_eq!(
            session_ids_of(&admission.observations()),
            BTreeSet::from(["live".to_owned(), "oldest".to_owned()]),
            "a live tail appending every pass starved the older in-project day"
        );
        assert_eq!(
            admission
                .observations()
                .iter()
                .filter(|stored| {
                    let envelope: CanonicalObservationEnvelopeV1 =
                        serde_json::from_value(stored.observation().payload().clone()).unwrap();
                    envelope.relations().session_id().as_str() == "live"
                })
                .count(),
            2 + appends,
            "every live append is admitted exactly once"
        );
        assert_eq!(
            read_host_provider_coverage(&admission, &scope, "codex")
                .await
                .unwrap(),
            Some(HostProviderCoverage::Complete)
        );
    }

    fn write_scoped_rollout(
        home: &std::path::Path,
        year: &str,
        month: &str,
        day: &str,
        session_id: &str,
        cwd: &std::path::Path,
        message: &str,
    ) {
        let directory = home
            .join(".codex/sessions")
            .join(year)
            .join(month)
            .join(day);
        std::fs::create_dir_all(&directory).unwrap();
        let lines = [
            json!({
                "timestamp": format!("{year}-{month}-{day}T12:00:00.000Z"),
                "type": "session_meta",
                "payload": {"id": session_id, "cwd": cwd}
            }),
            json!({
                "timestamp": format!("{year}-{month}-{day}T12:00:01.000Z"),
                "type": "event_msg",
                "payload": {"type": "user_message", "message": message}
            }),
        ];
        std::fs::write(
            directory.join(format!("rollout-{session_id}.jsonl")),
            lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
    }

    fn session_ids_of(observations: &[tracedecay_store::StoredObservation]) -> BTreeSet<String> {
        observations
            .iter()
            .map(|stored| {
                let envelope: CanonicalObservationEnvelopeV1 =
                    serde_json::from_value(stored.observation().payload().clone()).unwrap();
                envelope.relations().session_id().as_str().to_owned()
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod recent_first_discovery_tests {
    use std::collections::BTreeSet;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use serde_json::json;
    use tempfile::TempDir;
    use tracedecay_domain::ProjectId;

    use super::CodexSource;
    use crate::admission::test_support::MemoryHostAdmission;
    use crate::observation::ObservationCancellation;
    use crate::runtime::hosts::codex::try_admit_codex_jsonl_observations_for_project_window;
    use crate::runtime::hosts::codex::{
        CodexCorpusEpoch, CodexDiscoveryDelivery, CodexDiscoveryFrontier, CodexDiscoveryHub,
        CodexDiscoverySourceKey, CodexDiscoveryState, CodexExactSessionPathAuthority,
        CodexIndexedPath, CodexReplayIndex, EXACT_HOOK_DISCOVERY_UNITS_PER_CALL,
        IDLE_FULL_VALIDATION_CYCLES, MAX_EXACT_HOOK_SESSION_REQUESTS,
        MAX_EXACT_HOOK_SOURCE_AUTHORITIES, MAX_SCAN_DEPTH, indexed_replay_pass,
        replay_index_entries_visited_for_test, reset_replay_index_entries_visited_for_test,
    };
    use crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority;
    use crate::runtime::source::spin_until_jsonl_change_settled;
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
        // Discovery proves a file unchanged only by a settled identity.
        spin_until_jsonl_change_settled(&path);
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

    /// A consumer told to wait for a scan in progress is released when a scan
    /// finishes; a scan nobody waited on releases nothing, so a refused pass
    /// subscribed before its own scan is not woken by that scan.
    #[tokio::test]
    async fn a_finished_scan_releases_only_consumers_that_waited_on_it() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        write_dated_rollout(home, ("2026", "08", "23"), "release");
        let hub = CodexDiscoveryHub::default();
        hub.register("profile", Some(home));
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(128);
        let frontier = CodexDiscoveryFrontier::initial();
        let released = hub.subscribe_scan_release();

        let CodexDiscoveryDelivery::Ready(first) = hub
            .discover("profile", &source, bounds, frontier)
            .await
            .unwrap()
        else {
            panic!("the first scan delivers");
        };
        assert!(!released.has_changed().unwrap());
        hub.acknowledge("profile");

        // A consumer joining now replays the source's index, which another
        // consumer is scanning.
        hub.register("project", Some(home));
        let set_index_scanning = |scanning: bool| {
            hub.inner
                .lock()
                .unwrap()
                .replay_indexes
                .entry(source.discovery_key())
                .or_default()
                .scanning = scanning;
        };
        set_index_scanning(true);
        assert!(matches!(
            hub.discover("project", &source, bounds, frontier)
                .await
                .unwrap(),
            CodexDiscoveryDelivery::Waiting
        ));
        set_index_scanning(false);
        hub.discover("profile", &source, bounds, first.next_frontier)
            .await
            .unwrap();
        assert!(released.has_changed().unwrap());
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
        // Without a stat witness no file proves unchanged: the retained probe
        // honestly re-emits and re-enumerates the corpus.
        #[cfg(unix)]
        assert!(unchanged.is_empty());
        #[cfg(not(unix))]
        assert_eq!(unchanged, vec![removed.clone()]);
        #[cfg(unix)]
        assert_eq!(unchanged_frontier, secondary_frontier);
        // The corpus epoch folds each file's identity digest; without a stat
        // witness those digests are unvouched per observation, so only the
        // sweep state and file count stay comparable across enumerations.
        #[cfg(not(unix))]
        {
            assert_eq!(unchanged_frontier.state, secondary_frontier.state);
            assert_eq!(
                unchanged_frontier.epoch.files,
                secondary_frontier.epoch.files
            );
        }
        assert_eq!(
            hub.inner
                .lock()
                .unwrap()
                .replay_indexes
                .get(&secondary_source.discovery_key())
                .expect("secondary replay index")
                .completed_enumerations,
            if cfg!(unix) { 1 } else { 2 },
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
        // The earlier unchanged probe already enumerated a second time where
        // no stat witness proves the corpus unchanged.
        assert_eq!(index.completed_enumerations, if cfg!(unix) { 2 } else { 3 });
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
        let session_id = "0198-session-beyond-default-budget";
        let expected = directory.join(format!("rollout-2026-08-23-{session_id}.jsonl"));
        // Write the target mid-corpus: tmpfs lists newest entries first and
        // btrfs oldest first, so either end would land in the first slice.
        // Distractor names also sort before the target so ordered enumeration
        // (NTFS) only reaches it past the first retained slice; the loop below
        // must then continue across calls on every platform.
        for index in 0..4_100 {
            if index == 2_050 {
                std::fs::write(&expected, b"{}\n").unwrap();
            }
            std::fs::write(
                directory.join(format!("rollout-1999-12-31-distractor-{index:04}.jsonl")),
                b"{}\n",
            )
            .unwrap();
        }

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

    /// A dated tree whose older days hold more rollouts than one structural
    /// pass can charge must still surface today's session immediately. Listing
    /// those older files is not allowed to postpone the newest rollout.
    #[tokio::test]
    async fn codex_catch_up_surfaces_the_newest_rollout_before_older_days_are_listed() {
        crate::runtime::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        for index in 0..800 {
            write_dated_rollout(home, ("2026", "06", "01"), &format!("older-{index:04}"));
        }
        let newest = write_dated_rollout(home, ("2026", "09", "28"), "project-newest");
        let hub = CodexDiscoveryHub::default();
        hub.register("project", Some(home));
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::default_walk();
        let mut frontier = CodexDiscoveryFrontier::initial();
        let mut surfaced = false;
        for _ in 0..2 {
            let pass = match hub
                .discover("project", &source, bounds, frontier)
                .await
                .unwrap()
            {
                CodexDiscoveryDelivery::Ready(pass) => pass,
                CodexDiscoveryDelivery::Waiting => {
                    panic!("a single catch-up consumer must not wait on its own scan")
                }
            };
            frontier = pass.next_frontier;
            hub.acknowledge("project");
            if pass.report.paths.first() == Some(&newest) {
                surfaced = true;
                break;
            }
        }
        assert!(
            surfaced,
            "catch-up must surface the newest rollout before it finishes listing older days"
        );
        for _ in 0..64 {
            if frontier.is_complete() {
                break;
            }
            let pass = match hub
                .discover("project", &source, bounds, frontier)
                .await
                .unwrap()
            {
                CodexDiscoveryDelivery::Ready(pass) => pass,
                CodexDiscoveryDelivery::Waiting => continue,
            };
            frontier = pass.next_frontier;
            hub.acknowledge("project");
        }
        assert!(
            frontier.is_complete(),
            "catch-up must finish the rollout sweep after the newest session is visible"
        );
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

    /// An in-place rewrite keeps the inode and its generation; inside the
    /// change-time quantum of the identity discovery recorded it keeps every
    /// stat field too, so that identity must not prove the file unchanged.
    #[test]
    #[cfg(unix)]
    fn codex_same_size_in_place_rewrite_is_redelivered() {
        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let path = write_dated_rollout(home, ("2026", "08", "17"), "rewritten");
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::from_discovered_units(16);
        let mut frontier = CodexDiscoveryFrontier::initial();

        for round in 0..32_u8 {
            let completed = source
                .discover_transcript_paths_with_frontier(bounds, frontier)
                .unwrap()
                .next_frontier;
            let original = std::fs::metadata(&path).unwrap();
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all(if round % 2 == 0 { b"[]\n" } else { b"{}\n" })
                .unwrap();
            file.set_modified(original.modified().unwrap()).unwrap();
            drop(file);

            let rewritten = source
                .discover_transcript_paths_with_frontier(bounds, completed)
                .unwrap();
            assert_ne!(rewritten.next_frontier.epoch, completed.epoch);
            assert_eq!(rewritten.report.paths, vec![path.clone()]);
            frontier = rewritten.next_frontier;
        }
    }

    /// Coverage: retained traversal across passes must visit every historical
    /// file, tracked pending work, never a skipped range.
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
        // A settled identity proves the restart's validation complete in one
        // pass; without a stat witness the corpus validates in bounded slices
        // like the fresh-process contract above.
        #[cfg(unix)]
        assert!(!restarted.report.is_truncated());
        #[cfg(not(unix))]
        assert!(
            restarted.report.is_truncated(),
            "without a stat witness restart validation continues in bounded slices"
        );
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

    /// Sweep-complete is store-durable: a fresh source reading only admission
    /// parse offsets (the production persist path) idles instead of restarting
    /// truncated-from-zero. MemoryHostAdmission is the same offset table a
    /// process restart would reopen, not a process-local memo.
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
                None,
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
            // Without a stat witness no file proves unchanged, so restart
            // validation honestly re-emits the unchanged corpus; it must
            // never emit anything outside it.
            #[cfg(unix)]
            assert!(
                pass.report.paths.is_empty(),
                "unchanged restart validation must not re-emit transcripts"
            );
            #[cfg(not(unix))]
            assert!(
                pass.report.paths.iter().all(|path| all.contains(path)),
                "restart validation may re-emit only the unchanged corpus"
            );
            restarted_frontier = pass.next_frontier;
            if restarted_frontier.is_complete() {
                break;
            }
        }
        #[cfg(unix)]
        assert_eq!(restarted_frontier, stored);
        // The persisted epoch folds unvouched identity digests where no stat
        // witness exists, so restart validation converges to an equal sweep
        // state and file count rather than an equal salted epoch.
        #[cfg(not(unix))]
        {
            assert_eq!(restarted_frontier.state, stored.state);
            assert_eq!(restarted_frontier.epoch.files, stored.epoch.files);
        }

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

    fn write_project_rollout(home: &Path, project: &Path, name: &str) -> PathBuf {
        let path = write_dated_rollout(home, ("2026", "09", "28"), name);
        let body = format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-09-28T00:00:00.000Z",
                "type": "session_meta",
                "payload": {"id": name, "cwd": project}
            }),
            json!({
                "timestamp": "2026-09-28T00:00:01.000Z",
                "type": "event_msg",
                "payload": {"type": "agent_message", "message": "newest project turn"}
            })
        );
        std::fs::write(&path, body).unwrap();
        path
    }

    /// A ~9k Codex profile must surface and admit its newest project rollout on
    /// the first catch-up pass, then finish the retained sweep. The historical
    /// scheduler backs off 250ms only when a pass admits nothing; that wait is
    /// part of the catch-up the operator observes.
    #[tokio::test]
    async fn codex_catch_up_admits_newest_rollout_on_a_large_profile() {
        install_test_shared_jsonl_preparation_authority();

        let temp = TempDir::new().unwrap();
        let home = temp.path();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let backlog = 9_000usize;
        for index in 0..backlog {
            let month = format!("{:02}", 6 + index / 3_000);
            let day = format!("{:02}", 1 + (index / 100) % 30);
            write_dated_rollout(home, ("2025", &month, &day), &format!("old-{index:05}"));
        }
        let newest = write_project_rollout(home, &project, "newest");

        let hub = CodexDiscoveryHub::default();
        hub.register("project", Some(home));
        let source = CodexSource::with_home(home);
        let bounds = TranscriptDiscoveryBounds::default_walk();
        let mut frontier = CodexDiscoveryFrontier::initial();
        let started = Instant::now();
        let mut first_path_ms = None;
        let mut first_admit_ms = None;
        let mut ready_passes = 0u32;
        let project_id = ProjectId::new("project-large-codex").unwrap();
        let admission = MemoryHostAdmission::default();
        let cancellation = ObservationCancellation::default();
        let deadline = Duration::from_secs(30);

        loop {
            if started.elapsed() > deadline {
                panic!(
                    "catch-up did not finish in {}s (ready_passes={ready_passes} first_admit_ms={first_admit_ms:?})",
                    deadline.as_secs()
                );
            }
            let pass = match hub
                .discover("project", &source, bounds, frontier)
                .await
                .expect("codex discovery")
            {
                CodexDiscoveryDelivery::Waiting => continue,
                CodexDiscoveryDelivery::Ready(pass) => pass,
            };
            ready_passes = ready_passes.saturating_add(1);
            let includes_newest = pass.report.paths.iter().any(|path| path == &newest);
            if ready_passes == 1 {
                assert_eq!(
                    pass.report.paths.first(),
                    Some(&newest),
                    "the first catch-up pass must lead with the newest project rollout"
                );
            }
            if includes_newest && first_path_ms.is_none() {
                first_path_ms = Some(started.elapsed().as_millis());
                let progress = try_admit_codex_jsonl_observations_for_project_window(
                    &newest,
                    &project,
                    project_id.clone(),
                    &admission,
                    u64::MAX,
                    &cancellation,
                )
                .await
                .expect("admit newest rollout");
                assert!(
                    progress.frames_persisted > 0,
                    "newest project rollout must persist a frame"
                );
                first_admit_ms = Some(started.elapsed().as_millis());
                eprintln!(
                    "CATCHUP_MEASURE time_to_first_path_ms={} time_to_first_admit_ms={}",
                    first_path_ms.unwrap_or(0),
                    first_admit_ms.unwrap_or(0)
                );
            }
            let empty = pass.report.paths.is_empty();
            frontier = pass.next_frontier;
            hub.acknowledge("project");
            if frontier.is_complete() {
                eprintln!(
                    "CATCHUP_MEASURE time_to_complete_ms={} ready_passes={ready_passes} time_to_first_admit_ms={first_admit_ms:?}",
                    started.elapsed().as_millis()
                );
                break;
            }
            if empty {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }

        assert!(
            first_admit_ms.is_some(),
            "catch-up never admitted the newest rollout"
        );
        assert!(frontier.is_complete());
    }
}
