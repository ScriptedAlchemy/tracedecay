//! Work/workflow family: ~45 tools measured against the real disposable
//! lifecycle `seed_work` minted (create → proposal → accept → admit →
//! placement → start → cancel), plus per-iteration fresh-task effect chains.

use serde_json::{Value, json};
use std::collections::HashMap;
use tracedecay_contracts::{
    WorkHandoffFrontierV1, WorkRetryReceiptV1, WorkSynthesisSourceSetV1,
    workflow_artifact_payload_digest,
};

use crate::queries::{PrimeStep, Query, QueryContext, ToolGroup, five};

use super::{WorkSeeds, eqc, eqn, no_primes, now_micros, rqn};

pub(crate) fn verify_work_fixture(
    tool: &str,
    args: &Value,
    response: &Value,
) -> Option<Result<(), String>> {
    if !matches!(
        tool,
        "tracedecay_work_retry_attempt"
            | "tracedecay_work_synthesize"
            | "tracedecay_work_adjudicate_duplicate"
    ) {
        return None;
    }
    Some((|| {
        let payload = response
            .pointer("/value/outcome/value/payload")
            .or_else(|| response.pointer("/outcome/value/payload"))
            .ok_or_else(|| "Work result omitted its canonical payload".to_owned())?;
        let mut command = args.clone();
        if let Some(command) = command.as_object_mut() {
            command.remove("format");
        }
        match tool {
            "tracedecay_work_retry_attempt" => {
                let receipt: WorkRetryReceiptV1 =
                    serde_json::from_value(payload["receipt"].clone())
                        .map_err(|error| format!("invalid retry receipt: {error}"))?;
                let expected_identity = json!({
                    "task_id": args["original_attempt"]["task_id"],
                    "run_id": args["original_attempt"]["run_id"],
                    "attempt_id": args["new_attempt_id"],
                });
                if !receipt.validate_for_observation()
                    || payload["outcome"] != "created"
                    || payload["receipt"]["command"] != command
                    || payload["receipt"]["new_attempt"] != expected_identity
                    || payload["attempt"]["identity"] != expected_identity
                    || payload["attempt"]["state"] != "recovery_required"
                    || payload["attempt"]["recovery"]["reason"] != "failure_observed"
                    || payload["attempt"]["recovery"]["source_attempt_id"]
                        != args["original_attempt"]["attempt_id"]
                    || payload["attempt"]["recovery"]["observed_at"]
                        != payload["receipt"]["failure"]["observed_at"]
                    || payload["attempt"]["execution"]["instructions"]
                        != super::WORK_FAILURE_INSTRUCTIONS
                    || args["failure"]["evidence_ref"]
                        != format!(
                            "runtime-terminal:{}",
                            receipt.failure.evidence_digest.as_str()
                        )
                {
                    return Err("retry did not preserve the failed fixture's evidence and create the requested successor".to_owned());
                }
            }
            "tracedecay_work_adjudicate_duplicate" => {
                let prior_revision = match args.get("expected_revision") {
                    Some(Value::Null) => 0,
                    Some(value) => value
                        .as_u64()
                        .ok_or_else(|| "invalid prepared duplicate revision".to_owned())?,
                    None => return Err("prepared duplicate revision is absent".to_owned()),
                };
                let expected_revision = prior_revision
                    .checked_add(1)
                    .ok_or_else(|| "duplicate revision overflow".to_owned())?;
                if payload["outcome"] != "appended"
                    || payload["receipt"]["command"] != command
                    || payload["receipt"]["revision"] != expected_revision
                    || args["first_attempt"] == args["second_attempt"]
                    || args["verdict"] != "not_duplicate"
                {
                    return Err("duplicate adjudication did not append the exact prepared fixture command at its next revision".to_owned());
                }
            }
            "tracedecay_work_synthesize" => {
                let source_bytes = b"TraceDecay lifecycle source evidence.";
                let digest = workflow_artifact_payload_digest(source_bytes)
                    .map_err(|error| format!("fixture artifact digest failed: {error}"))?;
                let source_set: WorkSynthesisSourceSetV1 =
                    serde_json::from_value(payload["source_set"].clone())
                        .map_err(|error| format!("invalid synthesis source set: {error}"))?;
                let expected_identity = json!({
                    "task_id": args["start"]["task_id"],
                    "run_id": args["start"]["run_id"],
                    "attempt_id": args["start"]["attempt_id"],
                });
                let expected_sources = json!([{
                    "source": args["sources"][0],
                    "outcome": {"outcome": "succeeded", "artifacts": [digest]},
                }]);
                let original_instructions = args["start"]["instructions"]
                    .as_str()
                    .ok_or_else(|| "synthesis fixture omitted instructions".to_owned())?;
                let hydrated = payload["attempt"]["execution"]["instructions"]
                    .as_str()
                    .and_then(|text| text.strip_prefix(original_instructions))
                    .and_then(|text| text.strip_prefix("\n\n"))
                    .ok_or_else(|| {
                        "synthesis did not hydrate its source instructions".to_owned()
                    })?;
                let context: Value = serde_json::from_str(hydrated)
                    .map_err(|error| format!("invalid hydrated synthesis context: {error}"))?;
                let source = &context["work_synthesis_sources"][0];
                if payload["synthesis"] != "admitted"
                    || payload["attempt"]["identity"] != expected_identity
                    || payload["attempt"]["execution"]["execution_snapshot"]
                        != args["start"]["execution_snapshot"]
                    || payload["source_set"]["sources"] != expected_sources
                    || !source_set.verified()
                    || payload["draft"]
                        != json!({
                            "output_name": args["output_name"],
                            "synthesis_attempt": expected_identity,
                            "cited_source_digests": [digest],
                        })
                    || payload["groups"]
                        != json!([{
                            "artifacts": [digest], "sources": args["sources"],
                        }])
                    || payload["uncited"] != json!([])
                    || context["work_synthesis_sources"]
                        .as_array()
                        .map(|sources| sources.len())
                        != Some(1)
                    || source["identity"] != args["sources"][0]
                    || source["state"] != "succeeded"
                    || source["terminal"]["outcome"] != "succeeded"
                    || source["artifacts"]
                        != json!([{
                            "artifact_id": "artifact.provider.stdout",
                            "digest": digest,
                            "byte_length": source_bytes.len(),
                            "content": "TraceDecay lifecycle source evidence.",
                        }])
                {
                    return Err("synthesis did not admit the exact fixture artifact bytes, source identity, digest, and citation".to_owned());
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    })())
}

fn verify_work_effect_fixture(
    ctx: &QueryContext,
    tool: &str,
    args: &Value,
    response: &Value,
) -> Option<Result<(), String>> {
    let handled = matches!(
        tool,
        "tracedecay_work_prepare_graph_mutation"
            | "tracedecay_work_mutate_graph"
            | "tracedecay_work_create"
            | "tracedecay_work_generate_proposal"
            | "tracedecay_work_review_proposal"
            | "tracedecay_work_accept_proposal"
            | "tracedecay_work_admit_execution"
            | "tracedecay_work_placement_preflight"
            | "tracedecay_work_admit_placement"
            | "tracedecay_work_release_placement"
            | "tracedecay_work_start_attempt"
            | "tracedecay_work_cancel_attempt"
            | "tracedecay_work_pause_run"
            | "tracedecay_work_resume_run"
            | "tracedecay_work_prepare_duplicate_adjudication"
            | "tracedecay_workflow_register_definition"
            | "tracedecay_workflow_activate_definition"
            | "tracedecay_workflow_reject_definition"
            | "tracedecay_workflow_retire_definition"
            | "tracedecay_workflow_start_run"
            | "tracedecay_workflow_pause_run"
            | "tracedecay_workflow_resume_run"
            | "tracedecay_workflow_cancel_run"
            | "tracedecay_workflow_handoff_issue"
            | "tracedecay_workflow_handoff_redeem"
    );
    if !handled {
        return None;
    }
    Some((|| {
        let seed = ctx
            .seeds
            .work
            .as_ref()
            .ok_or("Work lifecycle seed is absent")?;
        let result = response
            .pointer("/value/outcome/value")
            .or_else(|| response.pointer("/outcome/value"))
            .ok_or("Work effect omitted its canonical result")?;
        let payload = result.get("payload").ok_or("Work effect omitted payload")?;
        if let Some(receipt) = result.get("receipt") {
            if receipt["outcome"] != "completed" || result["reconciliation"] != "reconciled" {
                return Err("Work effect did not complete and reconcile its receipt".to_owned());
            }
        }
        let identity = json!({"task_id": args["task_id"], "run_id": args["run_id"], "attempt_id": args["attempt_id"]});
        let run_identity = json!({"task_id": args["task_id"], "run_id": args["run_id"]});
        match tool {
            "tracedecay_work_prepare_graph_mutation" => {
                let request = &payload["request"];
                if args["change"]["change"] != "create_task"
                    || payload["mutation"] != "create_task"
                    || request["selection"] != args["selection"]
                {
                    return Err("create preparation selected another mutation or scope".to_owned());
                }
                for key in ["initiative", "plan", "milestone", "item"] {
                    if request[key] != args["change"][key] {
                        return Err(format!("prepared create changed {key}"));
                    }
                }
                verify_authored_task(&request["item"])?;
                if request["mutation"]["expected_authority"]["authority"] != "verified"
                    || request["mutation"]["command_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .is_none()
                {
                    return Err(
                        "prepared create omitted verified authority or command identity".to_owned(),
                    );
                }
            }
            "tracedecay_work_mutate_graph"
            | "tracedecay_work_create"
            | "tracedecay_work_review_proposal"
            | "tracedecay_work_accept_proposal"
            | "tracedecay_work_admit_execution" => {
                let request = if tool == "tracedecay_work_mutate_graph" {
                    &args["request"]
                } else {
                    args
                };
                let committed = if tool == "tracedecay_work_admit_execution" {
                    &payload["mutation"]
                } else {
                    payload
                };
                let event = &committed["event"];
                let prior =
                    request["mutation"]["expected_authority"]["verified_version"]["graph_version"]
                        .as_i64()
                        .ok_or("mutation request omitted verified graph version")?;
                let next = prior
                    .checked_add(1)
                    .ok_or("fixture graph version overflow")?;
                if committed["replayed"] != false
                    || event["command_id"] != request["mutation"]["command_id"]
                    || event["expected_graph_version"].as_i64() != Some(prior)
                    || event["result_graph_version"].as_i64() != Some(next)
                    || committed["verified_graph_version"]["graph_version"].as_i64() != Some(next)
                    || event["occurred_at"] != request["mutation"]["occurred_at"]
                {
                    return Err("mutation receipt did not commit the exact fresh command at its next version".to_owned());
                }
                let change = &event["payload"]["change"];
                if matches!(
                    tool,
                    "tracedecay_work_mutate_graph" | "tracedecay_work_create"
                ) {
                    if change["kind"] != "task_created" {
                        return Err("create omitted task-created event".to_owned());
                    }
                    for key in ["initiative", "plan", "milestone", "item"] {
                        if change[key] != request[key] {
                            return Err(format!("create receipt changed authored {key}"));
                        }
                    }
                    verify_authored_task(&change["item"])?;
                } else if tool == "tracedecay_work_admit_execution" {
                    if change["kind"] != "execution_admitted"
                        || change["task_id"] != args["task_id"]
                        || change["based_on_version"] != args["based_on_version"]
                        || payload["execution_snapshot"]["route"]
                            != seed.execution_snapshot["route"]
                        || payload["execution_snapshot"]["executable"]
                            != seed.execution_snapshot["executable"]
                        || payload["execution_snapshot"]["backend"] != "codex_cli"
                        || payload["execution_snapshot"]["egress"] != "deny"
                    {
                        return Err("execution admission lost the selected task or configured isolated provider".to_owned());
                    }
                } else {
                    let accepted = tool == "tracedecay_work_accept_proposal";
                    if change["proposal"] != args["proposal"]
                        || change["kind"]
                            != if accepted {
                                "proposal_accepted"
                            } else {
                                "proposal_decided"
                            }
                        || (!accepted && change["disposition"] != "rejected")
                        || args["disposition"] != if accepted { "accepted" } else { "rejected" }
                    {
                        return Err(
                            "proposal decision committed another proposal or disposition"
                                .to_owned(),
                        );
                    }
                }
            }
            "tracedecay_work_generate_proposal" => {
                let proposal = &payload["proposal"];
                if proposal["task_id"] != args["task_id"]
                    || proposal["proposal_id"] != args["proposal_id"]
                    || proposal["route"]["decision"] != "selected"
                    || proposal["route"]["recommended"] != seed.execution_snapshot["route"]
                    || proposal["sizing"]["likely"] != 1
                    || proposal["sizing"]["coverage"] != "declared_work_item_effort"
                    || payload["decision"]["task_id"] != args["task_id"]
                    || payload["decision"]["disposition"] != "allow"
                {
                    return Err(
                        "proposal lost the fresh task's declared effort or sole configured route"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_work_placement_preflight"
            | "tracedecay_work_admit_placement"
            | "tracedecay_work_release_placement" => {
                let target = json!({"kind":"no_managed_placement", "root":null, "network_free":true,"in_place_acknowledged":false});
                if payload["identity"] != run_identity
                    || payload["target"] != target
                    || payload["blockers"] != json!([])
                {
                    return Err(
                        "placement lost the exact run, authored target or blocker result"
                            .to_owned(),
                    );
                }
                if tool == "tracedecay_work_placement_preflight" {
                    if payload["observation"]["readable"] != true
                        || payload["observation"]["network_required"] != false
                    {
                        return Err(
                            "placement preflight did not observe the readable local fixture"
                                .to_owned(),
                        );
                    }
                } else {
                    let released = tool == "tracedecay_work_release_placement";
                    let revision = if released {
                        args["expected_authority_version"]
                            .as_u64()
                            .and_then(|n| n.checked_add(1))
                    } else {
                        Some(1)
                    };
                    if payload["state"] != if released { "released" } else { "admitted" }
                        || payload["authority_version"].as_u64() != revision
                        || payload["transitioned_at"] != args["occurred_at"]
                    {
                        return Err(
                            "placement transition lost its expected authority or requested state"
                                .to_owned(),
                        );
                    }
                }
            }
            "tracedecay_work_start_attempt" | "tracedecay_work_cancel_attempt" => {
                if payload["identity"] != identity
                    || payload["execution"]["attempt_identity"] != identity
                {
                    return Err("attempt effect selected another task/run/attempt".to_owned());
                }
                if tool == "tracedecay_work_start_attempt" {
                    for key in [
                        "instructions",
                        "commit",
                        "operation",
                        "effect_state",
                        "execution_snapshot",
                        "worktree_root",
                    ] {
                        if payload["execution"][key] != args[key] {
                            return Err(format!("attempt admission changed {key}"));
                        }
                    }
                    if args["instructions"] != "Bench lifecycle attempt."
                        || payload["requested_route"] != args["execution_snapshot"]["route"]
                        || !matches!(payload["state"].as_str(), Some("leased" | "running"))
                    {
                        return Err("attempt admission did not retain the authored command and runnable lease".to_owned());
                    }
                } else {
                    if payload["state"] != "cancellation_requested"
                        || payload["cancellation"]["state"] != "requested"
                        || payload["cancellation"]["value"]
                            != json!({"request_id":args["request_id"],"requested_at":args["occurred_at"]})
                    {
                        return Err(
                            "attempt cancellation did not record the exact request".to_owned()
                        );
                    }
                }
            }
            "tracedecay_work_pause_run" | "tracedecay_work_resume_run" => {
                let paused = tool == "tracedecay_work_pause_run";
                if payload["task_id"] != args["task_id"]
                    || payload["run_id"] != args["run_id"]
                    || payload["state"] != if paused { "paused" } else { "running" }
                    || payload["reason"] != "operator_request"
                    || payload["transitioned_at"] != args["occurred_at"]
                    || payload["deadline"]["checkpoint_at"] != args["occurred_at"]
                {
                    return Err(
                        "run control lost its exact run, requested state or deadline checkpoint"
                            .to_owned(),
                    );
                }
                if paused {
                    let run = args["run_id"]
                        .as_str()
                        .ok_or("pause omitted run identity")?;
                    let attempt = run
                        .strip_prefix("run.bench.pz.")
                        .ok_or("pause did not select fixture run")?;
                    if payload["fenced_attempts"] != json!([format!("attempt.bench.pz.{attempt}")])
                    {
                        return Err("pause did not fence the fixture's live attempt".to_owned());
                    }
                } else if payload["fenced_attempts"] != json!([])
                    || payload["authority"].as_u64()
                        != args["expected_authority_version"]
                            .as_u64()
                            .and_then(|v| v.checked_add(1))
                {
                    return Err("resume did not advance its fenced authority".to_owned());
                }
            }
            "tracedecay_work_prepare_duplicate_adjudication" => {
                let prepared: tracedecay_domain::WorkDuplicateAdjudicationCommandV1 =
                    serde_json::from_value(payload.clone())
                        .map_err(|error| format!("invalid prepared adjudication: {error}"))?;
                prepared
                    .validate()
                    .map_err(|error| format!("invalid adjudication evidence: {error}"))?;
                for key in [
                    "first_attempt",
                    "second_attempt",
                    "verdict",
                    "quantities",
                    "reason",
                ] {
                    if payload[key] != args[key] {
                        return Err(format!("duplicate preparation changed {key}"));
                    }
                }
                if payload["first_attempt"] != seed.attempt_identity
                    || Some(&payload["second_attempt"]) != seed.dup_attempt_identity.as_ref()
                    || payload["first_attempt"] == payload["second_attempt"]
                    || payload["verdict"] != "not_duplicate"
                {
                    return Err("duplicate preparation did not preserve the two real terminal fixture attempts".to_owned());
                }
            }
            "tracedecay_workflow_register_definition" => {
                if *payload != args["definition"] || payload["steps"] != seed.definition["steps"] {
                    return Err(
                        "registration did not persist the exact authored inspect workflow"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_workflow_activate_definition"
            | "tracedecay_workflow_reject_definition"
            | "tracedecay_workflow_retire_definition" => {
                let (state, increment) = match tool {
                    "tracedecay_workflow_activate_definition" => ("active", 2),
                    "tracedecay_workflow_reject_definition" => ("rejected", 1),
                    _ => ("retired", 1),
                };
                if payload["definition_id"] != args["definition_id"]
                    || payload["definition_version"] != args["definition_version"]
                    || payload["state"] != state
                    || payload["revision"].as_u64()
                        != args["expected_revision"]
                            .as_u64()
                            .and_then(|v| v.checked_add(increment))
                {
                    return Err(
                        "definition disposition lost the selected version or expected transition"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_workflow_start_run"
            | "tracedecay_workflow_pause_run"
            | "tracedecay_workflow_resume_run"
            | "tracedecay_workflow_cancel_run" => {
                let (state, event_kind) = match tool {
                    "tracedecay_workflow_start_run" => ("running", "admitted"),
                    "tracedecay_workflow_pause_run" => ("paused", "paused"),
                    "tracedecay_workflow_resume_run" => ("running", "resumed"),
                    _ => ("cancelled", "cancellation_requested"),
                };
                if payload["run_id"] != args["run_id"]
                    || payload["status"] != state
                    || payload["definition"]["steps"] != seed.definition["steps"]
                    || payload["steps"]["step.bench.inspect"]["status"]
                        != if state == "cancelled" {
                            "cancelled"
                        } else {
                            "ready"
                        }
                {
                    return Err(
                        "workflow transition lost the exact run or literal inspect-step state"
                            .to_owned(),
                    );
                }
                let history = payload["history"]
                    .as_array()
                    .ok_or("workflow transition omitted history")?;
                let events = history
                    .iter()
                    .filter(|row| row["command_id"] == args["command_id"])
                    .collect::<Vec<_>>();
                if events.len() != 1
                    || events[0]["run_id"] != args["run_id"]
                    || events[0]["event"]["type"] != event_kind
                {
                    return Err("workflow history lost the exact transition command".to_owned());
                }
                if tool == "tracedecay_workflow_start_run"
                    && (payload["definition"]["definition_id"] != args["definition_id"]
                        || payload["definition"]["definition_version"]
                            != args["definition_version"]
                        || payload["sequence"] != 1)
                {
                    return Err(
                        "workflow admission lost the authored definition version".to_owned()
                    );
                }
            }
            "tracedecay_workflow_handoff_issue" | "tracedecay_workflow_handoff_redeem" => {
                let frontier: WorkHandoffFrontierV1 =
                    serde_json::from_value(payload["frontier"].clone())
                        .map_err(|error| format!("invalid handoff frontier: {error}"))?;
                let digest = frontier.digest().map_err(|error| error.to_string())?;
                let scope = if tool == "tracedecay_workflow_handoff_issue" {
                    &args["scope"]
                } else {
                    &args["expected_scope"]
                };
                if payload["scope"] != *scope
                    || payload["frontier"]["task_id"] != scope["task_id"]
                    || payload["frontier"]["legal_actions"]
                        != json!(["Inspect the bench frontier."])
                    || payload["frontier"]["attempts"] != json!([])
                    || payload["frontier"]["blockers"] != json!([])
                    || payload["frontier"]["lineage"]["issued_by"] != seed.actor_id
                    || payload["frontier_digest"] != json!(digest)
                {
                    return Err(
                        "handoff lost the exact scope or authored inspect frontier".to_owned()
                    );
                }
                if tool == "tracedecay_workflow_handoff_issue"
                    && payload["frontier"] != args["frontier"]
                {
                    return Err("handoff issue changed the authored frontier".to_owned());
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    })())
}

fn verify_authored_task(item: &Value) -> Result<(), String> {
    if item["input"]["title"] != "Bench lifecycle task"
        || item["input"]["effort"] != 1
        || item["input"]["dependencies"] != json!([])
        || item["accepted_attempts"] != json!([])
        || item["accepted_proposal"] != Value::Null
    {
        return Err("task creation did not preserve the literal fixture requirements".to_owned());
    }
    Ok(())
}

pub(crate) fn verify_work_read_fixture(
    ctx: &QueryContext,
    tool: &str,
    label: &str,
    args: &Value,
    response: &Value,
    prepared_tokens: &HashMap<String, Value>,
) -> Option<Result<(), String>> {
    if tool == "tracedecay_work_experience" {
        return Some((|| {
            let source = prepared_tokens
                .get("experience_source")
                .ok_or("experience fixture omitted source identity")?;
            let payload = response
                .pointer("/value/outcome/value/payload")
                .or_else(|| response.pointer("/outcome/value/payload"))
                .ok_or("experience read omitted payload")?;
            if source["task_id"] == args["task_id"]
                || payload["task_id"] != args["task_id"]
                || payload["verified_version"] != args["verified_version"]
                || payload["expertise"]["availability"] != "available"
                || payload["expertise"]["durability"] != "ephemeral_only"
                || payload["expertise"]["categories"] != json!(["testing"])
            {
                return Err("experience read lost the exact selected task/version or explicit ephemeral consent".to_owned());
            }
            for key in ["experience_prior_user", "experience_prior_project"] {
                if prepared_tokens
                    .get(key)
                    .and_then(|v| v.pointer("/value/enabled"))
                    != Some(&Value::Bool(false))
                {
                    return Err(
                        "experience fixture did not begin with disabled isolated consent"
                            .to_owned(),
                    );
                }
            }
            let rows = payload["candidates"]
                .as_array()
                .ok_or("experience omitted candidates")?;
            let candidates = rows
                .iter()
                .filter(|candidate| candidate["item"]["input"]["task_id"] == source["task_id"])
                .collect::<Vec<_>>();
            if candidates.len() != 1 {
                return Err(
                    "experience read lost or duplicated the real successful candidate".to_owned(),
                );
            }
            let candidate = candidates[0];
            if candidate["item"]["input"]["title"] != "Bench lifecycle task"
                || candidate["item"]["accepted_at"].as_i64().is_none()
                || !candidate["applicability"]
                    .as_array()
                    .is_some_and(|values| values.contains(&json!("same_accepted_route")))
            {
                return Err(
                    "experience candidate lost task acceptance or route applicability".to_owned(),
                );
            }
            let receipt = unique_fixture_row(&candidate["attempt_receipts"], "identity", source)?;
            let digest = workflow_artifact_payload_digest(b"TraceDecay lifecycle source evidence.")
                .map_err(|error| error.to_string())?;
            if receipt["evidence"]["identity"] != *source
                || receipt["evidence"]["outcome"] != json!({"outcome":"exited","code":0})
                || receipt["artifacts"]
                    != json!([{"artifact_id":"artifact.provider.stdout","digest":digest,"byte_length":37}])
                || receipt["evidence"]["stdout"]
                    != json!({"byte_length":37,"digest":digest,"truncated":false})
            {
                return Err("experience candidate lost the real successful stdout artifact, exact bytes digest or exit status".to_owned());
            }
            Ok(())
        })());
    }
    if tool == "tracedecay_work_resume_attempts" {
        return Some((|| {
            let identity = prepared_tokens
                .get("resume_identity")
                .ok_or("recovery fixture omitted its successor identity")?;
            let original = prepared_tokens
                .get("retry_original")
                .ok_or("recovery fixture omitted its failed source identity")?;
            let payload = response
                .pointer("/value/outcome/value/payload")
                .or_else(|| response.pointer("/outcome/value/payload"))
                .ok_or("recovery sweep omitted its canonical payload")?;
            let attempt = unique_fixture_row(&payload["recovery_required"], "identity", identity)?;
            if identity["task_id"] != original["task_id"]
                || identity["run_id"] != original["run_id"]
                || identity["attempt_id"] == original["attempt_id"]
                || attempt["state"] != "recovery_required"
                || attempt["recovery"]["reason"] != "failure_observed"
                || attempt["recovery"]["source_attempt_id"] != original["attempt_id"]
                || attempt["execution"]["instructions"] != super::WORK_FAILURE_INSTRUCTIONS
            {
                return Err(
                    "recovery sweep lost the exact failed fixture's fenced successor".to_owned(),
                );
            }
            Ok(())
        })());
    }
    if let Some(result) = verify_work_effect_fixture(ctx, tool, args, response) {
        return Some(result);
    }
    if !matches!(
        tool,
        "tracedecay_work_attempt_status"
            | "tracedecay_work_topology"
            | "tracedecay_work_hydrate_artifacts"
            | "tracedecay_work_list_attempts"
            | "tracedecay_work_execution_history"
            | "tracedecay_work_placement_status"
            | "tracedecay_work_run_control"
            | "tracedecay_work_retrieve_evidence"
            | "tracedecay_work_compare_proposal"
            | "tracedecay_workflow_get_definition"
            | "tracedecay_workflow_list_definitions"
            | "tracedecay_workflow_definition_history"
            | "tracedecay_workflow_diff_definition"
            | "tracedecay_workflow_validate_definition"
            | "tracedecay_workflow_get_run"
    ) && tool != "tracedecay_work_views"
    {
        return None;
    }
    Some((|| {
        let seed = ctx
            .seeds
            .work
            .as_ref()
            .ok_or("Work lifecycle seed is absent")?;
        let payload = response
            .pointer("/value/outcome/value/payload")
            .or_else(|| response.pointer("/outcome/value/payload"))
            .ok_or("Work read omitted its canonical payload")?;
        let identities = std::iter::once(&seed.attempt_identity)
            .chain(seed.dup_attempt_identity.iter())
            .collect::<Vec<_>>();
        if seed.attempt_identity
            != json!({
                "task_id": seed.task_id, "run_id": seed.run_id, "attempt_id": seed.attempt_id,
            })
        {
            return Err("Work seed omitted the requested lifecycle identity".to_owned());
        }
        match tool {
            "tracedecay_work_topology" => {
                let rows = payload["execution_placement"]["lanes"]
                    .as_array()
                    .ok_or("topology omitted execution lanes")?;
                let selected = rows
                    .iter()
                    .filter(|row| row["task_id"] == seed.task_id && row["run_id"] == seed.run_id)
                    .collect::<Vec<_>>();
                if selected.len() != 1
                    || selected[0]["attempt_count"] != 1
                    || selected[0]["placement"]["state"] != "placed"
                    || selected[0]["placement"]["placement"]["state"] != "admitted"
                    || selected[0]["placement"]["placement"]["target"]["kind"]
                        != "no_managed_placement"
                {
                    return Err(
                        "topology lost the real seeded run's admitted placement and attempt"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_work_hydrate_artifacts" => {
                let empty_digest =
                    workflow_artifact_payload_digest(b"").map_err(|error| error.to_string())?;
                for identity in &identities {
                    let attempt = unique_fixture_row(&payload["attempts"], "identity", identity)?;
                    let record = &attempt["evidence"]["record"];
                    if payload["state"] != "hydrated"
                        || attempt["evidence"]["state"] != "sealed"
                        || attempt["artifacts"] != json!([])
                        || record["identity"] != **identity
                        || record["outcome"]["outcome"] != "cancelled"
                        || record["actual_route"] != seed.execution_snapshot["route"]
                    {
                        return Err("artifact hydration lost the cancelled fixture's sealed provider evidence".to_owned());
                    }
                    for stream in ["stdout", "stderr"] {
                        if record[stream]
                            != json!({"byte_length":0,"digest":empty_digest,"truncated":false})
                        {
                            return Err(format!(
                                "artifact hydration changed the known empty {stream} bytes"
                            ));
                        }
                    }
                }
            }
            "tracedecay_work_attempt_status" => {
                if args["task_id"] != seed.task_id
                    || args["run_id"] != seed.run_id
                    || args["attempt_id"] != seed.attempt_id
                {
                    return Err("attempt status did not select the seeded attempt".to_owned());
                }
                verify_cancelled_attempt(seed, &seed.attempt_identity, payload)?;
            }
            "tracedecay_work_list_attempts" => {
                for identity in &identities {
                    let attempt = unique_fixture_row(&payload["attempts"], "identity", identity)?;
                    verify_cancelled_attempt(seed, identity, attempt)?;
                }
            }
            "tracedecay_work_execution_history" => {
                if payload["state"] != "listed" {
                    return Err("execution history omitted the cancelled lifecycle".to_owned());
                }
                for identity in &identities {
                    let span = unique_fixture_row(&payload["spans"], "identity", identity)?;
                    let ordered =
                        unique_fixture_row(&payload["observed_order"], "identity", identity)?;
                    let start = span["started_at"]
                        .as_i64()
                        .ok_or("history omitted dispatch time")?;
                    let end = span["ended_at"]
                        .as_i64()
                        .ok_or("history omitted terminal time")?;
                    if span["state"] != "cancelled"
                        || span["effect_state"] != "observational"
                        || ordered["state"] != "cancelled"
                        || end < start
                        || span["wall_micros"].as_i64() != end.checked_sub(start)
                        || ordered["observed_at"].as_i64() != Some(end)
                        || span["terminal_evidence_digest"].as_str().is_none()
                        || ordered["evidence_digest"] != span["terminal_evidence_digest"]
                    {
                        return Err("execution history lost the exact attempt's cancellation or measured span".to_owned());
                    }
                }
            }
            "tracedecay_work_placement_status" => {
                let identity = json!({"task_id": seed.task_id, "run_id": seed.run_id});
                if args["task_id"] != seed.task_id
                    || args["run_id"] != seed.run_id
                    || payload["state"] != "placed"
                    || payload["placement"]["identity"] != identity
                    || payload["placement"]["state"] != "admitted"
                    || payload["placement"]["blockers"] != json!([])
                    || payload["placement"]["target"]
                        != json!({
                            "kind": "no_managed_placement", "root": null,
                            "network_free": true, "in_place_acknowledged": false,
                        })
                {
                    return Err("placement status lost the seeded run's admitted target".to_owned());
                }
            }
            "tracedecay_work_run_control" => {
                if args["task_id"] != seed.task_id
                    || args["run_id"] != seed.run_id
                    || payload["state"] != "uncontrolled"
                    || payload["live_attempts"] != json!([])
                    || payload["total_attempts"] != 1
                    || payload["deadline"] != seed.execution_snapshot["deadline"]
                {
                    return Err("run control did not retain the cancelled run's deadline and empty live frontier".to_owned());
                }
            }
            "tracedecay_work_views" => {
                if args["selection"] != seed.selection
                    || payload["authorized_scope"]["selection"] != seed.selection
                    || payload["mode"] != args["mode"]["mode"]
                {
                    return Err(
                        "Work view did not preserve the selected fixture scope and mode".to_owned(),
                    );
                }
                let snapshot = if label == "windowed" {
                    let rows = payload["timeline"]["entries"]
                        .as_array()
                        .ok_or("evolution view omitted its timeline")?;
                    let selected = rows
                        .iter()
                        .filter(|row| row["verified_version"] == seed.current_version)
                        .collect::<Vec<_>>();
                    if selected.len() != 1 {
                        return Err(
                            "evolution view lost the exact admitted lifecycle version".to_owned()
                        );
                    }
                    selected[0]
                } else {
                    &payload["snapshot"]
                };
                // Select by the task's product identity, independent of graph order.
                let items = snapshot["graph"]["items"]
                    .as_array()
                    .ok_or("Work view omitted task items")?;
                let selected = items
                    .iter()
                    .filter(|item| item["input"]["task_id"] == seed.task_id)
                    .collect::<Vec<_>>();
                if selected.len() != 1 {
                    return Err("Work view did not return exactly one seeded task".to_owned());
                }
                verify_lifecycle_item(seed, selected[0], &identities)?;
                for identity in &identities {
                    let attempt =
                        unique_fixture_row(&snapshot["runtime"]["attempts"], "identity", identity)?;
                    if attempt["state"] != "cancelled" {
                        return Err(
                            "Work runtime projection lost the exact seeded attempt's cancellation"
                                .to_owned(),
                        );
                    }
                }
            }
            "tracedecay_work_retrieve_evidence" => {
                if args["selection"] != seed.selection || args["task_id"] != seed.task_id {
                    return Err("evidence read did not select the seeded task".to_owned());
                }
                verify_lifecycle_item(seed, &payload["item"], &identities)?;
                let sources = payload["sources"]
                    .as_array()
                    .ok_or("evidence read omitted sources")?;
                for identity in &identities {
                    let receipts = sources
                        .iter()
                        .filter(|source| {
                            source["kind"] == "attempt_receipt"
                                && source["receipt"]["identity"] == **identity
                        })
                        .collect::<Vec<_>>();
                    if receipts.len() != 1 {
                        return Err(
                            "evidence read lost or duplicated a seeded attempt receipt".to_owned()
                        );
                    }
                    let evidence = &receipts[0]["receipt"]["evidence"];
                    if evidence["identity"] != **identity
                        || evidence["outcome"]["outcome"] != "cancelled"
                        || evidence["actual_route"] != seed.execution_snapshot["route"]
                        || evidence["stdout"]["byte_length"] != 0
                        || evidence["stderr"]["byte_length"] != 0
                    {
                        return Err("evidence hydration lost the cancelled provider's identity, route or empty streams".to_owned());
                    }
                }
            }
            "tracedecay_work_compare_proposal" => {
                if args["task_id"] != seed.task_id
                    || args["old_version"] != seed.initial_version
                    || args["new_version"] != seed.current_version
                    || payload["old"]["item"]["input"]["task_id"] != seed.task_id
                    || payload["old"]["item"]["accepted_attempts"] != json!([])
                    || payload["old"]["item"]["accepted_proposal"] != Value::Null
                    || payload["old"]["verified_version"] != seed.initial_version
                    || payload["new"]["verified_version"] != seed.current_version
                    || payload["item_changed"] != true
                    || payload["effect"] != "advisory_only"
                {
                    return Err(
                        "proposal comparison lost the authored-to-admitted lifecycle transition"
                            .to_owned(),
                    );
                }
                verify_lifecycle_item(seed, &payload["new"]["item"], &identities)?;
            }
            "tracedecay_workflow_get_definition" => {
                if args["definition_id"] != seed.definition_id
                    || args["definition_version"] != seed.definition["definition_version"]
                    || *payload != seed.definition
                {
                    return Err(
                        "definition read did not return the exact authored workflow".to_owned()
                    );
                }
            }
            "tracedecay_workflow_list_definitions" | "tracedecay_workflow_definition_history" => {
                if tool.ends_with("definition_history")
                    && args["definition_id"] != seed.definition_id
                {
                    return Err("definition history selected another workflow".to_owned());
                }
                let rows = payload
                    .as_array()
                    .ok_or("workflow definitions omitted rows")?;
                if rows.iter().filter(|row| **row == seed.definition).count() != 1 {
                    return Err(
                        "workflow listing lost or duplicated the exact authored definition"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_workflow_diff_definition" => {
                if args["definition_id"] != seed.definition_id
                    || args["from_version"] != seed.definition["definition_version"]
                    || args["to_version"] != seed.definition["definition_version"]
                    || *payload
                        != json!({
                            "definition_id": seed.definition_id,
                            "from_version": args["from_version"], "to_version": args["to_version"],
                            "catalog_changed": false, "configuration_changed": false,
                            "policy_changed": false, "changed_steps": [],
                        })
                {
                    return Err(
                        "workflow self-comparison invented a change or selected another version"
                            .to_owned(),
                    );
                }
            }
            "tracedecay_workflow_validate_definition" => {
                if args["definition"] != seed.definition || payload["definition"] != seed.definition
                {
                    return Err(
                        "workflow validation did not preserve the authored definition".to_owned(),
                    );
                }
            }
            "tracedecay_workflow_get_run" => {
                let run_id = seed
                    .wf_run_id
                    .as_ref()
                    .ok_or("seed workflow run is absent")?;
                if args["run_id"] != *run_id
                    || payload["run_id"] != *run_id
                    || payload["definition"] != seed.definition
                    || payload["status"] != "running"
                    || payload["steps"]["step.bench.inspect"]["status"] != "ready"
                    || seed.definition["steps"]
                        != json!([{
                            "step_id": "step.bench.inspect", "operation": "operation.work.start_attempt",
                            "predecessors": [], "inputs": [], "outputs": [], "fan_out": null,
                        }])
                {
                    return Err(
                        "workflow run read lost its admitted definition or ready inspect step"
                            .to_owned(),
                    );
                }
                let event = unique_fixture_row(&payload["history"], "sequence", &json!(1))?;
                if event["run_id"] != *run_id
                    || event["event"]["type"] != "admitted"
                    || event["event"]["definition"] != seed.definition
                {
                    return Err(
                        "workflow run history lost its admitted definition event".to_owned()
                    );
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    })())
}

fn unique_fixture_row<'a>(
    rows: &'a Value,
    key: &str,
    identity: &Value,
) -> Result<&'a Value, String> {
    let rows = rows
        .as_array()
        .ok_or_else(|| format!("fixture read omitted {key} rows"))?;
    let mut matches = rows.iter().filter(|row| row.get(key) == Some(identity));
    let row = matches
        .next()
        .ok_or_else(|| format!("fixture read omitted {identity}"))?;
    if matches.next().is_some() {
        return Err(format!("fixture read duplicated {identity}"));
    }
    Ok(row)
}

fn verify_cancelled_attempt(
    seed: &WorkSeeds,
    identity: &Value,
    attempt: &Value,
) -> Result<(), String> {
    if attempt["identity"] != *identity
        || attempt["execution"]["attempt_identity"] != *identity
        || attempt["execution"]["instructions"] != "Bench lifecycle attempt."
        || attempt["execution"]["commit"] != seed.commit
        || attempt["execution"]["execution_snapshot"] != seed.execution_snapshot
        || attempt["state"] != "cancelled"
        || attempt["terminal"]["outcome"] != "cancelled"
        || attempt["cancellation"]["state"] != "acknowledged"
        || attempt["actual_route"] != seed.execution_snapshot["route"]
    {
        return Err("attempt read lost the exact cancelled lifecycle's command, commit, route or terminal acknowledgement".to_owned());
    }
    Ok(())
}

fn verify_lifecycle_item(
    seed: &WorkSeeds,
    item: &Value,
    identities: &[&Value],
) -> Result<(), String> {
    if item["input"]["task_id"] != seed.task_id
        || item["input"]["title"] != "Bench lifecycle task"
        || item["input"]["effort"] != 1
        || item["accepted_route"]["decision"] != "selected"
        || item["accepted_route"]["recommended"] != seed.execution_snapshot["route"]
        || item["execution_admitted_at"].as_i64().is_none()
    {
        return Err(
            "Work read lost the seeded task's literal title, effort, selected route or admission"
                .to_owned(),
        );
    }
    let attempts = item["accepted_attempts"]
        .as_array()
        .ok_or("Work item omitted accepted attempts")?;
    for identity in identities {
        if attempts
            .iter()
            .filter(|attempt| **attempt == **identity)
            .count()
            != 1
        {
            return Err("Work item lost or duplicated an exact seeded accepted attempt".to_owned());
        }
    }
    Ok(())
}

fn w(ctx: &QueryContext) -> &WorkSeeds {
    ctx.seeds.work.as_ref().unwrap()
}

fn create_change() -> Value {
    super::work_create_change("{{iter}}", json!("{{now}}"))
}

fn step_prepare_create(ctx: &QueryContext) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_prepare_graph_mutation",
        args: json!({
            "selection": w(ctx).selection,
            "change": create_change(),
            "evidence": [],
            "format": "json",
        }),
        capture: &[("dig:request", "request")],
    }
}

fn step_create() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_create",
        args: json!("{{request}}"),
        capture: &[],
    }
}

fn step_generate(ctx: &QueryContext) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_generate_proposal",
        args: json!({
            "selection": w(ctx).selection,
            "task_id": "task.bench.{{iter}}",
            "proposal_id": "proposal.bench.{{iter}}",
            "occurred_at": "{{now}}",
            "format": "json",
        }),
        capture: &[("dig:proposal", "proposal")],
    }
}

fn step_prepare_decide(ctx: &QueryContext, disposition: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_prepare_graph_mutation",
        args: json!({
            "selection": w(ctx).selection,
            "change": {
                "change": "decide_proposal",
                "proposal": "{{proposal}}",
                "disposition": disposition,
            },
            "evidence": [],
            "format": "json",
        }),
        capture: &[("dig:request", "request")],
    }
}

fn step_prepare_admit(ctx: &QueryContext) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_prepare_graph_mutation",
        args: json!({
            "selection": w(ctx).selection,
            "change": {
                "change": "admit_execution",
                "task_id": "task.bench.{{iter}}",
                "based_on_version": "{{accepted_gv}}",
            },
            "evidence": [],
            "format": "json",
        }),
        capture: &[("dig:request", "request")],
    }
}

fn step_admit() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_admit_execution",
        args: json!("{{request}}"),
        capture: &[("dig:execution_snapshot", "execution_snapshot")],
    }
}

fn placement_args_run(run: &str) -> Value {
    json!({
        "task_id": "task.bench.{{iter}}",
        "run_id": run,
        "target": {
            "kind": "no_managed_placement",
            "root": null,
            "network_free": true,
            "in_place_acknowledged": false,
        },
        "occurred_at": "{{now}}",
    })
}

fn placement_args() -> Value {
    placement_args_run("run.bench.{{iter}}")
}

fn step_preflight_run(run: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_placement_preflight",
        args: placement_args_run(run),
        capture: &[],
    }
}

fn step_preflight() -> PrimeStep {
    step_preflight_run("run.bench.{{iter}}")
}

fn step_admit_placement_run(run: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_admit_placement",
        args: placement_args_run(run),
        capture: &[("digany:authority_version,graph_version", "auth_v")],
    }
}

fn step_admit_placement() -> PrimeStep {
    step_admit_placement_run("run.bench.{{iter}}")
}

fn start_args_ids(ctx: &QueryContext, run: &str, attempt: &str) -> Value {
    json!({
        "task_id": "task.bench.{{iter}}",
        "run_id": run,
        "attempt_id": attempt,
        "operation": "operation.work.start_attempt",
        "execution_snapshot": "{{execution_snapshot}}",
        "worktree_root": ctx.project_root,
        "reference": null,
        "commit": w(ctx).commit,
        "instructions": "Bench lifecycle attempt.",
        "effect_state": "observational",
        "occurred_at": "{{now}}",
    })
}

fn start_args(ctx: &QueryContext) -> Value {
    start_args_ids(ctx, "run.bench.{{iter}}", "attempt.bench.{{iter}}")
}

fn step_start_ids(ctx: &QueryContext, run: &'static str, attempt: &'static str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_start_attempt",
        args: start_args_ids(ctx, run, attempt),
        capture: &[],
    }
}

fn step_pause_run_id(run: &str) -> PrimeStep {
    // The run control's authority version bumps on every transition, so the
    // version resume must expect is the one pause just committed — captured
    // from pause's own payload, not the admit-time `auth_v`.
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_pause_run",
        args: json!({
            "task_id": "task.bench.{{iter}}",
            "run_id": run,
            "reason": "operator_request",
            "occurred_at": "{{now}}",
        }),
        capture: &[("digpath:payload:authority", "run_auth")],
    }
}

// ── prime fns (fn pointers; ctx carries the seeds) ────────────────────────

fn p_prepare(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![step_prepare_create(ctx)]
}

fn p_create(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![step_prepare_create(ctx), step_create()]
}

fn p_generate(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![step_prepare_create(ctx), step_create(), step_generate(ctx)]
}

fn p_decide(ctx: &QueryContext, _i: u64, disposition: &str) -> Vec<PrimeStep> {
    let mut v = p_generate(ctx, 0);
    v.push(step_prepare_decide(ctx, disposition));
    v
}

fn p_decide_accepted(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    p_decide(ctx, i, "accepted")
}

fn p_decide_rejected(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    p_decide(ctx, i, "rejected")
}

fn step_accept() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_accept_proposal",
        args: json!("{{request}}"),
        capture: &[("digany:graph_version", "accepted_gv")],
    }
}

fn p_accept(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_decide(ctx, i, "accepted");
    v.push(step_accept());
    v
}

fn p_admit_chain(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_accept(ctx, i);
    v.push(step_prepare_admit(ctx));
    v
}

fn p_admit(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit_chain(ctx, i);
    v.push(step_admit());
    v
}

fn p_placement(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit(ctx, i);
    v.push(step_preflight());
    v
}

fn p_admit_placement(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_placement(ctx, i);
    v.push(step_admit_placement());
    v
}

fn p_admit_placement_run(ctx: &QueryContext, i: u64, run: &'static str) -> Vec<PrimeStep> {
    let mut v = p_accept(ctx, i);
    v.push(step_prepare_admit(ctx));
    v.push(step_admit());
    v.push(step_preflight_run(run));
    v.push(step_admit_placement_run(run));
    v
}

// Attempt lifecycle groups each run on their own run/attempt identity so a
// cancelled or paused attempt in one group never collides with another's
// prime replay, and every open attempt is released by a cleanup cancel.
fn p_start_cx(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit_placement_run(ctx, i, "run.bench.cx.{{iter}}");
    v.push(step_start_ids(
        ctx,
        "run.bench.cx.{{iter}}",
        "attempt.bench.cx.{{iter}}",
    ));
    v
}

fn p_start_pz(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit_placement_run(ctx, i, "run.bench.pz.{{iter}}");
    v.push(step_start_ids(
        ctx,
        "run.bench.pz.{{iter}}",
        "attempt.bench.pz.{{iter}}",
    ));
    v
}

fn p_pause_rz(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_admit_placement_run(ctx, i, "run.bench.rz.{{iter}}");
    v.push(step_start_ids(
        ctx,
        "run.bench.rz.{{iter}}",
        "attempt.bench.rz.{{iter}}",
    ));
    v.push(step_pause_run_id("run.bench.rz.{{iter}}"));
    v
}

fn cancel_attempt_step(task: &str, run: &str, attempt: &str, request: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_cancel_attempt",
        args: json!({
            "task_id": task,
            "run_id": run,
            "attempt_id": attempt,
            "request_id": request,
            "occurred_at": "{{now}}",
        }),
        capture: &[],
    }
}

// The recovery sweep seals any `cancellation_*` residue the cancel left
// behind: a cancel that lands while the provider is still spawning parks the
// row in `cancellation_requested`, and the topology's single parallel-attempt
// slot stays occupied until the sweep completes it to `cancelled`.
fn resume_attempts_step() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_resume_attempts",
        args: json!({"occurred_at": "{{now}}"}),
        capture: &[],
    }
}

fn cleanup_cancel_start(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        cancel_attempt_step(
            "task.bench.{{iter}}",
            "run.bench.{{iter}}",
            "attempt.bench.{{iter}}",
            "cancel.cleanup.bench.{{iter}}",
        ),
        resume_attempts_step(),
    ]
}

