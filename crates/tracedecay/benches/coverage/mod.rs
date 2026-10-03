//! Coverage extension: per-tool query groups for the canonical catalog
//! surface beyond the original ~25-tool bench slice, plus the seeded entity
//! state those tools need (facts, configuration revisions, LCM sessions,
//! code-query node identities, git preview inputs).
//!
//! Seeds are minted through the same producer calls the catalog sweep uses
//! (tests/tool_sweep_suite): real `fact_store_add`, `configuration_set`, and
//! `git_hunks` producers, never fabricated ids. A seed that cannot mint its
//! state records `seeds.skipped` and its groups are omitted rather than
//! measuring a degraded path.

mod admin;
mod code;
mod effects;
mod git;
mod graph;
mod memory;
mod session;
mod work;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay::daemon::ProductionProjectCompositionHarnessV1;
use tracedecay_domain::configuration::{
    ConfigurationValueV1, WORK_EXECUTABLE_BINDINGS_SETTING_KEY, WorkExecutableBindingV1,
    WorkExecutableCapabilityV1,
};
use tracedecay_domain::{
    ManifestDigestHasher, WorkApprovalPolicy, WorkContentLocationClassV1, WorkEffortClassV1,
    WorkEgressPolicy, WorkExecutableReference, WorkExecutionLimits, WorkFallbackTopology,
    WorkFilesystemPolicy, WorkOrdinalBandV1, WorkProviderBackendV1, WorkRouteCandidateV1,
    WorkRouteExecutionProfileV1, WorkSandboxPolicy,
};

use crate::queries::{Query, QueryContext, ToolGroup, call_json_tool};

/// Deterministic codex rollout session id seeded per repo before `open`.
pub(crate) fn bench_session_id(repo_name: &str) -> String {
    format!("td-bench-{repo_name}")
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
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "bench sweep assistant reply"}],
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

