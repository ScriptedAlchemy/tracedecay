//! Memory family: fact store reads, fact effect writes (add/update/remove/
//! supersede with per-iteration fresh ids), and the feedback read surface
//! (handles minted by `feedback_advisory_cycle`).

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_contracts::RequestId;
use tracedecay_contracts::automation::{AgentTaskKind, AutomationTrigger};
use tracedecay_contracts::feedback::FeedbackAdvisoryCycleSurfaceResultV1;
use tracedecay_contracts::retained_surfaces::{FactStoreCurateRequestV1, FactStoreCurateResultV1};
use tracedecay_contracts::retrieval::{
    AUTOMATION_RUN_LIST_MAX_LIMIT, AutomationRunListResultV1, AutomationRunViewResultV1,
};

use crate::queries::{
    PrimeStep, Query, QueryContext, QueryKind, Seeds, ToolGroup, call_json_tool, five, prime_symbol,
};

use super::{eq, fact_add_args, file_at, no_primes, rq};

const FACT_MARKERS: [&str; 5] = [
    "amber observatory calibrates a lunar compass",
    "bravo orchard archives rainfall ledgers",
    "cinder railway schedules a northern cargo",
    "delta workshop replaces cracked ceramic valves",
    "ember harbor records a tide warning",
];

fn fact_marker(iter: usize) -> &'static str {
    FACT_MARKERS[iter % FACT_MARKERS.len()]
}

pub(crate) fn verify_fact_curate_admission(
    ctx: &QueryContext,
    args: &Value,
    response: &Value,
) -> Result<FactStoreCurateResultV1, String> {
    let mut bounds = args.clone();
    bounds
        .as_object_mut()
        .ok_or_else(|| "curator arguments are not an object".to_owned())?
        .remove("format");
    let bounds: FactStoreCurateRequestV1 =
        serde_json::from_value(bounds).map_err(|error| error.to_string())?;
    let request_id = response["request_id"]
        .as_str()
        .ok_or_else(|| "curator admission omitted its request identity".to_owned())?;
    let request_id = RequestId::new(request_id).map_err(|error| error.to_string())?;
    let expected = bounds
        .automation_request(&request_id)
        .map_err(|error| error.to_string())?;
    let admitted: FactStoreCurateResultV1 =
        serde_json::from_value(feedback_payload(response).clone())
            .map_err(|error| format!("invalid curator admission: {error}"))?;
    if !admitted.matches_admission(&expected)
        || response["scope"]["project_id"].as_str() != ctx.seeds.project_id.as_deref()
        || ctx.seeds.project_id.is_none()
        || response["outcome"]["outcome"] != "effect"
        || response["outcome"]["value"]["receipt"]["request_id"].as_str()
            != Some(request_id.as_str())
        || response["outcome"]["value"]["receipt"]["operation"]
            != "use-case.application.retained.fact-store-curate"
    {
        return Err(
            "curator admission did not bind the exact caller bounds, run and project".into(),
        );
    }
    Ok(admitted)
}

