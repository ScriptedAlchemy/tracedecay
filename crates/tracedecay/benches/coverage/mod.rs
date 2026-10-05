//! Stateful benchmark fixtures use canonical producer receipts. Setup failures
//! remain visible in the finite audit rather than becoming success timings.

mod admin;
mod admin_fixture;
mod code;
mod effects;
mod git;
mod graph;
mod memory;
mod session;
mod work;
pub(crate) use admin_fixture::verify_admin_fixture;

pub(crate) use admin::{
    finish_context_scout_fixture, verify_context_scout_fixture, verify_github_stack_signal_fixture,
    verify_native_fixture,
};
#[cfg(unix)]
pub(crate) use code::prepare_source_reconciliation;
pub(crate) use code::{verify_code_fixture_effect, verify_code_fixture_read};
pub(crate) use effects::{configuration_effect_key, verify_configuration_effect};
pub(crate) use git::verify_git_fixture;
pub(crate) use graph::verify_fixture_read;
pub(crate) use memory::{
    configuration_read_prime, follow_fact_curate_completion, verify_fact_curate_admission,
    verify_feedback_fixture, verify_memory_fixture,
};
pub(crate) use session::verify_session_fixture;
pub(crate) use work::{verify_work_fixture, verify_work_read_fixture};

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_application::stack_coordinator::{StackSignalDraftV1, StackSignalV1};
use tracedecay_automation_runtime::automation::backend::AgentTaskKind;
use tracedecay_automation_runtime::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillReadError, ManagedSkillSource,
    ManagedSupportFile, ManagedSupportFileExt, create_managed_skill, default_managed_skill_targets,
    load_managed_skill,
};
use tracedecay_automation_runtime::automation::run_ledger::{
    AutomationRunArtifactKind, AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    append_run_record, write_run_artifact,
};
use tracedecay_contracts::git::{
    GitHubStackSignalEvidenceRefV1, GitHubStackSignalExpandSurfaceResultV1,
    GitHubStackSignalExpandUnavailableV1, GitHubStackSignalNativePreviewV1,
    GitHubStackSignalNativeSourceV1,
};
use tracedecay_contracts::{
    MultiRootScopeSetCasRequestV1, NativeIntegrationSelectionBindingV1,
    NativeIntegrationSelectionDeclarationV1, NativeIntegrationStackSnapshotSurfaceRequest,
    NativeIntegrationSurfaceResultV1, RegisteredRootSelectorV1, ResolvedScope,
};
use tracedecay_domain::configuration::{
    ConfigurationValueV1, WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1,
    WorkExecutableCapabilityV1,
};
use tracedecay_domain::{
    BranchStackEdgeV1, BranchStackId, BranchStackNodeV1, BranchStackRevisionId, CommitId, GitOidV1,
    ManifestDigest, ManifestDigestHasher, NativeIntegrationDirectionV1,
    NativeIntegrationPreviewDispositionV1, ProjectId, RefId, RepositoryId, StackNodeId,
    StackSignalKindV1, WorkApprovalPolicy, WorkContentLocationClassV1, WorkEffortClassV1,
    WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits, WorkFallbackTopology,
    WorkFilesystemPolicy, WorkOrdinalBandV1, WorkProviderBackendV1, WorkRouteCandidateV1,
    WorkRouteExecutionProfileV1, WorkSandboxPolicy, WorktreeId, WorktreeInventoryEpoch,
    WorktreeInventorySnapshotId,
};

use crate::queries::{
    NativeSeeds, Query, QueryContext, Seeds, ToolGroup, WorkSeeds, call_json_tool,
};

/// Deterministic codex rollout session id seeded per repo before `open`.
pub(crate) fn bench_session_id(repo_name: &str) -> String {
    format!("td-bench-{repo_name}")
}

const NATIVE_LINKED_BRANCH: &str = "bench-native-source";

fn native_linked_root(project_root: &Path) -> Option<std::path::PathBuf> {
    project_root
        .parent()
        .map(|parent| parent.join(".tracedecay-bench-native-source"))
}

async fn prepare_native_linked_worktree(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    base_branch: Option<&str>,
    project_id: &str,
    seeds: &mut Seeds,
) -> Option<std::path::PathBuf> {
    if !crate::repos::small_fixture_enabled() {
        seeds.skipped.push(
            "native_integration_*: linked source worktree requires the isolated small fixture"
                .to_owned(),
        );
        return None;
    }
    let Some(base_branch) = base_branch else {
        seeds
            .skipped
            .push("native_integration_*: no base branch for linked source worktree".to_owned());
        return None;
    };
    let Some(linked_root) = native_linked_root(project_root) else {
        seeds
            .skipped
            .push("native_integration_*: fixture has no staging parent".to_owned());
        return None;
    };
    let was_existing = linked_root.exists();
    let linked_root = match std::fs::canonicalize(&linked_root) {
        Ok(existing) => {
            let listed = std::process::Command::new("git")
                .args(["-C"])
                .arg(project_root)
                .args(["worktree", "list", "--porcelain"])
                .output();
            let valid = listed
                .ok()
                .filter(|output| output.status.success())
                .map(|output| {
                    let listing = String::from_utf8_lossy(&output.stdout);
                    listing.split("\n\n").any(|entry| {
                        entry
                            .lines()
                            .any(|line| line == format!("worktree {}", existing.to_string_lossy()))
                            && entry.lines().any(|line| {
                                line == format!("branch refs/heads/{NATIVE_LINKED_BRANCH}")
                            })
                    })
                })
                .unwrap_or(false);
            if !valid {
                seeds.skipped.push(
                    "native_integration_*: existing linked source path is not the owned worktree"
                        .to_owned(),
                );
                return None;
            }
            existing
        }
        Err(_) => {
            let output = std::process::Command::new("git")
                .args(["-C"])
                .arg(project_root)
                .args(["worktree", "add", "-b", NATIVE_LINKED_BRANCH])
                .arg(&linked_root)
                .arg(base_branch)
                .output();
            let Ok(output) = output else {
                seeds.skipped.push(
                    "native_integration_*: git could not create linked source worktree".to_owned(),
                );
                return None;
            };
            if !output.status.success() {
                seeds.skipped.push(format!(
                    "native_integration_*: linked source worktree creation failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
                return None;
            }
            linked_root
        }
    };
    // The fixture destination is the checked-out `bench` branch. Start the
    // enrolled source from `bench-base`, then give it one real source-only
    // commit so the native adapter has a meaningful dependency to integrate.
    // Reused runs already have a distinct source tip and must not append more
    // commits to the disposable branch.
    let source_tip = std::process::Command::new("git")
        .args(["-C"])
        .arg(&linked_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    let base_tip = std::process::Command::new("git")
        .args(["-C"])
        .arg(project_root)
        .args(["rev-parse", base_branch])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    if source_tip.is_some() && source_tip == base_tip {
        let marker = linked_root.join("src/native_integration_source.rs");
        if let Err(error) = std::fs::write(
            &marker,
            "pub const BENCH_NATIVE_SOURCE: &str = \"source\";\n",
        ) {
            seeds.skipped.push(format!(
                "native_integration_*: source commit file setup failed: {error}"
            ));
            return None;
        }
        for args in [
            vec!["add", "--", "src/native_integration_source.rs"],
            vec!["commit", "-m", "bench native source"],
        ] {
            let output = std::process::Command::new("git")
                .args(["-C"])
                .arg(&linked_root)
                .args(args)
                .output();
            if !output.as_ref().is_ok_and(|output| output.status.success()) {
                let detail = output
                    .ok()
                    .map(|output| String::from_utf8_lossy(&output.stderr).trim().to_owned())
                    .unwrap_or_else(|| "git command failed to start".to_owned());
                seeds.skipped.push(format!(
                    "native_integration_*: source commit setup failed: {detail}"
                ));
                return None;
            }
        }
    }
    if !was_existing {
        let revision = match harness.configuration_revision(project_root).await {
            Ok(revision) => revision,
            Err(error) => {
                seeds.skipped.push(format!(
                    "native_integration_*: linked-worktree opt-in revision unavailable: {error}"
                ));
                return None;
            }
        };
        if let Err(error) = call_json_tool(
            harness,
            project_root,
            "tracedecay_configuration_set",
            json!({
                "layer": {"kind": "project", "project_id": project_id},
                "key": tracedecay_domain::configuration::SYNC_WATCH_LINKED_WORKTREES_SETTING_KEY,
                "value": {"kind": "boolean", "value": true},
                "expected_revision": revision,
                "idempotency_key": format!("configuration.idempotency.native-bench.{}", now_micros()),
                "format": "json",
            }),
        )
        .await
        {
            seeds.skipped.push(format!(
                "native_integration_*: linked-worktree opt-in failed: {error}"
            ));
            return None;
        }
        seeds.additional_project_roots.push(linked_root.clone());
        seeds.needs_reopen_for_native_worktree = true;
        seeds.skipped.push(
            "native_integration_*: linked route created; deferring enrollment until composition reopen"
                .to_owned(),
        );
        return Some(linked_root);
    }
    if let Err(error) = harness
        .track_worktree_branch(project_root, &linked_root, NATIVE_LINKED_BRANCH)
        .await
    {
        seeds.skipped.push(format!(
            "native_integration_*: linked source worktree enrollment failed: {error}"
        ));
        return None;
    }
    Some(linked_root)
}

/// Writes one provider rollout per repo into the composition's isolated
/// transcript home so session/LCM surfaces have real ingested sessions.
/// Must run after clones exist and before the harness opens: the profile
/// home under `isolation_root` is the only home the composition reads.
pub(crate) fn seed_transcripts(isolation_root: &Path, repos: &[(String, std::path::PathBuf)]) {
    let Some(home) = ProductionProjectCompositionHarnessV1::transcript_source_home(isolation_root)
    else {
        eprintln!("[bench] no transcript source home; session/lcm groups will skip");
        return;
    };
    for (name, dir) in repos {
        if crate::repos::small_fixture_enabled() {
            let skill = home.join(".hermes/skills/fixture-inventory/SKILL.md");
            let seeded = skill.parent().ok_or_else(|| std::io::Error::other("Hermes fixture has no parent"))
                .and_then(std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&skill, "---\nname: fixture-inventory\ndescription: Inspect the isolated runtime fixture.\n---\nRead the fixture catalog before editing it.\n"));
            if let Err(error) = seeded {
                eprintln!("[bench] Hermes skill fixture: {error}");
            }
        }
        if crate::repos::small_fixture_enabled()
            && let Err(error) =
                session::seed_host_workflow(&home, dir, session::WORKFLOW_SESSION_ID)
        {
            eprintln!("[bench] workflow transcript fixture: {error}");
        }
        let session = bench_session_id(name);
        let rollout_dir = home.join(".codex/sessions/2026/09/12");
        if let Err(e) = std::fs::create_dir_all(&rollout_dir) {
            eprintln!("[bench] transcript dir {}: {e}", rollout_dir.display());
            continue;
        }
        let path = rollout_dir.join(format!("rollout-2026-09-12T00-00-00-{session}.jsonl"));
        let lines = [
            json!({
                "timestamp": "2026-09-12T00:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": session,
                    "cwd": dir,
                    "model": "bench-fixture",
                },
            }),
            json!({
                "timestamp": "2026-09-12T00:00:01.000Z",
                "type": "event_msg",
                "payload": {"type": "user_message", "message": "bench sweep user message"},
            }),
            json!({
                "timestamp": "2026-09-12T00:00:02.000Z",
                "type": "event_msg",
                "payload": {
                    "type": "agent_message",
                    "message": "bench sweep assistant reply",
                },
            }),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        if let Err(e) = std::fs::write(&path, format!("{lines}\n")) {
            eprintln!("[bench] transcript {}: {e}", path.display());
        }
    }
}

fn opt_err(
    skipped: &mut Vec<String>,
    what: &'static str,
    outcome: Result<Value, String>,
) -> Option<Value> {
    match outcome {
        Ok(v) => Some(v),
        Err(e) => {
            skipped.push(format!("{what}: {e}"));
            None
        }
    }
}

/// Pull `field` as a string out of a json-tool payload, searching any depth:
/// application envelopes bury results under outcome.value.*.
pub(crate) fn dig<'a>(value: &'a Value, field: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => {
            if let Some(v) = map.get(field) {
                return Some(v);
            }
            map.values().find_map(|v| dig(v, field))
        }
        Value::Array(items) => items.iter().find_map(|v| dig(v, field)),
        _ => None,
    }
}

pub(crate) fn dig_str<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    dig(value, field).and_then(Value::as_str)
}

/// The session refresh status result is an evidence envelope.  Do not use
/// `dig_str` here: the envelope itself has an `outcome` object, while the
/// refresh state lives in the canonical surface payload.
fn session_refresh_outcome(value: &Value) -> Option<&str> {
    value
        .pointer("/outcome/value/payload/outcome")
        .and_then(Value::as_str)
}

fn bounded_json(value: &Value) -> String {
    let rendered = value.to_string();
    let mut preview = rendered.chars().take(512).collect::<String>();
    if rendered.chars().count() > 512 {
        preview.push_str("…");
    }
    preview
}

/// Dot-path lookup with numeric segments, for PrimeStep captures:
/// `structuredContent.facts.0.fact_id`.
pub fn extract_path(root: &Value, path: &str) -> Option<Value> {
    let mut cur = root;
    for seg in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur.clone())
}

/// Read the identity from the canonical multi-root CAS result payload.  A
/// recursive lookup is unsafe here because the evidence envelope also carries
/// policy and scope digests with different authority.
fn scope_set_cas_identity(value: &Value) -> Option<(i64, String)> {
    let scope_set = value.pointer("/application/outcome/value/payload/scope_set")?;
    Some((
        scope_set.get("revision")?.as_i64()?,
        scope_set.get("digest")?.as_str()?.to_owned(),
    ))
}

/// Read the identity from the canonical multi-root read result payload.
fn scope_set_read_identity(value: &Value) -> Option<(i64, String)> {
    let payload = value.pointer("/application/outcome/value/payload")?;
    Some((
        payload.get("revision")?.as_i64()?,
        payload.get("digest")?.as_str()?.to_owned(),
    ))
}