fn cleanup_cancel_cx(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    // The timed call already requested the cancellation; what the next
    // iteration needs is the topology's single parallel slot back. The
    // recovery sweep seals the parked cancellation to a terminal state —
    // a duplicate cancel would only burn the transient window on
    // `not-cancellable` while the row stays non-terminal.
    vec![resume_attempts_step()]
}

fn cleanup_cancel_pz(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        cancel_attempt_step(
            "task.bench.{{iter}}",
            "run.bench.pz.{{iter}}",
            "attempt.bench.pz.{{iter}}",
            "cancel.cleanup.bench.pz.{{iter}}",
        ),
        resume_attempts_step(),
    ]
}

fn cleanup_cancel_rz(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        cancel_attempt_step(
            "task.bench.{{iter}}",
            "run.bench.rz.{{iter}}",
            "attempt.bench.rz.{{iter}}",
            "cancel.cleanup.bench.rz.{{iter}}",
        ),
        resume_attempts_step(),
    ]
}

fn p_retry(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut steps = p_admit_placement_run(ctx, i, "run.bench.retry.{{iter}}");
    let mut start = step_start_ids(
        ctx,
        "run.bench.retry.{{iter}}",
        "attempt.bench.failed.{{iter}}",
    );
    start.args["instructions"] = json!(super::WORK_FAILURE_INSTRUCTIONS);
    start.capture = &[
        ("dig:identity", "retry_original"),
        ("digpath:terminal:evidence_digest", "failure_digest"),
    ];
    steps.push(start);
    steps
}