pub(crate) async fn follow_fact_curate_completion(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    ctx: &QueryContext,
    args: &Value,
    response: &Value,
) -> Result<AutomationRunViewResultV1, String> {
    let admitted = verify_fact_curate_admission(ctx, args, response)?;
    let expires_at = response
        .pointer("/outcome/value/execution/effective_deadline/expires_at")
        .and_then(Value::as_i64)
        .ok_or_else(|| "curator admission omitted its settlement deadline".to_owned())?;
    loop {
        let remaining = expires_at.saturating_sub(super::now_micros());
        let remaining = u64::try_from(remaining)
            .ok()
            .filter(|remaining| *remaining > 0)
            .ok_or_else(|| {
                format!(
                    "curator run {} did not settle by its admitted deadline",
                    admitted.run_id.as_str()
                )
            })?;
        let listing = tokio::time::timeout(
            Duration::from_micros(remaining),
            call_json_tool(
                harness,
                project_root,
                "tracedecay_automation_run_list",
                json!({
                    "limit": AUTOMATION_RUN_LIST_MAX_LIMIT,
                }),
            ),
        )
        .await
        .map_err(|_| "curator ledger read exceeded its admitted deadline".to_owned())??;
        let listing: AutomationRunListResultV1 =
            serde_json::from_value(feedback_payload(&listing).clone())
                .map_err(|error| format!("invalid curator ledger listing: {error}"))?;
        if let Some(run) = listing
            .runs
            .iter()
            .find(|run| run.run_id == admitted.run_id.as_str())
        {
            if run.task != AgentTaskKind::MemoryCurator
                || run.trigger != AutomationTrigger::Application
            {
                return Err("curator ledger returned another task or admission source".into());
            }
            if run.status.is_terminal() {
                let remaining = expires_at.saturating_sub(super::now_micros());
                let remaining = u64::try_from(remaining)
                    .ok()
                    .filter(|remaining| *remaining > 0)
                    .ok_or_else(|| {
                        "curator terminal view exceeded its admitted deadline".to_owned()
                    })?;
                let terminal = tokio::time::timeout(
                    Duration::from_micros(remaining),
                    call_json_tool(
                        harness,
                        project_root,
                        "tracedecay_automation_run_view",
                        json!({"run_id":admitted.run_id}),
                    ),
                )
                .await
                .map_err(|_| "curator terminal view exceeded its admitted deadline".to_owned())??;
                let terminal: AutomationRunViewResultV1 =
                    serde_json::from_value(feedback_payload(&terminal).clone())
                        .map_err(|error| format!("invalid curator terminal view: {error}"))?;
                if terminal.run.run_id != admitted.run_id.as_str()
                    || terminal.run.task != AgentTaskKind::MemoryCurator
                    || terminal.run.trigger != AutomationTrigger::Application
                    || !terminal.run.status.is_terminal()
                    || terminal.run.completed_at.is_empty()
                    || terminal.run.completed_at_micros.is_none()
                {
                    return Err("curator terminal view did not bind the admitted run and durable completion".into());
                }
                return Ok(terminal);
            }
        } else if listing.has_more {
            return Err("curator run was absent from an incomplete public ledger page".into());
        }
        tokio::time::sleep(Duration::from_micros(remaining.min(250_000))).await;
    }
}

const FEEDBACK_DIAGNOSTIC: &str = "unused variable: `feedback_bench_unused_probe`";
const FEEDBACK_PATH: &str = "src/lib.rs";