/// Resolve one PrimeStep capture spec against a step response:
/// - `"a.b.0.c"` — explicit dot path
/// - `"dig:field"` — first deep match of the field name anywhere
/// - `"deep_array:hunk_digests"` — all `digest` values inside objects that
///   carry a `hunk` member (the git_hunks wire shape)
pub fn extract_token(root: &Value, spec: &str) -> Option<Value> {
    if let Some(field) = spec.strip_prefix("dig:") {
        return dig(root, field).cloned();
    }
    if let Some(fields) = spec.strip_prefix("digany:") {
        return fields
            .split(',')
            .find_map(|field| dig(root, field).cloned());
    }
    if let Some(rest) = spec.strip_prefix("digpath:") {
        // "digpath:container:a.b" — first deep `container` object, then a dot
        // path inside it (disambiguates repeated field names like `id`).
        let (outer, inner) = rest.split_once(':')?;
        return dig(root, outer).and_then(|o| extract_path(o, inner));
    }
    if spec == "deep_array:hunk_digests" {
        let mut out = Vec::new();
        collect_hunk_digests(root, &mut out);
        return Some(Value::Array(out.into_iter().map(Value::from).collect()));
    }
    if spec == "transform:rename_acceptance" {
        // The immutable rename capability minted by rename_symbol's dry run.
        return Some(json!({
            "preview_id": dig(root, "preview_id")?,
            "preview_digest": dig(root, "preview_digest")?,
            "plan_digest": dig(root, "plan_digest")?,
            "graph_revision": dig(root, "graph_revision")?,
            "repository_revision": dig(root, "repository_revision")
                .cloned()
                .unwrap_or(Value::Null),
        }));
    }
    if spec == "transform:toggle_boolean" {
        let current = dig(root, "effective_value")?;
        if current.get("kind").and_then(Value::as_str) != Some("boolean") {
            return None;
        }
        return Some(json!({"kind": "boolean", "value": !current.get("value")?.as_bool()?}));
    }
    if spec == "transform:trim_review_allowed" {
        // Alternate the allowed review modes: pop when >1, add back when
        // down to one — protected_apply commits each change, so a one-way
        // pop would drain the set below its non-empty invariant.
        // The get response carries the tagged ConfigurationValueV1 envelope;
        // ProtectedChange wants the bare policy payload.
        let mut policy = dig(root, "effective_value")?.clone();
        if let Some(inner) = policy.get("value").cloned() {
            policy = inner;
        }
        if let Some(allowed) = policy
            .get_mut("review_topology")
            .and_then(|rt| rt.get_mut("allowed"))
            .and_then(Value::as_array_mut)
        {
            if allowed.len() > 1 {
                allowed.pop();
            } else {
                for kind in [
                    "no_review",
                    "independent_review",
                    "standard_pull_requests",
                    "github_stacked_pull_requests",
                ] {
                    if !allowed.iter().any(|v| v.as_str() == Some(kind)) {
                        allowed.push(json!(kind));
                        break;
                    }
                }
            }
        }
        return Some(policy);
    }
    extract_path(root, spec)
}

/// Replace `{{token}}` occurrences inside string leaves; a string that IS a
/// single `{{token}}` splices the typed captured value instead.
pub fn substitute_tokens(value: &mut Value, tokens: &HashMap<String, Value>) {
    match value {
        Value::String(s) => {
            if let Some(inner) = s
                .strip_prefix("{{")
                .and_then(|inner| inner.strip_suffix("}}"))
                && let Some(v) = tokens.get(inner)
            {
                *value = v.clone();
                return;
            }
            let mut out = s.clone();
            for (k, v) in tokens {
                let needle = format!("{{{{{k}}}}}");
                if out.contains(&needle) {
                    let repl = match v {
                        Value::String(inner) => inner.clone(),
                        other => other.to_string(),
                    };
                    out = out.replace(&needle, &repl);
                }
            }
            *s = out;
        }
        Value::Array(a) => a.iter_mut().for_each(|v| substitute_tokens(v, tokens)),
        Value::Object(m) => m.values_mut().for_each(|v| substitute_tokens(v, tokens)),
        _ => {}
    }
}

/// Fact entity names that only exist because the bench seeded them.
fn seeded_fact_terms(iter: u64) -> (String, String, String) {
    (
        format!("bench-alpha-{iter}"),
        format!("bench-beta-{iter}"),
        format!("bench-gamma-{iter}"),
    )
}

/// `tracedecay_fact_store_add` args for a seeded pair (used both by seed_all
/// and by the per-iteration primes of the fact effect tools).
pub(crate) fn fact_add_args(content: &str, entities: &[String], trust: f64) -> Value {
    json!({
        "content": content,
        "category": "tool",
        "entities": entities,
        "trust": trust,
        "source_label": "bench",
        "format": "json",
    })
}