fn p_recovery(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut steps = p_retry(ctx, i);
    steps.push(PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_retry_attempt",
        args: json!({
            "original_attempt": "{{retry_original}}",
            "new_attempt_id": "attempt.recovery.bench.{{iter}}",
            "failure": {
                "source": "runtime", "cause": "runtime_failure",
                "evidence_ref": "runtime-terminal:{{failure_digest}}",
            },
            "command_id": "command.bench.recovery.{{iter}}",
        }),
        capture: &[("digpath:payload:attempt.identity", "resume_identity")],
    });
    steps
}

fn p_experience(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut steps = p_admit_placement_run(ctx, i, "run.bench.experience.{{iter}}");
    let mut source = step_start_ids(
        ctx,
        "run.bench.experience.{{iter}}",
        "attempt.bench.experience.{{iter}}",
    );
    source
        .inject
        .push(("experience_not_before".to_owned(), json!(now_micros())));
    source.args["instructions"] = json!(super::WORK_SOURCE_INSTRUCTIONS);
    source.capture = &[("dig:identity", "experience_source")];
    steps.push(source);
    steps.push(PrimeStep {
        inject: Vec::new(), tool: "tracedecay_work_prepare_graph_mutation",
        args: json!({"selection":w(ctx).selection,
            "change":{"change":"accept_task","task_id":"task.bench.{{iter}}","evidence_by_criterion":{}},
            "evidence":[],"format":"json"}),
        capture: &[("dig:request","experience_accept_request")],
    });
    steps.push(PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_mutate_graph",
        args: json!({"mutation":"accept_task","request":"{{experience_accept_request}}"}),
        capture: &[],
    });
    steps.push(PrimeStep {
        inject: Vec::new(), tool:"tracedecay_work_views",
        args:json!({"selection":w(ctx).selection,"mode":{"mode":"current"},"continuation":null,"observed_at":"{{now}}"}),
        capture:&[("digpath:payload:authorized_scope.owner_profile_id","experience_profile"),
            ("dig:verified_version","experience_version")],
    });
    let now = now_micros();
    let consent = json!({"kind":"work_expertise_consent","value":{
        "schema_version":1,"enabled":true,"granted_at":now,"expires_at":now+3_600_000_000_i64,
        "allowed_categories":["testing"],
    }});
    for (key, layer, prior_token) in [
        (
            tracedecay_domain::USER_WORK_EXPERTISE_CONSENT_SETTING_KEY,
            json!({"kind":"user_profile","profile_id":"{{experience_profile}}"}),
            "experience_prior_user",
        ),
        (
            tracedecay_domain::PROJECT_WORK_EXPERTISE_CONSENT_SETTING_KEY,
            json!({"kind":"project","project_id":ctx.seeds.project_id}),
            "experience_prior_project",
        ),
    ] {
        let capture = if prior_token == "experience_prior_user" {
            &[
                ("digpath:payload:revision_id", "experience_revision"),
                ("digpath:payload:effective_value", "experience_prior_user"),
            ][..]
        } else {
            &[
                ("digpath:payload:revision_id", "experience_revision"),
                (
                    "digpath:payload:effective_value",
                    "experience_prior_project",
                ),
            ][..]
        };
        steps.push(PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_configuration_get",
            args: json!({"key":key,"format":"json"}),
            capture,
        });
        steps.push(PrimeStep { inject:Vec::new(),tool:"tracedecay_configuration_set",
            args:json!({"key":key,"layer":layer,"value":consent,"expected_revision":"{{experience_revision}}",
                "idempotency_key":format!("bench-experience-grant-{prior_token}-{{{{iter}}}}"),"format":"json"}),capture:&[] });
    }
    steps
}