/// Entity state minted during context build; `None`/empty means the producer
/// failed and the dependent groups are skipped (the reason lands in `skipped`).
#[derive(Default)]
pub struct Seeds {
    pub project_id: Option<String>,
    pub repository_id: Option<String>,
    pub branch: Option<String>,
    pub head_commit: Option<String>,
    /// First-parent ancestor of `head_commit` for range-diff coverage.
    pub parent_commit: Option<String>,
    /// (fact_id, related_fact_id, search_query, [entities])
    pub fact_pair: Option<(String, String, String, Vec<String>)>,
    /// Scalar boolean key toggled during seed (diagnostics.prewarm.v1).
    pub config_key: Option<String>,
    /// Latest configuration revision after the seed set/unset pair.
    pub config_revision: Option<String>,
    /// The seeded revision (rollback target).
    pub config_rollback_target: Option<String>,
    /// Toggled scalar value to set next ({kind:boolean,value}).
    pub config_scalar: Option<Value>,
    /// Effective work.topology_policy.v1 value for protected previews.
    pub topology_policy: Option<Value>,
    /// Code-query node identities minted via `code_symbol_search` (distinct
    /// from graph node ids; these tools consume the query-side identity).
    pub code_node_ids: Vec<String>,
    /// A graph node whose rename_preview is unblocked (source-edit lane needs
    /// a symbol the policy can actually rename — most corpus symbols refuse
    /// with ambiguous/hazard evidence, so seed time probes candidates).
    pub rename_node: Option<Value>,
    /// Symbol qualified names whose per-op dry_run completed small enough to
    /// return `expected_state` inline (big symbols truncate the preview into
    /// a result handle, which timing must not depend on).
    pub replace_target: Option<String>,
    pub insert_target: Option<String>,
    pub move_target: Option<String>,
    /// Ingested codex session id for this repo (td-bench-<name>).
    pub lcm_session: Option<String>,
    /// Canonical occurrence id minted by lcm_load_session.
    pub lcm_message_id: Option<String>,
    /// multi_root scope_set committed via compare_and_swap (id/revision/digest).
    pub scope_set_id: Option<String>,
    pub scope_set_revision: Option<i64>,
    pub scope_set_digest: Option<String>,
    /// session_refresh_begin handle + its selector arguments.
    pub refresh_handle: Option<String>,
    /// Operation id returned alongside the begin handle.
    pub refresh_operation_id: Option<String>,
    pub refresh_selectors: Option<Value>,
    pub automation_run_id: Option<String>,
    /// git_hunks producer output for `git_preview`/`git_apply`.
    pub preview_input_id: Option<String>,
    pub hunk_digests: Vec<String>,
    /// Repo-relative file left modified for the git working-tree lane.
    pub dirty_file: Option<String>,
    /// Skill id minted via profile skill file drop (managed skills surface).
    pub skill_id: Option<String>,
    /// First worktree entry id seen in the seeded worktree inventory (cleanup
    /// lane targets `kind:"worktree"` objects).
    pub worktree_id: Option<String>,
    /// Changed path whose run_affected_tests plan maps to covering tests
    /// (minted a request handle at seed time).
    pub test_results_path: Option<String>,
    /// Reversible-truncation handle (`rh_…`) minted by a deliberately fat
    /// search response at seed time; feeds `tracedecay_retrieve`.
    pub retrieve_handle: Option<String>,
    /// Tools whose application authority never mounts under the bench
    /// composition, discovered by seed-time probes — their groups are
    /// omitted and named in `skipped`.
    pub unavailable_tools: std::collections::BTreeSet<String>,
    /// Tools/families whose seed producer failed — the bench prints these so
    /// skipped coverage is visible instead of silent.
    pub skipped: Vec<String>,
    /// Real repo-relative file paths sampled from `tracedecay_files`.
    pub sample_files: Vec<String>,
    /// Work/workflow lifecycle artifacts minted by `seed_work`.
    pub work: Option<WorkSeeds>,
    /// A fresh work-executable binding committed this run; route resolution
    /// reads bindings at composition open, so the caller must reopen the
    /// harness once (then rebuild context) before the admit lane can pass.
    pub needs_reopen_for_provider: bool,
    /// Second bench branch pinned at HEAD~1 (branch_diff base ref; the tool
    /// resolves branch names, not raw oids).
    pub base_branch: Option<String>,
    /// Native-integration journey artifacts minted at seed (inventory →
    /// stack_snapshot → preflight → approve). Absent = family skipped.
    pub native: Option<NativeSeeds>,
}

/// Minted once per run: the stack_snapshot body that seals, plus the live
/// transaction the status lane reads.
#[derive(Clone, Default)]
pub struct NativeSeeds {
    /// Validated `stack_snapshot` request body — the groups replay it to mint
    /// fresh per-iteration transactions.
    pub snapshot_body: Value,
    /// transaction_id of the seed-minted (approved) transaction.
    pub transaction_id: String,
}