pub(crate) async fn seed_all(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    function_qnames: &[String],
    // The context builder's one successful `tracedecay_files` listing —
    // re-fetching it here could drop every file probe on a transient error.
    files: &[Value],
) -> Seeds {
    let mut seeds = Seeds {
        sample_files: files
            .iter()
            .filter_map(|f| f.get("path").and_then(Value::as_str))
            .take(64)
            .map(str::to_owned)
            .collect(),
        ..Seeds::default()
    };
    let call = |tool: &'static str, args: Value| call_transient(harness, project_root, tool, args);

    // ── identity ─────────────────────────────────────────────────────────
    if let Some(v) = opt_err(
        &mut seeds.skipped,
        "active_project",
        call("tracedecay_active_project", json!({"format":"json"})).await,
    ) {
        seeds.project_id = dig_str(&v, "project_id").map(str::to_owned);
        seeds.repository_id = dig_str(&v, "repository_id").map(str::to_owned);
        seeds.branch = dig_str(&v, "current_branch").map(str::to_owned);
    }

    // ── configuration revisions (scalar set/unset pair + topology policy) ──
    seed_configuration(harness, project_root, &mut seeds).await;

    // ── facts: one connected pair for the fact read surface ──────────────
    seed_facts(harness, project_root, &mut seeds).await;

    // ── code-query node identities (navigation consumers) ────────────────
    for name in function_qnames.iter().take(8) {
        let short = name.rsplit("::").next().unwrap_or(name);
        match call(
            "tracedecay_code_symbol_search",
            json!({
                "query": short,
                "scope": {"path_prefix": null},
                "lazy_index_ignored_dependencies": false,
                "meta": {"projection": "summary", "order": "relevance"},
                "format": "json",
            }),
        )
        .await
        {
            Ok(v) => collect_node_ids(&v, &mut seeds.code_node_ids),
            Err(e) => seeds.skipped.push(format!("code_symbol_search: {e}")),
        }
        if seeds.code_node_ids.len() >= 16 {
            break;
        }
    }

    // ── renameable symbol: apply refuses ambiguous/hazardous renames, so
    // probe candidates until a preview binds a node whose own dry-run apply
    // also succeeds with zero blocked dispositions. ────────────────────────
    for node_id in seeds.code_node_ids.iter().rev().take(24) {
        let Ok(v) = call(
            "tracedecay_rename_preview",
            json!({
                "node_id": node_id,
                "new_name": "bench_probe_candidate",
                "format": "json",
            }),
        )
        .await
        else {
            continue;
        };
        let Some(node) = dig(&v, "node").cloned() else {
            continue;
        };
        let (Some(id), Some(qname), Some(kind), Some(file), Some(name)) = (
            node.get("id").and_then(Value::as_str),
            node.get("qualified_name").and_then(Value::as_str),
            node.get("kind").and_then(Value::as_str),
            node.get("file").and_then(Value::as_str),
            node.get("name").and_then(Value::as_str),
        ) else {
            continue;
        };
        let Ok(dry) = call(
            "tracedecay_rename_symbol",
            json!({
                "node_id": id,
                "qualified_name": qname,
                "kind": kind,
                "file": file,
                "old_name": name,
                "new_name": "bench_probe_candidate",
                "dry_run": true,
                "format": "json",
            }),
        )
        .await
        else {
            continue;
        };
        let blocked = dig(&dry, "blocked").and_then(Value::as_i64).unwrap_or(0);
        if blocked == 0 && dig(&dry, "expected_state").is_some() {
            seeds.rename_node = Some(node);
            break;
        }
    }
    if seeds.rename_node.is_none() {
        seeds
            .skipped
            .push("rename_apply: no corpus node yielded an unblocked rename dry-run".to_owned());
    }

    // ── source-edit apply targets: each op needs a symbol whose dry-run is
    // both unblocked and small (a truncated preview is a handle envelope
    // without an inline expected_state). Probe qnames until one works. ────
    let dest_file = seeds
        .sample_files
        .first()
        .cloned()
        .unwrap_or_else(|| "src/lib.rs".to_owned());
    let probes: [(&str, &str, Value); 3] = [
        (
            "replace",
            "tracedecay_replace_symbol",
            json!({"new_source": "pub fn bench_target() -> i32 { 42 }"}),
        ),
        (
            "insert",
            "tracedecay_insert_at_symbol",
            json!({"content": "// bench insert marker", "position": "after"}),
        ),
        (
            "move",
            "tracedecay_move_symbol",
            json!({"dest_file": dest_file, "update_references": false}),
        ),
    ];
    // `largest` ranks biggest-first: the head symbols produce dry-run
    // previews past the response bound (handle envelopes, no inline
    // expected_state). Probe from the smallest end.
    for (key, tool, extra) in probes {
        let mut found = false;
        for qname in function_qnames.iter().rev().take(24) {
            let mut args = json!({"symbol": qname, "dry_run": true, "format": "json"});
            if let Value::Object(map) = &mut args {
                for (k, v) in extra.as_object().into_iter().flatten() {
                    map.insert(k.clone(), v.clone());
                }
            }
            match call(tool, args).await {
                Ok(v) if dig(&v, "expected_state").is_some() => {
                    match key {
                        "replace" => seeds.replace_target = Some(qname.clone()),
                        "insert" => seeds.insert_target = Some(qname.clone()),
                        _ => seeds.move_target = Some(qname.clone()),
                    }
                    found = true;
                    break;
                }
                _ => continue,
            }
        }
        if !found {
            seeds.skipped.push(format!(
                "{key}_apply: no small corpus symbol produced an inline expected_state"
            ));
        }
    }

    // ── lcm/session: the rollout was written before open; poll until the
    // ingester admits it (bounded) ─────────────────────────────────────────
    seed_sessions(harness, project_root, &mut seeds).await;

    // ── session refresh handle ───────────────────────────────────────────
    seed_refresh(harness, project_root, &mut seeds).await;

    seed_automation_run(harness, project_root, &mut seeds).await;

    seeds.head_commit = std::process::Command::new("git")
        .args(["-C"])
        .arg(project_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    seeds.parent_commit = std::process::Command::new("git")
        .args(["-C"])
        .arg(project_root)
        .args(["rev-parse", "HEAD~1"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());

    // The pinned bench clone sits detached at FETCH_HEAD with no branch refs;
    // branch-scoped surfaces need one real ref, so mint bench refs before the
    // authorized scope set is persisted.
    if seeds.branch.is_none()
        && std::process::Command::new("git")
            .args(["-C"])
            .arg(project_root)
            .args(["branch", "-f", "bench", "HEAD"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    {
        seeds.branch = Some("bench".to_owned());
    }
    if seeds.parent_commit.is_some()
        && std::process::Command::new("git")
            .args(["-C"])
            .arg(project_root)
            .args(["branch", "-f", "bench-base", "HEAD~1"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    {
        seeds.base_branch = Some("bench-base".to_owned());
    }

    // ── multi_root scope_set: CAS-commit the project root once so the
    // federated read lane has a persisted identity (id/revision/digest). ──
    if let (Some(project_id), Some(root)) = (
        seeds.project_id.clone(),
        project_root.to_str().map(str::to_owned),
    ) {
        let base_branch = seeds.base_branch.clone();
        let linked_root = prepare_native_linked_worktree(
            harness,
            project_root,
            base_branch.as_deref(),
            &project_id,
            &mut seeds,
        )
        .await;
        let mut scope_roots = vec![json!({"project_id": project_id, "root": root})];
        if let Some(linked_root) = linked_root {
            scope_roots.push(json!({
                "project_id": project_id,
                "root": linked_root,
            }));
        }
        // Bind this enrollment's current exact roots to a fresh scope set.
        let scope_set_id = format!("scope-set.bench.{project_id}.{}", now_micros());
        // The wire contract requires canonical ordering even when both roots
        // share one project. Let its constructor order the exact selectors.
        let request = (|| -> Result<Value, String> {
            let roots =
                serde_json::from_value::<Vec<RegisteredRootSelectorV1>>(Value::Array(scope_roots))
                    .map_err(|error| error.to_string())?;
            let request = MultiRootScopeSetCasRequestV1::new(
                tracedecay_domain::ScopeSetId::new(scope_set_id.clone())
                    .map_err(|error| error.to_string())?,
                None,
                roots,
            )
            .map_err(|error| error.to_string())?;
            serde_json::to_value(request).map_err(|error| error.to_string())
        })();
        let result = match request {
            Ok(request) => call("tracedecay_multi_root_scope_set_compare_and_swap", request).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(v) => {
                seeds.scope_set_id = Some(scope_set_id.clone());
                if let Some((revision, digest)) = scope_set_cas_identity(&v) {
                    seeds.scope_set_revision = Some(revision);
                    seeds.scope_set_digest = Some(digest);
                }
                if seeds.scope_set_revision.is_none() || seeds.scope_set_digest.is_none() {
                    seeds
                        .skipped
                        .push("multi_root_execute: CAS omitted revision/digest".to_owned());
                }
            }
            Err(error) => seeds
                .skipped
                .push(format!("multi_root_scope_set_compare_and_swap: {error}")),
        }
    }

    // Some read surfaces mount their application authority lazily; probe the
    // ones seen cold in runs so an unmountable lane degrades to a named skip
    // rather than a timed panic.
    let probe_path = files
        .first()
        .and_then(|f| f.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("LICENSE.txt");
    for (tool, args) in [(
        "tracedecay_git_blame",
        json!({"path": probe_path, "format": "json"}),
    )] {
        // The probe outlives the typed `retryable` window a cold authority
        // asks for (~60s), so a skip only names authorities that truly never
        // mount under this composition.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match call_json_tool(harness, project_root, tool, args.clone()).await {
                Ok(_) => break,
                Err(e) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                    let _ = e;
                }
                Err(e) => {
                    seeds.unavailable_tools.insert(tool.to_owned());
                    seeds.skipped.push(format!("{tool}: {e}"));
                    break;
                }
            }
        }
    }

    // ── native integration: inventory → stack_snapshot → preflight/approve ─
    // A newly-created linked route is not mounted until the parent reopens the
    // composition with its additional root. Defer all native reads to that
    // clean second pass rather than recording scheduler-unavailable noise.
    if !seeds.needs_reopen_for_native_worktree {
        seed_native_integration(harness, project_root, &mut seeds).await;
    }

    // ── work/workflow lifecycle: one disposable task graph + workflow run ──
    seed_work(harness, project_root, &mut seeds).await;

    // ── affected-tests result read: find a changed path that maps to tests ─
    seed_affected_tests(harness, project_root, files, &mut seeds).await;

    if crate::repos::small_fixture_enabled()
        && !seeds.needs_reopen_for_native_worktree
        && !seeds.needs_reopen_for_provider
        && let Err(error) = memory::seed_feedback_fixture(harness, project_root, &mut seeds).await
    {
        seeds
            .skipped
            .push(format!("feedback compiler fixture: {error}"));
    }

    if crate::repos::small_fixture_enabled()
        && !seeds.needs_reopen_for_native_worktree
        && !seeds.needs_reopen_for_provider
    {
        if let Err(error) = session::prepare_host_workflow(harness, project_root).await {
            seeds
                .skipped
                .push(format!("host workflow fixture: {error}"));
        }
        match seed_context_scout_address(harness, project_root).await {
            Ok(address) => seeds.context_scout_address = Some(address),
            Err(error) => seeds
                .skipped
                .push(format!("context scout hook fixture: {error}")),
        }
    }

    seed_skill(harness, &mut seeds).await;

    // ── response handle: a fat search truncates → reversible handle ──────
    seed_retrieve_handle(harness, project_root, &mut seeds).await;

    // Native inventory and blame require the committed worktree state. Dirty
    // the tracked fixture only after those reads, then mint the hunk input
    // from the exact working-tree diff.
    if !seeds.needs_reopen_for_native_worktree {
        seed_dirty_worktree(project_root, files, &mut seeds).await;
    }

    if !seeds.needs_reopen_for_native_worktree {
        seed_git_preview(harness, project_root, &mut seeds).await;
    }

    if !seeds.skipped.is_empty() {
        eprintln!(
            "[bench] seed ledger ({}): {}",
            seeds.skipped.len(),
            seeds.skipped.join(" | ")
        );
    }
    seeds
}

fn collect_node_ids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(m) => {
            if let Some(Value::String(id)) = m.get("node_id") {
                crate::queries::push_unique(out, id);
            }
            for v in m.values() {
                collect_node_ids(v, out);
            }
        }
        Value::Array(a) => {
            for v in a {
                collect_node_ids(v, out);
            }
        }
        _ => {}
    }
}

async fn seed_configuration(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    let Some(project_id) = seeds.project_id.clone() else {
        seeds
            .skipped
            .push("configuration_*: no project_id (active_project failed)".to_owned());
        return;
    };
    let call = |tool: &'static str, args: Value| call_transient(harness, project_root, tool, args);

    // Attempt receipts persist across runs and bind their authority; keys
    // must be run-unique.
    let nonce = format!("{}", now_micros());
    let scalar_key = "diagnostics.prewarm.v1";
    // CAS conflicts on the configuration registry are retryable; re-read the
    // revision and retry the whole set/unset roundtrip a few times.
    let mut seeded_revision = None;
    let mut toggled = Value::Null;
    let mut last_err = String::new();
    for _attempt in 0..3 {
        let Ok(current) = call(
            "tracedecay_configuration_get",
            json!({"key": scalar_key, "format": "json"}),
        )
        .await
        else {
            break;
        };
        // dig() hits the defaulted candidate's revision_id first; the CAS
        // anchor must be the effective head at outcome.value.payload.revision_id.
        let revision = extract_path(&current, "outcome.value.payload.revision_id")
            .and_then(|v| v.as_str().map(str::to_owned));
        let effective = dig(&current, "effective_value").cloned();
        let (Some(revision), Some(effective)) = (revision, effective) else {
            seeds
                .skipped
                .push("configuration_*: scalar key omitted revision/effective_value".to_owned());
            return;
        };
        toggled = json!({
            "kind": "boolean",
            "value": !effective.get("value").and_then(Value::as_bool).unwrap_or(false),
        });
        match call(
            "tracedecay_configuration_set",
            json!({
                "layer": {"kind": "project", "project_id": project_id},
                "key": scalar_key,
                "value": toggled,
                "expected_revision": revision,
                "idempotency_key": format!("bench-cfg-seed-{nonce}"),
                "format": "json",
            }),
        )
        .await
        {
            Ok(seeded) => {
                seeded_revision = dig_str(&seeded, "result_revision_id").map(str::to_owned);
                break;
            }
            Err(e) => last_err = e,
        }
    }
    let Some(seeded_revision) = seeded_revision else {
        seeds.skipped.push(format!("configuration_set: {last_err}"));
        return;
    };
    let restored = opt_err(
        &mut seeds.skipped,
        "configuration_unset",
        call(
            "tracedecay_configuration_unset",
            json!({
                "layer": {"kind": "project", "project_id": project_id},
                "key": scalar_key,
                "expected_revision": seeded_revision,
                "idempotency_key": format!("bench-cfg-seed-rollback-{nonce}"),
                "format": "json",
            }),
        )
        .await,
    );
    if let Some(restored) = restored {
        seeds.config_rollback_target = Some(seeded_revision.clone());
        seeds.config_revision = dig_str(&restored, "result_revision_id").map(str::to_owned);
        seeds.config_key = Some(scalar_key.to_owned());
        seeds.config_scalar = Some(toggled);
    }
    if let Ok(v) = call(
        "tracedecay_configuration_get",
        json!({"key": "work.topology_policy.v1", "format": "json"}),
    )
    .await
    {
        seeds.topology_policy = dig(&v, "effective_value")
            .and_then(|e| e.get("value"))
            .cloned()
            .or_else(|| dig(&v, "effective_value").cloned());
    }
    if seeds.topology_policy.is_none() {
        seeds
            .skipped
            .push("configuration_protected_*: work.topology_policy.v1 not readable".to_owned());
    }
}

async fn seed_facts(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    let (alpha, beta, gamma) = seeded_fact_terms(0);
    let call = |tool: &'static str, args: Value| call_transient(harness, project_root, tool, args);
    let first = call(
        "tracedecay_fact_store_add",
        fact_add_args(
            &format!("bench constellation {alpha} {beta}"),
            &[alpha.clone(), beta.clone()],
            0.8,
        ),
    )
    .await;
    let second = call(
        "tracedecay_fact_store_add",
        fact_add_args(
            &format!("bench constellation {beta} {gamma}"),
            &[beta.clone(), gamma.clone()],
            0.7,
        ),
    )
    .await;
    match (first, second) {
        (Ok(a), Ok(b)) => {
            let fid = a
                .pointer("/outcome/value/payload/result/fact/fact/fact_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let rid = b
                .pointer("/outcome/value/payload/result/fact/fact/fact_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            match (fid, rid) {
                (Some(f), Some(r)) => {
                    seeds.fact_pair = Some((
                        f,
                        r,
                        format!("bench constellation {alpha}"),
                        vec![alpha, beta],
                    ));
                }
                _ => seeds
                    .skipped
                    .push("fact_store_*: adds omitted fact_id".to_owned()),
            }
        }
        (Err(e), _) | (_, Err(e)) => {
            seeds.skipped.push(format!("fact_store_*: {e}"));
        }
    }
}

async fn seed_sessions(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    // The session id is deterministic per repo name; derive it from the
    // directory basename we seeded transcripts for.
    let name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");
    let session_id = bench_session_id(name);

    // Session ingest runs as bounded background work after mount; poll the
    // read surface until the rollout is admitted (bounded ~60s).
    let mut loaded = None;
    for _ in 0..30 {
        match call_json_tool(
            harness,
            project_root,
            "tracedecay_lcm_load_session",
            json!({
                "provider": "codex",
                "session_id": session_id,
                "limit": 5,
                "format": "json",
            }),
        )
        .await
        {
            Ok(v) => {
                loaded = Some(v);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(2000)).await,
        }
    }
    match loaded {
        Some(v) => {
            seeds.lcm_session = Some(session_id);
            // Canonical occurrence id: first message_id deep in the payload.
            seeds.lcm_message_id = dig_str(&v, "message_id").map(str::to_owned);
            if seeds.lcm_message_id.is_none() {
                seeds
                    .skipped
                    .push("lcm_expand: load_session returned no message_id".to_owned());
            }
        }
        None => {
            let probe = call_json_tool(
                harness,
                project_root,
                "tracedecay_lcm_describe",
                json!({"provider": "codex", "session_id": session_id, "format": "json"}),
            )
            .await;
            seeds.skipped.push(format!(
                "session/lcm reads: rollout for {session_id} never admitted; list_sessions={probe:?}"
            ));
        }
    }
}

async fn seed_refresh(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    let Some(session_id) = seeds.lcm_session.clone() else {
        seeds
            .skipped
            .push("session_refresh_*: no ingested session".to_owned());
        return;
    };
    let selectors = json!({
        "scope": {"kind": "profile"},
        "session": {"id": session_id},
        "source": {"scope": "codex"},
        "target": {
            "temporal_mode": {"kind": "current"},
            "grain": "session",
            "frontier": {"observed_through": 0, "committed_through": 0},
        },
    });
    let mut args = selectors.clone();
    args["format"] = json!("json");
    match call_json_tool(
        harness,
        project_root,
        "tracedecay_session_refresh_begin",
        args,
    )
    .await
    {
        Ok(v) => match dig_str(&v, "handle").map(str::to_owned) {
            Some(h) if !h.is_empty() => {
                let operation_id = dig_str(&v, "operation_id").map(str::to_owned);
                let mut status_args = selectors.clone();
                status_args["handle"] = json!(h);
                status_args["format"] = json!("json");
                let mut complete = false;
                for _ in 0..60 {
                    match call_json_tool(
                        harness,
                        project_root,
                        "tracedecay_session_refresh_status",
                        status_args.clone(),
                    )
                    .await
                    {
                        Ok(status) => match session_refresh_outcome(&status) {
                            Some("complete") => {
                                complete = true;
                                break;
                            }
                            Some("running") => {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                            Some(outcome) => {
                                seeds.skipped.push(format!(
                                    "session_refresh_*: refresh ended before completion ({outcome}); payload={}",
                                    bounded_json(&status)
                                ));
                                break;
                            }
                            None => {
                                seeds.skipped.push(format!(
                                    "session_refresh_status: missing canonical outcome payload; response={}",
                                    bounded_json(&status)
                                ));
                                break;
                            }
                        },
                        Err(e) => {
                            seeds.skipped.push(format!("session_refresh_status: {e}"));
                            break;
                        }
                    }
                }
                if complete {
                    seeds.refresh_handle = Some(h);
                    seeds.refresh_operation_id = operation_id;
                    seeds.refresh_selectors = Some(selectors);
                } else {
                    seeds
                        .skipped
                        .push("session_refresh_*: refresh did not reach complete".to_owned());
                }
            }
            _ => seeds
                .skipped
                .push("session_refresh_*: begin returned no handle".to_owned()),
        },
        Err(e) => seeds.skipped.push(format!("session_refresh_*: {e}")),
    }
}

/// Dirty one tracked file early in seeding so the git watcher's refresh
/// cadence has the whole remaining seed window to observe it; the hunk poll
/// happens last (see `seed_git_preview`), giving the lane minutes, not
/// seconds, before the preview is declared unseedable.
async fn seed_dirty_worktree(project_root: &Path, files: &[Value], seeds: &mut Seeds) {
    let tracked: BTreeSet<String> = std::process::Command::new("git")
        .args(["-C"])
        .arg(project_root)
        .args(["ls-files"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let Some(rel) = files
        .iter()
        .filter_map(|f| f.get("path").and_then(Value::as_str))
        .find(|p| {
            !p.starts_with(crate::queries::SCRATCH_DIR)
                && tracked.contains(*p)
                && project_root.join(p).is_file()
                && matches!(
                    p.rsplit('.').next(),
                    Some("rs" | "py" | "c" | "h" | "cc" | "cpp" | "f" | "f90" | "toml")
                )
        })
        .map(str::to_owned)
    else {
        seeds
            .skipped
            .push("git_preview/apply: no tracked source file to dirty".to_owned());
        return;
    };
    let abs = project_root.join(&rel);
    let original = std::fs::read_to_string(&abs).unwrap_or_default();
    let marker = match rel.rsplit('.').next() {
        Some("py" | "toml") => "# bench hunk line",
        _ => "// bench hunk line",
    };
    if std::fs::write(&abs, format!("{original}\n{marker}\n")).is_err() {
        seeds
            .skipped
            .push(format!("git_preview/apply: cannot dirty {rel}"));
        return;
    }
    seeds.dirty_file = Some(rel);
}

/// Retain a changed path only after the real runner executes covering tests.
/// Feedback handles belong to advisory publications, not this runner.
async fn seed_affected_tests(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    files: &[Value],
    seeds: &mut Seeds,
) {
    let mut last_error = None;
    let mut runner_failure = false;
    let mut candidates: Vec<String> = files
        .iter()
        .filter_map(|f| f.get("path").and_then(Value::as_str))
        .filter(|p| p.contains("test"))
        .take(3)
        .map(str::to_owned)
        .collect();
    if crate::repos::small_fixture_enabled() {
        candidates.insert(0, "src/lib.rs".to_owned());
    }
    candidates.extend(
        files
            .iter()
            .filter_map(|f| f.get("path").and_then(Value::as_str))
            .take(2)
            .map(str::to_owned),
    );
    // The files surface may intentionally omit test-only targets while the
    // graph has already indexed them. Recover only tracked Rust test files;
    // this keeps the changed path canonical instead of inventing a test name.
    let has_test_candidate = candidates.iter().any(|path| path.contains("test"));
    if !has_test_candidate
        && let Ok(output) = std::process::Command::new("git")
            .args(["-C"])
            .arg(project_root)
            .args(["ls-files"])
            .output()
        && output.status.success()
    {
        candidates.extend(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|path| path.starts_with("tests/") && path.ends_with(".rs"))
                .take(3)
                .map(str::to_owned),
        );
    }
    for path in candidates {
        match call_json_tool(
            harness,
            project_root,
            "tracedecay_run_affected_tests",
            json!({
                "changed_paths": [path],
                "max_tests": 3,
                "timeout_secs": 20,
                "format": "json",
            }),
        )
        .await
        {
            Ok(v) => {
                let payload = v.pointer("/outcome/value/payload").unwrap_or(&v);
                let results = payload.get("results").and_then(Value::as_array);
                let ran_tests = results.is_some_and(|results| !results.is_empty());
                let fixture_passed = !crate::repos::small_fixture_enabled()
                    || (payload.get("passed") == Some(&json!(1))
                        && payload.get("failed") == Some(&json!(0))
                        && results.is_some_and(|results| {
                            results.iter().any(|result| {
                                result.get("test").and_then(Value::as_str)
                                    == Some("fixture_catalog_has_stable_total")
                                    && result.get("passed") == Some(&json!(true))
                            })
                        }));
                if ran_tests && fixture_passed {
                    seeds.test_results_path = Some(path);
                    return;
                }
                last_error = Some(format!(
                    "runner returned no verified covering test for {path}"
                ));
            }
            Err(error) => {
                runner_failure |= error.contains("cargo")
                    || error.contains("rustup")
                    || error.contains("toolchain");
                last_error = Some(error);
            }
        }
    }
    seeds.skipped.push(format!(
        "test_results: {}{}",
        if runner_failure {
            "covering test mapped but runner failed"
        } else {
            "no changed path maps to covering tests"
        },
        last_error
            .as_deref()
            .map(|error| format!(" ({error})"))
            .unwrap_or_default()
    ));
}

const BENCH_RETRIEVE_MARKER: &str = "bench-retrieve-payload-4a71c8";

/// Mint a reversible response handle through the production response-handle
/// store. This keeps the retrieve journey independent of corpus size while
/// preserving the same durable authority used by truncated MCP responses.
async fn seed_retrieve_handle(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    let server = match harness.server(project_root) {
        Ok(server) => server,
        Err(error) => {
            seeds
                .skipped
                .push(format!("retrieve: server unavailable: {error}"));
            return;
        }
    };
    let response_handle_root = server
        .cg()
        .await
        .store_layout()
        .response_handle_root
        .clone();
    let content = format!(
        "{{\"marker\":\"{BENCH_RETRIEVE_MARKER}\",\"items\":[\"real production response handle\"]}}"
    );
    match tracedecay_mcp::response_handles::store_response_handle(
        &response_handle_root,
        &content,
        tracedecay_runtime_core::tracedecay::current_timestamp(),
    ) {
        Ok(record) => seeds.retrieve_handle = Some(record.handle),
        Err(error) => seeds.skipped.push(format!("retrieve: {error}")),
    }
}

/// Poll the hunk lane at the end of seeding; the dirty file has been in the
/// working tree since `seed_dirty_worktree`, so the watcher has had the full
/// seed duration to observe it.
async fn seed_git_preview(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    if seeds.dirty_file.is_none() {
        return;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match call_json_tool(
            harness,
            project_root,
            "tracedecay_git_hunks",
            json!({"scope": "working_tree", "format": "json"}),
        )
        .await
        {
            Ok(v) => {
                seeds.preview_input_id = dig_str(&v, "preview_input_id").map(str::to_owned);
                let mut digests = Vec::new();
                collect_hunk_digests(&v, &mut digests);
                seeds.hunk_digests = digests;
                if seeds.preview_input_id.is_some() && !seeds.hunk_digests.is_empty() {
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    seeds.skipped.push(
                        "git_preview/apply: hunk producer returned no applicable hunks".to_owned(),
                    );
                    return;
                }
            }
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    seeds.skipped.push(format!("git_preview/apply: {e}"));
                    return;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Native-integration journey seed: inventory → stack_snapshot → preflight.
/// The groups replay the snapshot to mint per-iteration transactions; the
/// seeded transaction backs the status read. Any leg's failure is an honest
/// family skip — the sealed-stack authority needs the multi_root scope set
/// plus real branch refs to validate.
pub(crate) async fn prepare_native_snapshot(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    ctx: &QueryContext,
    advance_source: bool,
) -> Result<Value, String> {
    if !crate::repos::small_fixture_enabled() {
        return Err("native preparation requires the isolated small fixture".into());
    }
    let git = |root: &Path, args: &[&str]| -> Result<Vec<u8>, String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "native fixture git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(output.stdout)
    };
    if let Some(path) = &ctx.seeds.dirty_file {
        let original = git(project_root, &["show", &format!("HEAD:{path}")])?;
        let current = std::fs::read(project_root.join(path)).map_err(|error| error.to_string())?;
        let indexed = git(project_root, &["show", &format!(":{path}")])?;
        let marker = match path.rsplit('.').next() {
            Some("py" | "toml") => b"\n# bench hunk line\n".as_slice(),
            _ => b"\n// bench hunk line\n".as_slice(),
        };
        let mut owned = original.clone();
        owned.extend_from_slice(marker);
        if current == owned && (indexed == original || indexed == owned) {
            git(
                project_root,
                &[
                    "restore",
                    "--source=HEAD",
                    "--staged",
                    "--worktree",
                    "--",
                    path,
                ],
            )?;
        } else if current != original || indexed != original {
            return Err(format!(
                "native preparation refuses non-marker changes in {path}"
            ));
        }
    }
    if !git(project_root, &["status", "--porcelain"])?.is_empty() {
        return Err(
            "native preparation requires a clean fixture after exact marker cleanup".into(),
        );
    }
    let template = ctx
        .seeds
        .native
        .as_ref()
        .ok_or("native seed route is absent")?;
    let mut request: NativeIntegrationStackSnapshotSurfaceRequest =
        serde_json::from_value(template.snapshot_body.clone())
            .map_err(|error| error.to_string())?;
    if advance_source {
        let source_root =
            native_linked_root(project_root).ok_or("native linked fixture root is absent")?;
        let source_ref = request
            .source
            .reference
            .as_ref()
            .ok_or("native source reference is absent")?;
        let destination_ref = request
            .destination
            .reference
            .as_ref()
            .ok_or("native destination reference is absent")?;
        let listed = String::from_utf8(git(project_root, &["worktree", "list", "--porcelain"])?)
            .map_err(|error| error.to_string())?;
        if !listed.split("\n\n").any(|entry| {
            entry
                .lines()
                .any(|line| line == format!("worktree {}", source_root.display()))
                && entry
                    .lines()
                    .any(|line| line == format!("branch {}", source_ref.as_str()))
        }) || !git(&source_root, &["status", "--porcelain"])?.is_empty()
        {
            return Err("native source is not the clean owned linked worktree".into());
        }
        let ancestry = std::process::Command::new("git")
            .arg("-C")
            .arg(project_root)
            .args([
                "merge-base",
                "--is-ancestor",
                source_ref.as_str(),
                destination_ref.as_str(),
            ])
            .status()
            .map_err(|error| error.to_string())?;
        match ancestry.code() {
            Some(0) => {
                let path = source_root.join("src/native_integration_source.rs");
                if !std::fs::symlink_metadata(&path)
                    .map_err(|error| error.to_string())?
                    .file_type()
                    .is_file()
                {
                    return Err("native fixture source is not a regular file".into());
                }
                let mut source =
                    std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
                let original = "pub const BENCH_NATIVE_SOURCE: &str = \"source\";\n";
                let marker = "// benchmark integration update\n";
                if !source
                    .strip_prefix(original)
                    .is_some_and(|suffix| suffix.lines().all(|line| line == marker.trim_end()))
                {
                    return Err(
                        "native source contains changes outside the owned fixture marker".into(),
                    );
                }
                source.push_str(marker);
                std::fs::write(&path, source).map_err(|error| error.to_string())?;
                git(
                    &source_root,
                    &["add", "--", "src/native_integration_source.rs"],
                )?;
                git(
                    &source_root,
                    &[
                        "commit",
                        "--only",
                        "-m",
                        "test(bench): advance native source fixture",
                        "--",
                        "src/native_integration_source.rs",
                    ],
                )?;
            }
            Some(1) => {}
            _ => return Err("native fixture ancestry observation failed".into()),
        }
    }
    let scope = call_json_tool(
        harness,
        project_root,
        "tracedecay_multi_root_scope_set_read",
        json!({"scope_set_id": request.authorized_scope_set_id}),
    )
    .await?;
    let (revision, digest) =
        scope_set_read_identity(&scope).ok_or("current scope-set identity is absent")?;
    request.authorized_scope_set_revision = tracedecay_domain::ScopeSetRevision::new(
        u64::try_from(revision).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    request.authorized_scope_set_digest =
        ManifestDigest::new(digest).map_err(|error| error.to_string())?;
    let inventory = call_json_tool(
        harness,
        project_root,
        "tracedecay_worktree_inventory",
        json!({
            "scope_set_id": request.authorized_scope_set_id,
            "scope_set_revision": request.authorized_scope_set_revision,
            "scope_set_digest": request.authorized_scope_set_digest,
            "target": {"kind":"repository", "project_id": request.source.project_id,
                       "repository_id": request.source.repository_id},
        }),
    )
    .await?;
    let payload = inventory
        .pointer("/outcome/value/payload")
        .ok_or("current inventory omitted evidence payload")?;
    request.inventory_snapshot_id = WorktreeInventorySnapshotId::new(
        dig_str(payload, "snapshot_id").ok_or("current inventory omitted snapshot_id")?,
    )
    .map_err(|error| error.to_string())?;
    request.inventory_epoch = WorktreeInventoryEpoch::new(
        dig(payload, "epoch")
            .and_then(Value::as_u64)
            .ok_or("current inventory omitted epoch")?,
    )
    .map_err(|error| error.to_string())?;
    request.grant_digest = ManifestDigest::new(
        inventory
            .pointer("/outcome/value/authority/grant_digest")
            .and_then(Value::as_str)
            .ok_or("current inventory omitted grant_digest")?,
    )
    .map_err(|error| error.to_string())?;
    request.policy_digest = ManifestDigest::new(
        inventory
            .pointer("/outcome/value/authority/policy/digest")
            .and_then(Value::as_str)
            .ok_or("current inventory omitted policy digest")?,
    )
    .map_err(|error| error.to_string())?;
    let entries = dig(payload, "entries")
        .and_then(Value::as_array)
        .ok_or("current inventory omitted entries")?;
    let NativeIntegrationSelectionDeclarationV1::DeclaredStackEdge {
        nodes, revision_id, ..
    } = &mut request.selection
    else {
        return Err("native fixture requires its declared stack edge".into());
    };
    *revision_id = BranchStackRevisionId::new(format!("stack-revision.bench.{}", now_micros()))
        .map_err(|error| error.to_string())?;
    for node in nodes {
        let scope = if request.source.reference.as_ref() == Some(&node.reference) {
            &request.source
        } else if request.destination.reference.as_ref() == Some(&node.reference) {
            &request.destination
        } else {
            return Err("native route template contains an unrelated node".into());
        };
        let entry = entries
            .iter()
            .find(|entry| {
                entry["reference"].as_str() == Some(node.reference.as_str())
                    && entry["worktree_id"].as_str() == Some(scope.worktree_id.as_str())
            })
            .ok_or_else(|| {
                format!(
                    "current inventory omitted enrolled reference {}",
                    node.reference.as_str()
                )
            })?;
        node.tip = CommitId::new(
            entry["head"]
                .as_str()
                .ok_or("current inventory entry omitted head")?,
        )
        .map_err(|error| error.to_string())?;
    }
    request.clone().seal().map_err(|error| error.to_string())?;
    let mut body = serde_json::to_value(request).map_err(|error| error.to_string())?;
    body["format"] = json!("json");
    Ok(body)
}

/// The first public expansion waits for durable host publication and settles
/// the real recipient delivery. The measured request replays that exact handle.
#[tracing::instrument(name = "bench.setup.github_stack_signal", skip_all)]
pub(crate) async fn prepare_github_stack_signal(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    ctx: &QueryContext,
) -> Result<(Value, Value), String> {
    let body = prepare_native_snapshot(harness, project_root, ctx, true).await?;
    let snapshot = call_json_tool(harness, project_root, "tracedecay_stack_snapshot", body).await?;
    let snapshot = serde_json::from_value::<NativeIntegrationSurfaceResultV1>(
        snapshot
            .pointer("/outcome/value/payload")
            .ok_or("stack signal snapshot omitted its canonical payload")?
            .clone(),
    )
    .map_err(|error| error.to_string())?;
    let NativeIntegrationSurfaceResultV1::StackSnapshot(snapshot) = snapshot else {
        return Err("stack signal preparation did not resolve its native snapshot".into());
    };
    let preflight = call_json_tool(
        harness,
        project_root,
        "tracedecay_preflight_native_integration",
        json!({"snapshot": snapshot.sealed_snapshot}),
    )
    .await?;
    let preflight = serde_json::from_value::<NativeIntegrationSurfaceResultV1>(
        preflight
            .pointer("/outcome/value/payload")
            .ok_or("stack signal preflight omitted its canonical payload")?
            .clone(),
    )
    .map_err(|error| error.to_string())?;
    let NativeIntegrationSurfaceResultV1::Preview(preview) = preflight else {
        return Err("stack signal preparation did not produce a native preview".into());
    };
    if !matches!(
        preview.disposition,
        NativeIntegrationPreviewDispositionV1::MechanicalIntegrationEligible(_)
    ) {
        return Err(format!(
            "stack signal fixture is not mechanically eligible: {:?}",
            preview.disposition
        ));
    }
    let NativeIntegrationSelectionBindingV1::DeclaredStackEdge {
        revision_id,
        revision_digest,
        declared_revision,
        source_node_id,
        destination_node_id,
        direction,
        ..
    } = &snapshot.sealed_snapshot.selection
    else {
        return Err("stack signal fixture has no declared stack edge".into());
    };
    // Preflight enqueued this transition through the daemon coordinator. Its
    // canonical constructor derives handles from the returned evidence only.
    let signal = StackSignalV1::seal(
        &snapshot.sealed_snapshot.destination,
        StackSignalDraftV1 {
            stack_revision_id: revision_id.clone(),
            stack_revision_digest: revision_digest.clone(),
            kind: StackSignalKindV1::DependencyReady,
            state_digest: preview.preview_digest.clone(),
            github_stack_digest: None,
            observed_at: preview.created_at,
        },
    )
    .map_err(|error| format!("stack signal sealing failed: {error:?}"))?;
    let source = declared_revision
        .nodes
        .iter()
        .find(|node| node.node_id == *source_node_id)
        .ok_or("stack signal source node is absent")?;
    let destination = declared_revision
        .nodes
        .iter()
        .find(|node| node.node_id == *destination_node_id)
        .ok_or("stack signal destination node is absent")?;
    let args = json!({
        "signal_id": signal.signal_id,
        "expected_watermark_id": signal.watermark_id,
        "format": "json",
    });
    let expected = GitHubStackSignalEvidenceRefV1::new(
        signal.signal_id,
        signal.watermark_id,
        signal.kind,
        signal.stack_revision_id,
        signal.stack_revision_digest,
        signal.state_digest,
        signal.github_stack_digest,
        signal.observed_at,
        GitHubStackSignalNativeSourceV1::Preflight {
            preview: GitHubStackSignalNativePreviewV1 {
                preview_id: preview.preview_id,
                preview_digest: preview.preview_digest,
                direction: *direction,
                source_ref: source.reference.clone(),
                destination_ref: destination.reference.clone(),
                source_tip: GitOidV1::new(source.tip.as_str())
                    .map_err(|error| error.to_string())?,
                destination_tip: GitOidV1::new(destination.tip.as_str())
                    .map_err(|error| error.to_string())?,
                disposition: preview.disposition,
            },
        },
    )
    .map_err(|error| error.to_string())?;
    let expected = serde_json::to_value(expected).map_err(|error| error.to_string())?;
    let expected_scope = serde_json::to_value(&snapshot.sealed_snapshot.destination.scope_digest)
        .map_err(|error| error.to_string())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut readiness = tokio::time::interval(Duration::from_millis(50));
    readiness.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let response = tokio::time::timeout_at(
            deadline,
            call_json_tool(
                harness,
                project_root,
                "tracedecay_github_stack_signal_expand",
                args.clone(),
            ),
        )
        .await
        .map_err(|_| "stack signal host publication exceeded its setup deadline".to_owned())??;
        let result = serde_json::from_value::<GitHubStackSignalExpandSurfaceResultV1>(
            response
                .pointer("/outcome/value/payload")
                .ok_or("stack signal expansion omitted its canonical payload")?
                .clone(),
        )
        .map_err(|error| error.to_string())?;
        match result {
            GitHubStackSignalExpandSurfaceResultV1::Expanded { evidence } => {
                if serde_json::to_value(evidence).map_err(|error| error.to_string())? != expected
                    || response.pointer("/outcome/value/authority/authorized_scope_digest")
                        != Some(&expected_scope)
                {
                    return Err(
                        "initial stack signal expansion differs from its real native preview"
                            .into(),
                    );
                }
                return Ok((
                    args,
                    json!({
                        "evidence": expected,
                        "authorized_scope_digest": expected_scope,
                    }),
                ));
            }
            GitHubStackSignalExpandSurfaceResultV1::Unavailable {
                reason: GitHubStackSignalExpandUnavailableV1::AuthorityUnmounted,
            } => {
                // The adapter reports unavailable while its durable recipient
                // row is awaiting host publication. Other refusal states fail.
                tracing::debug!("stack signal expansion awaits durable host publication");
                tokio::time::timeout_at(deadline, readiness.tick())
                    .await
                    .map_err(|_| {
                        "stack signal remained authority_unmounted through its setup deadline"
                            .to_owned()
                    })?;
            }
            GitHubStackSignalExpandSurfaceResultV1::Unavailable { reason } => {
                return Err(format!("stack signal expansion refused setup: {reason:?}"));
            }
        }
    }
}

pub(crate) async fn prepare_worktree_claim(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    ctx: &QueryContext,
) -> Result<Value, String> {
    let body = prepare_native_snapshot(harness, project_root, ctx, false).await?;
    Ok(json!({
        "scope_set_id": body["authorized_scope_set_id"],
        "scope_set_revision": body["authorized_scope_set_revision"],
        "scope_set_digest": body["authorized_scope_set_digest"],
        "target": {
            "kind": "worktree", "project_id": body["source"]["project_id"],
            "repository_id": body["source"]["repository_id"],
            "worktree_id": body["source"]["worktree_id"],
        },
    }))
}

async fn seed_native_integration(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    let (
        Some(project_id),
        Some(repository_id),
        Some(scope_set_id),
        Some(scope_set_revision),
        Some(scope_set_digest),
        Some(branch),
    ) = (
        seeds.project_id.clone(),
        seeds.repository_id.clone(),
        seeds.scope_set_id.clone(),
        seeds.scope_set_revision,
        seeds.scope_set_digest.clone(),
        seeds.branch.clone(),
    )
    else {
        seeds
            .skipped
            .push("native_integration_*: scope-set/branch/commit seeds missing".to_owned());
        return;
    };
    // The identity minted at seed start can go stale while intervening seeds
    // (lcm, work) move the scope set; claim the current revision/digest.
    let (scope_set_revision, scope_set_digest) = match call_json_tool(
        harness,
        project_root,
        "tracedecay_multi_root_scope_set_read",
        json!({"scope_set_id": scope_set_id, "format": "json"}),
    )
    .await
    {
        Ok(v) => (
            scope_set_read_identity(&v)
                .map(|(revision, _)| revision)
                .unwrap_or(scope_set_revision),
            scope_set_read_identity(&v)
                .map(|(_, digest)| digest)
                .unwrap_or(scope_set_digest),
        ),
        Err(_) => (scope_set_revision, scope_set_digest),
    };
    let inventory_args = json!({
        "scope_set_id": scope_set_id,
        "scope_set_revision": scope_set_revision,
        "scope_set_digest": scope_set_digest,
        "target": {
            "kind": "repository",
            "project_id": project_id,
            "repository_id": repository_id,
        },
        "format": "json",
    });
    let mut inventory = None;
    let mut inventory_error = None;
    for _ in 0..30 {
        match call_json_tool(
            harness,
            project_root,
            "tracedecay_worktree_inventory",
            inventory_args.clone(),
        )
        .await
        {
            Ok(v)
                if dig_str(&v, "snapshot_id").is_some()
                    || dig_str(&v, "inventory_snapshot_id").is_some() =>
            {
                inventory = Some(v);
                break;
            }
            Ok(v) if dig_str(&v, "state") == Some("stale") => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Ok(v) => {
                inventory = Some(v);
                break;
            }
            Err(e) => inventory_error = Some(e),
        }
    }
    let Some(inventory) = inventory else {
        seeds.skipped.push(format!(
            "native_integration_*: worktree_inventory: {}",
            inventory_error.unwrap_or_else(|| "inventory remained stale".to_owned())
        ));
        return;
    };
    let Some(inventory_snapshot_id) = dig_str(&inventory, "snapshot_id")
        .or_else(|| dig_str(&inventory, "inventory_snapshot_id"))
        .map(str::to_owned)
    else {
        let state = dig_str(&inventory, "state").unwrap_or("unknown");
        seeds.skipped.push(format!(
            "native_integration_*: inventory returned no snapshot id (state={state})"
        ));
        return;
    };
    let Some(inventory_epoch) = dig(&inventory, "epoch")
        .and_then(Value::as_i64)
        .or_else(|| dig(&inventory, "inventory_epoch").and_then(Value::as_i64))
    else {
        let state = dig_str(&inventory, "state").unwrap_or("unknown");
        seeds.skipped.push(format!(
            "native_integration_*: inventory returned no epoch (state={state})"
        ));
        return;
    };
    let source_ref = format!("refs/heads/{NATIVE_LINKED_BRANCH}");
    let destination_ref = format!("refs/heads/{branch}");
    let Some(entries) = dig(&inventory, "entries").and_then(Value::as_array) else {
        seeds
            .skipped
            .push("native_integration_*: inventory omitted entry list".to_owned());
        return;
    };
    let entry_for = |reference: &str| {
        entries
            .iter()
            .find(|entry| entry.get("reference").and_then(Value::as_str) == Some(reference))
    };
    let Some(destination_entry) = entry_for(&destination_ref) else {
        seeds.skipped.push(format!(
            "native_integration_*: inventory omitted destination reference {destination_ref}"
        ));
        return;
    };
    let Some(source_entry) = entry_for(&source_ref) else {
        seeds.skipped.push(format!(
            "native_integration_*: inventory omitted linked source reference {source_ref}"
        ));
        return;
    };
    let Some(destination_worktree_id) = destination_entry
        .get("worktree_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        seeds.skipped.push(
            "native_integration_*: destination inventory entry omitted worktree_id".to_owned(),
        );
        return;
    };
    let Some(source_worktree_id) = source_entry
        .get("worktree_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        seeds
            .skipped
            .push("native_integration_*: source inventory entry omitted worktree_id".to_owned());
        return;
    };
    let (Some(destination_head), Some(source_head)) = (
        destination_entry.get("head").and_then(Value::as_str),
        source_entry.get("head").and_then(Value::as_str),
    ) else {
        seeds.skipped.push(
            "native_integration_*: current inventory omitted source or destination tip".into(),
        );
        return;
    };
    seeds.cleanup_worktree_id = Some(source_worktree_id.clone());
    let Some(grant_digest) = inventory
        .pointer("/outcome/value/authority/grant_digest")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        seeds
            .skipped
            .push("native_integration_*: inventory omitted grant_digest".to_owned());
        return;
    };
    let Some(policy_digest) = inventory
        .pointer("/outcome/value/authority/policy/digest")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        seeds
            .skipped
            .push("native_integration_*: inventory omitted policy_digest".to_owned());
        return;
    };
    let typed_body = (|| {
        let project = ProjectId::new(project_id.clone())?;
        let repository = RepositoryId::new(repository_id.clone())?;
        let source_worktree = WorktreeId::new(source_worktree_id.clone())?;
        let destination_worktree = WorktreeId::new(destination_worktree_id.clone())?;
        let source = ResolvedScope::new(
            project.clone(),
            repository.clone(),
            source_worktree.clone(),
            Some(RefId::new(source_ref.clone())?),
        )?;
        let destination = ResolvedScope::new(
            project.clone(),
            repository.clone(),
            destination_worktree.clone(),
            Some(RefId::new(destination_ref.clone())?),
        )?;
        let source_node = StackNodeId::new("node.source")?;
        let destination_node = StackNodeId::new("node.destination")?;
        let selection = NativeIntegrationSelectionDeclarationV1::DeclaredStackEdge {
            stack_id: BranchStackId::new("stack.bench")?,
            revision_id: BranchStackRevisionId::new(format!(
                "stack-revision.bench.{}",
                now_micros()
            ))?,
            nodes: vec![
                BranchStackNodeV1 {
                    node_id: destination_node.clone(),
                    project_id: project.clone(),
                    repository_id: repository.clone(),
                    reference: RefId::new(destination_ref.clone())?,
                    tip: CommitId::new(destination_head)?,
                    // The primary destination is the checked-out repository
                    // root. Omitting this optional worktree identity tells
                    // the native adapter to operate on that clean root;
                    // supplying it would classify the destination as an
                    // occupied linked worktree and make apply a no-op.
                    worktree_id: None,
                },
                BranchStackNodeV1 {
                    node_id: source_node.clone(),
                    project_id: project.clone(),
                    repository_id: repository.clone(),
                    reference: RefId::new(source_ref.clone())?,
                    tip: CommitId::new(source_head)?,
                    worktree_id: Some(source_worktree),
                },
            ],
            edges: vec![BranchStackEdgeV1 {
                dependency: source_node.clone(),
                dependent: destination_node.clone(),
            }],
            source_node_id: source_node,
            destination_node_id: destination_node,
            direction: NativeIntegrationDirectionV1::PropagateDependencyToDependent,
        };
        Ok::<_, Box<dyn std::error::Error>>(NativeIntegrationStackSnapshotSurfaceRequest {
            source,
            destination,
            authorized_scope_set_id: tracedecay_domain::ScopeSetId::new(scope_set_id.clone())?,
            authorized_scope_set_revision: tracedecay_domain::ScopeSetRevision::new(
                scope_set_revision as u64,
            )?,
            authorized_scope_set_digest: ManifestDigest::new(scope_set_digest.clone())?,
            inventory_snapshot_id: WorktreeInventorySnapshotId::new(inventory_snapshot_id.clone())?,
            inventory_epoch: WorktreeInventoryEpoch::new(inventory_epoch as u64)?,
            selection,
            grant_digest: ManifestDigest::new(grant_digest.clone())?,
            policy_digest: ManifestDigest::new(policy_digest.clone())?,
        })
    })();
    let typed_request = match typed_body {
        Ok(request) => request,
        Err(error) => {
            seeds.skipped.push(format!(
                "native_integration_*: invalid typed snapshot seed: {error}"
            ));
            return;
        }
    };
    if let Err(error) = typed_request.clone().seal() {
        seeds.skipped.push(format!(
            "native_integration_*: stack snapshot declaration rejected: {error}"
        ));
        return;
    }
    if typed_request.source.worktree_id == typed_request.destination.worktree_id {
        seeds.skipped.push(
            "native_integration_*: declared stack requires two distinct enrolled worktrees"
                .to_owned(),
        );
        return;
    }
    let body = match serde_json::to_value(typed_request) {
        Ok(body) => body,
        Err(error) => {
            seeds.skipped.push(format!(
                "native_integration_*: stack snapshot request serialization failed: {error}"
            ));
            return;
        }
    };
    let snapshot = match call_json_tool(
        harness,
        project_root,
        "tracedecay_stack_snapshot",
        body.clone(),
    )
    .await
    {
        Ok(v) => dig(&v, "sealed_snapshot")
            .cloned()
            .or_else(|| dig(&v, "snapshot").cloned()),
        Err(e) => {
            seeds
                .skipped
                .push(format!("native_integration_*: stack_snapshot: {e}"));
            return;
        }
    };
    let Some(snapshot) = snapshot else {
        seeds
            .skipped
            .push("native_integration_*: stack_snapshot returned no sealed snapshot".to_owned());
        return;
    };
    let preflight = match call_json_tool(
        harness,
        project_root,
        "tracedecay_preflight_native_integration",
        json!({"snapshot": snapshot, "format": "json"}),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            seeds
                .skipped
                .push(format!("native_integration_*: preflight: {e}"));
            return;
        }
    };
    let Some(preview_id) = dig_str(&preflight, "preview_id").map(str::to_owned) else {
        let state = dig_str(&preflight, "state").unwrap_or("unknown");
        seeds.skipped.push(format!(
            "native_integration_*: preflight returned no preview (state={state})"
        ));
        return;
    };
    let Some(preview_digest) = dig_str(&preflight, "preview_digest").map(str::to_owned) else {
        seeds.skipped.push(format!(
            "native_integration_*: preflight preview {preview_id} omitted preview_digest"
        ));
        return;
    };
    // Keep the valid snapshot capability even when approval/apply cannot
    // advance this fixture. A preflight preview is not a transaction receipt.
    seeds.native = Some(NativeSeeds {
        snapshot_body: body,
        transaction_id: None,
    });
    let approval = match call_json_tool(
        harness,
        project_root,
        "tracedecay_approve_native_integration",
        json!({
            "preview_id": preview_id.clone(),
            "preview_digest": preview_digest.clone(),
            "format": "json",
        }),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            seeds
                .skipped
                .push(format!("native_integration_*: seed approval: {error}"));
            return;
        }
    };
    let Some(approval_id) = dig_str(&approval, "approval_id").map(str::to_owned) else {
        seeds
            .skipped
            .push("native_integration_*: seed approval omitted approval_id".to_owned());
        return;
    };
    let Some(approval_digest) = dig_str(&approval, "approval_digest").map(str::to_owned) else {
        seeds
            .skipped
            .push("native_integration_*: seed approval omitted approval_digest".to_owned());
        return;
    };
    let approval = approval
        .pointer("/outcome/value/payload")
        .unwrap_or(&approval);
    let Some(transaction_id) = approval
        .get("transaction_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        seeds
            .skipped
            .push("native_integration_*: approval omitted daemon transaction_id".to_owned());
        return;
    };
    let apply = match call_json_tool(
        harness,
        project_root,
        "tracedecay_apply_native_integration",
        json!({
            "preview_id": preview_id,
            "preview_digest": preview_digest,
            "approval_id": approval_id,
            "approval_digest": approval_digest,
            "transaction_id": transaction_id.clone(),
            "format": "json",
        }),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            seeds
                .skipped
                .push(format!("native_integration_*: seed apply: {error}"));
            return;
        }
    };
    let apply = apply.pointer("/outcome/value/payload").unwrap_or(&apply);
    let receipt_tx = apply
        .pointer("/status/transaction_id")
        .and_then(Value::as_str)
        .or_else(|| apply.get("transaction_id").and_then(Value::as_str));
    let terminal = apply
        .get("terminal_outcome")
        .and_then(Value::as_str)
        .or_else(|| {
            apply
                .pointer("/status/terminal_outcome")
                .and_then(Value::as_str)
        });
    if apply.get("outcome").and_then(Value::as_str) != Some("receipt")
        || receipt_tx != Some(transaction_id.as_str())
        || terminal != Some("committed")
    {
        seeds.skipped.push(format!(
            "native_integration_*: seed apply was not a committed receipt (outcome={}, transaction_id={:?}, terminal={:?})",
            apply.get("outcome").and_then(Value::as_str).unwrap_or("missing"),
            receipt_tx,
            terminal,
        ));
        return;
    }
    let Some(final_ref_tip) = apply.get("final_ref_tip").and_then(Value::as_str) else {
        seeds
            .skipped
            .push("native_integration_*: committed receipt omitted final_ref_tip".to_owned());
        return;
    };
    let observed_tip = std::process::Command::new("git")
        .args(["-C"])
        .arg(project_root)
        .args(["rev-parse", &format!("refs/heads/{branch}")])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    if observed_tip.as_deref() != Some(final_ref_tip) {
        seeds.skipped.push(format!(
            "native_integration_*: committed receipt ref tip mismatch (receipt={final_ref_tip}, observed={observed_tip:?})"
        ));
        return;
    }
    let Some(final_tree) = apply.get("final_tree").and_then(Value::as_str) else {
        seeds
            .skipped
            .push("native_integration_*: committed receipt omitted final_tree".to_owned());
        return;
    };
    let observed_tree = std::process::Command::new("git")
        .args(["-C"])
        .arg(project_root)
        .args(["rev-parse", &format!("refs/heads/{branch}^{{tree}}")])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    if observed_tree.as_deref() != Some(final_tree) {
        seeds.skipped.push(format!("native_integration_*: committed receipt tree mismatch (receipt={final_tree}, observed={observed_tree:?})"));
        return;
    }
    if let Some(head) = observed_tip {
        seeds.head_commit = Some(head);
    }
    seeds.parent_commit = std::process::Command::new("git")
        .args(["-C"])
        .arg(project_root)
        .args(["rev-parse", "HEAD~1"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    if let Some(native) = seeds.native.as_mut() {
        native.transaction_id = Some(transaction_id);
    }
}

fn collect_hunk_digests(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(m) => {
            if m.contains_key("hunk")
                && let Some(Value::String(d)) = m.get("digest")
            {
                crate::queries::push_unique(out, d);
            }
            for v in m.values() {
                collect_hunk_digests(v, out);
            }
        }
        Value::Array(a) => {
            for v in a {
                collect_hunk_digests(v, out);
            }
        }
        _ => {}
    }
}

/// Admit a native saved-edit callback through the production hook runtime.
/// The native lifecycle is committed by host admission; the background producer
/// then binds its exact address and queues the real compiler finding. Reading
/// the mounted authority leaves the suggestion available for the public tools.
pub(crate) async fn seed_context_scout_address(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
) -> Result<Value, String> {
    use tracedecay_domain::configuration::{
        CONTEXT_SCOUT_SETTINGS_SETTING_KEY, ContextScoutConfigurationStateV1,
        ContextScoutSettingsV1,
    };

    let project_id = harness
        .project_id(project_root)
        .await
        .map_err(|error| format!("context scout project identity unavailable: {error}"))?;
    let mut settings = ContextScoutSettingsV1::disabled();
    settings.state = ContextScoutConfigurationStateV1::Active;
    let expected = serde_json::to_value(ConfigurationValueV1::ContextScoutSettings(settings))
        .map_err(|error| format!("context scout settings encode failed: {error}"))?;
    let current = call_json_tool(
        harness,
        project_root,
        "tracedecay_configuration_get",
        json!({"key": CONTEXT_SCOUT_SETTINGS_SETTING_KEY, "format": "json"}),
    )
    .await?;
    if dig(&current, "effective_value") != Some(&expected) {
        let revision = current
            .pointer("/outcome/value/payload/revision_id")
            .and_then(Value::as_str)
            .ok_or("context scout configuration omitted current revision")?;
        call_json_tool(
            harness,
            project_root,
            "tracedecay_configuration_set",
            json!({
                "layer": {"kind": "project", "project_id": project_id},
                "key": CONTEXT_SCOUT_SETTINGS_SETTING_KEY,
                "value": expected,
                "expected_revision": revision,
                "idempotency_key": format!("bench-context-scout-enable-{}", now_micros()),
                "format": "json",
            }),
        )
        .await?;
    }

    let project_id = ProjectId::new(project_id)
        .map_err(|error| format!("context scout project identity invalid: {error}"))?;
    let scope =
        tracedecay_code_index_runtime::resolved_scope_for_project(project_root, &project_id)
            .map_err(|error| format!("context scout native scope unavailable: {error:?}"))?;
    let (_, worktree_id) = tracedecay_agent_hosts::hooks::hook_scope_locators(&scope);
    let data_root = harness
        .project_data_root(project_root)
        .await
        .map_err(|error| format!("context scout data root unavailable: {error}"))?;
    let host = tracedecay_domain::NativeHostIdentityV1::KimiCode;
    let path = tracedecay_hooks::hook_configuration_path(&data_root, worktree_id, host);
    let snapshot = tracedecay_hooks::HookConfigurationFileReaderV1::new(path)
        .load(host)
        .map_err(|error| format!("context scout hook binding decode failed: {error}"))?
        .ok_or("context scout hook admission has no daemon-published Kimi binding")?;

    let session_id = format!("bench-context-scout-{}", now_micros());
    let call_id = format!("{session_id}-edit");
    let mut payload: Value = serde_json::from_slice(include_bytes!(
        "../../../tracedecay-hooks/fixtures/host_events/kimi/post-tool-use-edit.json"
    ))
    .map_err(|error| format!("context scout native fixture decode failed: {error}"))?;
    payload["session_id"] = json!(session_id);
    payload["tool_call_id"] = json!(call_id);
    payload["cwd"] = json!(project_root);
    payload["tool_input"]["path"] = json!(project_root.join("src/lib.rs"));
    let payload = serde_json::to_vec(&payload)
        .map_err(|error| format!("context scout native fixture encode failed: {error}"))?;
    let observed_at = tracedecay_domain::UtcMicros(now_micros());
    let material = tracedecay_agent_hosts::hooks::native_capture_material(
        tracedecay_hooks::NativeHookCaptureSourceV1::Host(host),
        &payload,
        observed_at,
    )
    .map_err(|error| format!("context scout native fixture material failed: {error}"))?;
    let lifecycle = tracedecay_agent_hosts::hooks::NativeContextScoutLifecycleV1::new(
        &session_id,
        &call_id,
        material.event_id,
    )
    .ok_or("context scout native lifecycle identity invalid")?;
    let envelope = tracedecay_hooks::decode_native_hook_event(host, &payload)
        .map_err(|error| format!("context scout native fixture decode failed: {error}"))?
        .into_envelope(&snapshot.binding, material)
        .map_err(|error| format!("context scout hook envelope rejected: {error}"))?;
    let response = call_lenient(
        harness,
        project_root,
        "tracedecay_hook_runtime",
        json!({
            "action": "hook_v2_admit",
            "envelope": envelope,
            "native_session_id": session_id,
            "native_lifecycle": lifecycle,
            "format": "json",
        }),
    )
    .await?;
    if dig_str(&response, "disposition") != Some("accepted") {
        return Err(format!(
            "context scout native admission failed: {}",
            bounded_json(&response)
        ));
    }
    let session_id = tracedecay_domain::SessionId::new(session_id)
        .map_err(|error| format!("context scout native session invalid: {error}"))?;
    let lifecycle = tracedecay_daemon_service::context_scout_lifecycle::lookup_registered_context_scout_lifecycle(
        envelope.project_id,
        envelope.worktree_id,
        &session_id,
    )
    .await
    .ok_or("context scout native admission did not commit a complete canonical lifecycle")?;
    let graph = harness
        .server(project_root)
        .map_err(|error| format!("context scout project route unavailable: {error}"))?
        .cg()
        .await;
    let owner = graph
        .context_scout_owner()
        .ok_or("context scout runtime owner unavailable")?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((address, _)) = graph
            .resolve_mounted_context_scout_claim_authority(&lifecycle)
            .await
            && let Ok(recent) = owner.recent_exact(address, 1).await
            && recent
                .pending
                .iter()
                .any(|entry| entry.work.address == address)
        {
            return serde_json::to_value(address)
                .map_err(|error| format!("context scout address encode failed: {error}"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "context scout native producer did not mount a queued suggestion: {:?}",
                owner.configured_status().await,
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn seed_skill(harness: &ProductionProjectCompositionHarnessV1, seeds: &mut Seeds) {
    const SKILL_ID: &str = "bench-skill-4a71c8";
    let support_file = match ManagedSupportFile::new(
        "references/bench.md",
        b"bench managed skill support marker\n".to_vec(),
    ) {
        Ok(file) => file,
        Err(error) => {
            seeds
                .skipped
                .push(format!("skill_view: support file: {error}"));
            return;
        }
    };
    let draft = ManagedSkillDraft {
        id: SKILL_ID.to_owned(),
        title: "Bench Skill".to_owned(),
        summary: "Read the benchmark checklist.".to_owned(),
        routing_description: "Use for benchmark coverage.".to_owned(),
        category: "maintenance".to_owned(),
        targets: default_managed_skill_targets(),
        body_markdown: "Benchmark managed skill body marker.".to_owned(),
        support_files: vec![support_file],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "tracedecay-tool-performance".to_owned(),
            run_id: Some("bench-run-skill-4a71c8".to_owned()),
        },
    };
    match load_managed_skill(harness.profile_root(), SKILL_ID).await {
        Ok(skill)
            if skill.metadata.title == draft.title
                && skill.metadata.summary == draft.summary
                && skill.metadata.routing_description == draft.routing_description
                && skill.metadata.category == draft.category
                && skill.metadata.targets == draft.targets
                && skill.body_markdown == draft.body_markdown
                && skill.support_files == draft.support_files
                && skill.metadata.provenance == draft.provenance =>
        {
            seeds.skill_id = Some(SKILL_ID.to_owned());
            return;
        }
        Ok(_) => {
            seeds.skipped.push(
                "skill_view: existing benchmark skill does not match expected draft".to_owned(),
            );
            return;
        }
        Err(ManagedSkillReadError::NotFound { .. }) => {}
        Err(error) => {
            seeds.skipped.push(format!("skill_view: {error}"));
            return;
        }
    }
    match create_managed_skill(harness.profile_root(), draft).await {
        Ok(skill) => seeds.skill_id = Some(skill.metadata.id),
        Err(error) => seeds.skipped.push(format!("skill_view: {error}")),
    }
}

async fn seed_automation_run(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    const RUN_ID: &str = "bench-run-performance-4a71c8";
    const MARKER: &str = "bench-automation-artifact-4a71c8";
    let server = match harness.server(project_root) {
        Ok(server) => server,
        Err(error) => {
            seeds
                .skipped
                .push(format!("automation_run_*: server unavailable: {error}"));
            return;
        }
    };
    let dashboard_root = server.cg().await.store_layout().dashboard_root.clone();
    let artifact = match write_run_artifact(
        &dashboard_root,
        RUN_ID,
        AutomationRunArtifactKind::Traces,
        &json!({"marker": MARKER, "status": "captured"}),
        Some("benchmark trace artifact".to_owned()),
        "1782283200",
    )
    .await
    {
        Ok(artifact) => artifact,
        Err(error) => {
            seeds
                .skipped
                .push(format!("automation_run_artifact_view: {error}"));
            return;
        }
    };
    let record = AutomationRunLedgerRecord {
        schema_version: 2,
        run_id: RUN_ID.to_owned(),
        trigger: AutomationTrigger::ManualCli,
        task: AgentTaskKind::MemoryCurator,
        task_key: Some("memory_curator".to_owned()),
        backend: "codex_app_server".to_owned(),
        backend_identity: None,
        host_mode: Some("standalone".to_owned()),
        prompt_version: Some("memory_curator:v1".to_owned()),
        response_schema: None,
        strict_json: Some(true),
        model: Some("bench-model".to_owned()),
        status: AutomationRunStatus::Succeeded,
        evidence_hash: None,
        input_hash: None,
        output_hash: None,
        proposed_ops: Some(json!({"ops": []})),
        applied_ops: None,
        rejected_ops: None,
        validation_report: Some(json!({"passed": true})),
        reviewed_count: 1,
        accepted_count: 1,
        rejected_count: 0,
        skipped_count: 0,
        error: None,
        error_classification: None,
        error_retryable: None,
        backend_attempt_count: 1,
        backend_attempts: Vec::new(),
        fallback_status: None,
        session_evidence_budget_stage: None,
        report_ref: None,
        artifacts: vec![artifact],
        started_at: "1782283199".to_owned(),
        completed_at: "1782283200".to_owned(),
        completed_at_micros: Some(1_782_283_200_000_000),
    };
    match append_run_record(&dashboard_root, &record).await {
        Ok(()) => seeds.automation_run_id = Some(RUN_ID.to_owned()),
        Err(error) => seeds.skipped.push(format!("automation_run_*: {error}")),
    }
}

/// Every object value reachable in `value` (pre-order), for producer
/// responses that bury identities a level deeper than `dig` reaches.
fn objects<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    walk_objects(value, out);
}

fn walk_objects<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value {
        Value::Object(map) => {
            out.push(value);
            for v in map.values() {
                walk_objects(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                walk_objects(v, out);
            }
        }
        _ => {}
    }
}

/// `call_json_tool` that also yields problem envelopes: choreography like pin
/// repair needs the typed problem body (it carries the corrected digests),
/// which `isError` handling would otherwise hide.
async fn call_lenient(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    tool_name: &str,
    arguments: Value,
) -> Result<Value, String> {
    let response = harness
        .call_tool(project_root, tool_name, arguments)
        .await
        .map_err(|error| format!("{tool_name} transport failed: {error}"))?;
    if let Some(error) = &response.error {
        return Err(format!("{tool_name} JSON-RPC failed: {error:?}"));
    }
    let text = response
        .result
        .as_ref()
        .and_then(|result| result.pointer("/content/0/text"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{tool_name} returned no text payload"))?;
    serde_json::from_str(text)
        .map_err(|error| format!("{tool_name} returned non-JSON output: {error}"))
}

/// `call_json_tool` with a bounded wall-clock retry on the same transient
/// codes the prime steps use: seed chains hit identical settlement and lease
/// fences, and a seed that dies on the first transient leaves its whole tool
/// family silently unmeasured. The budget is time, not attempts — a slow
/// call (proposal generation can outlive the transport's own deadline)
/// converges or is recorded, it cannot grind retries for hours.
#[tracing::instrument(
    name = "bench.setup.call",
    level = "trace",
    skip_all,
    fields(tool = tool)
)]
async fn call_transient(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    tool: &str,
    args: Value,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        match call_json_tool(harness, project_root, tool, args.clone()).await {
            Err(error)
                if tokio::time::Instant::now() < deadline
                    && crate::TRANSIENT_STEP_CODES
                        .iter()
                        .any(|code| error.contains(code)) =>
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            result => return result,
        }
    }
}

/// `<field> expected sha256:<hex>` repair hints published by
/// `workflow_validate_definition` denials — mirrors the suite's
/// `_WORKFLOW_PIN_MISMATCH` contract.
/// A `sha256:`-prefixed manifest digest is exactly 71 chars — the "sha256:"
/// scheme is not itself hexadecimal, so it is sliced, not scanned.
fn digest_at(message: &str, pos: usize) -> Option<String> {
    let digest: String = message[pos..].chars().take(71).collect();
    if digest.len() == 71
        && digest.starts_with("sha256:")
        && digest[7..].chars().all(|c| c.is_ascii_hexdigit())
    {
        Some(digest)
    } else {
        None
    }
}

fn repair_definition_pins(response: &Value, pins: &mut serde_json::Map<String, Value>) -> bool {
    let mut found = false;
    let mut all = Vec::new();
    objects(response, &mut all);
    for obj in all {
        let Some(diagnostic) = obj.get("diagnostic") else {
            continue;
        };
        let Some(code) = diagnostic.get("code").and_then(Value::as_str) else {
            continue;
        };
        if !code.ends_with(".pin_mismatch") {
            continue;
        }
        let Some(message) = diagnostic.get("message").and_then(Value::as_str) else {
            continue;
        };
        for part in message.split(',') {
            let part = part.trim();
            let Some(field) = part.strip_suffix(" digest") else {
                continue;
            };
            if !field.starts_with("pinned_") {
                continue;
            }
            let Some(expect_pos) = message.find("expected sha256:") else {
                continue;
            };
            if let Some(digest) = digest_at(message, expect_pos + "expected ".len()) {
                pins.insert(field.to_owned(), Value::String(digest));
                found = true;
            }
        }
        // Fallback for the exact suite phrasing: "<field> expected sha256:<hex>, observed"
        if !found && let Some(expect_pos) = message.find(" expected sha256:") {
            let field = message[..expect_pos]
                .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next();
            let digest = digest_at(message, expect_pos + " expected ".len());
            if let (Some(field), Some(digest)) = (field, digest)
                && field.starts_with("pinned_")
                && field.ends_with("_digest")
            {
                pins.insert(field.to_owned(), Value::String(digest));
                found = true;
            }
        }
    }
    found
}

async fn seed_work_attempt_provider(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    project_id: &str,
    seeds: &mut Seeds,
) -> bool {
    let call = |tool: &'static str, args: Value| call_transient(harness, project_root, tool, args);
    // A provider that exits instantly settles every attempt before the
    // lifecycle can be exercised: cancel/pause/resume then have nothing live
    // to operate on (`not-cancellable`). Eight seconds keeps the attempt
    // running when cancel lands but frees the topology's single parallel
    // slot well inside the prime's transient-retry window.
    let executable_bytes: &[u8] = b"#!/bin/sh\nIFS= read -r instruction\ncase \"$instruction\" in\n  'Bench lifecycle failure.') exit 17 ;;\n  'Bench lifecycle source.') printf '%s' 'TraceDecay lifecycle source evidence.'; exit 0 ;;\nesac\nsleep 8\n";
    // Keep the provider outside the clone: `restore_repo` stashes and drops
    // untracked files between runs, which would leave the persisted
    // binding's canonical_path dangling and fail the next open's route
    // resolution (proposal routing mounts at open and digests the file).
    let dir = project_root
        .parent()
        .map(|parent| parent.join(".bench-providers"))
        .unwrap_or_else(|| project_root.join(crate::queries::SCRATCH_DIR));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        seeds
            .skipped
            .push(format!("work_provider: scratch dir: {e}"));
        return false;
    }
    let path = dir.join("work-attempt-provider");
    if let Err(e) = std::fs::write(&path, executable_bytes) {
        seeds
            .skipped
            .push(format!("work_provider: script write: {e}"));
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    let canonical_path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            seeds
                .skipped
                .push(format!("work_provider: canonicalize: {e}"));
            return false;
        }
    };
    let mut hasher = ManifestDigestHasher::new();
    hasher.update(executable_bytes);
    let Some(artifact_digest) = hasher.finalize().ok() else {
        seeds
            .skipped
            .push("work_provider: executable digest failed".to_owned());
        return false;
    };
    let Some(binding) = (|| {
        let executable = WorkExecutableReference::new(
            "executable.work.bench-provider".to_owned(),
            artifact_digest.clone(),
        )
        .ok()?;
        let route = WorkRouteCandidateV1 {
            route_id: "route.work.bench-codex.v1".to_owned(),
            provider_capability_id: WorkProviderBackendV1::CodexCli
                .provider_id()
                .as_str()
                .to_owned(),
            model_id: "gpt-5.6-sol".to_owned(),
            effort: WorkEffortClassV1::Standard,
            declared_budget_ceiling: 1,
            content_location: WorkContentLocationClassV1::Local,
            correctness: WorkOrdinalBandV1::High,
            sensitive_data_fitness: WorkOrdinalBandV1::High,
            latency: WorkOrdinalBandV1::Moderate,
            cost: WorkOrdinalBandV1::Moderate,
            autonomy: WorkOrdinalBandV1::High,
            evidence_quality: WorkOrdinalBandV1::High,
            execution: WorkRouteExecutionProfileV1 {
                sandbox: WorkSandboxPolicy::Required,
                approval: WorkApprovalPolicy::Never,
                filesystem: WorkFilesystemPolicy::WorkspaceWrite,
                egress: WorkEgressPolicy::Deny,
                environment_allowlist: BTreeSet::new(),
                credential_references: BTreeSet::new(),
                limits: WorkExecutionLimits::new(128_000, 8_192, 16_384, 16_384, 65_536, 1).ok()?,
                // Attempt deadline = admit instant + this budget. It must span
                // the whole prime chain (admit → placement → start) under
                // transient retries: an expired deadline refuses provider
                // dispatch (invalid-holder) and the attempt ends terminal
                // before the lifecycle steps can cancel it.
                maximum_duration_micros: 900_000_000,
                fallback: WorkFallbackTopology::Disabled,
            },
        };
        WorkExecutableBindingV1::new(
            executable,
            canonical_path,
            vec![WorkExecutableCapabilityV1::CodexCliExecJson],
            vec![route],
        )
        .ok()
    })() else {
        seeds
            .skipped
            .push("work_provider: binding assembly failed domain validation".to_owned());
        return false;
    };
    let Ok(current) = call(
        "tracedecay_configuration_get",
        json!({"key": WORK_EXECUTABLE_BINDINGS_SETTING_KEY, "format": "json"}),
    )
    .await
    else {
        seeds
            .skipped
            .push("work_provider: configuration_get failed".to_owned());
        return false;
    };
    // Structural compare, not a text match: the response re-serializes with a
    // different field order than the struct writes. Every persisted field —
    // digest, path, and execution profile like maximum_duration_micros —
    // must match: a stale profile (e.g. a shorter attempt budget) is as
    // wrong as a stale file.
    let want = serde_json::to_value(&binding).unwrap_or(Value::Null);
    let mut all = Vec::new();
    objects(&current, &mut all);
    if all.iter().any(|obj| **obj == want) {
        return true;
    }
    let mut last_err = String::new();
    let mut current = current;
    for _ in 0..3 {
        let Some(revision) = extract_path(&current, "outcome.value.payload.revision_id")
            .and_then(|v| v.as_str().map(str::to_owned))
        else {
            seeds
                .skipped
                .push("work_provider: get omitted effective revision".to_owned());
            return false;
        };
        match call(
            "tracedecay_configuration_set",
            json!({
                "layer": {"kind": "project", "project_id": project_id},
                "key": WORK_EXECUTABLE_BINDINGS_SETTING_KEY,
                "value": serde_json::to_value(ConfigurationValueV1::WorkExecutableBindings(vec![
                    binding.clone(),
                ]))
                .unwrap_or(Value::Null),
                "expected_revision": revision,
                "idempotency_key": format!("bench-work-provider-{}", now_micros()),
                "format": "json",
            }),
        )
        .await
        {
            Ok(_) => {
                // Bindings mount at composition open; a commit this run only
                // takes effect after the caller reopens the harness.
                seeds.needs_reopen_for_provider = true;
                return true;
            }
            Err(e) => last_err = e,
        }
        let Ok(next) = call(
            "tracedecay_configuration_get",
            json!({"key": WORK_EXECUTABLE_BINDINGS_SETTING_KEY, "format": "json"}),
        )
        .await
        else {
            break;
        };
        current = next;
    }
    seeds
        .skipped
        .push(format!("work_provider: configuration_set: {last_err}"));
    false
}

/// Work lifecycle builder shared by `seed_work` and the per-iteration effect
/// primes: the same canonical create/proposal/accept/admit choreography the
/// suite drives, so `iter` only renames ids rather than re-recording behavior.
fn work_create_change(suffix: &str, occurred_at: Value) -> Value {
    let initiative_id = format!("initiative.bench.{suffix}");
    let plan_id = format!("plan.bench.{suffix}");
    let milestone_id = format!("milestone.bench.{suffix}");
    let task_id = format!("task.bench.{suffix}");
    json!({
        "change": "create_task",
        "initiative": {"id": initiative_id, "title": "Bench initiative", "created_at": occurred_at},
        "plan": {"id": plan_id, "initiative_id": initiative_id, "title": "Bench plan", "created_at": occurred_at},
        "milestone": {"id": milestone_id, "plan_id": plan_id, "title": "Bench milestone", "created_at": occurred_at},
        "item": {
            "input": {
                "task_id": task_id,
                "hierarchy": {
                    "initiative_id": initiative_id,
                    "plan_id": plan_id,
                    "milestone_id": milestone_id,
                },
                "title": "Bench lifecycle task",
                "dependencies": [],
                "informational_relations": [],
                "causal_candidates": [],
                "acceptance_criteria": [],
                "effort": 1,
                "scheduled_at": null,
                "deadline": null,
                "created_at": occurred_at,
                "updated_at": occurred_at,
            },
            "accepted_proposal": null,
            "accepted_route": null,
            "execution_admitted_at": null,
            "accepted_attempts": [],
            "accepted_criteria": {},
            "accepted_at": null,
            "archived_at": null,
            "evidence_links": [],
            "handoffs": [],
        },
    })
}

pub(crate) const WORK_FAILURE_INSTRUCTIONS: &str = "Bench lifecycle failure.";
pub(crate) const WORK_SOURCE_INSTRUCTIONS: &str = "Bench lifecycle source.";

pub(crate) async fn wait_work_attempt(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    identity: Value,
    expected_states: &[&str],
) -> Result<Value, String> {
    let mut state = String::new();
    for _ in 0..250 {
        let status = call_json_tool(
            harness,
            project_root,
            "tracedecay_work_attempt_status",
            identity.clone(),
        )
        .await?;
        state = dig_str(&status, "state")
            .ok_or_else(|| format!("Work attempt status omitted state: {status}"))?
            .to_owned();
        if expected_states.contains(&state.as_str()) {
            return Ok(status);
        }
        if matches!(
            state.as_str(),
            "succeeded" | "failed" | "timed_out" | "cancelled"
        ) {
            return Err(format!(
                "Work attempt became {state}; expected {expected_states:?}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(format!(
        "Work attempt remained {state}; expected {expected_states:?}"
    ))
}

/// Start an observational attempt, await readiness, then cancel it to release capacity.
async fn settle_attempt(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    task_id: &str,
    run_id: &str,
    attempt_id: &str,
    execution_snapshot: &Value,
    commit: &str,
) -> (Result<Value, String>, Value, String) {
    let call = |tool: &'static str, mut args: Value| {
        args["format"] = json!("json");
        call_transient(harness, project_root, tool, args)
    };
    let status =
        |attempt: &str| json!({"task_id": task_id, "run_id": run_id, "attempt_id": attempt});
    let started = call(
        "tracedecay_work_start_attempt",
        json!({
            "task_id": task_id,
            "run_id": run_id,
            "attempt_id": attempt_id,
            "operation": "operation.work.start_attempt",
            "execution_snapshot": execution_snapshot,
            "worktree_root": project_root,
            "reference": null,
            "commit": commit,
            "instructions": "Bench lifecycle attempt.",
            "effect_state": "observational",
            "occurred_at": now_micros(),
        }),
    )
    .await;
    let identity = started
        .as_ref()
        .ok()
        .and_then(|v| dig(v, "identity").cloned())
        .unwrap_or(Value::Null);
    if started.is_err() {
        return (started, identity, String::new());
    }
    let terminal =
        |state: &str| matches!(state, "succeeded" | "failed" | "timed_out" | "cancelled");
    let settled = wait_work_attempt(
        harness,
        project_root,
        status(attempt_id),
        &["running", "succeeded", "failed", "timed_out", "cancelled"],
    )
    .await;
    let mut state = match settled {
        Ok(status) => dig_str(&status, "state").unwrap_or("").to_owned(),
        Err(error) => return (Err(error), identity, String::new()),
    };
    // Never cancel a lease that has not crossed the running boundary. A
    // cancellation request racing mark-running can invalidate the original
    // lease and leaves retry evidence unusable.
    if state != "running" && !terminal(&state) {
        return (started, identity, state);
    }
    // The topology admits a single global active attempt: any non-terminal
    // residue here, not just a running one, occupies that slot for every
    // later start_attempt.
    for _ in 0..3 {
        if terminal(&state) {
            break;
        }
        let _ = call(
            "tracedecay_work_cancel_attempt",
            json!({
                "task_id": task_id,
                "run_id": run_id,
                "attempt_id": attempt_id,
                "request_id": format!("cancel.bench.{attempt_id}.{}", now_micros()),
                "occurred_at": now_micros(),
            }),
        )
        .await;
        for _ in 0..60 {
            if let Ok(s) = call("tracedecay_work_attempt_status", status(attempt_id)).await
                && let Some(st) = dig_str(&s, "state")
                && terminal(st)
            {
                state = st.to_owned();
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    (started, identity, state)
}

/// The shared worktop: a running `work_*`/`workflow_*` capability, or the
/// recorded reason it could not be minted.
async fn seed_work(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    let (Some(project_id), Some(repository_id), Some(commit)) = (
        seeds.project_id.clone(),
        seeds.repository_id.clone(),
        seeds.head_commit.clone(),
    ) else {
        seeds
            .skipped
            .push("work_*: project/repository/commit identity unavailable".to_owned());
        return;
    };
    let call = |tool: &'static str, args: Value| {
        let mut args = args;
        args["format"] = json!("json");
        call_transient(harness, project_root, tool, args)
    };
    let mut work = WorkSeeds {
        selection: json!({
            "selection": "relations",
            "relation_scopes": [{
                "kind": "repository",
                "project_id": project_id,
                "repository_id": repository_id,
            }],
        }),
        commit: commit.clone(),
        ..WorkSeeds::default()
    };
    let suffix = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let occurred_at = now_micros();
    let ids = [
        ("initiative_id", format!("initiative.bench.{suffix}")),
        ("plan_id", format!("plan.bench.{suffix}")),
        ("milestone_id", format!("milestone.bench.{suffix}")),
        ("task_id", format!("task.bench.{suffix}")),
        ("proposal_id", format!("proposal.bench.{suffix}")),
        ("run_id", format!("run.bench.{suffix}")),
        ("attempt_id", format!("attempt.bench.{suffix}")),
        ("dup_attempt_id", format!("attempt.dup.bench.{suffix}")),
    ];
    let id = |k: &str| {
        ids.iter()
            .find(|(key, _)| *key == k)
            .map(|(_, v)| v.clone())
            .unwrap()
    };
    work.task_id = id("task_id");
    work.run_id = id("run_id");
    work.attempt_id = id("attempt_id");

    // An accepted proposal only admits execution when its route recommends a
    // provider, so a Work executable binding must be configured first
    // (mirrors the mcp_suite attempt-provider fixture).
    if !seed_work_attempt_provider(harness, project_root, &project_id, seeds).await {
        return;
    }

    // ── create → generate → accept → admit → placement ──────────────────
    let create = work_create_change(&suffix, json!(occurred_at));
    let prepared = match opt_err(
        &mut seeds.skipped,
        "work_prepare_graph_mutation",
        call(
            "tracedecay_work_prepare_graph_mutation",
            json!({
                "selection": work.selection,
                "change": create,
                "evidence": [],
            }),
        )
        .await,
    ) {
        Some(v) => v,
        None => return,
    };
    let Some(request) = dig(&prepared, "request").cloned() else {
        seeds
            .skipped
            .push("work_create: prepare omitted request".to_owned());
        return;
    };
    if let Some(v) = opt_err(
        &mut seeds.skipped,
        "work_create",
        call("tracedecay_work_create", request).await,
    ) {
        if v.get("replayed") == Some(&Value::Bool(true)) {
            seeds
                .skipped
                .push("work_create: replayed a fresh request".to_owned());
            return;
        }
    } else {
        return;
    }

    let generated = match opt_err(
        &mut seeds.skipped,
        "work_generate_proposal",
        call(
            "tracedecay_work_generate_proposal",
            json!({
                "selection": work.selection,
                "task_id": work.task_id,
                "proposal_id": id("proposal_id"),
                "occurred_at": now_micros(),
            }),
        )
        .await,
    ) {
        Some(v) => v,
        None => return,
    };
    let proposal = dig(&generated, "proposal").cloned().unwrap_or(Value::Null);
    work.initial_version = dig(&generated, "verified_graph_version")
        .cloned()
        .unwrap_or(Value::Null);

    let prepared_accept = match opt_err(
        &mut seeds.skipped,
        "work_accept_proposal.prepare",
        call("tracedecay_work_prepare_graph_mutation", json!({
            "selection": work.selection,
            "change": {"change": "decide_proposal", "proposal": proposal, "disposition": "accepted"},
            "evidence": [],
        })).await,
    ) {
        Some(v) => v,
        None => return,
    };
    let Some(accept_request) = dig(&prepared_accept, "request").cloned() else {
        seeds
            .skipped
            .push("work_accept_proposal: prepare omitted request".to_owned());
        return;
    };
    let accepted = match opt_err(
        &mut seeds.skipped,
        "work_accept_proposal",
        call("tracedecay_work_accept_proposal", accept_request).await,
    ) {
        Some(v) => v,
        None => return,
    };
    let accepted_version = dig(&accepted, "verified_graph_version")
        .cloned()
        .unwrap_or(Value::Null);
    work.accepted_gv = dig(&accepted_version, "graph_version")
        .and_then(Value::as_i64)
        .unwrap_or(0);

    let prepared_admit = match opt_err(
        &mut seeds.skipped,
        "work_admit_execution.prepare",
        call(
            "tracedecay_work_prepare_graph_mutation",
            json!({
                "selection": work.selection,
                "change": {
                    "change": "admit_execution",
                    "task_id": work.task_id,
                },
                "evidence": [],
            }),
        )
        .await,
    ) {
        Some(v) => v,
        None => return,
    };
    let Some(admit_request) = dig(&prepared_admit, "request").cloned() else {
        seeds
            .skipped
            .push("work_admit_execution: prepare omitted request".to_owned());
        return;
    };
    let admitted = match call("tracedecay_work_admit_execution", admit_request.clone()).await {
        Ok(v) => v,
        Err(e) => {
            seeds
                .skipped
                .push(format!("work_admit_execution: {e} request={admit_request}"));
            return;
        }
    };
    work.execution_snapshot = dig(&admitted, "execution_snapshot")
        .cloned()
        .unwrap_or(Value::Null);

    let placement = json!({
        "task_id": work.task_id,
        "run_id": work.run_id,
        "target": {
            "kind": "no_managed_placement",
            "root": null,
            "network_free": true,
            "in_place_acknowledged": false,
        },
        "occurred_at": now_micros(),
    });
    let _ = call("tracedecay_work_placement_preflight", placement.clone()).await;
    let _ = opt_err(
        &mut seeds.skipped,
        "work_admit_placement",
        call("tracedecay_work_admit_placement", placement).await,
    );

    // ── start → status → cancel (attempt settles to a terminal state) ───
    let (started, identity, state) = settle_attempt(
        harness,
        project_root,
        &work.task_id,
        &work.run_id,
        &work.attempt_id,
        &work.execution_snapshot,
        &commit,
    )
    .await;
    if started.is_err() || identity.is_null() {
        seeds.skipped.push(format!(
            "work_start_attempt: seed attempt never admitted ({state})"
        ));
    } else {
        work.attempt_identity = identity;
    }
    if !matches!(
        state.as_str(),
        "succeeded" | "failed" | "timed_out" | "cancelled"
    ) {
        seeds.skipped.push(format!(
            "work_start_attempt: seed attempt left active ({state}); \
             it occupies the topology's single parallel-attempt slot"
        ));
    }
    // A second attempt gives duplicate-adjudication a pair of identities.
    // It starts on its own run: two attempts on one lease fence race the
    // first attempt's terminal transition and wedge both transitions on a
    // lease-fence conflict.
    let dup = id("dup_attempt_id");
    let (dup_started, dup_identity, dup_state) = settle_attempt(
        harness,
        project_root,
        &work.task_id,
        &format!("run.dup.bench.{suffix}"),
        &dup,
        &work.execution_snapshot,
        &commit,
    )
    .await;
    if dup_started.is_ok() && !dup_identity.is_null() {
        work.dup_attempt_identity = Some(dup_identity);
    }
    if dup_started.is_ok()
        && !matches!(
            dup_state.as_str(),
            "succeeded" | "failed" | "timed_out" | "cancelled"
        )
    {
        seeds.skipped.push(format!(
            "work_start_attempt: dup seed attempt left active ({dup_state})"
        ));
    }

    if let Ok(v) = call(
        "tracedecay_work_views",
        json!({
            "selection": work.selection,
            "mode": {"mode": "current"},
            "continuation": null,
            "observed_at": now_micros(),
        }),
    )
    .await
    {
        work.current_version = dig(&v, "verified_version").cloned().unwrap_or(Value::Null);
    }

    // ── workflow: repair environment pins, register + activate + run ─────
    let mut pins = serde_json::Map::from_iter([
        (
            "pinned_policy_digest".to_owned(),
            json!("sha256:".to_owned() + &"0".repeat(64)),
        ),
        (
            "pinned_configuration_digest".to_owned(),
            json!("sha256:".to_owned() + &"0".repeat(64)),
        ),
        (
            "pinned_catalog_digest".to_owned(),
            json!("sha256:".to_owned() + &"0".repeat(64)),
        ),
    ]);
    let definition_id = format!("workflow.bench.{suffix}");
    let mut definition = json!({
        "definition_id": definition_id,
        "definition_version": 1,
        "project_id": project_id,
        "steps": [{
            "step_id": "step.bench.inspect",
            "operation": "operation.work.start_attempt",
            "predecessors": [],
            "inputs": [],
            "outputs": [],
            "fan_out": null,
        }],
        "pinned_policy_digest": pins["pinned_policy_digest"],
        "pinned_configuration_digest": pins["pinned_configuration_digest"],
        "pinned_catalog_digest": pins["pinned_catalog_digest"],
    });
    let mut repaired = false;
    for _ in 0..(pins.len() + 1) {
        let resp = call_lenient(
            harness,
            project_root,
            "tracedecay_workflow_validate_definition",
            json!({"definition": definition}),
        )
        .await;
        let Ok(resp) = resp else { break };
        if repair_definition_pins(&resp, &mut pins) {
            for k in [
                "pinned_policy_digest",
                "pinned_configuration_digest",
                "pinned_catalog_digest",
            ] {
                if let Some(v) = pins.get(k) {
                    definition[k] = v.clone();
                }
            }
            continue;
        }
        repaired = true;
        break;
    }
    if repaired {
        // The catalog pin is only checked at activation, so drive the same
        // repair loop through register+activate until activation lands or a
        // non-pin denial stops it.
        let mut activated = false;
        let mut activated_version = 1;
        for attempt in 1..=(pins.len() + 1) {
            // Versions are immutable per definition_id — a repaired pin needs
            // a fresh version.
            definition["definition_version"] = json!(attempt);
            let _ = call(
                "tracedecay_workflow_register_definition",
                json!({"definition": definition}),
            )
            .await;
            match call_lenient(
                harness,
                project_root,
                "tracedecay_workflow_activate_definition",
                json!({
                    "definition_id": definition_id,
                    "definition_version": attempt,
                    // Each version's disposition starts at revision 1 after
                    // register; the number is not the definition_version.
                    "expected_revision": 1,
                    "format": "json",
                }),
            )
            .await
            {
                Ok(resp) => {
                    if repair_definition_pins(&resp, &mut pins) {
                        for k in [
                            "pinned_policy_digest",
                            "pinned_configuration_digest",
                            "pinned_catalog_digest",
                        ] {
                            if let Some(v) = pins.get(k) {
                                definition[k] = v.clone();
                            }
                        }
                        continue;
                    }
                    activated = dig(&resp, "problem").is_none();
                    activated_version = attempt;
                    if !activated {
                        seeds.skipped.push(format!(
                            "workflow seed: activate v{attempt} denied: {}",
                            dig(&resp, "code")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                        ));
                    }
                    break;
                }
                Err(error) => {
                    seeds.skipped.push(format!(
                        "workflow seed: activate v{attempt} transport-failed: {error}"
                    ));
                    break;
                }
            }
        }
        if !activated {
            seeds.skipped.push(String::from(
                "workflow seed: activation never landed; definition keeps unrepaired pins",
            ));
        }
        work.definition = definition.clone();
        work.definition_id = definition_id.clone();
        if activated {
            let run_id = format!("workflow-run.bench.{suffix}");
            let run = call("tracedecay_workflow_start_run", json!({
                "run_id": run_id,
                "definition_id": definition_id,
                "definition_version": activated_version,
                "provider": {
                    "route": {"provider_id": "provider.work.codex-cli", "route_id": "route.bench.workflow"},
                    "backend": "codex_cli",
                    "model": "bench",
                    "priority": 1,
                },
                "fan_out": null,
                "command_id": format!("command.workflow.bench.start.{suffix}"),
            })).await;
            if let Ok(run) = run {
                let mut all = Vec::new();
                objects(&run, &mut all);
                for obj in all {
                    if let Some(actor) = obj.get("actor").and_then(Value::as_str)
                        && let Some(scope) = obj.get("scope")
                        && scope.get("project_id").and_then(Value::as_str).is_some()
                    {
                        work.actor_id = actor.to_owned();
                        work.worktree_id = scope
                            .get("worktree_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                }
                if dig(&run, "run_id").and_then(Value::as_str).is_some() {
                    work.wf_run_id = Some(run_id);
                }
            }
        }
    } else {
        seeds
            .skipped
            .push("workflow_*: pin repair did not converge".to_owned());
    }

    seeds.work = Some(work);
}

/// The 5-query groups contributed by the coverage extension.
pub fn coverage_groups(ctx: &QueryContext) -> Vec<ToolGroup> {
    let mut groups = Vec::new();
    code::groups(ctx, &mut groups);
    graph::groups(ctx, &mut groups);
    git::groups(ctx, &mut groups);
    session::groups(ctx, &mut groups);
    memory::groups(ctx, &mut groups);
    admin::groups(ctx, &mut groups);
    admin::context_scout_groups(&mut groups);
    admin::context_scout_control_groups(&mut groups);
    admin::context_scout_mutation_groups(&mut groups);
    effects::groups(ctx, &mut groups);
    work::groups(ctx, &mut groups);
    groups
}

/// Convenience: one read query with `format: "json"`.
pub(crate) fn rq(tool: &'static str, label: &'static str, extra: Value) -> Query {
    let mut args = extra;
    args["format"] = json!("json");
    Query::read(label, tool, args)
}

/// Read query using the adapter's default presentation.
pub(crate) fn rqn(tool: &'static str, label: &'static str, args: Value) -> Query {
    Query::read(label, tool, args)
}

/// Effect query (fmt): the timed call gets `format: "json"`.
pub(crate) fn eq(
    tool: &'static str,
    label: &'static str,
    extra: Value,
    prime: crate::queries::PrimeFn,
) -> Query {
    let mut args = extra;
    args["format"] = json!("json");
    Query {
        label,
        tool,
        args,
        kind: crate::queries::QueryKind::Effect {
            prime,
            cleanup: None,
            repeatable: true,
        },
    }
}

/// Effect query without `format`.
pub(crate) fn eqn(
    tool: &'static str,
    label: &'static str,
    args: Value,
    prime: crate::queries::PrimeFn,
) -> Query {
    Query {
        label,
        tool,
        args,
        kind: crate::queries::QueryKind::Effect {
            prime,
            cleanup: None,
            repeatable: true,
        },
    }
}

/// Current wall-clock micros for `occurred_at`/horizon args.
pub(crate) fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// A prime factory that produces no setup calls (fresh-id effects only).
pub(crate) fn no_primes(_ctx: &QueryContext, _iter: u64) -> Vec<crate::queries::PrimeStep> {
    Vec::new()
}

/// `eq` + a post-timed cleanup chain (journaled restores for corpus-mutating
/// source edits).
pub(crate) fn eqc(
    tool: &'static str,
    label: &'static str,
    args: Value,
    prime: crate::queries::PrimeFn,
    cleanup: crate::queries::EffectCleanup,
) -> Query {
    let mut a = args;
    a["format"] = json!("json");
    Query {
        label,
        tool,
        args: a,
        kind: crate::queries::QueryKind::Effect {
            prime,
            cleanup: Some(cleanup),
            repeatable: true,
        },
    }
}

/// Sampled real file path (wrap-around like `pick`).
pub(crate) fn file_at(ctx: &QueryContext, i: usize) -> String {
    if ctx.seeds.sample_files.is_empty() {
        "missing".to_owned()
    } else {
        ctx.seeds.sample_files[i % ctx.seeds.sample_files.len()].clone()
    }
}