fn cleanup_experience(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    let mut steps = Vec::new();
    for (key, layer) in [
        (
            tracedecay_domain::USER_WORK_EXPERTISE_CONSENT_SETTING_KEY,
            json!({"kind":"user_profile","profile_id":"{{experience_profile}}"}),
        ),
        (
            tracedecay_domain::PROJECT_WORK_EXPERTISE_CONSENT_SETTING_KEY,
            json!({"kind":"project","project_id":ctx.seeds.project_id}),
        ),
    ] {
        steps.push(PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_configuration_get",
            args: json!({"key":key,"format":"json"}),
            capture: &[("digpath:payload:revision_id", "experience_restore_revision")],
        });
        steps.push(PrimeStep { inject:Vec::new(),tool:"tracedecay_configuration_unset",
            args:json!({"key":key,"layer":layer,"expected_revision":"{{experience_restore_revision}}",
                "idempotency_key":format!("bench-experience-revoke-{key}-{{{{iter}}}}"),"format":"json"}),capture:&[] });
    }
    steps
}

fn cleanup_retry(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_attempt_status",
        args: json!({
            "task_id": "task.bench.{{iter}}",
            "run_id": "run.bench.retry.{{iter}}",
            "attempt_id": "attempt.retry.bench.{{iter}}",
        }),
        capture: &[],
    }]
}