pub(crate) async fn seed_feedback_fixture(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) -> Result<(), String> {
    if !crate::repos::small_fixture_enabled() {
        return Err("feedback warning requires the isolated small fixture".into());
    }
    let output_directory = tempfile::tempdir().map_err(|error| error.to_string())?;
    let compiled = Command::new("rustc")
        .current_dir(project_root)
        .args([
            "--crate-type=lib",
            "--edition=2024",
            "--emit=metadata",
            "--color=never",
            FEEDBACK_PATH,
            "--out-dir",
        ])
        .arg(output_directory.path())
        .output()
        .map_err(|error| format!("compile feedback fixture: {error}"))?;
    let diagnostic = String::from_utf8(compiled.stderr).map_err(|error| error.to_string())?;
    if !compiled.status.success() || !diagnostic.contains(FEEDBACK_DIAGNOSTIC) {
        return Err(format!(
            "compiler did not emit the fixture warning: {diagnostic}"
        ));
    }
    seeds.compiler_diagnostic = Some(diagnostic.clone());
    seeds.compiler_diagnostic_path = Some(FEEDBACK_PATH.into());
    let diagnosed = call_json_tool(
        harness,
        project_root,
        "tracedecay_diagnose",
        json!({
            "cargo_output": diagnostic, "include_callers": false,
        }),
    )
    .await?;
    verify_diagnosed_fixture(&diagnosed)?;
    let document_uri = url::Url::from_file_path(project_root.join(FEEDBACK_PATH))
        .map_err(|()| "feedback fixture has no file URI".to_owned())?
        .to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let response = super::call_lenient(
            harness,
            project_root,
            "tracedecay_feedback_advisory_cycle",
            json!({
                "document_uri": document_uri, "format": "json",
            }),
        )
        .await?;
        let payload = feedback_payload(&response);
        if let Ok(cycle) =
            serde_json::from_value::<FeedbackAdvisoryCycleSurfaceResultV1>(payload.clone())
        {
            if cycle.cycle.published
                && cycle.read_handles.is_some()
                && cycle.cycle.cycle.findings.iter().any(|finding| {
                    finding.safe_bounded_preview.as_deref() == Some(FEEDBACK_DIAGNOSTIC)
                        && finding.retrieval_anchor_id.is_some()
                        && finding
                            .diagnostic_projection
                            .as_ref()
                            .is_some_and(|projection| {
                                projection.safe_bounded_message == FEEDBACK_DIAGNOSTIC
                                    && projection.span.start_byte == 70
                                    && projection.span.end_byte == 106
                                    && cycle
                                        .cycle
                                        .cycle
                                        .impact
                                        .as_ref()
                                        .is_some_and(|impact| impact.target.file == projection.file)
                            })
                        && cycle.finding_handles.iter().any(|handle| {
                            handle.finding_id == finding.finding_id
                                && handle.expansion_handle.is_some()
                        })
                })
            {
                seeds.feedback = Some(cycle);
                return Ok(());
            }
        }
        let problem = response
            .get("problem")
            .or_else(|| response.pointer("/value/problem"));
        if tokio::time::Instant::now() >= deadline
            || problem.is_some_and(|problem| problem["retryable"] != true)
        {
            return Err(format!(
                "feedback fixture warning was not published: {response}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn feedback_payload(response: &Value) -> &Value {
    response
        .pointer("/value/outcome/value/payload")
        .or_else(|| response.pointer("/outcome/value/payload"))
        .or_else(|| response.get("value"))
        .unwrap_or(response)
}

fn verify_diagnosed_fixture(response: &Value) -> Result<(), String> {
    let payload = feedback_payload(response);
    if payload["published"]["status"] == "published"
        && payload["diagnostics"]
            .as_array()
            .is_some_and(|diagnostics| {
                diagnostics.iter().any(|diagnostic| {
                    diagnostic["message"] == FEEDBACK_DIAGNOSTIC
                        && diagnostic["file"] == FEEDBACK_PATH
                        && diagnostic["line"] == 2
                        && diagnostic["severity"] == "warning"
                        && diagnostic["node"]["name"] == "fixture_catalog_total"
                })
            })
    {
        Ok(())
    } else {
        Err(format!(
            "diagnose did not publish the literal fixture warning at src/lib.rs:2: {payload}"
        ))
    }
}

pub(crate) fn verify_feedback_fixture(
    ctx: &QueryContext,
    tool: &str,
    args: &Value,
    response: &Value,
) -> Option<Result<(), String>> {
    if tool == "tracedecay_diagnose"
        && ctx.seeds.compiler_diagnostic.as_deref() == args["cargo_output"].as_str()
        && ctx.seeds.compiler_diagnostic.is_some()
    {
        return Some(verify_diagnosed_fixture(response));
    }
    let cycle = ctx.seeds.feedback.as_ref()?;
    let read = cycle.read_handles.as_ref()?;
    let expected = cycle
        .cycle
        .cycle
        .findings
        .iter()
        .find(|finding| finding.safe_bounded_preview.as_deref() == Some(FEEDBACK_DIAGNOSTIC))?;
    let handles = cycle
        .finding_handles
        .iter()
        .find(|handle| handle.finding_id == expected.finding_id)?;
    let handle = match tool {
        "tracedecay_feedback_diagnostics" => Some(read.diagnostics_handle.as_str()),
        "tracedecay_feedback_list" => Some(read.list_handle.as_str()),
        "tracedecay_feedback_get" => Some(handles.get_handle.as_str()),
        "tracedecay_feedback_expand" => handles.expansion_handle.as_deref(),
        _ => return None,
    }?;
    if args["request_handle"].as_str() != Some(handle) {
        return None;
    }
    Some((|| {
        let payload = feedback_payload(response);
        let findings = match tool {
            "tracedecay_feedback_diagnostics" => payload["cycle"]["findings"]
                .as_array()
                .map(|findings| findings.iter().collect::<Vec<_>>()),
            "tracedecay_feedback_list" => payload["findings"]
                .as_array()
                .map(|findings| findings.iter().map(|finding| &finding["finding"]).collect()),
            _ => Some(vec![&payload["finding"]["finding"]]),
        }
        .ok_or_else(|| "feedback read omitted its retained findings".to_owned())?;
        let finding = findings
            .into_iter()
            .find(|finding| finding["finding_id"].as_str() == Some(expected.finding_id.as_str()))
            .ok_or_else(|| "feedback read omitted the published fixture finding".to_owned())?;
        let expected_file = expected.diagnostic_projection.as_ref().ok_or_else(|| {
            "published fixture finding omitted its diagnostic file anchor".to_owned()
        })?;
        if finding["safe_bounded_preview"] != FEEDBACK_DIAGNOSTIC
            || finding["lifecycle"] != "active"
            || finding["diagnostic_projection"]["safe_bounded_message"] != FEEDBACK_DIAGNOSTIC
            || finding["diagnostic_projection"]["severity"] != "warning"
            || finding["diagnostic_projection"]["producer"] != "code_diagnostic"
            || finding["diagnostic_projection"]["file"].as_str()
                != Some(expected_file.file.as_str())
            || finding["diagnostic_projection"]["span"] != json!({"start_byte":70,"end_byte":106})
            || finding["retrieval_anchor_id"].as_str()
                != expected
                    .retrieval_anchor_id
                    .as_ref()
                    .map(|anchor| anchor.as_str())
        {
            return Err("feedback read did not retain the literal unused-variable warning and exact src/lib.rs:2 anchor".into());
        }
        if tool == "tracedecay_feedback_expand"
            && payload["expansion"]["anchors"] != json!([expected.retrieval_anchor_id])
        {
            return Err("feedback expansion did not resolve the published warning anchor".into());
        }
        Ok(())
    })())
}

pub(crate) fn verify_memory_fixture(
    ctx: &QueryContext,
    tool: &str,
    label: &str,
    args: &Value,
    response: &Value,
    prepared: &HashMap<String, Value>,
) -> Option<Result<(), String>> {
    let payload = feedback_payload(response);
    if let Some((first, second, query, entities)) = &ctx.seeds.fact_pair {
        let selection = match (tool, label) {
            ("tracedecay_fact_store_get", "get") if args["fact_id"] == *first => {
                Some((vec![first.as_str()], vec![&payload["fact"]["fact"]]))
            }
            ("tracedecay_fact_store_get", "get") if args["fact_id"] == *second => {
                Some((vec![second.as_str()], vec![&payload["fact"]["fact"]]))
            }
            ("tracedecay_fact_store_list", "list") => Some((
                vec![first.as_str(), second.as_str()],
                payload["facts"]
                    .as_array()
                    .map(|facts| facts.iter().map(|fact| &fact["fact"]).collect())
                    .unwrap_or_default(),
            )),
            ("tracedecay_fact_store_related", "related")
                if entities.iter().any(|entity| args["entity"] == *entity) =>
            {
                Some((vec![first.as_str(), second.as_str()], memory_hits(payload)))
            }
            ("tracedecay_fact_store_probe", "read")
                if entities
                    .first()
                    .is_some_and(|entity| args["entity"] == *entity) =>
            {
                Some((vec![first.as_str()], memory_hits(payload)))
            }
            ("tracedecay_fact_store_reason", "read") if args["entities"] == json!(entities) => {
                Some((vec![first.as_str()], memory_hits(payload)))
            }
            ("tracedecay_fact_store_search", "read") if args["query"] == *query => {
                Some((vec![first.as_str()], memory_hits(payload)))
            }
            _ => None,
        };
        if let Some((identities, facts)) = selection {
            if response["truncated"] == true {
                return Some(Err(format!(
                    "{tool} returned a truncated wire response; the complete literal fixture payload is unavailable to this check"
                )));
            }
            return Some(identities.into_iter().try_for_each(|identity| {
                let (content, entities, trust) = if identity == first {
                    (
                        "bench constellation bench-alpha-0 bench-beta-0",
                        json!(["bench-alpha-0", "bench-beta-0"]),
                        800_000,
                    )
                } else {
                    (
                        "bench constellation bench-beta-0 bench-gamma-0",
                        json!(["bench-beta-0", "bench-gamma-0"]),
                        700_000,
                    )
                };
                let fact = facts
                    .iter()
                    .find(|fact| fact["fact_id"] == identity)
                    .ok_or_else(|| {
                        format!("{tool} omitted the seeded constellation fact {identity}")
                    })?;
                if fact["content"] != content
                    || fact["entities"] != entities
                    || fact["category"] != "tool"
                    || fact["source_label"] != "bench"
                    || fact["trust_score_millionths"] != trust
                    || fact["owner"]["project_id"].as_str() != ctx.seeds.project_id.as_deref()
                    || fact["owner"]["kind"] != "project"
                {
                    return Err(format!(
                        "{tool} changed the literal seeded constellation fact {identity}"
                    ));
                }
                Ok(())
            }));
        }
    }
    verify_configuration_fixture(ctx, tool, label, args, payload, prepared)
}

pub(crate) fn configuration_read_prime(ctx: &QueryContext, _selection: u64) -> Vec<PrimeStep> {
    let Some(key) = ctx.seeds.config_key.as_deref() else {
        return Vec::new();
    };
    vec![PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_configuration_get",
        args: json!({"key":key, "format":"json"}),
        capture: &[("digpath:payload:revision_id", "config_read_revision")],
    }]
}

fn memory_hits(payload: &Value) -> Vec<&Value> {
    payload["hits"]
        .as_array()
        .map(|hits| hits.iter().map(|hit| &hit["fact"]).collect())
        .unwrap_or_default()
}

fn verify_configuration_fixture(
    ctx: &QueryContext,
    tool: &str,
    label: &str,
    args: &Value,
    payload: &Value,
    prepared: &HashMap<String, Value>,
) -> Option<Result<(), String>> {
    let key = ctx.seeds.config_key.as_deref()?;
    let seeded_revision = ctx.seeds.config_revision.as_deref()?;
    let rollback_target = ctx.seeds.config_rollback_target.as_deref()?;
    let valid = match (tool, label) {
        ("tracedecay_configuration_get", "config_get") if args["key"] == key => {
            let Some(revision) = prepared.get("config_read_revision").and_then(Value::as_str)
            else {
                return Some(Err(
                    "configuration get omitted its freshly prepared head revision".into(),
                ));
            };
            let toggled = ctx.seeds.config_scalar.as_ref()?["value"].as_bool()?;
            payload["key"] == key
                && payload["revision_id"] == revision
                && payload["effective_value"] == json!({"kind":"boolean","value":!toggled})
        }
        ("tracedecay_configuration_list", "config_list") => {
            payload.as_array().is_some_and(|settings| {
                settings.iter().any(|setting| {
                    setting["key"] == key
                        && setting["sensitivity"] == "public"
                        && setting["restart_requirement"] == "none"
                })
            })
        }
        ("tracedecay_configuration_audit", "config_audit") => {
            payload["events"].as_array().is_some_and(|events| {
                events.iter().any(|event| {
                    event["event_kind"] == "applied"
                        && event["base_revision_id"] == rollback_target
                        && event["result_revision_id"] == seeded_revision
                        && event["idempotency_key"]
                            .as_str()
                            .is_some_and(|key| key.starts_with("bench-cfg-seed-rollback-"))
                })
            })
        }
        ("tracedecay_configuration_observed_state", "config_observed") => {
            let Some(revision) = prepared.get("config_read_revision").and_then(Value::as_str)
            else {
                return Some(Err(
                    "configuration observed state omitted its freshly prepared head revision"
                        .into(),
                ));
            };
            payload.as_array().is_some_and(|observations| {
                observations.iter().any(|observation| {
                    observation["component"] == "configuration.runtime-cache"
                        && observation["desired_revision_id"] == revision
                        && observation["observed_revision_id"] == revision
                        && observation["last_working_revision_id"] == revision
                        && observation["drift"] == "current"
                        && observation["restart_required"] == false
                        && observation
                            .get("activation_error_code")
                            .is_some_and(Value::is_null)
                })
            })
        }
        _ => return None,
    };
    Some(if valid {
        Ok(())
    } else {
        Err(format!(
            "{tool} did not retain the isolated configuration fixture's setting or applied revision"
        ))
    })
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    let pair = ctx.seeds.fact_pair.clone();
    let (fact_id, related_id, fact_query) = pair
        .as_ref()
        .map(|p| (p.0.clone(), p.1.clone(), p.2.clone()))
        .unwrap_or_else(|| ("missing".into(), "missing".into(), "missing".into()));
    let entities = pair.as_ref().map(|p| p.3.clone()).unwrap_or_default();

    out.push(ToolGroup {
        tool: "tracedecay_fact_store_get",
        queries: five(|i| {
            rq(
                "tracedecay_fact_store_get",
                "get",
                json!({"fact_id": if i % 2 == 0 { fact_id.clone() } else { related_id.clone() }}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_list",
        queries: five(|i| {
            rq(
                "tracedecay_fact_store_list",
                "list",
                json!({"limit": 10 + i as u32}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_related",
        queries: five(|i| {
            rq(
                "tracedecay_fact_store_related",
                "related",
                json!({
                    "entity": entities
                        .get(i % entities.len().max(1))
                        .cloned()
                        .unwrap_or_else(|| "missing".into()),
                    "limit": 10,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_add",
        queries: five(|i| {
            eq(
                "tracedecay_fact_store_add",
                "add",
                fact_add_args(
                    &format!("bench added fact {}", fact_marker(i)),
                    &[format!("bench-alpha-{i}"), format!("bench-beta-{i}")],
                    0.9,
                ),
                no_primes,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_update",
        queries: five(|_i| {
            eq(
                "tracedecay_fact_store_update",
                "update",
                json!({
                    "fact_id": "{{fact_id}}",
                    "content": "bench updated fact {{iter}}",
                    "entities": ["bench-updated-{{iter}}"],
                    "trust": 0.95,
                    "source_label": {"kind": "set", "value": "bench"},
                }),
                |_ctx, iter| vec![add_step(iter, "update")],
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_remove",
        queries: five(|_i| {
            eq(
                "tracedecay_fact_store_remove",
                "remove",
                json!({
                    "fact_id": "{{fact_id}}",
                }),
                |_ctx, iter| vec![add_step(iter, "remove")],
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_supersede",
        queries: five(|_i| {
            eq(
                "tracedecay_fact_store_supersede",
                "supersede",
                json!({
                    "fact_id": "{{old_fact_id}}",
                    "superseded_by": "{{new_fact_id}}",
                }),
                |_ctx, iter| {
                    vec![
                        add_step_named(iter, "sup-old", "old_fact_id"),
                        add_step_named(iter, "sup-new", "new_fact_id"),
                    ]
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_fact_feedback",
        queries: five(|_i| {
            eq(
                "tracedecay_fact_feedback",
                "fact_feedback",
                json!({
                    "fact_id": "{{fact_id}}",
                    "action": "helpful",
                    "reason": "bench feedback {{iter}}",
                    "source_label": "bench",
                }),
                |_ctx, iter| vec![add_step(iter, "feedback")],
            )
        }),
    });
    for (tool, args) in [
        (
            "tracedecay_fact_store_search",
            json!({"query": fact_query, "limit": 1}),
        ),
        (
            "tracedecay_fact_store_probe",
            json!({"entity": entities.first().cloned().unwrap_or_else(|| "missing".into()), "limit": 10}),
        ),
        (
            "tracedecay_fact_store_reason",
            json!({"entities": entities, "limit": 10}),
        ),
        ("tracedecay_fact_store_contradict", json!({"limit": 10})),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|_i| rq(tool, "read", args.clone())),
        });
    }
    let mut curate = eq(
        "tracedecay_fact_store_curate",
        "curator_admission",
        json!({"fact_review_limit": 10, "min_confidence_millionths": 0}),
        no_primes,
    );
    if let QueryKind::Effect { repeatable, .. } = &mut curate.kind {
        *repeatable = false;
    }
    out.push(ToolGroup {
        tool: "tracedecay_fact_store_curate",
        queries: vec![curate],
    });
    if let Some(cycle) = &ctx.seeds.feedback {
        if let Some(read) = &cycle.read_handles {
            for (tool, handle) in [
                ("tracedecay_feedback_impact", &read.impact_handle),
                ("tracedecay_feedback_list", &read.list_handle),
                ("tracedecay_feedback_diagnostics", &read.diagnostics_handle),
                ("tracedecay_affected_tests", &read.affected_tests_handle),
            ] {
                out.push(ToolGroup {
                    tool,
                    queries: five(|_| {
                        rq(tool, "fixture_feedback", json!({"request_handle": handle}))
                    }),
                });
            }
        }
        if let Some(finding) =
            cycle.cycle.cycle.findings.iter().find(|finding| {
                finding.safe_bounded_preview.as_deref() == Some(FEEDBACK_DIAGNOSTIC)
            })
        {
            if let Some(handle) = cycle
                .finding_handles
                .iter()
                .find(|handle| handle.finding_id == finding.finding_id)
            {
                for (tool, request_handle) in [
                    ("tracedecay_feedback_get", Some(&handle.get_handle)),
                    (
                        "tracedecay_feedback_expand",
                        handle.expansion_handle.as_ref(),
                    ),
                ] {
                    if let Some(request_handle) = request_handle {
                        out.push(ToolGroup {
                            tool,
                            queries: five(|_| {
                                rq(
                                    tool,
                                    "fixture_feedback",
                                    json!({"request_handle": request_handle}),
                                )
                            }),
                        });
                    }
                }
            }
        }
    }
    if let Some(path) = &ctx.seeds.compiler_diagnostic_path {
        if let Ok(document_uri) = url::Url::from_file_path(ctx.project_root.join(path)) {
            out.push(ToolGroup {
                tool: "tracedecay_feedback_advisory_cycle",
                queries: five(|i| {
                    Query::prepared_read(
                        "retained_diagnostic_cycle",
                        "tracedecay_feedback_advisory_cycle",
                        json!({"document_uri": document_uri.as_str()}),
                        i,
                        feedback_cycle_prime,
                    )
                }),
            });
        }
    }
    out.push(ToolGroup {
        tool: "tracedecay_test_results",
        queries: five(|_i| {
            eq(
                "tracedecay_test_results",
                "test_results",
                json!({}),
                affected_results_prime,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_feedback_proximity",
        queries: five(|i| {
            Query::prepared_read(
                "proximity",
                "tracedecay_feedback_proximity",
                json!({"observed_at": "{{now}}"}),
                i,
                feedback_code_prime,
            )
        }),
    });
}

fn feedback_code_prime(ctx: &QueryContext, _selection: u64) -> Vec<PrimeStep> {
    let query = if crate::repos::small_fixture_enabled() {
        "src/lib.rs::fixture_catalog_total"
    } else if let Some(query) = ctx.function_qnames.first() {
        query.as_str()
    } else {
        return Vec::new();
    };
    vec![prime_symbol(query.to_owned(), &[])]
}

fn feedback_cycle_prime(ctx: &QueryContext, selection: u64) -> Vec<PrimeStep> {
    let mut steps = feedback_code_prime(ctx, selection);
    if let Some(diagnostic) = &ctx.seeds.compiler_diagnostic {
        steps.push(PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_diagnose",
            args: json!({"cargo_output": diagnostic, "include_callers": false, "format":"json"}),
            capture: &[],
        });
    }
    steps
}

fn affected_results_prime(ctx: &QueryContext, iter: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_run_affected_tests",
        args: json!({
            "changed_paths": [ctx
                .seeds
                .test_results_path
                .clone()
                .unwrap_or_else(|| file_at(ctx, iter as usize))],
            "max_tests": 5,
            "timeout_secs": 30,
            "format": "json",
        }),
        capture: &[],
    }]
}

fn add_step(iter: u64, tag: &str) -> PrimeStep {
    add_step_named(iter, tag, "fact_id")
}

fn add_step_named(iter: u64, tag: &str, token: &'static str) -> PrimeStep {
    // Reintroduce the shared identity after each iteration retires it.
    let a = format!("bench-{tag}-a");
    let b = format!("bench-{tag}-b-{iter}");
    let mut args = fact_add_args(
        &format!("bench seeded fact {tag} {}", fact_marker(iter as usize)),
        &[a, b],
        0.9,
    );
    args["format"] = json!("json");
    let capture: &[(&str, &str)] = match token {
        "old_fact_id" => &[("digpath:result:fact.fact.fact_id", "old_fact_id")],
        "new_fact_id" => &[("digpath:result:fact.fact.fact_id", "new_fact_id")],
        _ => &[("digpath:result:fact.fact.fact_id", "fact_id")],
    };
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_fact_store_add",
        args,
        capture,
    }
}