/// Artifacts of one real disposable Work lifecycle plus one workflow
/// definition/run, minted once so the ~45 work_* and workflow_* tools measure
/// against real graph state. Field absence means that leg failed and only its
/// dependent groups skip.
#[derive(Default)]
pub struct WorkSeeds {
    pub selection: Value,
    pub task_id: String,
    pub run_id: String,
    pub attempt_id: String,
    /// Started-then-cancelled terminal attempt identity {task,run,attempt}.
    pub attempt_identity: Value,
    /// Second terminal attempt for duplicate-adjudication reads/effects.
    pub dup_attempt_identity: Option<Value>,
    /// verified_graph_version from the first generate_proposal.
    pub initial_version: Value,
    /// accepted graph_version integer (base for admit_execution).
    pub accepted_gv: i64,
    /// Current verified_version from a post-lifecycle work_views.
    pub current_version: Value,
    /// WorkExecutionSnapshot from admit_execution (start_attempt input).
    pub execution_snapshot: Value,
    /// Head commit id used by the seed attempts.
    pub commit: String,
    /// workflow_definition validated with environment-repaired pins.
    pub definition: Value,
    pub definition_id: String,
    pub actor_id: String,
    /// worktree_id from the start_run authority (handoff scope component).
    pub worktree_id: Option<String>,
    /// A live workflow run (id + current sequence) for run-control effects.
    pub wf_run_id: Option<String>,
    /// Projection generation pair for duplicate-adjudication evidence.
    pub work_generation: Option<Value>,
    pub topology_generation: Option<Value>,
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
    files: &[Value],
) -> Seeds {
    let mut seeds = Seeds::default();
    seeds.sample_files = files
        .iter()
        .filter_map(|f| f.get("path").and_then(Value::as_str))
        .take(64)
        .map(str::to_owned)
        .collect();
    let call = |tool: &'static str, args: Value| call_json_tool(harness, project_root, tool, args);

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

    // ── automation run id (only present if a run exists) ──────────────────
    if let Some(v) = call(
        "tracedecay_automation_run_list",
        json!({"limit": 1, "format": "json"}),
    )
    .await
    .ok()
    {
        seeds.automation_run_id = dig_str(&v, "run_id").map(str::to_owned);
    }
    if seeds.automation_run_id.is_none() {
        seeds
            .skipped
            .push("automation_run_*: no automation runs in a fresh composition".to_owned());
    }

    // ── multi_root scope_set: CAS-commit the project root once so the
    // federated read lane has a persisted identity (id/revision/digest). ──
    if let (Some(project_id), Some(root)) = (
        seeds.project_id.clone(),
        project_root.to_str().map(str::to_owned),
    ) {
        // Persisted scope sets are actor-sealed (only the persisting actor may
        // read them back), and the bench profile survives runs — a fixed id
        // collides with an earlier actor's set and stays invisible forever.
        // Mint a per-run identity so the set is always ours.
        let scope_set_id = format!("scope-set.bench.{project_id}.{}", now_micros());
        match call(
            "tracedecay_multi_root_scope_set_compare_and_swap",
            json!({
                "scope_set_id": scope_set_id,
                "expected_revision": null,
                "roots": [{"project_id": project_id, "root": root}],
            }),
        )
        .await
        {
            Ok(v) => {
                seeds.scope_set_id = Some(scope_set_id.clone());
                seeds.scope_set_revision = dig(&v, "scope_set_revision")
                    .or_else(|| dig(&v, "revision"))
                    .and_then(Value::as_i64);
                seeds.scope_set_digest = dig_str(&v, "scope_set_digest")
                    .or_else(|| dig_str(&v, "digest"))
                    .map(str::to_owned);
                if seeds.scope_set_revision.is_none() || seeds.scope_set_digest.is_none() {
                    seeds
                        .skipped
                        .push("multi_root_execute: CAS omitted revision/digest".to_owned());
                }
            }
            Err(_) => {
                // Persisted from a prior run (or a pre-reopen build): adopt the
                // committed set's identities instead of minting a duplicate.
                match call(
                    "tracedecay_multi_root_scope_set_read",
                    json!({"scope_set_id": scope_set_id, "format": "json"}),
                )
                .await
                {
                    Ok(v) => {
                        seeds.scope_set_id = Some(scope_set_id.clone());
                        seeds.scope_set_revision = dig(&v, "scope_set_revision")
                            .or_else(|| dig(&v, "revision"))
                            .and_then(Value::as_i64);
                        seeds.scope_set_digest = dig_str(&v, "scope_set_digest")
                            .or_else(|| dig_str(&v, "digest"))
                            .map(str::to_owned);
                        if seeds.scope_set_revision.is_none() || seeds.scope_set_digest.is_none() {
                            seeds.skipped.push(
                                "multi_root_execute: scope_set read omitted revision/digest"
                                    .to_owned(),
                            );
                        }
                    }
                    Err(e) => seeds
                        .skipped
                        .push(format!("multi_root_scope_set_compare_and_swap: {e}")),
                }
            }
        }
    }

    // ── git preview input: dirty one tracked file now so the git watcher
    // has the whole remaining seed window to observe it; hunk poll runs last
    seed_dirty_worktree(project_root, files, &mut seeds).await;

    // Some read surfaces mount their application authority lazily; probe the
    // ones seen cold in runs so an unmountable lane degrades to a named skip
    // rather than a timed panic.
    let probe_path = files
        .first()
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
    // branch-scoped surfaces need one real ref, so mint a bench branch at
    // HEAD (the clone is bench-owned scratch).
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

    // ── native integration: inventory → stack_snapshot → preflight/approve ─
    seed_native_integration(harness, project_root, &mut seeds).await;

    // ── work/workflow lifecycle: one disposable task graph + workflow run ──
    seed_work(harness, project_root, &mut seeds).await;

    // ── affected-tests result read: find a changed path that maps to tests ─
    seed_affected_tests(harness, project_root, files, &mut seeds).await;

    // ── managed skill: drop a skill into the profile skills dir ──────────
    seed_skill(&mut seeds);

    // ── response handle: a fat search truncates → reversible handle ──────
    seed_retrieve_handle(harness, project_root, &mut seeds).await;

    // ── hunk poll last: the worktree has been dirty since seed start ──────
    seed_git_preview(harness, project_root, &mut seeds).await;

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
    let call = |tool: &'static str, args: Value| call_json_tool(harness, project_root, tool, args);

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
    if let Some(v) = call(
        "tracedecay_configuration_get",
        json!({"key": "work.topology_policy.v1", "format": "json"}),
    )
    .await
    .ok()
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
    let call = |tool: &'static str, args: Value| call_json_tool(harness, project_root, tool, args);
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
            let fid = dig_str(&a, "fact_id")
                .map(str::to_owned)
                .or_else(|| dig_str(&a, "id").map(str::to_owned));
            let rid = dig_str(&b, "fact_id")
                .map(str::to_owned)
                .or_else(|| dig_str(&b, "id").map(str::to_owned));
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
                seeds.refresh_handle = Some(h);
                seeds.refresh_operation_id = dig_str(&v, "operation_id").map(str::to_owned);
                seeds.refresh_selectors = Some(selectors);
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
    if std::fs::write(&abs, format!("{original}\n// bench hunk line\n")).is_err() {
        seeds
            .skipped
            .push(format!("git_preview/apply: cannot dirty {rel}"));
        return;
    }
    // Hunk evidence omits paths carrying text/eol normalization attributes,
    // and corpora like scipy blanket everything with `* text=auto`. A
    // per-path `-text` override in `.git/info/attributes` (repo-local
    // metadata, never committed) un-filters exactly the dirty file.
    let info_dir = project_root.join(".git/info");
    let info_attrs = info_dir.join("attributes");
    let override_line = format!("{rel} -text");
    let existing = std::fs::read_to_string(&info_attrs).unwrap_or_default();
    if !existing.lines().any(|line| line == override_line) {
        let mut merged = existing;
        if !merged.is_empty() && !merged.ends_with('\n') {
            merged.push('\n');
        }
        merged.push_str(&override_line);
        merged.push('\n');
        if std::fs::create_dir_all(&info_dir)
            .and_then(|_| std::fs::write(&info_attrs, merged))
            .is_err()
        {
            seeds
                .skipped
                .push("git_preview/apply: cannot write .git/info/attributes".to_owned());
            return;
        }
    }
    seeds.dirty_file = Some(rel);
}