fn duplicate_args(ctx: &QueryContext) -> Value {
    json!({
        "first_attempt": w(ctx).attempt_identity,
        "second_attempt": w(ctx).dup_attempt_identity,
        "verdict": "not_duplicate",
        "quantities": {
            "wall_micros": null, "token_count": null, "cost_micros": null,
            "test_count": null, "effect_count": null,
            "evidence": "owner_receipt",
            "effect_outcome": "not_applicable",
            "coverage": "known",
        },
        "reason": "bench duplicate probe",
    })
}

fn p_duplicate(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_work_prepare_duplicate_adjudication",
        args: duplicate_args(ctx),
        capture: &[("dig:payload", "duplicate_request")],
    }]
}

fn p_synthesize(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut steps = p_admit_placement_run(ctx, i, "run.bench.source.{{iter}}");
    let mut source = step_start_ids(
        ctx,
        "run.bench.source.{{iter}}",
        "attempt.bench.source.{{iter}}",
    );
    source.args["instructions"] = json!(super::WORK_SOURCE_INSTRUCTIONS);
    source.capture = &[
        ("dig:identity", "synthesis_source"),
        (
            "digpath:payload:artifacts.0.digest",
            "source_artifact_digest",
        ),
    ];
    steps.push(source);
    steps.push(step_preflight_run("run.bench.synth.{{iter}}"));
    steps.push(step_admit_placement_run("run.bench.synth.{{iter}}"));
    steps
}

