//! Session / LCM / refresh family: transcript-derived reads plus the
//! refresh lifecycle (begin → status/cancel as effects over the
//! {scope,session,source,target} selector envelope).

use serde_json::{Value, json};
use std::path::Path;

use tracedecay::daemon::ProductionProjectCompositionHarnessV1;

use crate::queries::{PrimeStep, Query, QueryContext, ToolGroup, call_json_tool, five};

use super::{eq, rq};

pub(crate) const WORKFLOW_SESSION_ID: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
pub(crate) const WORKFLOW_RUN_ID: &str = "wf-performance-fixture-4a71c8";

pub(crate) fn seed_host_workflow(
    home: &Path,
    project_root: &Path,
    session: &str,
) -> Result<(), String> {
    if session != WORKFLOW_SESSION_ID {
        return Err("host workflow seed requires its isolated fixture session identity".to_owned());
    }
    let session_dir = home
        .join(".claude/projects/performance-fixture")
        .join(session);
    let agents = session_dir
        .join("subagents/workflows")
        .join(WORKFLOW_RUN_ID);
    let metadata = session_dir.join("workflows");
    std::fs::create_dir_all(&agents)
        .map_err(|error| format!("workflow agent directory: {error}"))?;
    std::fs::create_dir_all(&metadata)
        .map_err(|error| format!("workflow metadata directory: {error}"))?;
    let agent = "a17141dbe5a308242";
    let write = |path: &Path, records: Vec<Value>| -> Result<(), String> {
        let body = records
            .into_iter()
            .map(|record| record.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, format!("{body}\n"))
            .map_err(|error| format!("workflow fixture {}: {error}", path.display()))
    };
    write(
        &session_dir.with_extension("jsonl"),
        vec![json!({
            "type":"user", "sessionId":session, "cwd":project_root,
            "timestamp":"2026-09-12T00:00:01.000Z",
            "message":{"role":"user","content":"Read the retained workflow fixture."}
        })],
    )?;
    write(
        &metadata.join(format!("{WORKFLOW_RUN_ID}.json")),
        vec![json!({
            "runId":WORKFLOW_RUN_ID, "workflowName":"tracedecay-performance-fixture",
            "summary":"Retained host workflow evidence.", "status":"completed",
            "startTime":1789171201000_i64, "durationMs":1000, "agentCount":1,
            "defaultModel":"fixture-model",
            "phases":[{"title":"Validate","detail":"Read the fixture"}],
            "workflowProgress":[{"type":"workflow_agent","label":"validate:fixture",
                "phaseTitle":"Validate","phaseIndex":1,"agentId":agent,"state":"done",
                "startedAt":1789171201000_i64,"lastProgressAt":1789171202000_i64}]
        })],
    )?;
    write(
        &agents.join(format!("agent-{agent}.jsonl")),
        vec![
            json!({"type":"user","isSidechain":true,"sessionId":format!("agent-{agent}"),
            "cwd":project_root,"timestamp":"2026-09-12T00:00:01.000Z",
            "message":{"role":"user","content":"Read the retained fixture."}}),
            json!({"type":"assistant","isSidechain":true,"sessionId":format!("agent-{agent}"),
            "timestamp":"2026-09-12T00:00:02.000Z",
            "message":{"role":"assistant","content":"Fixture read complete.",
                "usage":{"input_tokens":10,"output_tokens":8}}}),
        ],
    )?;
    write(
        &agents.join("journal.jsonl"),
        vec![
            json!({"type":"started","agentId":agent}),
            json!({"type":"result","agentId":agent}),
        ],
    )
}

pub(crate) async fn prepare_host_workflow(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
) -> Result<(), String> {
    let args = json!({"session_id":WORKFLOW_SESSION_ID,"limit":10});
    let mut last_failure = "workflow ingestion has not published the fixture".to_owned();
    for _ in 0..30 {
        match call_json_tool(harness, project_root, "tracedecay_workflows", args.clone()).await {
            Ok(response) => match verify_host_workflow(
                &args,
                response
                    .pointer("/outcome/value/payload")
                    .unwrap_or(&response),
            ) {
                Ok(()) => return Ok(()),
                Err(error) => last_failure = error,
            },
            Err(error) => last_failure = error.to_string(),
        }
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    }
    Err(last_failure)
}