/// affected_tests/test_results consume a request handle minted by a
/// run_affected_tests call whose changed path maps to covering tests. Probe
/// test-shaped paths first — a test file is covered by itself — then a few
/// ordinary corpus files.
async fn seed_affected_tests(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    files: &[Value],
    seeds: &mut Seeds,
) {
    let mut candidates: Vec<String> = files
        .iter()
        .filter_map(Value::as_str)
        .filter(|p| p.contains("test"))
        .take(3)
        .map(str::to_owned)
        .collect();
    candidates.extend(
        files
            .iter()
            .filter_map(Value::as_str)
            .take(2)
            .map(str::to_owned),
    );
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
            Ok(v)
                if extract_token(&v, "digany:request_handle,handle,run_handle,result_handle")
                    .is_some() =>
            {
                seeds.test_results_path = Some(path);
                return;
            }
            _ => {}
        }
    }
    seeds
        .skipped
        .push("affected_tests/test_results: no changed path maps to covering tests".to_owned());
}

/// Mint a reversible response handle: `tracedecay_search` over the indexed
/// corpus returns a payload past the 15KB truncation budget, so the envelope
/// stores the full response and emits `handle` for `tracedecay_retrieve`.
async fn seed_retrieve_handle(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    seeds: &mut Seeds,
) {
    for query in ["test", "the", "data"] {
        match call_json_tool(
            harness,
            project_root,
            "tracedecay_search",
            json!({"query": query, "limit": 50, "format": "json"}),
        )
        .await
        {
            Ok(v) => {
                if let Some(handle) = dig_str(&v, "handle") {
                    seeds.retrieve_handle = Some(handle.to_owned());
                    return;
                }
            }
            Err(e) => {
                seeds.skipped.push(format!("retrieve: {e}"));
                return;
            }
        }
    }
    seeds
        .skipped
        .push("retrieve: no search response truncated".to_owned());
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
                    seeds
                        .skipped
                        .push(format!("git_preview/apply: hunks stayed empty: {v}"));
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
        Some(head),
        Some(parent),
        Some(branch),
        Some(base_branch),
    ) = (
        seeds.project_id.clone(),
        seeds.repository_id.clone(),
        seeds.scope_set_id.clone(),
        seeds.scope_set_revision,
        seeds.scope_set_digest.clone(),
        seeds.head_commit.clone(),
        seeds.parent_commit.clone(),
        seeds.branch.clone(),
        seeds.base_branch.clone(),
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
            dig(&v, "scope_set_revision")
                .or_else(|| dig(&v, "revision"))
                .and_then(Value::as_i64)
                .unwrap_or(scope_set_revision),
            dig_str(&v, "scope_set_digest")
                .or_else(|| dig_str(&v, "digest"))
                .map(str::to_owned)
                .unwrap_or(scope_set_digest),
        ),
        Err(_) => (scope_set_revision, scope_set_digest),
    };
    let inventory = match call_json_tool(
        harness,
        project_root,
        "tracedecay_worktree_inventory",
        json!({
            "scope_set_id": scope_set_id,
            "scope_set_revision": scope_set_revision,
            "scope_set_digest": scope_set_digest,
            "target": {
                "kind": "repository",
                "project_id": project_id,
                "repository_id": repository_id,
            },
            "format": "json",
        }),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            seeds
                .skipped
                .push(format!("native_integration_*: worktree_inventory: {e}"));
            return;
        }
    };
    let Some(inventory_snapshot_id) = dig_str(&inventory, "snapshot_id")
        .or_else(|| dig_str(&inventory, "inventory_snapshot_id"))
        .map(str::to_owned)
    else {
        seeds.skipped.push(format!(
            "native_integration_*: inventory returned no snapshot id: {inventory}"
        ));
        return;
    };
    let Some(inventory_epoch) = dig(&inventory, "epoch")
        .and_then(Value::as_i64)
        .or_else(|| dig(&inventory, "inventory_epoch").and_then(Value::as_i64))
    else {
        seeds.skipped.push(format!(
            "native_integration_*: inventory returned no epoch: {inventory}"
        ));
        return;
    };
    let worktree_id = dig_str(&inventory, "worktree_id")
        .map(str::to_owned)
        .unwrap_or_else(|| "worktree.bench".to_owned());
    seeds.worktree_id = Some(worktree_id.clone());
    let grant_digest = dig_str(&inventory, "grant_digest")
        .map(str::to_owned)
        .unwrap_or_else(|| format!("sha256:{}", "0".repeat(64)));
    let policy_digest = dig_str(&inventory, "policy_digest")
        .map(str::to_owned)
        .unwrap_or_else(|| grant_digest.clone());
    let scope_digest = dig_str(&inventory, "scope_digest")
        .map(str::to_owned)
        .unwrap_or_else(|| scope_set_digest.clone());

    let worktree = worktree_id.clone();
    let project = project_id.clone();
    let repo = repository_id.clone();
    let stack_node = |reference: &str| {
        json!({
            "project_id": project,
            "repository_id": repo,
            "worktree_id": worktree,
            "reference": reference,
            "scope_digest": scope_digest,
        })
    };
    let graph_node = |node_id: &str, reference: &str, tip: &str| {
        json!({
            "node_id": node_id,
            "project_id": project_id,
            "repository_id": repository_id,
            "reference": reference,
            "tip": tip,
            "worktree_id": worktree_id,
        })
    };
    let body = json!({
        "source": stack_node(&format!("refs/heads/{base_branch}")),
        "destination": stack_node(&format!("refs/heads/{branch}")),
        "authorized_scope_set_id": scope_set_id,
        "authorized_scope_set_revision": scope_set_revision,
        "authorized_scope_set_digest": scope_set_digest,
        "inventory_snapshot_id": inventory_snapshot_id,
        "inventory_epoch": inventory_epoch,
        "selection": {
            "kind": "declared_stack_edge",
            "binding": {
                "stack_id": "stack.bench",
                "revision_id": format!("stack-revision.bench.{}", now_micros()),
                "nodes": [
                    graph_node("node.destination", &format!("refs/heads/{branch}"), &head),
                    graph_node("node.source", &format!("refs/heads/{base_branch}"), &parent),
                ],
                "edges": [{
                    "dependency": "node.source",
                    "dependent": "node.destination",
                }],
                "source_node_id": "node.source",
                "destination_node_id": "node.destination",
                "direction": "propagate_dependency_to_dependent",
            },
        },
        "grant_digest": grant_digest,
        "policy_digest": policy_digest,
        "format": "json",
    });
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
    let Some(transaction_id) = dig_str(&preflight, "transaction_id").map(str::to_owned) else {
        seeds.skipped.push(format!(
            "native_integration_*: preflight returned no transaction_id: {preflight}"
        ));
        return;
    };
    seeds.native = Some(NativeSeeds {
        snapshot_body: body,
        transaction_id,
    });
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