fn cleanup_cancel_synth(_ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        cancel_attempt_step(
            "task.bench.{{iter}}",
            "run.bench.synth.{{iter}}",
            "attempt.bench.synth.{{iter}}",
            "cancel.cleanup.bench.synth.{{iter}}",
        ),
        resume_attempts_step(),
    ]
}

// ── workflow primes ─────────────────────────────────────────────────────

fn wf_definition(ctx: &QueryContext, suffix: &str) -> Value {
    let mut def = w(ctx).definition.clone();
    def["definition_id"] = json!(format!("workflow.bench.{suffix}"));
    def["definition_version"] = json!(1);
    def
}

fn step_wf_register(ctx: &QueryContext, suffix: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_register_definition",
        args: json!({"definition": wf_definition(ctx, suffix), "format": "json"}),
        capture: &[],
    }
}

fn wf_activate_args(suffix: &str) -> Value {
    json!({
        "definition_id": format!("workflow.bench.{suffix}"),
        "definition_version": 1,
        "expected_revision": 1,
    })
}

fn wf_run_args(suffix: &str) -> Value {
    json!({
        "run_id": format!("workflow-run.bench.{suffix}"),
        "definition_id": format!("workflow.bench.{suffix}"),
        "definition_version": 1,
        "provider": {
            "route": {"provider_id": "provider.work.codex-cli", "route_id": "route.bench.workflow"},
            "backend": "codex_cli",
            "model": "bench",
            "priority": 1,
        },
        "fan_out": null,
        "command_id": format!("command.workflow.bench.start.{suffix}"),
    })
}