fn lookup_meta(order: &str, page_size: u32) -> Value {
    json!({
        "order": order,
        "page": {"page_size": page_size, "cursor": null},
        "projection": "summary",
        "temporal": {"kind": "current"},
    })
}

fn sid(ctx: &QueryContext) -> String {
    ctx.seeds
        .lcm_session
        .clone()
        .unwrap_or_else(|| "td-bench-missing".into())
}

/// `{scope,session,source,target}` refreshed per call; `None` means the seed
/// begin failed and the refresh groups run the not-found lane instead.
fn selectors(ctx: &QueryContext) -> Value {
    ctx.seeds
        .refresh_selectors
        .clone()
        .unwrap_or_else(|| json!({"missing": true}))
}

/// Selectors + a minted handle (status/cancel inputs).
fn refresh_args(ctx: &QueryContext) -> Value {
    let mut args = selectors(ctx);
    args["format"] = json!("json");
    args["handle"] = json!(
        ctx.seeds
            .refresh_handle
            .clone()
            .unwrap_or_else(|| "td-bench-refresh-missing".into())
    );
    args
}

pub(crate) fn verify_session_fixture(
    ctx: &QueryContext,
    tool: &str,
    label: &str,
    args: &Value,
    response: &Value,
    prepared_anchors: Option<&Value>,
) -> Option<Result<(), String>> {
    if !crate::repos::small_fixture_enabled()
        || !matches!(
            (tool, label),
            ("tracedecay_lcm_describe", "lcm_describe")
                | ("tracedecay_lcm_status", "lcm_status")
                | ("tracedecay_sessions_for", "sessions_for")
                | ("tracedecay_message_search", "message_search")
                | ("tracedecay_lcm_grep", "lcm_grep")
                | ("tracedecay_lcm_expand_query", "lcm_expand_query")
                | ("tracedecay_lcm_doctor", "lcm_doctor")
                | ("tracedecay_session_lookup", "session_lookup")
                | ("tracedecay_workflows", "workflows")
                | ("tracedecay_session_refresh_begin", "refresh_begin")
                | ("tracedecay_session_refresh_status", "refresh_status")
                | ("tracedecay_session_refresh_cancel", "refresh_cancel")
        )
    {
        return None;
    }
    let payload = response
        .pointer("/outcome/value/payload")
        .unwrap_or(response);
    Some((|| {
        let session = ctx
            .seeds
            .lcm_session
            .as_deref()
            .ok_or_else(|| "session read has no admitted fixture session".to_owned())?;
        if tool == "tracedecay_workflows" {
            return verify_host_workflow(args, payload);
        }
        if tool == "tracedecay_session_lookup" {
            if args["session_id"] != session {
                return Err("session lookup selected another session".to_owned());
            }
            let anchors = |value: &Value| -> Result<Vec<String>, String> {
                let mut anchors = value
                    .as_array()
                    .ok_or_else(|| "session lookup omitted its anchors".to_owned())?
                    .iter()
                    .map(|anchor| {
                        anchor
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "session lookup returned a malformed anchor".to_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                anchors.sort();
                if anchors.len() != 2 || anchors[0] == anchors[1] {
                    return Err(
                        "session lookup did not retain distinct anchors for both fixture messages"
                            .to_owned(),
                    );
                }
                Ok(anchors)
            };
            let prepared = prepared_anchors.ok_or_else(|| {
                "session lookup has no validated message-authority anchors".to_owned()
            })?;
            return if anchors(&payload["anchors"])? == anchors(prepared)? {
                Ok(())
            } else {
                Err(
                    "session lookup anchors differ from the validated literal transcript authority"
                        .to_owned(),
                )
            };
        }
        if matches!(
            tool,
            "tracedecay_message_search" | "tracedecay_lcm_grep" | "tracedecay_lcm_expand_query"
        ) {
            return verify_transcript_query(session, tool, args, payload);
        }
        if tool.starts_with("tracedecay_session_refresh_") {
            return verify_refresh_fixture(ctx, tool, args, payload);
        }
        if tool == "tracedecay_lcm_doctor" {
            return verify_paths(
                payload,
                &[
                    ("/status", json!("complete")),
                    ("/authority_outcome/state", json!("ready")),
                    ("/health/status", json!("complete")),
                    ("/health/findings", json!([])),
                    ("/projection/state", json!("current")),
                    ("/projection/convergence/state", json!("converged")),
                    ("/projection/worker/backlog", json!(0)),
                    ("/projection/worker/blocker", Value::Null),
                ],
            );
        }
        let mut expected = vec![("/status", json!("ok"))];
        if tool == "tracedecay_sessions_for" {
            let branch = ctx
                .seeds
                .branch
                .as_deref()
                .ok_or_else(|| "session Git read has no fixture branch".to_owned())?;
            if args["git_ref"] != "branch" || args["value"] != branch {
                return Err("session Git read did not select the fixture branch".to_owned());
            }
            expected.extend([
                ("/git_ref", json!("branch")),
                ("/value", json!(branch)),
                ("/index_empty", json!(false)),
                ("/index/projection_available", json!(true)),
            ]);
            let results = payload["results"]
                .as_array()
                .ok_or_else(|| "session Git read omitted results".to_owned())?;
            let row = results
                .iter()
                .find(|row| row["session_id"] == session)
                .ok_or_else(|| "session Git read omitted the seeded transcript".to_owned())?;
            for (field, value) in [
                ("provider", json!("codex")),
                ("branch", json!(branch)),
                ("worktree", json!(ctx.project_root)),
                ("first_ts", json!(1789171201_i64)),
                ("last_ts", json!(1789171202_i64)),
            ] {
                if row.get(field) != Some(&value) {
                    return Err(format!(
                        "session Git evidence {field} differs from fixture {value}"
                    ));
                }
            }
        } else {
            if args["session_id"] != session || args["provider"] != "codex" {
                return Err("LCM read did not select the admitted Codex fixture session".to_owned());
            }
            expected.extend([
                ("/session_id", json!(session)),
                ("/provider", json!("codex")),
            ]);
            if tool == "tracedecay_lcm_describe" {
                if args.pointer("/target/kind") != Some(&json!("session")) {
                    return Err("LCM describe did not select the session target".to_owned());
                }
                expected.extend([
                    ("/state", json!("available")),
                    ("/description/session_id", json!(session)),
                    ("/description/provider", json!("codex")),
                    ("/description/target", json!("session")),
                ]);
                let messages = payload
                    .pointer("/description/raw_messages")
                    .and_then(Value::as_array)
                    .ok_or_else(|| "LCM describe omitted raw fixture messages".to_owned())?;
                if messages.len() != 2 {
                    return Err(
                        "LCM describe did not retain exactly the two seeded messages".to_owned(),
                    );
                }
                for (role, content) in [
                    ("user", "bench sweep user message"),
                    ("assistant", "bench sweep assistant reply"),
                ] {
                    let message = messages
                        .iter()
                        .find(|message| message["role"] == role)
                        .ok_or_else(|| format!("LCM describe omitted the seeded {role} message"))?;
                    if message["content_preview"] != content
                        || message.pointer("/content_range/truncated") != Some(&json!(false))
                        || message.pointer("/content_range/offset") != Some(&json!(0))
                        || message.pointer("/content_range/returned_chars")
                            != Some(&json!(content.chars().count()))
                        || message.pointer("/content_range/total_chars")
                            != Some(&json!(content.chars().count()))
                    {
                        return Err(format!(
                            "LCM describe changed or truncated the seeded {role} message"
                        ));
                    }
                }
            } else {
                let deep = args["deep"]
                    .as_bool()
                    .ok_or_else(|| "LCM status request omitted its depth".to_owned())?;
                expected.extend([
                    ("/deep", json!(deep)),
                    ("/authority_outcome/state", json!("ready")),
                    ("/lcm/raw_message_count", json!(2)),
                    ("/lcm/store/messages", json!(2)),
                    ("/lcm/payload/root_contained", json!(true)),
                    ("/lcm/missing_payload_count", json!(0)),
                    ("/lcm/store/token_estimate/complete", json!(deep)),
                ]);
                if deep {
                    expected.extend([
                        ("/lcm/store/token_estimate/scanned_messages", json!(2)),
                        ("/lcm/payload/coverage/state", json!("complete")),
                        ("/lcm/payload/integrity_mismatch_count", json!(0)),
                    ]);
                } else {
                    expected.extend([
                        ("/lcm/payload/coverage/state", json!("partial")),
                        (
                            "/lcm/payload/coverage/reason",
                            json!("payload_file_census_requires_deep_status"),
                        ),
                        ("/lcm/payload/integrity_mismatch_count", Value::Null),
                    ]);
                }
            }
        }
        for (path, value) in expected {
            if payload.pointer(path) != Some(&value) {
                return Err(format!(
                    "{path} expected {value}, received {:?}",
                    payload.pointer(path)
                ));
            }
        }
        Ok(())
    })())
}

fn prime_session_lookup(ctx: &QueryContext, _iteration: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: vec![],
        tool: "tracedecay_lcm_load_session",
        args: json!({"provider": "codex", "session_id": sid(ctx), "limit": 10, "format": "json"}),
        capture: &[(
            "outcome.value.payload.temporal.anchors",
            "session_lookup_anchors",
        )],
    }]
}

