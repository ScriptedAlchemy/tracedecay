//! Admin / status family: store status, runtime, config read surface, health,
//! application summaries, project registry, observatory, automation, native
//! integration reads, and the multi_root + worktree read lanes.

use serde_json::{Value, json};
use tracedecay_contracts::git::{
    GitHubStackSignalExpandSurfaceResultV1, NativeIntegrationCancellationProjectionV1,
    NativeIntegrationSelectionDeclarationV1, NativeIntegrationStackSnapshotSurfaceRequest,
    NativeIntegrationSurfaceResultV1, NativeWorktreeSurfaceResultV1, NativeWorktreeTargetV1,
    WorktreeCleanupReconciliationV1, WorktreeCleanupRemovalV1, WorktreeConfirmationOutcomeV1,
    WorktreeCoverageV1, WorktreeInspectionOutcomeV1, WorktreeInventoryOutcomeV1, WorktreeKindV1,
    WorktreePresenceV1, worktree_confirmation_digest, worktree_inspection_digest,
};
use tracedecay_contracts::retrieval::{CodeQueryPage, SymbolPrimitiveRecord};
use tracedecay_contracts::{
    AuthorizedScopeSet, MultiRootQueryPageV1, MultiRootScopeSetCasResultV1,
    MultiRootScopeSetCasStatusV1, ResolvedScope,
};
use tracedecay_domain::{
    NativeIntegrationPhaseV1, NativeIntegrationTerminalOutcomeV1, ScopeOutcome,
};

use crate::queries::{PrimeStep, Query, QueryContext, QueryKind, ToolGroup, five};

use super::{eq, eqn, no_primes, rq};

pub(crate) fn verify_github_stack_signal_fixture(
    args: &Value,
    response: &Value,
    prepared_signal: Option<&Value>,
) -> Result<(), String> {
    let prepared = prepared_signal.ok_or("stack signal replay omitted its prepared evidence")?;
    let packet = response
        .pointer("/value/outcome/value")
        .or_else(|| response.pointer("/outcome/value"))
        .ok_or("stack signal expansion omitted its evidence packet")?;
    let payload = packet
        .get("payload")
        .ok_or("stack signal expansion omitted its canonical payload")?;
    let result: GitHubStackSignalExpandSurfaceResultV1 =
        serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
    let GitHubStackSignalExpandSurfaceResultV1::Expanded { evidence } = result else {
        return Err("stack signal replay did not expand its durable evidence".into());
    };
    if args["signal_id"].as_str() != Some(evidence.signal_id.as_str())
        || args["expected_watermark_id"].as_str() != Some(evidence.watermark_id.as_str())
        || Some(&serde_json::to_value(evidence).map_err(|error| error.to_string())?)
            != prepared.get("evidence")
        || packet.pointer("/authority/authorized_scope_digest")
            != Some(&prepared["authorized_scope_digest"])
    {
        return Err(
            "stack signal replay did not match its prepared native preview and delivery watermark"
                .into(),
        );
    }
    Ok(())
}

/// Scout reads use fresh native admission outside the timed interval.
pub(crate) fn context_scout_groups(out: &mut Vec<ToolGroup>) {
    for (tool, label, extra) in [
        (
            "tracedecay_context_scout_status",
            "context_scout_status",
            json!({}),
        ),
        (
            "tracedecay_context_scout_capability",
            "context_scout_capability",
            json!({}),
        ),
        (
            "tracedecay_context_scout_budget",
            "context_scout_budget",
            json!({}),
        ),
        (
            "tracedecay_context_scout_recent",
            "context_scout_recent",
            json!({"limit": 8}),
        ),
        (
            "tracedecay_context_scout_explain",
            "context_scout_explain",
            json!({"limit": 8}),
        ),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|_| {
                let mut args = extra.clone();
                args["address"] = json!("{{scout_address}}");
                eq(tool, label, args, no_primes)
            }),
        });
    }
}

fn context_scout_revision_prime() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_configuration_get",
        args: json!({"key": "context_scout.settings.v1", "format": "json"}),
        capture: &[("digpath:payload:revision_id", "scout_revision")],
    }
}

/// Each control request uses a current CAS revision. Resume first pauses the
/// same admitted session, so both timed operations commit a real transition.
pub(crate) fn context_scout_control_groups(out: &mut Vec<ToolGroup>) {
    for (tool, label, state) in [
        (
            "tracedecay_context_scout_pause",
            "context_scout_pause",
            "paused",
        ),
        (
            "tracedecay_context_scout_resume",
            "context_scout_resume",
            "active",
        ),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|iter| {
                eq(
                    tool,
                    label,
                    json!({
                        "address": "{{scout_address}}",
                        "expected_revision": "{{scout_revision}}",
                        "idempotency_key": format!("bench-context-scout-{state}-{iter}-{{{{iter}}}}"),
                    }),
                    if state == "paused" {
                        |_ctx, _| vec![context_scout_revision_prime()]
                    } else {
                        |_ctx, _| vec![
                            context_scout_revision_prime(),
                            PrimeStep {
                                inject: Vec::new(),
                                tool: "tracedecay_context_scout_pause",
                                args: json!({
                                    "address": "{{scout_address}}",
                                    "expected_revision": "{{scout_revision}}",
                                    "idempotency_key": "bench-context-scout-resume-pause-{{iter}}",
                                    "format": "json",
                                }),
                                capture: &[],
                            },
                            context_scout_revision_prime(),
                        ]
                    },
                )
            }),
        });
    }
}