fn step_wf_activate(suffix: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_activate_definition",
        args: wf_activate_args(suffix),
        capture: &[("digpath:payload:revision", "wf_rev")],
    }
}

fn step_wf_start_run(suffix: &str) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_start_run",
        args: wf_run_args(suffix),
        capture: &[("digany:sequence", "wf_seq")],
    }
}

fn p_wf_register(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![step_wf_register(ctx, "{{iter}}")]
}

fn p_wf_active(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        step_wf_register(ctx, "{{iter}}"),
        step_wf_activate("{{iter}}"),
    ]
}

fn p_wf_run(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        step_wf_register(ctx, "{{iter}}"),
        step_wf_activate("{{iter}}"),
        step_wf_start_run("{{iter}}"),
    ]
}

fn p_wf_paused(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_wf_run(ctx, i);
    v.push(PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_pause_run",
        args: json!({
            "run_id": "workflow-run.bench.{{iter}}",
            "expected_sequence": "{{wf_seq}}",
            "command_id": "command.workflow.bench.pause.{{iter}}",
        }),
        capture: &[("digany:sequence", "wf_seq")],
    });
    v
}

/// Handoff scope: identical in issue + redeem; {{iter}} keeps it unique.
fn handoff_scope(ctx: &QueryContext) -> Value {
    let w = w(ctx);
    json!({
        "project_id": w.definition.get("project_id").cloned().unwrap_or(Value::Null),
        "repository_id": ctx.seeds.repository_id,
        "worktree_id": w.worktree_id,
        "definition_id": "workflow.bench.handoff.{{iter}}",
        "definition_version": 1,
        "step_id": "step.bench.inspect",
        "task_id": "task.bench.handoff.{{iter}}",
        "thread_id": "thread.bench.{{iter}}",
        "run_id": "workflow-run.bench.handoff.{{iter}}",
        "from_actor_id": w.actor_id,
        "to_actor_id": w.actor_id,
    })
}

fn handoff_frontier(ctx: &QueryContext) -> Value {
    json!({
        "task_id": "task.bench.handoff.{{iter}}",
        "work_version": 1,
        "attempts": [],
        "unknowns": [],
        "blockers": [],
        "legal_actions": ["Inspect the bench frontier."],
        "lineage": {
            "issued_by": w(ctx).actor_id,
            "issued_at": "{{now}}",
            "prior_frontier_digest": null,
        },
    })
}

fn issue_args(ctx: &QueryContext) -> Value {
    json!({
        "scope": handoff_scope(ctx),
        "secret": "bench-handoff-{{iter}}-ssssssssssssssssssssssssssssssss",
        "frontier": handoff_frontier(ctx),
    })
}

/// Primes for a live handoff run: register + activate + start_run on the
/// handoff-scoped definition.
fn p_handoff_run(ctx: &QueryContext, _i: u64) -> Vec<PrimeStep> {
    vec![
        step_wf_register(ctx, "handoff.{{iter}}"),
        step_wf_activate("handoff.{{iter}}"),
        step_wf_start_run("handoff.{{iter}}"),
    ]
}

fn p_handoff(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    let mut v = p_handoff_run(ctx, i);
    v.push(PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_workflow_handoff_issue",
        args: issue_args(ctx),
        capture: &[],
    });
    v
}

fn p_handoff_run_only(ctx: &QueryContext, i: u64) -> Vec<PrimeStep> {
    p_handoff_run(ctx, i)
}

/// Per-tool group with `queries` variants of one arg shape.
fn tg(tool: &'static str, queries: Vec<Query>) -> ToolGroup {
    ToolGroup { tool, queries }
}

/// One static arg shape ×5 — the harness invariant.
fn fiveq(q: &dyn Fn(usize) -> Query) -> Vec<Query> {
    five(q)
}