fn verify_paths(payload: &Value, expected: &[(&str, Value)]) -> Result<(), String> {
    for (path, value) in expected {
        if payload.pointer(path) != Some(value) {
            return Err(format!(
                "{path} expected {value}, received {:?}",
                payload.pointer(path)
            ));
        }
    }
    Ok(())
}

fn verify_host_workflow(args: &Value, payload: &Value) -> Result<(), String> {
    if args["session_id"] != WORKFLOW_SESSION_ID {
        return Err("workflow query did not select the persisted host fixture session".to_owned());
    }
    verify_paths(
        payload,
        &[
            ("/status", json!("ok")),
            ("/mode", json!("session")),
            ("/session_id", json!(WORKFLOW_SESSION_ID)),
        ],
    )?;
    let runs = payload["runs"]
        .as_array()
        .ok_or_else(|| "workflow query omitted retained runs".to_owned())?;
    let run = runs
        .iter()
        .find(|run| run["run_id"] == WORKFLOW_RUN_ID)
        .ok_or_else(|| "workflow query omitted the persisted host fixture run".to_owned())?;
    verify_paths(
        run,
        &[
            ("/parent_session_id", json!(WORKFLOW_SESSION_ID)),
            ("/name", json!("tracedecay-performance-fixture")),
            ("/description", json!("Retained host workflow evidence.")),
            ("/status", json!("completed")),
            ("/result_summary", json!("Retained host workflow evidence.")),
            ("/started_ts", json!(1789171201_i64)),
            ("/ended_ts", json!(1789171202_i64)),
            ("/agent_count", json!(1)),
        ],
    )
}