/// Claims invoke the real explicit producer. Delivery and feedback consume
/// the exact lease and receipt returned by the preceding untimed operations.
pub(crate) fn context_scout_mutation_groups(out: &mut Vec<ToolGroup>) {
    out.push(ToolGroup {
        tool: "tracedecay_context_scout_claim",
        queries: five(|iter| {
            eq(
                "tracedecay_context_scout_claim",
                "context_scout_claim",
                json!({
                    "address": "{{scout_address}}",
                    "window": "on_request",
                    "idempotency_key": format!("bench-context-scout-claim-timed-{iter}-{{{{iter}}}}"),
                }),
                no_primes,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_context_scout_delivery",
        queries: five(|iter| {
            eq(
                "tracedecay_context_scout_delivery",
                "context_scout_delivery",
                json!({
                    "address": "{{scout_address}}",
                    "claim": "{{scout_claim}}",
                    "delivered_at": "{{now}}",
                    "outcome": "displayed",
                    "idempotency_key": format!("bench-context-scout-delivery-timed-{iter}-{{{{iter}}}}"),
                }),
                |_ctx, _| vec![
                    PrimeStep {
                        inject: Vec::new(),
                        tool: "tracedecay_context_scout_claim",
                        args: json!({
                            "address": "{{scout_address}}",
                            "window": "on_request",
                            "idempotency_key": "bench-context-scout-claim-{{iter}}",
                        }),
                        capture: &[("dig:claim", "scout_claim")],
                    },
                ],
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_context_scout_feedback",
        queries: five(|iter| {
            eq(
                "tracedecay_context_scout_feedback",
                "context_scout_feedback",
                json!({
                    "address": "{{scout_address}}",
                    "receipt": "{{scout_receipt}}",
                    "feedback": {
                        "receipt_id": "{{scout_receipt_id}}",
                        "kind": "explicitly_accepted",
                    },
                    "idempotency_key": format!("bench-context-scout-feedback-{iter}-{{{{iter}}}}"),
                }),
                |_ctx, _| {
                    vec![
                        PrimeStep {
                            inject: Vec::new(),
                            tool: "tracedecay_context_scout_claim",
                            args: json!({
                                "address": "{{scout_address}}",
                                "window": "on_request",
                                "idempotency_key": "bench-context-scout-feedback-claim-{{iter}}",
                            }),
                            capture: &[("dig:claim", "scout_claim")],
                        },
                        PrimeStep {
                            inject: Vec::new(),
                            tool: "tracedecay_context_scout_delivery",
                            args: json!({
                                "address": "{{scout_address}}",
                                "claim": "{{scout_claim}}",
                                "delivered_at": "{{now}}",
                                "outcome": "displayed",
                                "idempotency_key": "bench-context-scout-feedback-delivery-{{iter}}",
                            }),
                            capture: &[
                                ("dig:receipt", "scout_receipt"),
                                ("digpath:receipt:receipt_id", "scout_receipt_id"),
                            ],
                        },
                    ]
                },
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_context_scout_cancel",
        queries: five(|iter| {
            eq(
                "tracedecay_context_scout_cancel",
                "context_scout_cancel",
                json!({
                    "address": "{{scout_address}}",
                    "work": "{{scout_work}}",
                    "idempotency_key": format!("bench-context-scout-cancel-{iter}-{{{{iter}}}}"),
                }),
                |_ctx, _| {
                    vec![PrimeStep {
                        inject: Vec::new(),
                        tool: "tracedecay_context_scout_claim",
                        args: json!({
                            "address": "{{scout_address}}",
                            "window": "on_request",
                            "idempotency_key": "bench-context-scout-cancel-claim-{{iter}}",
                        }),
                        capture: &[("digpath:claim:work", "scout_work")],
                    }]
                },
            )
        }),
    });
}

pub(crate) fn verify_context_scout_fixture(
    tool: &str,
    args: &Value,
    response: &Value,
) -> Option<Result<(), String>> {
    if !tool.starts_with("tracedecay_context_scout_") {
        return None;
    }
    Some((|| {
        let payload = response
            .pointer("/value/outcome/value/payload")
            .or_else(|| response.pointer("/outcome/value/payload"))
            .ok_or("Context Scout response omitted its canonical payload")?;
        match tool {
            "tracedecay_context_scout_status" => {
                let status: tracedecay_contracts::context_scout::ContextScoutStatusResultV1 =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                if status.configuration_revision == [0; 32] {
                    return Err(
                        "Context Scout status omitted its live configuration revision".into(),
                    );
                }
            }
            "tracedecay_context_scout_capability" => {
                let capability: tracedecay_contracts::context_scout::ContextScoutCapabilityResultV1 =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                if !capability.deterministic_available {
                    return Err("Context Scout capability did not advertise deterministic production support".into());
                }
            }
            "tracedecay_context_scout_budget" => {
                let budget: tracedecay_contracts::context_scout::ContextScoutBudgetResultV1 =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                if budget.limits.max_candidates == 0 || budget.limits.max_evidence == 0 {
                    return Err("Context Scout budget returned zero production limits".into());
                }
            }
            "tracedecay_context_scout_recent" => {
                let recent: tracedecay_contracts::context_scout::ContextScoutRecentResultV1 =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                if recent.configuration_revision == [0; 32] || recent.observed_at.0 <= 0 {
                    return Err("Context Scout recent state lacked live authority evidence".into());
                }
            }
            "tracedecay_context_scout_explain" => {
                let explain: tracedecay_contracts::context_scout::ContextScoutExplanationResultV1 =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                if explain.status.configuration_revision == [0; 32]
                    || explain.recent.configuration_revision == [0; 32]
                {
                    return Err("Context Scout explanation lacked live authority evidence".into());
                }
            }
            "tracedecay_context_scout_claim" => {
                let claim: tracedecay_contracts::context_scout::ContextScoutClaimResultV1 =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                let tracedecay_contracts::context_scout::ContextScoutClaimResultV1::Claimed {
                    claim,
                    suggestion,
                } = claim
                else {
                    return Err("Context Scout claim did not return a real lease handle".into());
                };
                if claim.work.generation == 0
                    || claim.lease_id == [0; 16]
                    || serde_json::to_value(claim.work.address).map_err(|error| error.to_string())? != args["address"]
                    || suggestion.delivery_window != tracedecay_contracts::context_scout::ContextScoutDeliveryWindowV1::OnRequest
                    || suggestion.work != claim.work
                    || !suggestion.suggestion_text.contains("feedback_bench_unused_probe")
                    || suggestion.evidence.anchor_ids.is_empty()
                {
                    return Err(
                        "Context Scout claim returned an invalid lease/suggestion identity".into(),
                    );
                }
            }
            "tracedecay_context_scout_delivery" => {
                let delivery: tracedecay_contracts::context_scout::ContextScoutDeliveryResultV1 =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                if delivery.outcome != tracedecay_contracts::context_scout::ContextScoutStoreOutcomeV1::Stored
                    || delivery.receipt.receipt_id == [0; 16]
                    || serde_json::to_value(delivery.receipt.envelope_id).map_err(|error| error.to_string())? != args["claim"]["envelope_id"]
                    || serde_json::to_value(delivery.receipt.delivered_at).map_err(|error| error.to_string())? != args["delivered_at"]
                    || delivery.receipt.outcome != tracedecay_contracts::context_scout::ContextScoutDeliveryOutcomeV1::Displayed
                {
                    return Err(
                        "Context Scout delivery did not return a durable receipt identity".into(),
                    );
                }
            }
            "tracedecay_context_scout_feedback" | "tracedecay_context_scout_cancel" => {
                let mutation: tracedecay_contracts::context_scout::ContextScoutMutationResultV1 =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                if mutation.outcome
                    != tracedecay_contracts::context_scout::ContextScoutStoreOutcomeV1::Stored
                {
                    return Err("Context Scout mutation did not commit its real transition".into());
                }
            }
            "tracedecay_context_scout_pause" | "tracedecay_context_scout_resume" => {
                let receipt: tracedecay_contracts::ConfigurationMutationReceipt =
                    serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
                if serde_json::to_value(&receipt.base_revision_id)
                    .map_err(|error| error.to_string())?
                    != args["expected_revision"]
                    || receipt.base_revision_id == receipt.result_revision_id
                {
                    return Err("Context Scout control did not commit the expected configuration transition".into());
                }
            }
            _ => return Err("unknown Context Scout verifier operation".into()),
        }
        Ok(())
    })())
}

/// Verify persisted state and release remaining fixture work through the same
/// public cancellation surface, keeping repeated native sessions bounded.
pub(crate) async fn finish_context_scout_fixture(
    harness: &tracedecay::daemon::ProductionProjectCompositionHarnessV1,
    project_root: &std::path::Path,
    tool: &str,
    args: Value,
    response: &Value,
    seeded_work: Option<&Value>,
) -> Result<(), String> {
    let payload = super::dig(response, "payload").ok_or("Scout result omitted payload")?;
    if matches!(
        tool,
        "tracedecay_context_scout_pause" | "tracedecay_context_scout_resume"
    ) {
        let status = crate::queries::call_json_tool(
            harness,
            project_root,
            "tracedecay_context_scout_status",
            json!({"address": args["address"], "format": "json"}),
        )
        .await?;
        let expected = if tool.ends_with("pause") {
            "paused"
        } else {
            "active"
        };
        if super::dig(&status, "state") != Some(&json!(expected)) {
            return Err("Scout control did not activate its committed state".into());
        }
    } else if matches!(
        tool,
        "tracedecay_context_scout_claim"
            | "tracedecay_context_scout_delivery"
            | "tracedecay_context_scout_feedback"
            | "tracedecay_context_scout_cancel"
    ) {
        let replay =
            crate::queries::call_json_tool(harness, project_root, tool, args.clone()).await?;
        if super::dig(&replay, "payload") != Some(payload) {
            return Err("Scout idempotent replay changed its canonical result".into());
        }
    }
    let recent = crate::queries::call_json_tool(
        harness,
        project_root,
        "tracedecay_context_scout_recent",
        json!({"address": args["address"], "limit": 8, "format": "json"}),
    )
    .await?;
    let recent: tracedecay_contracts::context_scout::ContextScoutRecentResultV1 =
        serde_json::from_value(
            super::dig(&recent, "payload")
                .cloned()
                .ok_or("Scout recent omitted payload")?,
        )
        .map_err(|error| error.to_string())?;
    if tool == "tracedecay_context_scout_delivery"
        && !recent.deliveries.iter().any(|entry| {
            serde_json::to_value(&entry.receipt).ok().as_ref() == payload.get("receipt")
        })
    {
        return Err("Scout delivery receipt was not persisted".into());
    }
    if tool == "tracedecay_context_scout_feedback"
        && !recent.deliveries.iter().any(|entry| {
            serde_json::to_value(&entry.receipt).ok().as_ref() == args.get("receipt")
                && serde_json::to_value(entry.feedback).ok().as_ref() == args.get("feedback")
        })
    {
        return Err("Scout feedback was not persisted against its exact delivery receipt".into());
    }
    if tool == "tracedecay_context_scout_cancel"
        && recent
            .pending
            .iter()
            .any(|entry| serde_json::to_value(entry.work).ok().as_ref() == args.get("work"))
    {
        return Err("Scout cancellation left its exact work pending".into());
    }
    let mut work = recent
        .pending
        .iter()
        .map(|entry| serde_json::to_value(entry.work).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    if matches!(
        tool,
        "tracedecay_context_scout_pause" | "tracedecay_context_scout_resume"
    ) && let Some(seeded) = seeded_work
        && !work.contains(seeded)
    {
        work.push(seeded.clone());
    }
    for work in work {
        let cancel_args = json!({"address": args["address"], "work": work,
            "idempotency_key": format!("bench-context-scout-cleanup-{}", super::now_micros()), "format": "json"});
        let cancelled = crate::queries::call_json_tool(
            harness,
            project_root,
            "tracedecay_context_scout_cancel",
            cancel_args.clone(),
        )
        .await?;
        verify_context_scout_fixture("tracedecay_context_scout_cancel", &cancel_args, &cancelled)
            .ok_or("Scout cleanup has no verifier")??;
    }
    Ok(())
}

fn fixture_git(root: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "fixture Git check failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| error.to_string())
}

fn fixture_scopes(ctx: &QueryContext) -> Result<Vec<(ResolvedScope, std::path::PathBuf)>, String> {
    let native = ctx
        .seeds
        .native
        .as_ref()
        .ok_or("native fixture scope is absent")?;
    let source_root =
        super::native_linked_root(&ctx.project_root).ok_or("linked fixture root is absent")?;
    [
        (&native.snapshot_body["destination"], &ctx.project_root),
        (&native.snapshot_body["source"], &source_root),
    ]
    .into_iter()
    .map(|(value, path)| {
        serde_json::from_value(value.clone())
            .map(|scope| (scope, path.clone()))
            .map_err(|error| error.to_string())
    })
    .collect()
}

fn verify_worktree_fixture(
    ctx: &QueryContext,
    tool: &str,
    args: &Value,
    response: &Value,
) -> Result<(), String> {
    let payload = response
        .pointer("/value/outcome/value/payload")
        .or_else(|| response.pointer("/outcome/value/payload"))
        .ok_or("worktree result omitted its canonical payload")?;
    let result: NativeWorktreeSurfaceResultV1 =
        serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
    let scopes = fixture_scopes(ctx)?;
    let (source, source_root) = &scopes[1];
    let target: NativeWorktreeTargetV1 =
        serde_json::from_value(args["target"].clone()).map_err(|error| error.to_string())?;
    if target.project_id() != &source.project_id
        || target.repository_id() != &source.repository_id
        || (tool != "tracedecay_worktree_inventory"
            && target.worktree_id() != Some(&source.worktree_id))
    {
        return Err(
            "worktree request did not select the fixture's exact registered identity".into(),
        );
    }
    let removed = tool == "tracedecay_worktree_cleanup_remove";
    let listed = fixture_git(&ctx.project_root, &["worktree", "list", "--porcelain"])?
        .lines()
        .any(|line| line.strip_prefix("worktree ") == source_root.to_str());
    if source_root.is_dir() == removed || listed == removed {
        return Err(
            "worktree result disagrees with the linked directory and Git registration".into(),
        );
    }
    fixture_git(
        &ctx.project_root,
        &["rev-parse", "--verify", "refs/heads/bench-native-source"],
    )?;
    fixture_git(&ctx.project_root, &["rev-parse", "--verify", "HEAD"])?;
    match (tool, result) {
        (
            "tracedecay_worktree_inventory",
            NativeWorktreeSurfaceResultV1::Inventory(WorktreeInventoryOutcomeV1::Snapshot(
                snapshot,
            )),
        ) => {
            if snapshot.coverage != WorktreeCoverageV1::Complete
                || snapshot.entries.len() != scopes.len()
                || serde_json::to_value((
                    &snapshot.scope_set_id,
                    snapshot.scope_set_revision,
                    &snapshot.scope_set_digest,
                ))
                .map_err(|error| error.to_string())?
                    != json!([
                        args["scope_set_id"],
                        args["scope_set_revision"],
                        args["scope_set_digest"]
                    ])
            {
                return Err(
                    "worktree inventory did not cover the exact claimed fixture scope set".into(),
                );
            }
            for (scope, root) in &scopes {
                let entries: Vec<_> = snapshot
                    .entries
                    .iter()
                    .filter(|entry| entry.worktree_id.as_ref() == Some(&scope.worktree_id))
                    .collect();
                if entries.len() != 1 {
                    return Err("inventory omitted or duplicated a fixture worktree".into());
                }
                let entry = entries[0];
                let expected_kind = if root == &ctx.project_root {
                    WorktreeKindV1::Main
                } else {
                    WorktreeKindV1::Linked
                };
                if entry.target.worktree_id() != Some(&scope.worktree_id)
                    || entry.reference != scope.reference
                    || entry.presence != WorktreePresenceV1::Present
                    || entry.kind != Some(expected_kind)
                    || entry.head.as_ref().map(|head| head.as_str())
                        != Some(fixture_git(root, &["rev-parse", "HEAD"])?.as_str())
                {
                    return Err(
                        "inventory identity, kind, reference or HEAD disagrees with native Git"
                            .into(),
                    );
                }
            }
        }
        (
            "tracedecay_worktree_cleanup_inspect",
            NativeWorktreeSurfaceResultV1::Inspection(WorktreeInspectionOutcomeV1::Inspection(
                inspection,
            )),
        ) => {
            if inspection.target != target
                || inspection.worktree_id != source.worktree_id
                || inspection.reference != source.reference
                || !inspection.removal_eligible()
                || inspection.head.as_ref().map(|head| head.as_str())
                    != Some(fixture_git(source_root, &["rev-parse", "HEAD"])?.as_str())
                || inspection.inspection_digest
                    != worktree_inspection_digest(&inspection).map_err(|error| error.to_string())?
                || !fixture_git(source_root, &["status", "--porcelain"])?.is_empty()
            {
                return Err(
                    "cleanup inspection did not seal the eligible, clean, exact native worktree"
                        .into(),
                );
            }
        }
        (
            "tracedecay_worktree_cleanup_confirm",
            NativeWorktreeSurfaceResultV1::Confirmation(WorktreeConfirmationOutcomeV1::Confirmed(
                confirmation,
            )),
        ) => {
            if confirmation.target != target
                || args["inspection_digest"].as_str()
                    != Some(confirmation.inspection_digest.as_str())
                || confirmation.confirmation_digest
                    != worktree_confirmation_digest(
                        &target,
                        &confirmation.inspection_digest,
                        confirmation.confirmed_at,
                    )
                    .map_err(|error| error.to_string())?
            {
                return Err(
                    "cleanup confirmation did not bind the requested inspection and exact target"
                        .into(),
                );
            }
        }
        (
            "tracedecay_worktree_cleanup_reconcile",
            NativeWorktreeSurfaceResultV1::Reconciliation(
                WorktreeCleanupReconciliationV1::StillPresent,
            ),
        ) => {}
        (
            "tracedecay_worktree_cleanup_remove",
            NativeWorktreeSurfaceResultV1::Removal(WorktreeCleanupRemovalV1::Removed {
                confirmation_digest,
                ..
            }),
        ) => {
            if args["confirmation_digest"].as_str() != Some(confirmation_digest.as_str()) {
                return Err("cleanup removal did not settle its exact confirmation".into());
            }
        }
        _ => {
            return Err(
                "worktree journey returned a refusal, replay, or unexpected operation result"
                    .into(),
            );
        }
    }
    Ok(())
}

fn verify_multi_root_fixture(
    ctx: &QueryContext,
    tool: &str,
    args: &Value,
    response: &Value,
) -> Result<(), String> {
    let payload = response
        .pointer("/value/application/outcome/value/payload")
        .or_else(|| response.pointer("/application/outcome/value/payload"))
        .ok_or("multi-root result omitted its canonical application payload")?;
    let scopes = fixture_scopes(ctx)?;
    if tool == "tracedecay_multi_root_execute" {
        let page: MultiRootQueryPageV1<Value> =
            serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
        if serde_json::to_value((
            &page.scope_set_id,
            page.scope_set_revision,
            &page.scope_set_digest,
        ))
        .map_err(|error| error.to_string())?
            != json!([
                args["scope_set_id"],
                args["scope_set_revision"],
                args["scope_set_digest"]
            ])
            || page.roots.len() != scopes.len()
            || page.continuation.is_some()
        {
            return Err(
                "multi-root execute did not return the complete claimed fixture page".into(),
            );
        }
        let mut expected_aggregate = Vec::new();
        for root in &page.roots {
            let (_, path) = scopes
                .iter()
                .find(|(scope, _)| scope.scope_digest == root.scope_digest)
                .ok_or("multi-root execute returned an unrequested root identity")?;
            if page
                .roots
                .iter()
                .filter(|candidate| candidate.scope_digest == root.scope_digest)
                .count()
                != 1
            {
                return Err("multi-root execute duplicated a root".into());
            }
            let ScopeOutcome::Exact(values) = &root.outcome else {
                return Err(
                    "multi-root fixture query was not exact for every enrolled root".into(),
                );
            };
            if values.len() != 1 {
                return Err("multi-root execute omitted a root's symbol page".into());
            }
            let symbols: CodeQueryPage<SymbolPrimitiveRecord> =
                serde_json::from_value(values[0].clone()).map_err(|error| error.to_string())?;
            if symbols.next_cursor.is_some()
                || symbols.items.len() != 1
                || symbols.items[0].name != "fixture_catalog_total"
                || symbols.items[0].file != "src/lib.rs"
                || !std::fs::read_to_string(path.join("src/lib.rs"))
                    .map_err(|error| error.to_string())?
                    .contains("pub fn fixture_catalog_total(quantities: &[usize]) -> usize")
            {
                return Err(
                    "multi-root query did not find the literal fixture function in each exact root"
                        .into(),
                );
            }
            expected_aggregate.extend(values.iter().cloned());
        }
        if page.aggregate != ScopeOutcome::Exact(expected_aggregate) {
            return Err("multi-root aggregate disagrees with its exact root results".into());
        }
        return Ok(());
    }
    let scope_set: AuthorizedScopeSet =
        if tool == "tracedecay_multi_root_scope_set_compare_and_swap" {
            let result: MultiRootScopeSetCasResultV1 =
                serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
            if result.status != MultiRootScopeSetCasStatusV1::Applied
                || !args["expected_revision"].is_null()
            {
                return Err("scope-set CAS did not create its fresh fixture identity".into());
            }
            result
                .scope_set
                .ok_or("scope-set CAS omitted its committed set")?
        } else {
            serde_json::from_value::<Option<AuthorizedScopeSet>>(payload.clone())
                .map_err(|error| error.to_string())?
                .ok_or("scope-set read did not recover its durable set")?
        };
    let creating = tool == "tracedecay_multi_root_scope_set_compare_and_swap";
    let expected = if creating { &scopes[..1] } else { &scopes[..] };
    if args["scope_set_id"].as_str() != Some(scope_set.scope_set_id().as_str())
        || scope_set.roots().len() != expected.len()
        || (creating
            && serde_json::to_value(scope_set.revision()).map_err(|error| error.to_string())?
                != json!(1))
        || (!creating
            && (ctx.seeds.scope_set_digest.as_deref() != Some(scope_set.digest().as_str())
                || serde_json::to_value(scope_set.revision())
                    .map_err(|error| error.to_string())?
                    != json!(ctx.seeds.scope_set_revision)))
    {
        return Err(
            "scope-set result changed its exact identity, revision, digest, or roots".into(),
        );
    }
    for (scope, path) in expected {
        if !scope_set.roots().iter().any(|root| {
            root.scope() == scope
                && root.locator().is_some_and(|locator| {
                    locator.project_id == scope.project_id && locator.canonical_root == *path
                })
        }) {
            return Err(
                "scope-set result did not retain the exact registered fixture scope and path"
                    .into(),
            );
        }
    }
    if creating && args["roots"] != mr_roots(ctx) {
        return Err(
            "scope-set CAS fixture request did not select the primary registered root".into(),
        );
    }
    Ok(())
}

pub(crate) fn verify_native_fixture(
    ctx: &QueryContext,
    tool: &str,
    args: &Value,
    response: &Value,
    prepared_snapshot: Option<&Value>,
) -> Option<Result<(), String>> {
    if tool.starts_with("tracedecay_worktree_") {
        return Some(verify_worktree_fixture(ctx, tool, args, response));
    }
    if tool.starts_with("tracedecay_multi_root_") {
        return Some(verify_multi_root_fixture(ctx, tool, args, response));
    }
    if !matches!(
        tool,
        "tracedecay_stack_snapshot"
            | "tracedecay_preflight_native_integration"
            | "tracedecay_approve_native_integration"
            | "tracedecay_apply_native_integration"
            | "tracedecay_native_integration_status"
            | "tracedecay_cancel_native_integration"
    ) {
        return None;
    }
    Some((|| {
        let payload = response
            .pointer("/value/outcome/value/payload")
            .or_else(|| response.pointer("/outcome/value/payload"))
            .ok_or("native result omitted its canonical payload")?;
        let result: NativeIntegrationSurfaceResultV1 =
            serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
        match (tool, result) {
            (
                "tracedecay_stack_snapshot",
                NativeIntegrationSurfaceResultV1::StackSnapshot(snapshot),
            ) => {
                let mut body = args.clone();
                body.as_object_mut()
                    .ok_or("snapshot request is not an object")?
                    .remove("format");
                let request: NativeIntegrationStackSnapshotSurfaceRequest =
                    serde_json::from_value(body).map_err(|error| error.to_string())?;
                if snapshot.sealed_snapshot != request.seal().map_err(|error| error.to_string())?
                    || snapshot.selection.source_ref.as_str() != "refs/heads/bench-native-source"
                    || snapshot.selection.destination_ref.as_str() != "refs/heads/bench"
                {
                    return Err("stack snapshot did not seal the exact current fixture edge".into());
                }
            }
            (
                "tracedecay_preflight_native_integration",
                NativeIntegrationSurfaceResultV1::Preview(preview),
            ) => {
                if preview.selection.source_ref.as_str() != "refs/heads/bench-native-source"
                    || preview.selection.destination_ref.as_str() != "refs/heads/bench"
                    || serde_json::to_value(&preview.disposition)
                        .map_err(|error| error.to_string())?["state"]
                        != "already_integrated"
                {
                    return Err(
                        "preflight did not recognize the fixture's already-integrated source"
                            .into(),
                    );
                }
            }
            (
                "tracedecay_approve_native_integration",
                NativeIntegrationSurfaceResultV1::Approval(approval),
            ) => {
                if args["preview_id"].as_str() != Some(approval.preview_id.as_str())
                    || args["preview_digest"].as_str() != Some(approval.preview_digest.as_str())
                    || approval.expires_at <= approval.issued_at
                {
                    return Err("approval did not bind the prepared native preview".into());
                }
            }
            (
                "tracedecay_apply_native_integration",
                NativeIntegrationSurfaceResultV1::Receipt(receipt),
            ) => {
                if receipt.status.transaction_id.as_str()
                    != args["transaction_id"]
                        .as_str()
                        .ok_or("apply transaction is absent")?
                    || receipt.terminal_outcome != NativeIntegrationTerminalOutcomeV1::Committed
                {
                    return Err("native apply did not commit the approved transaction".into());
                }
                let git = |args: &[&str]| -> Result<String, String> {
                    let output = std::process::Command::new("git")
                        .arg("-C")
                        .arg(&ctx.project_root)
                        .args(args)
                        .output()
                        .map_err(|error| error.to_string())?;
                    if !output.status.success() {
                        return Err(format!(
                            "native receipt Git check failed: {}",
                            String::from_utf8_lossy(&output.stderr)
                        ));
                    }
                    String::from_utf8(output.stdout)
                        .map(|text| text.trim().to_owned())
                        .map_err(|error| error.to_string())
                };
                if git(&["rev-parse", "HEAD"])? != receipt.final_ref_tip
                    || git(&["rev-parse", "HEAD^{tree}"])? != receipt.final_tree
                    || !std::fs::read_to_string(ctx.project_root.join("src/native_integration_source.rs"))
                        .map_err(|error| error.to_string())?.starts_with("pub const BENCH_NATIVE_SOURCE: &str = \"source\";\n// benchmark integration update\n") {
                    return Err("native committed receipt did not match the actual Git tip/tree and integrated source bytes".into());
                }
                let mut body = prepared_snapshot
                    .ok_or("native apply omitted its prepared snapshot")?
                    .clone();
                body.as_object_mut()
                    .ok_or("native prepared snapshot is not an object")?
                    .remove("format");
                let request: NativeIntegrationStackSnapshotSurfaceRequest =
                    serde_json::from_value(body).map_err(|error| error.to_string())?;
                let NativeIntegrationSelectionDeclarationV1::DeclaredStackEdge { nodes, .. } =
                    request.selection
                else {
                    return Err("native fixture has no declared edge".into());
                };
                for node in nodes {
                    git(&["merge-base", "--is-ancestor", node.tip.as_str(), "HEAD"])?;
                }
            }
            (
                "tracedecay_native_integration_status",
                NativeIntegrationSurfaceResultV1::Status(status),
            ) => {
                if args["transaction_id"].as_str() != Some(status.transaction_id.as_str())
                    || status.phase != NativeIntegrationPhaseV1::Terminal
                    || status.terminal_outcome
                        != Some(NativeIntegrationTerminalOutcomeV1::Committed)
                {
                    return Err(
                        "native status did not return the committed fixture transaction".into(),
                    );
                }
            }
            (
                "tracedecay_cancel_native_integration",
                NativeIntegrationSurfaceResultV1::Cancellation(
                    NativeIntegrationCancellationProjectionV1::AlreadyTerminal(
                        NativeIntegrationTerminalOutcomeV1::Committed,
                    ),
                ),
            ) => {}
            _ => return Err("native fixture returned an unexpected lifecycle outcome".into()),
        }
        Ok(())
    })())
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    for (tool, label, extra) in [
        ("tracedecay_status", "status", json!({})),
        (
            "tracedecay_storage_status",
            "storage_status",
            json!({"include_details": true}),
        ),
        ("tracedecay_health", "health", json!({})),
        ("tracedecay_health_read", "health_read", json!({})),
        ("tracedecay_runtime", "runtime", json!({})),
        ("tracedecay_configuration_list", "config_list", json!({})),
        (
            "tracedecay_configuration_observed_state",
            "config_observed",
            json!({}),
        ),
        (
            "tracedecay_configuration_audit",
            "config_audit",
            json!({"limit": 25}),
        ),
        ("tracedecay_active_project", "active_project", json!({})),
        ("tracedecay_remote_status", "remote_status", json!({})),
        ("tracedecay_analytics", "analytics", json!({})),
        ("tracedecay_dashboard", "dashboard", json!({"port": 0})),
        (
            "tracedecay_observatory_read",
            "observatory_read",
            json!({"window_days": 7}),
        ),
        (
            "tracedecay_skill_list",
            "skill_list",
            json!({"include_body": false}),
        ),
        (
            "tracedecay_hermes_skill_bridge",
            "hermes_bridge",
            json!({"include_skill_bodies": true}),
        ),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|i| {
                let mut args = extra.clone();
                if tool == "tracedecay_status" && i % 2 == 0 {
                    args["include_staleness"] = json!(true);
                    args["include_storage_health"] = json!(true);
                }
                if tool == "tracedecay_configuration_observed_state" {
                    Query::prepared_read(label, tool, args, super::configuration_read_prime)
                } else {
                    rq(tool, label, args)
                }
            }),
        });
    }
    if let Some(handle) = ctx.seeds.retrieve_handle.clone() {
        out.push(ToolGroup {
            tool: "tracedecay_retrieve",
            queries: vec![rq(
                "tracedecay_retrieve",
                "retrieve",
                json!({
                    "handle": handle,
                    "offset": 0,
                    "max_chars": 4096,
                }),
            )],
        });
    }
    if let Some(skill_id) = ctx.seeds.skill_id.clone() {
        out.push(ToolGroup {
            tool: "tracedecay_skill_view",
            queries: five(|_i| {
                rq(
                    "tracedecay_skill_view",
                    "skill_view",
                    json!({
                        "id": skill_id,
                        "include_support_files": false,
                    }),
                )
            }),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_configuration_get",
        queries: five(|_i| {
            Query::prepared_read(
                "config_get",
                "tracedecay_configuration_get",
                json!({"key": ctx.seeds.config_key.clone().unwrap_or_else(|| "diagnostics.prewarm.v1".into())}),
                super::configuration_read_prime,
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_automation_run_list",
        queries: five(|i| {
            rq(
                "tracedecay_automation_run_list",
                "automation_runs",
                json!({"limit": 10 + i as u32}),
            )
        }),
    });
    if let Some(native) = ctx.seeds.native.clone() {
        out.push(ToolGroup {
            tool: "tracedecay_stack_snapshot",
            queries: five(|i| {
                let mut query = Query::prepared_read(
                    "native_snapshot",
                    "tracedecay_stack_snapshot",
                    json!({}),
                    i,
                    no_primes,
                );
                query.args = json!("{{native_snapshot_body}}");
                query
            }),
        });

        if let Some(transaction_id) = native.transaction_id {
            out.push(ToolGroup {
                tool: "tracedecay_native_integration_status",
                queries: five(|_i| {
                    rq(
                        "tracedecay_native_integration_status",
                        "native_status",
                        json!({"transaction_id": transaction_id}),
                    )
                }),
            });
            out.push(ToolGroup {
                tool: "tracedecay_cancel_native_integration",
                queries: five(|_i| {
                    rq(
                        "tracedecay_cancel_native_integration",
                        "native_cancel",
                        json!({"transaction_id": transaction_id}),
                    )
                }),
            });
        }
        out.push(ToolGroup {
            tool: "tracedecay_preflight_native_integration",
            queries: five(|_i| {
                eq(
                    "tracedecay_preflight_native_integration",
                    "native_preflight",
                    json!({
                        "snapshot": "{{snapshot}}",
                        "format": "json",
                    }),
                    |ctx, _iter| vec![native_snapshot_step(ctx)],
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_approve_native_integration",
            queries: five(|_i| {
                eq(
                    "tracedecay_approve_native_integration",
                    "native_approve",
                    json!({
                        "preview_id": "{{ni_preview_id}}",
                        "preview_digest": "{{ni_preview_digest}}",
                    }),
                    |ctx, _iter| vec![native_snapshot_step(ctx), native_preflight_step()],
                )
            }),
        });
        // Expansion needs the pending dependency; the following native apply
        // then integrates it before the linked-worktree cleanup journey.
        out.push(ToolGroup {
            tool: "tracedecay_github_stack_signal_expand",
            queries: five(|_i| {
                eqn(
                    "tracedecay_github_stack_signal_expand",
                    "github_stack_signal_expand_replay",
                    json!("{{native_signal_args}}"),
                    no_primes,
                )
            }),
        });
        out.push(ToolGroup {
            tool: "tracedecay_apply_native_integration",
            queries: five(|_i| {
                eq(
                    "tracedecay_apply_native_integration",
                    "native_apply",
                    json!({
                        "preview_id": "{{ni_preview_id}}",
                        "preview_digest": "{{ni_preview_digest}}",
                        "approval_id": "{{ni_approval_id}}",
                        "approval_digest": "{{ni_approval_digest}}",
                        "transaction_id": "{{ni_transaction_id}}",
                    }),
                    |ctx, _iter| {
                        vec![
                            native_snapshot_step(ctx),
                            native_preflight_step(),
                            native_approve_step(),
                        ]
                    },
                )
            }),
        });
        // Preflight is read-only and does not mint a transaction. Status and
        // cancel are included only when the seed above captured a real apply
        // transaction receipt.
    }
    // worktree inventory + cleanup chain all claim the seeded scope_set
    // identity; cleanup tools target the registered worktree (repository
    // targets are only valid for inventory), and inspect seeds digests for
    // confirm/reconcile/remove.
    let wt_claim = |extra: Value| {
        let mut claim = json!({
            "scope_set_id": "{{wt_scope_set_id}}",
            "scope_set_revision": "{{wt_scope_set_revision}}",
            "scope_set_digest": "{{wt_scope_set_digest}}",
            "target": "{{wt_target}}",
        });
        if let (Some(extra), Some(fields)) = (extra.as_object(), claim.as_object_mut()) {
            fields.extend(extra.clone());
        }
        claim
    };
    out.push(ToolGroup {
        tool: "tracedecay_worktree_inventory",
        queries: five(|_i| {
            rq(
                "tracedecay_worktree_inventory",
                "wt_inventory",
                json!({
                    "scope_set_id": ctx.seeds.scope_set_id,
                    "scope_set_revision": ctx.seeds.scope_set_revision,
                    "scope_set_digest": ctx.seeds.scope_set_digest,
                    "target": {
                        "kind": "repository",
                        "project_id": ctx
                            .seeds
                            .project_id
                            .clone()
                            .unwrap_or_else(|| "td-bench-missing".into()),
                        "repository_id": ctx
                            .seeds
                            .repository_id
                            .clone()
                            .unwrap_or_else(|| "td-bench-missing".into()),
                    }
                }),
            )
        }),
    });
    let mut cleanup_removal = None;
    if ctx.seeds.cleanup_worktree_id.is_some() {
        out.push(ToolGroup {
            tool: "tracedecay_worktree_cleanup_inspect",
            queries: five(|_i| {
                eq(
                    "tracedecay_worktree_cleanup_inspect",
                    "wt_cleanup_inspect",
                    wt_claim(json!({})),
                    |_ctx, _iter| Vec::new(),
                )
            }),
        });
        for (tool, label) in [
            ("tracedecay_worktree_cleanup_confirm", "wt_cleanup_confirm"),
            (
                "tracedecay_worktree_cleanup_reconcile",
                "wt_cleanup_reconcile",
            ),
            ("tracedecay_worktree_cleanup_remove", "wt_cleanup_remove"),
        ] {
            let group = ToolGroup {
                tool,
                queries: (0..if tool == "tracedecay_worktree_cleanup_remove" {
                    1
                } else {
                    5
                })
                    .map(|_i| {
                        let args = match tool {
                            "tracedecay_worktree_cleanup_confirm" => json!({
                                "inspection_digest": "{{wt_inspection_digest}}",
                            }),
                            "tracedecay_worktree_cleanup_reconcile" => json!({
                                "confirmation_digest": "{{wt_confirmation_digest}}",
                            }),
                            _ => json!({
                                "inspection_digest": "{{wt_inspection_digest}}",
                                "confirmation_digest": "{{wt_confirmation_digest}}",
                                "confirmed_at": "{{wt_confirmed_at}}",
                            }),
                        };
                        let mut query = eq(
                            tool,
                            label,
                            wt_claim(args),
                            if tool == "tracedecay_worktree_cleanup_confirm" {
                                wt_inspection_prime
                            } else {
                                wt_cleanup_primes
                            },
                        );
                        if tool == "tracedecay_worktree_cleanup_remove"
                            && let QueryKind::Effect { repeatable, .. } = &mut query.kind
                        {
                            *repeatable = false;
                        }
                        query
                    })
                    .collect(),
            };
            if tool == "tracedecay_worktree_cleanup_remove" {
                cleanup_removal = Some(group);
            } else {
                out.push(group);
            }
        }
    }
    // multi_root: scope_set_read hits the CAS-minted set; execute re-runs one
    // federated symbol read over both exact enrolled fixture roots.
    let scope_set_id = ctx
        .seeds
        .scope_set_id
        .clone()
        .unwrap_or_else(|| "td-bench-missing".into());
    out.push(ToolGroup {
        tool: "tracedecay_multi_root_scope_set_read",
        queries: five(|_i| {
            rq(
                "tracedecay_multi_root_scope_set_read",
                "mr_scope_read",
                json!({"scope_set_id": scope_set_id}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_multi_root_execute",
        queries: five(|_i| {
            rq(
                "tracedecay_multi_root_execute",
                "mr_execute",
                json!({
                    "scope_set_id": scope_set_id,
                    "scope_set_revision": ctx.seeds.scope_set_revision.unwrap_or(1),
                    "scope_set_digest": ctx
                        .seeds
                        .scope_set_digest
                        .clone()
                        .unwrap_or_else(|| "td-bench-missing".into()),
                    "operation": {
                        "kind": "query",
                        "request": {
                            "operation": "code_symbol_search",
                            "request": {
                                "query": if crate::repos::small_fixture_enabled() {
                                    "fixture_catalog_total".to_owned()
                                } else {
                                    ctx.function_qnames.first().cloned().unwrap_or_else(|| "fit".into())
                                },
                                "lazy_index_ignored_dependencies": true,
                                "scope": {"path_prefix": null},
                                "meta": {
                                    "projection": "summary",
                                    "order": "relevance",
                                    "cursor": null,
                                },
                            },
                        },
                    },
                    "page": 0,
                }),
            )
        }),
    });
    // CAS re-commit of a fresh scope-set identity: expected_revision=null
    // mints, so the timed call is the real CAS create lane.
    out.push(ToolGroup {
        tool: "tracedecay_multi_root_scope_set_compare_and_swap",
        queries: five(|i| {
            eq(
                "tracedecay_multi_root_scope_set_compare_and_swap",
                "mr_cas",
                json!({
                    "scope_set_id": format!("scope-set.bench.q{i}.{{{{iter}}}}"),
                    "expected_revision": null,
                    "roots": mr_roots(ctx),
                }),
                |_ctx, _iter| Vec::new(),
            )
        }),
    });

    if let Some(removal) = cleanup_removal {
        out.push(removal);
    }

    let configs: Vec<_> = ctx
        .files
        .iter()
        .filter_map(|file| {
            let path = file["path"].as_str()?;
            let key = match std::path::Path::new(path).file_name()?.to_str()? {
                "Cargo.toml" => "package.name",
                "package.json" => "name",
                "pyproject.toml" => "project.name",
                _ => return None,
            };
            ctx.project_root.join(path).is_file().then_some((path, key))
        })
        .collect();
    if !configs.is_empty() {
        out.push(ToolGroup {
            tool: "tracedecay_config",
            queries: five(|i| {
                let (path, key) = configs[i % configs.len()];
                rq(
                    "tracedecay_config",
                    "config",
                    json!({"key": key, "path": path}),
                )
            }),
        });
    }
    for (tool, label, args) in [
        ("tracedecay_project_context", "project_ctx", json!({})),
        (
            "tracedecay_project_list",
            "project_list",
            json!({"limit": 10}),
        ),
        (
            "tracedecay_project_search",
            "project_search",
            json!({"query": "runtime-fixture", "limit": 10}),
        ),
        (
            "tracedecay_memory_status",
            "memory_status",
            json!({"memory_scope": "project"}),
        ),
    ] {
        out.push(ToolGroup {
            tool,
            queries: five(|_i| rq(tool, label, args.clone())),
        });
    }
    out.push(ToolGroup {
        tool: "tracedecay_health_delta",
        queries: five(|i| {
            rq(
                "tracedecay_health_delta",
                "health_delta",
                json!({"meta": {
                    "order": "temporal_descending",
                    "page": {"page_size": 5 + i as u32, "cursor": null},
                    "projection": "summary",
                    "temporal": {"kind": "current"},
                }}),
            )
        }),
    });
    if let Some(run_id) = ctx.seeds.automation_run_id.clone() {
        for (tool, label) in [
            ("tracedecay_automation_run_view", "automation_view"),
            (
                "tracedecay_automation_run_artifact_view",
                "automation_artifact",
            ),
        ] {
            out.push(ToolGroup {
                tool,
                queries: five(|_i| {
                    let mut args = json!({"run_id": run_id});
                    if tool == "tracedecay_automation_run_artifact_view" {
                        args["kind"] = json!("traces");
                        // Application protocol extraction removes format before
                        // validation; rq preserves the JSON response request.
                        rq(tool, label, args)
                    } else {
                        rq(tool, label, args)
                    }
                }),
            });
        }
    }
}

fn wt_inspection_prime(ctx: &QueryContext, iter: u64) -> Vec<PrimeStep> {
    let mut steps = wt_cleanup_primes(ctx, iter);
    steps.truncate(1);
    steps
}

/// Prime chain for the worktree cleanup lifecycle: inspect mints the
/// inspection digest, confirm mints the confirmation digest + timestamp the
/// downstream reconcile/remove calls consume.
fn wt_cleanup_primes(_ctx: &QueryContext, _iter: u64) -> Vec<PrimeStep> {
    let mut claim = json!({
        "scope_set_id": "{{wt_scope_set_id}}",
        "scope_set_revision": "{{wt_scope_set_revision}}",
        "scope_set_digest": "{{wt_scope_set_digest}}",
        "target": "{{wt_target}}",
    });
    // Application surfaces default to markdown; the prime chain parses JSON.
    claim["format"] = json!("json");
    let mut confirm_args = claim.clone();
    confirm_args["inspection_digest"] = json!("{{wt_inspection_digest}}");
    vec![
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_worktree_cleanup_inspect",
            args: claim,
            capture: &[("dig:inspection_digest", "wt_inspection_digest")],
        },
        PrimeStep {
            inject: Vec::new(),
            tool: "tracedecay_worktree_cleanup_confirm",
            args: confirm_args,
            capture: &[
                ("dig:confirmation_digest", "wt_confirmation_digest"),
                ("digany:confirmed_at,confirmed_at_micros", "wt_confirmed_at"),
            ],
        },
    ]
}

/// One registered {project_id, root} pair, matching the seeded scope set.
fn mr_roots(ctx: &QueryContext) -> serde_json::Value {
    let project_id = ctx
        .seeds
        .project_id
        .clone()
        .unwrap_or_else(|| "td-bench-missing".into());
    let root = ctx.project_root.to_string_lossy().to_string();
    json!([{"project_id": project_id, "root": root}])
}

fn native_snapshot_step(_ctx: &QueryContext) -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_stack_snapshot",
        args: json!("{{native_snapshot_body}}"),
        capture: &[("dig:sealed_snapshot", "snapshot")],
    }
}

fn native_preflight_step() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_preflight_native_integration",
        args: json!({"snapshot": "{{snapshot}}", "format": "json"}),
        capture: &[
            ("dig:preview_id", "ni_preview_id"),
            ("dig:preview_digest", "ni_preview_digest"),
        ],
    }
}

fn native_approve_step() -> PrimeStep {
    PrimeStep {
        inject: Vec::new(),
        tool: "tracedecay_approve_native_integration",
        args: json!({
            "preview_id": "{{ni_preview_id}}",
            "preview_digest": "{{ni_preview_digest}}",
            "format": "json",
        }),
        capture: &[
            ("dig:approval_id", "ni_approval_id"),
            ("dig:approval_digest", "ni_approval_digest"),
            ("dig:transaction_id", "ni_transaction_id"),
        ],
    }
}