pub fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    let Some(w) = ctx.seeds.work.as_ref() else {
        return;
    };
    let sel = || w.selection.clone();
    let ident = || w.attempt_identity.clone();

    // ── reads ────────────────────────────────────────────────────────────
    let now = now_micros();
    out.push(tg(
        "tracedecay_work_views",
        vec![
            rqn(
                "tracedecay_work_views",
                "current",
                json!({
                    "selection": sel(), "mode": {"mode": "current"},
                    "continuation": null, "observed_at": now,
                }),
            ),
            rqn(
                "tracedecay_work_views",
                "as_of",
                json!({
                    "selection": sel(),
                    "mode": {"mode": "as_of", "valid_at": now},
                    "continuation": null, "observed_at": now,
                }),
            ),
            rqn(
                "tracedecay_work_views",
                "current_again",
                json!({
                    "selection": sel(), "mode": {"mode": "current"},
                    "continuation": null, "observed_at": now,
                }),
            ),
            rqn(
                "tracedecay_work_views",
                "windowed",
                json!({
                    "selection": sel(),
                    "mode": {"mode": "evolution", "from_valid_at": 0, "through_valid_at": now},
                    "continuation": null, "observed_at": now,
                }),
            ),
            rqn(
                "tracedecay_work_views",
                "current_paged",
                json!({
                    "selection": sel(), "mode": {"mode": "current"},
                    "continuation": null, "observed_at": now,
                }),
            ),
        ],
    ));
    for (tool, args) in [
        ("tracedecay_work_topology", json!({"page_size": 8})),
        (
            "tracedecay_work_topology_metrics",
            json!({
                "horizon": {"since_micros": 0, "until_micros": now},
                "max_events": 4096,
            }),
        ),
        ("tracedecay_work_list_attempts", json!({"page_size": 8})),
        ("tracedecay_work_execution_history", json!({"page_size": 8})),
        ("tracedecay_work_hydrate_artifacts", json!({"page_size": 8})),
    ] {
        out.push(tg(tool, fiveq(&|_| rqn(tool, "page", args.clone()))));
    }
    for (tool, args) in [
        (
            "tracedecay_work_attempt_status",
            json!({
                "task_id": w.task_id, "run_id": w.run_id, "attempt_id": w.attempt_id,
            }),
        ),
        (
            "tracedecay_work_run_control",
            json!({
                "task_id": w.task_id, "run_id": w.run_id,
            }),
        ),
        (
            "tracedecay_work_placement_status",
            json!({
                "task_id": w.task_id, "run_id": w.run_id,
            }),
        ),
    ] {
        out.push(tg(tool, fiveq(&|_| rqn(tool, "identity", args.clone()))));
    }
    out.push(tg(
        "tracedecay_work_retrieve_evidence",
        fiveq(&|_| {
            rqn(
                "tracedecay_work_retrieve_evidence",
                "evidence",
                json!({
                    "selection": sel(), "task_id": w.task_id,
                    "verified_version": w.current_version,
                    "temporal": {"kind": "current"},
                    "page_size": 8, "observed_at": now_micros(),
                }),
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_experience",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_experience",
                "experience",
                json!({
                    "selection": sel(), "task_id": w.task_id,
                    "verified_version": "{{experience_version}}",
                    "evidence_not_before": "{{experience_not_before}}",
                    "expertise_categories": ["testing"],
                    "limit": 8, "observed_at": now_micros(),
                }),
                p_experience,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_experience,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_compare_proposal",
        fiveq(&|_| {
            rqn(
                "tracedecay_work_compare_proposal",
                "compare",
                json!({
                    "selection": sel(), "task_id": w.task_id,
                    "old_version": w.initial_version,
                    "new_version": w.current_version,
                    "observed_at": now_micros(),
                }),
            )
        }),
    ));

    // ── write/effect chain ────────────────────────────────────────────────
    out.push(tg(
        "tracedecay_work_prepare_graph_mutation",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_prepare_graph_mutation",
                "prepare_create",
                json!({
                    "selection": sel(), "change": create_change(), "evidence": [],
                }),
                no_primes,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_mutate_graph",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_mutate_graph",
                "commit",
                json!({"mutation": "create_task", "request": "{{request}}"}),
                p_prepare,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_create",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_create",
                "commit",
                json!("{{request}}"),
                p_prepare,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_generate_proposal",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_generate_proposal",
                "generate",
                json!({
                    "selection": sel(),
                    "task_id": "task.bench.{{iter}}",
                    "proposal_id": "proposal.bench.{{iter}}",
                    "occurred_at": "{{now}}",
                }),
                p_create,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_review_proposal",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_review_proposal",
                "review",
                json!("{{request}}"),
                p_decide_rejected,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_accept_proposal",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_accept_proposal",
                "accept",
                json!("{{request}}"),
                p_decide_accepted,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_admit_execution",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_admit_execution",
                "admit",
                json!("{{request}}"),
                p_admit_chain,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_placement_preflight",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_placement_preflight",
                "preflight",
                placement_args(),
                p_admit,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_admit_placement",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_admit_placement",
                "admit",
                placement_args(),
                p_admit,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_start_attempt",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_start_attempt",
                "start",
                start_args(ctx),
                p_admit_placement,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_start,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_cancel_attempt",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_cancel_attempt",
                "cancel",
                json!({
                    "task_id": "task.bench.{{iter}}",
                    "run_id": "run.bench.cx.{{iter}}",
                    "attempt_id": "attempt.bench.cx.{{iter}}",
                    "request_id": "cancel.bench.{{iter}}",
                    "occurred_at": "{{now}}",
                }),
                p_start_cx,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_cx,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_pause_run",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_pause_run",
                "pause",
                json!({
                    "task_id": "task.bench.{{iter}}",
                    "run_id": "run.bench.pz.{{iter}}",
                    "reason": "operator_request",
                    "occurred_at": "{{now}}",
                }),
                p_start_pz,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_pz,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_resume_run",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_resume_run",
                "resume",
                json!({
                    "task_id": "task.bench.{{iter}}",
                    "run_id": "run.bench.rz.{{iter}}",
                    "reason": "operator_request",
                    "expected_authority_version": "{{run_auth}}",
                    "occurred_at": "{{now}}",
                }),
                p_pause_rz,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_rz,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_release_placement",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_release_placement",
                "release",
                json!({
                    "task_id": "task.bench.{{iter}}",
                    "run_id": "run.bench.{{iter}}",
                    "expected_authority_version": "{{auth_v}}",
                    "occurred_at": "{{now}}",
                }),
                p_admit_placement,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_resume_attempts",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_resume_attempts",
                "resume_all",
                json!({
                    "occurred_at": "{{now}}",
                }),
                p_recovery,
            )
        }),
    ));
    if !w.attempt_identity.is_null() && w.dup_attempt_identity.is_some() {
        out.push(tg(
            "tracedecay_work_prepare_duplicate_adjudication",
            fiveq(&|_| {
                eqn(
                    "tracedecay_work_prepare_duplicate_adjudication",
                    "prepare",
                    duplicate_args(ctx),
                    no_primes,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_work_adjudicate_duplicate",
            fiveq(&|_| {
                eqn(
                    "tracedecay_work_adjudicate_duplicate",
                    "commit",
                    json!("{{duplicate_request}}"),
                    p_duplicate,
                )
            }),
        ));
    }
    out.push(tg(
        "tracedecay_work_adjudicate_leak",
        fiveq(&|_| {
            eqn(
                "tracedecay_work_adjudicate_leak",
                "leak",
                json!({
                    "adjudication_id": "adjudication.bench.{{iter}}",
                    "attempt": ident(),
                    "command_id": "command.bench.leak.{{iter}}",
                    "detection_horizon_micros": 3_600_000_000_i64,
                    "expected_revision": null,
                }),
                no_primes,
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_retry_attempt",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_retry_attempt",
                "retry",
                json!({
                    "original_attempt": "{{retry_original}}",
                    "new_attempt_id": "attempt.retry.bench.{{iter}}",
                    "failure": {
                        "source": "runtime",
                        "cause": "runtime_failure",
                        "evidence_ref": "runtime-terminal:{{failure_digest}}",
                    },
                    "command_id": "command.bench.retry.{{iter}}",
                }),
                p_retry,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_retry,
                },
            )
        }),
    ));
    out.push(tg(
        "tracedecay_work_synthesize",
        fiveq(&|_| {
            eqc(
                "tracedecay_work_synthesize",
                "synthesize",
                json!({
                    "output_name": "bench.synthesis",
                    "sources": ["{{synthesis_source}}"],
                    "start": {
                        "task_id": "task.bench.{{iter}}",
                        "run_id": "run.bench.synth.{{iter}}",
                        "attempt_id": "attempt.bench.synth.{{iter}}",
                        "operation": "operation.work.start_attempt",
                        "execution_snapshot": "{{execution_snapshot}}",
                        "worktree_root": ctx.project_root,
                        "commit": w.commit,
                        "instructions": "Bench synthesis attempt.",
                        "effect_state": "observational",
                        "occurred_at": "{{now}}",
                    },
                }),
                p_synthesize,
                crate::queries::EffectCleanup {
                    capture: &[],
                    steps: cleanup_cancel_synth,
                },
            )
        }),
    ));

    if !w.definition_id.is_empty() {
        out.push(tg(
            "tracedecay_workflow_list_definitions",
            fiveq(&|_| rqn("tracedecay_workflow_list_definitions", "list", json!({}))),
        ));
        out.push(tg(
            "tracedecay_workflow_get_definition",
            fiveq(&|_| {
                rqn(
                    "tracedecay_workflow_get_definition",
                    "get",
                    json!({
                        "definition_id": w.definition_id,
                        "definition_version": w.definition["definition_version"],
                    }),
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_definition_history",
            fiveq(&|_| {
                rqn(
                    "tracedecay_workflow_definition_history",
                    "history",
                    json!({
                        "definition_id": w.definition_id,
                    }),
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_diff_definition",
            fiveq(&|_| {
                rqn(
                    "tracedecay_workflow_diff_definition",
                    "diff",
                    json!({
                        "definition_id": w.definition_id,
                        "from_version": w.definition["definition_version"],
                        "to_version": w.definition["definition_version"],
                    }),
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_validate_definition",
            fiveq(&|_| {
                rqn(
                    "tracedecay_workflow_validate_definition",
                    "validate",
                    json!({
                        "definition": w.definition,
                    }),
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_register_definition",
            fiveq(&|_| {
                // No primes: the timed call IS the registration — priming the
                // same definition would only measure the idempotent replay.
                eqn(
                    "tracedecay_workflow_register_definition",
                    "register",
                    json!({
                        "definition": wf_definition(ctx, "{{iter}}"),
                    }),
                    no_primes,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_activate_definition",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_activate_definition",
                    "activate",
                    wf_activate_args("{{iter}}"),
                    p_wf_register,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_reject_definition",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_reject_definition",
                    "reject",
                    wf_activate_args("{{iter}}"),
                    p_wf_register,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_retire_definition",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_retire_definition",
                    "retire",
                    json!({
                        "definition_id": "workflow.bench.{{iter}}",
                        "definition_version": 1,
                        "expected_revision": "{{wf_rev}}",
                    }),
                    p_wf_active,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_start_run",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_start_run",
                    "start",
                    wf_run_args("{{iter}}"),
                    p_wf_active,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_pause_run",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_pause_run",
                    "pause",
                    json!({
                        "run_id": "workflow-run.bench.{{iter}}",
                        "expected_sequence": "{{wf_seq}}",
                        "command_id": "command.workflow.bench.pause.{{iter}}",
                    }),
                    p_wf_run,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_resume_run",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_resume_run",
                    "resume",
                    json!({
                        "run_id": "workflow-run.bench.{{iter}}",
                        "expected_sequence": "{{wf_seq}}",
                        "command_id": "command.workflow.bench.resume.{{iter}}",
                    }),
                    p_wf_paused,
                )
            }),
        ));
        out.push(tg(
            "tracedecay_workflow_cancel_run",
            fiveq(&|_| {
                eqn(
                    "tracedecay_workflow_cancel_run",
                    "cancel",
                    json!({
                        "run_id": "workflow-run.bench.{{iter}}",
                        "expected_sequence": "{{wf_seq}}",
                        "command_id": "command.workflow.bench.cancel.{{iter}}",
                    }),
                    p_wf_run,
                )
            }),
        ));
        if w.worktree_id.is_some() && !w.actor_id.is_empty() {
            out.push(tg(
                "tracedecay_workflow_handoff_issue",
                fiveq(&|_| {
                    eqn(
                        "tracedecay_workflow_handoff_issue",
                        "issue",
                        issue_args(ctx),
                        p_handoff_run_only,
                    )
                }),
            ));
            out.push(tg(
                "tracedecay_workflow_handoff_redeem",
                fiveq(&|_| {
                    eqn(
                        "tracedecay_workflow_handoff_redeem",
                        "redeem",
                        json!({
                            "secret": "bench-handoff-{{iter}}-ssssssssssssssssssssssssssssssss",
                            "expected_scope": handoff_scope(ctx),
                        }),
                        p_handoff,
                    )
                }),
            ));
        }
        if let Some(run_id) = &w.wf_run_id {
            out.push(tg(
                "tracedecay_workflow_get_run",
                fiveq(&|_| {
                    rqn(
                        "tracedecay_workflow_get_run",
                        "get_run",
                        json!({"run_id": run_id}),
                    )
                }),
            ));
        }
    }
}