fn verify_transcript_query(
    session: &str,
    tool: &str,
    args: &Value,
    payload: &Value,
) -> Result<(), String> {
    let (query_field, items_path, content_path) = match tool {
        "tracedecay_message_search" => ("query", "/results", "/message/text"),
        "tracedecay_lcm_grep" => ("query", "/hits", "/snippet"),
        _ => ("prompt", "/context_blocks", "/content"),
    };
    let query = args[query_field]
        .as_str()
        .ok_or_else(|| "transcript query omitted search text".to_owned())?;
    if args["provider"] != "codex" {
        return Err("transcript query selected another provider".to_owned());
    }
    if !["bench", "sweep", "user", "assistant", "reply"].contains(&query) {
        return Err(format!(
            "transcript query '{query}' has no literal fixture witness"
        ));
    }
    let expected = ["bench sweep user message", "bench sweep assistant reply"]
        .into_iter()
        .filter(|content| content.contains(query))
        .collect::<Vec<_>>();
    let items = payload
        .pointer(items_path)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("transcript query omitted {items_path}"))?;
    if items.len() != expected.len()
        || expected.iter().any(|content| {
            items
                .iter()
                .filter(|item| item.pointer(content_path).and_then(Value::as_str) == Some(*content))
                .count()
                != 1
        })
    {
        return Err(format!(
            "transcript query '{query}' changed its literal matching messages"
        ));
    }
    let mut expected_fields = vec![("/status", json!("ok")), ("/provider", json!("codex"))];
    if tool == "tracedecay_message_search" {
        expected_fields.extend([
            ("/outcome", json!("complete")),
            ("/refresh_required", json!(false)),
        ]);
        for item in items {
            verify_paths(
                item,
                &[
                    ("/message/session_id", json!(session)),
                    ("/message/provider", json!("codex")),
                ],
            )?;
        }
    } else {
        if args["session_id"] != session {
            return Err("transcript query selected another session".to_owned());
        }
        if tool == "tracedecay_lcm_expand_query" {
            expected_fields.extend([
                ("/session_id", json!(session)),
                ("/context_truncated", json!(false)),
                ("/needs_synthesis", json!(true)),
            ]);
        } else {
            for item in items {
                verify_paths(
                    item,
                    &[
                        ("/session_id", json!(session)),
                        ("/provider", json!("codex")),
                    ],
                )?;
            }
        }
    }
    verify_paths(payload, &expected_fields)
}