fn seed_skill(seeds: &mut Seeds) {
    // Managed skills live in the composed profile; creating one requires the
    // automation CLI which the bench process cannot drive mid-mount. Probe
    // `skill_list` instead — only skips cleanly if the surface is empty.
    seeds.skill_id = None;
    seeds
        .skipped
        .push("skill_view: no profile skill seed (CLI-only producer)".to_owned());
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

/// `<field> expected sha256:<hex>` repair hints published by
/// `workflow_validate_definition` denials — mirrors the suite's
/// `_WORKFLOW_PIN_MISMATCH` contract.
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
            let digest = &message[expect_pos + "expected ".len()..];
            let digest: String = digest
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == ':')
                .collect();
            if digest.len() == 71 {
                pins.insert(field.to_owned(), Value::String(digest));
                found = true;
            }
        }
        // Fallback for the exact suite phrasing: "<field> expected sha256:<hex>, observed"
        if !found && let Some(expect_pos) = message.find(" expected sha256:") {
            let field = message[..expect_pos]
                .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next();
            let digest = &message[expect_pos + " expected ".len()..];
            let digest: String = digest
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == ':')
                .collect();
            if let (Some(field), true) = (field, digest.len() == 71)
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

/// Configure one Work executable binding (a no-op provider script + CodexCli
/// route) so a generated proposal names a route and execution can be
/// admitted — an abstained route refuses `admit_execution` outright.
async fn seed_work_attempt_provider(
    harness: &ProductionProjectCompositionHarnessV1,
    project_root: &Path,
    project_id: &str,
    seeds: &mut Seeds,
) -> bool {
    let call = |tool: &'static str, args: Value| call_json_tool(harness, project_root, tool, args);
    let executable_bytes: &[u8] = b"#!/bin/sh\nexit 0\n";
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
    let canonical_path_text = canonical_path.to_string_lossy().into_owned();
    let mut hasher = ManifestDigestHasher::new();
    hasher.update(executable_bytes);
    let Some(binding) = (|| {
        let executable = WorkExecutableReference::new(
            "executable.work.bench-provider".to_owned(),
            hasher.finalize().ok()?,
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
                maximum_duration_micros: 60_000_000,
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
    let current_text = json!(current).to_string();
    if current_text.contains("executable.work.bench-provider")
        && current_text.contains(&canonical_path_text)
    {
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

/// Start one observational attempt, ride the spawn boundary, cancel it into a
/// terminal state, and return (start result, identity, terminal state).
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
        call_json_tool(harness, project_root, tool, args)
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
    let mut state = String::new();
    for _ in 0..60 {
        if let Ok(s) = call("tracedecay_work_attempt_status", status(attempt_id)).await {
            state = dig_str(&s, "state").unwrap_or("").to_owned();
            if state == "running"
                || matches!(
                    state.as_str(),
                    "succeeded" | "failed" | "timed_out" | "cancelled"
                )
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if state == "running" {
        let _ = call(
            "tracedecay_work_cancel_attempt",
            json!({
                "task_id": task_id,
                "run_id": run_id,
                "attempt_id": attempt_id,
                "request_id": format!("cancel.bench.{attempt_id}"),
                "occurred_at": now_micros(),
            }),
        )
        .await;
        for _ in 0..60 {
            if let Ok(s) = call("tracedecay_work_attempt_status", status(attempt_id)).await
                && let Some(st) = dig_str(&s, "state")
                && matches!(st, "succeeded" | "failed" | "timed_out" | "cancelled")
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
        call_json_tool(harness, project_root, tool, args)
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
    // A second attempt gives duplicate-adjudication a pair of identities.
    let dup = id("dup_attempt_id");
    let (dup_started, dup_identity, _) = settle_attempt(
        harness,
        project_root,
        &work.task_id,
        &work.run_id,
        &dup,
        &work.execution_snapshot,
        &commit,
    )
    .await;
    if dup_started.is_ok() && !dup_identity.is_null() {
        work.dup_attempt_identity = Some(dup_identity);
    }

    // ── current graph read for verified versions + generation ids ────────
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
        work.work_generation = dig(&v, "work_generation").cloned();
        work.topology_generation = dig(&v, "topology_generation").cloned();
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
                    break;
                }
                Err(_) => break,
            }
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

/// Read query without a `format` arg — the work/workflow/multi_root surfaces
/// reject `format` (they emit one structured wire shape).
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
    Query::effect(label, tool, args, prime)
}

/// Effect query without `format`.
pub(crate) fn eqn(
    tool: &'static str,
    label: &'static str,
    args: Value,
    prime: crate::queries::PrimeFn,
) -> Query {
    Query::effect(label, tool, args, prime)
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
    Query::effect_with_cleanup(label, tool, a, prime, cleanup)
}

/// Sampled real file path (wrap-around like `pick`).
pub(crate) fn file_at(ctx: &QueryContext, i: usize) -> String {
    if ctx.seeds.sample_files.is_empty() {
        "missing".to_owned()
    } else {
        ctx.seeds.sample_files[i % ctx.seeds.sample_files.len()].clone()
    }
}