fn verify_refresh_fixture(
    ctx: &QueryContext,
    tool: &str,
    args: &Value,
    payload: &Value,
) -> Result<(), String> {
    let session = ctx
        .seeds
        .lcm_session
        .as_deref()
        .ok_or_else(|| "refresh fixture has no admitted session".to_owned())?;
    let handle = ctx
        .seeds
        .refresh_handle
        .as_deref()
        .ok_or_else(|| "refresh fixture has no completed handle".to_owned())?;
    let operation = ctx
        .seeds
        .refresh_operation_id
        .as_deref()
        .ok_or_else(|| "refresh fixture has no completed operation".to_owned())?;
    verify_paths(
        args,
        &[
            ("/session/id", json!(session)),
            ("/scope/kind", json!("profile")),
            ("/source/scope", json!("codex")),
            (
                "/target/frontier",
                json!({"observed_through": 0, "committed_through": 0}),
            ),
        ],
    )?;
    verify_paths(
        payload,
        &[
            ("/tool", json!(tool)),
            ("/scope", json!("profile")),
            ("/operation_id", json!(operation)),
            ("/error", Value::Null),
        ],
    )?;
    if tool == "tracedecay_session_refresh_begin" {
        return verify_paths(
            payload,
            &[("/outcome", json!("joined")), ("/handle", json!(handle))],
        );
    }
    if args["handle"] != handle {
        return Err("refresh request did not retain the completed fixture handle".to_owned());
    }
    if tool == "tracedecay_session_refresh_cancel" && payload["handle"] != handle {
        return Err("refresh cancel changed the completed fixture handle".to_owned());
    }
    verify_paths(
        payload,
        &[
            ("/outcome", json!("complete")),
            ("/receipt/state", json!("complete")),
            ("/receipt/session_id", json!(session)),
            ("/receipt/operation_id", json!(operation)),
            ("/receipt/failure_code", Value::Null),
            ("/receipt/frontier", args["target"]["frontier"].clone()),
        ],
    )?;
    let sources = payload
        .pointer("/receipt/source_coverage")
        .and_then(Value::as_array)
        .ok_or_else(|| "refresh receipt omitted its source coverage".to_owned())?;
    let source = sources
        .iter()
        .find(|source| source["source_id"] == format!("{session}:codex"))
        .ok_or_else(|| "refresh receipt omitted the requested Codex source".to_owned())?;
    verify_paths(
        source,
        &[
            ("/state", json!("fresh")),
            ("/reason/kind", json!("caught_up")),
            ("/committed_frontier", json!(0)),
            ("/observed_frontier", json!(0)),
            ("/missing_intervals", json!([])),
        ],
    )
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    let session = sid(ctx);
    let msg_id = ctx
        .seeds
        .lcm_message_id
        .clone()
        .unwrap_or_else(|| "td-bench-message-missing".into());

    // Session/LCM reads need an ingested session — the seed writes one when
    // transcript ingest is mounted under this composition; otherwise the
    // whole family stays an honest seed-ledger skip.
    let has_session = ctx.seeds.lcm_session.is_some();
    if has_session {
        let search_queries = if crate::repos::small_fixture_enabled() {
            ["bench", "sweep", "user", "assistant", "reply"]
        } else {
            ["bench", "import", "fn", "error", "test"]
        };
        let expansion_queries = if crate::repos::small_fixture_enabled() {
            search_queries
        } else {
            ["bench", "mount", "session", "store", "index"]
        };
        out.push(ToolGroup {
            tool: "tracedecay_lcm_load_session",
            queries: five(|i| {
                rq(
                    "tracedecay_lcm_load_session",
                    "lcm_load_session",
                    json!({
                        "provider": "codex",
                        "session_id": session,
                        "limit": 5 + i as u32,
                        "format": "json",
                    }),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_message_search",
            queries: five(|i| {
                rq(
                    "tracedecay_message_search",
                    "message_search",
                    json!({
                        "query": search_queries[i],
                        "provider": "codex",
                        "limit": 10,
                    }),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_session_lookup",
            queries: five(|i| {
                let args = json!({
                    "session_id": session,
                    "meta": lookup_meta("temporal_descending", 5 + i as u32),
                });
                if crate::repos::small_fixture_enabled() {
                    Query::prepared_read(
                        "session_lookup",
                        "tracedecay_session_lookup",
                        args,
                        i,
                        prime_session_lookup,
                    )
                } else {
                    rq("tracedecay_session_lookup", "session_lookup", args)
                }
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_workflows",
            queries: five(|_i| {
                rq(
                    "tracedecay_workflows",
                    "workflows",
                    json!({"session_id": if crate::repos::small_fixture_enabled() { WORKFLOW_SESSION_ID } else { session.as_str() }, "limit": 10}),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_lcm_describe",
            queries: five(|_i| {
                rq(
                    "tracedecay_lcm_describe",
                    "lcm_describe",
                    json!({
                        "provider": "codex",
                        "session_id": session,
                        // `canonical_occurrence` is an expansion target.  Describe
                        // accepts a session, summary node, or external payload.
                        "target": {"kind": "session"},
                    }),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_lcm_expand_query",
            queries: five(|i| {
                rq(
                    "tracedecay_lcm_expand_query",
                    "lcm_expand_query",
                    json!({
                        "provider": "codex",
                        "session_id": session,
                        "prompt": expansion_queries[i],
                        "max_results": 10,
                    }),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_lcm_expand",
            queries: five(|_i| {
                rq(
                    "tracedecay_lcm_expand",
                    "lcm_expand",
                    json!({
                        "provider": "codex",
                        "session_id": session,
                        "target": {"kind": "canonical_occurrence", "message_id": msg_id},
                    }),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_lcm_grep",
            queries: five(|i| {
                rq(
                    "tracedecay_lcm_grep",
                    "lcm_grep",
                    json!({
                        "query": search_queries[i],
                        "session_id": session,
                        "provider": "codex",
                        "limit": 10,
                    }),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_lcm_status",
            queries: five(|i| {
                rq(
                    "tracedecay_lcm_status",
                    "lcm_status",
                    json!({
                        "session_id": session,
                        "provider": "codex",
                        "deep": i % 2 == 0,
                    }),
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_lcm_doctor",
        queries: five(|_i| rq("tracedecay_lcm_doctor", "lcm_doctor", json!({}))),
    });
    if let Some(branch) = ctx.seeds.branch.clone() {
        out.push(ToolGroup {
            tool: "tracedecay_sessions_for",
            queries: five(|_i| {
                rq(
                    "tracedecay_sessions_for",
                    "sessions_for",
                    json!({"git_ref": "branch", "value": branch, "limit": 10}),
                )
            }),
        });
    }

    // refresh: begin is the timed effect; status/cancel take the same
    // selector envelope plus the minted handle. Skips with the session
    // family when no refreshable session exists.
    if ctx.seeds.refresh_selectors.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_session_refresh_begin",
            queries: five(|_i| {
                eq(
                    "tracedecay_session_refresh_begin",
                    "refresh_begin",
                    selectors(ctx),
                    |_ctx, _iter| Vec::new(),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_session_refresh_status",
            queries: five(|_i| {
                rq(
                    "tracedecay_session_refresh_status",
                    "refresh_status",
                    refresh_args(ctx),
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_session_refresh_cancel",
            queries: five(|_i| {
                let mut args = selectors(ctx);
                args["handle"] = json!("{{handle}}");
                eq(
                    "tracedecay_session_refresh_cancel",
                    "refresh_cancel",
                    args,
                    |ctx, _iter| vec![begin_prime(ctx)],
                )
            }),
        });
    }
}

fn begin_prime(ctx: &QueryContext) -> PrimeStep {
    let mut args = selectors(ctx);
    args["format"] = json!("json");
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_session_refresh_begin",
        args,
        capture: &[("dig:handle", "handle")],
    }
}
