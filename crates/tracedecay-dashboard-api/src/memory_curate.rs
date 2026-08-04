//! `tracedecay memory curate` — dashboard-free curation core.
//!
//! The similarity-dedup curate and the LLM-review tier's plan/apply halves run
//! directly against the project memory store, so automation can call the CLI
//! without the dashboard server or a host wrapper.
//!
//! The LLM tier mirrors the LCM summarizer's two-phase `needs_summary` →
//! `provided` contract: this binary never calls an LLM itself. `--llm` emits
//! a `llm_review` request (clusters + chat messages); the caller runs the
//! one-shot review with whatever LLM it owns and feeds the strict-JSON ops back through
//! `--llm-ops`, which validates them against freshly recomputed clusters
//! (the evidence guard) and applies them through the canonical store paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Map, Value, json};
use tokio::sync::RwLock;

use super::memory_queries::normalize_fact_metadata;
use super::memory_service::{
    apply_delete_op, apply_merge_op, build_delete_plan, delete_fact, similarity_computation,
};
use super::util::{qmarks, query_rows};
use super::{DashboardAccountingMode, DashboardState, code_diagnostics_broker, token_count};
use crate::db::Database;
use crate::errors::{Result, TraceDecayError};
use crate::memory::store::MemoryStore;
use crate::memory::types::MemoryGroomingOperation;

pub const CURATION_DEFAULT_MAX_CLUSTERS: usize = 12;
pub const CURATION_DEFAULT_MIN_CONFIDENCE: f64 = 0.5;
const CURATION_CLUSTER_CLASSIFICATIONS: [&str; 2] = ["likely_duplicate", "merge_candidate"];

/// Adapted from the `holographic_plus` curator's one-shot LLM review tier.
const CURATION_SYSTEM_PROMPT: &str = "You are a memory hygiene engine for an AI agent's long-term fact store. \
You are given candidate fact clusters and must return STRICT JSON \
describing one op per reviewed cluster. NEVER invent facts. Be \
conservative: only act when confident.\n\n\
Duplicate policy: semantic relatedness is not enough. Only merge facts \
when they assert the same durable fact about the same subject, with \
matching key nouns/numbers/entities or direct textual evidence. Related \
facts, same-topic facts, implementation notes about the same project, \
and facts that merely share an entity should remain separate (use \
\"keep\").\n\n\
Conflict policy: when two facts about the SAME subject conflict, keep \
the higher-trust one and delete the stale one. Only use age/recency \
after the same-subject / same-claim conflict is established. Freshness \
signals, in order, are asserted_at, effective_at, observed_at, occurred_at, \
then created_at; these may appear in metadata. updated_at is maintenance \
metadata, never truth freshness. If the \
facts describe an EVOLUTION over time (a preference pivot, not a true \
contradiction, e.g. 'used React' then 'switched to Vue'), emit a merge \
whose merged_content is ONE time-aware fact built strictly from the \
cluster's own text. Distinct contexts that merely look similar are NOT \
contradictions - leave them with \"keep\".\n\n\
There is NO archive: delete and merge losers are removed permanently, \
so prefer \"keep\" whenever unsure.\n\n\
Return JSON of shape: {\"ops\": [ ... ]}. Each op MUST include: \
cluster_id (string, from the input), op (one of merge, delete, keep, \
normalize_tags, merge_entities, add_alias, link_facts, repair_vector), \
confidence (0.0-1.0), and reason (short string). Use op \"keep\" for \
reviewed clusters that need no change; do not omit keep reviews.\n\
Per-op required fields:\n\
  merge: {\"winner_id\": <id>, \"loser_ids\": [<id>, ...]} and optional \
\"merged_content\" (string) when the winner's text should be replaced \
by a consolidated fact.\n\
  delete: {\"fact_id\": <id>}\n\
  normalize_tags: {\"fact_id\": <id>, \"tags\": [<canonical_tag>, ...], \
\"evidence_fact_ids\": [<id>, ...]}\n\
  merge_entities: {\"winner_entity_id\": <id>, \"loser_entity_ids\": [<id>, ...], \
\"evidence_fact_ids\": [<id>, ...]}\n\
  add_alias: {\"entity_id\": <id>, \"alias\": <bounded alias>, \
\"evidence_fact_ids\": [<id>, ...]}\n\
  link_facts: {\"source_fact_id\": <id>, \"target_fact_id\": <id>, \
\"relation\": <supports|contradicts|supersedes|derived_from>, \
\"source\": <provenance>, \"evidence_fact_ids\": [<id>, ...]}\n\
  repair_vector: {\"fact_id\": <id>, \"evidence_fact_ids\": [<id>, ...]}\n\
Only reference fact ids that appear in the input clusters or in \
hygiene_candidates. Return ONLY the JSON object.\n\n\
Hygiene categories: the input may also carry \"hygiene_candidates\" — \
deterministic rule-flagged evidence with status=\"candidate\", \
review_required=true, and recommended_op hints. Review these candidates with \
the same conservatism; do not treat them as already-approved operations. \
secret_like: flagged as credential-like content; delete unless it is clearly \
a false positive (e.g. prose ABOUT secret handling with no actual \
credential). transient: looks like ephemeral run output (ports, PIDs, temp \
paths, run logs); delete unless it encodes a durable decision. supersession: \
a negation/state-change cue pairs an older fact with a newer one; confirm \
from the texts which fact is current, delete the stale one, or emit a \
time-aware merge when both matter. Usage signals: members may carry \
access_count / last_recalled_at (recall-search returns). Treat high access \
as evidence a fact is actively used — avoid deleting the more-accessed fact \
of a pair unless the duplication is near-exact. Low trust alone is never a \
delete reason; use it only to temper confidence.";

/// Options for one `tracedecay memory curate` run.
pub struct MemoryCurateOptions {
    /// Apply the similarity-dedup plan (and any provided `--llm-ops`)
    /// instead of reporting a dry-run preview.
    pub apply: bool,
    /// Include the LLM-review request (clusters + chat messages) in the
    /// report so an external LLM owner can produce ops for `--llm-ops`.
    pub llm: bool,
    /// Externally produced LLM ops (`{"ops": [...]}`) to validate against
    /// freshly recomputed clusters and apply (dry-run unless `apply`).
    pub llm_ops: Option<Value>,
    pub max_clusters: usize,
    pub min_confidence: f64,
}

impl Default for MemoryCurateOptions {
    fn default() -> Self {
        Self {
            apply: false,
            llm: false,
            llm_ops: None,
            max_clusters: CURATION_DEFAULT_MAX_CLUSTERS,
            min_confidence: CURATION_DEFAULT_MIN_CONFIDENCE,
        }
    }
}

fn user_state(
    memory_db: &Database,
    memory_db_path: &std::path::Path,
    profile_root: &std::path::Path,
    dashboard_root: &std::path::Path,
) -> DashboardState {
    let conn = memory_db.conn().clone();
    DashboardState {
        project_id: None,
        graph_conn: conn.clone(),
        database_guards: vec![Arc::new(memory_db.clone())],
        graph_db_path: memory_db_path.display().to_string(),
        mem_conn: conn,
        mem_db_path: memory_db_path.display().to_string(),
        lcm_conn: None,
        global_database_guards: Vec::new(),
        lcm_db_path: String::new(),
        lcm_scope: "user".to_string(),
        accounting_store: None,
        accounting_mode: DashboardAccountingMode::default(),
        product_version: env!("CARGO_PKG_VERSION"),
        release_channel: "stable",
        pr_autotrack_reader: None,
        savings_db_path: String::new(),
        project_root: profile_root.to_path_buf(),
        storage_mode: "user".to_string(),
        store_root: profile_root.to_path_buf(),
        config_path: profile_root.join("config.json"),
        dashboard_root: dashboard_root.to_path_buf(),
        curation_activity: Arc::new(RwLock::new(Vec::new())),
        token_counts: Arc::new(token_count::TokenCountCache::new()),
        code_diagnostics: Arc::new(RwLock::new(code_diagnostics_broker(
            profile_root.to_path_buf(),
            crate::diagnostics::lsp::settings::CodeDiagnosticsSettings::default(),
        ))),
        code_diagnostics_backfill_started: Arc::new(AtomicBool::new(false)),
        automation_scheduler_reconciler: None,
        automation_writer: super::direct_dashboard_automation_writer(),
        automation_executor: None,
        skill_analytics_sync: None,
        profile_root_resolver: {
            let profile_root = profile_root.to_path_buf();
            Arc::new(move || Ok(profile_root.clone()))
        },
        managed_skill_exporter: Arc::new(|_, _| Box::pin(async { Vec::new() })),
        project_registry: None,
        project_state_builder: None,
    }
}

/// Runs memory curation against the profile-level user memory store.
pub async fn run_user_memory_curate(
    memory_db: &Database,
    memory_db_path: &std::path::Path,
    profile_root: &std::path::Path,
    dashboard_root: &std::path::Path,
    options: &MemoryCurateOptions,
) -> Result<Value> {
    let state = user_state(memory_db, memory_db_path, profile_root, dashboard_root);
    run_memory_curate_with_state(&state, options).await
}

pub async fn run_memory_curate_with_state(
    state: &DashboardState,
    options: &MemoryCurateOptions,
) -> Result<Value> {
    let mut report = Map::new();
    report.insert("mode".to_string(), json!("similarity_dedup"));
    report.insert("dry_run".to_string(), json!(!options.apply));

    // Externally produced LLM ops are validated and applied FIRST: they were
    // planned against the current store, and running the similarity-dedup
    // deletions beforehand would invalidate the very clusters the ops
    // reference (their fact ids would already be gone).
    if let Some(provided) = options.llm_ops.as_ref() {
        let clusters = build_clusters(state, options.max_clusters).await?;
        // Evidence guard: cluster members plus the deterministic hygiene
        // candidates (recomputed against the same pre-apply state the ops
        // were planned on) are the only legal delete targets.
        let mut allowed_ids: BTreeSet<i64> = cluster_fact_ids(&clusters);
        let (_, pre_apply_hygiene_candidates, _, _) =
            build_delete_plan(state)
                .await
                .map_err(|message| TraceDecayError::Config {
                    message: format!("curation analysis failed: {message}"),
                })?;
        allowed_ids.extend(hygiene_candidate_fact_ids(&pre_apply_hygiene_candidates));
        let raw_ops = provided
            .get("ops")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: "--llm-ops payload must be a JSON object with an `ops` array".to_string(),
            })?;
        let (valid, rejected) = validate_llm_ops(&raw_ops, &allowed_ids, options.min_confidence);
        let mut llm_report = Map::new();
        llm_report.insert("clusters_reviewed".to_string(), json!(clusters.len()));
        llm_report.insert("rejected_ops".to_string(), Value::Array(rejected));
        if options.apply {
            let mut results = Vec::new();
            let mut applied = 0i64;
            if let Err(error) = prevalidate_destructive_ops(state, &valid).await {
                results.extend(valid.iter().map(|op| {
                    json!({
                        "status": "error",
                        "error": error.to_string(),
                        "op": op,
                    })
                }));
            } else {
                let grooming_ops = parse_grooming_ops(&valid)?;
                if !grooming_ops.is_empty() {
                    let store = MemoryStore::new(&state.mem_conn);
                    let grooming_report = store
                        .apply_grooming_batch(&grooming_ops, options.min_confidence)
                        .await?;
                    applied += grooming_ops.len() as i64;
                    results.push(json!({
                        "status": "applied",
                        "operations": grooming_ops.len(),
                        "grooming": grooming_report,
                    }));
                }
                for op in &valid {
                    let (result, ok) = match op.get("op").and_then(Value::as_str) {
                        Some("delete") => apply_delete_op(state, op).await,
                        Some("merge") => apply_merge_op(state, op).await,
                        Some(
                            "normalize_tags" | "merge_entities" | "add_alias" | "link_facts"
                            | "repair_vector",
                        ) => continue,
                        _ => (json!({ "status": "error", "error": "unknown op" }), false),
                    };
                    if ok {
                        applied += 1;
                    }
                    results.push(result);
                }
            }
            llm_report.insert("applied".to_string(), json!(applied));
            llm_report.insert("results".to_string(), Value::Array(results));
            llm_report.insert("ops".to_string(), Value::Array(valid));
        } else {
            llm_report.insert("ops".to_string(), Value::Array(valid));
            llm_report.insert(
                "note".to_string(),
                json!("dry run: re-run with --apply to execute these ops"),
            );
        }
        report.insert("llm_apply".to_string(), Value::Object(llm_report));
    }

    // Similarity-dedup tier (the dashboard's `/curate` semantics), planned on
    // the post-LLM-ops store state. `hygiene_candidates` carries deterministic
    // rule-based evidence (secret_like / transient / supersession); candidates
    // are never auto-applied here — the external LLM (or a human) confirms
    // them and feeds delete/merge ops back through `--llm-ops`.
    let (actions, hygiene_candidates, counts, total) =
        build_delete_plan(state)
            .await
            .map_err(|message| TraceDecayError::Config {
                message: format!("curation analysis failed: {message}"),
            })?;
    report.insert("counts".to_string(), Value::Object(counts));
    report.insert("hygiene_candidates".to_string(), hygiene_candidates.clone());
    report.insert(
        "coverage".to_string(),
        json!({ "scanned": total, "active_total": total }),
    );

    if options.apply {
        let mut applied = 0i64;
        let mut skipped = 0i64;
        for action in &actions {
            let Some(fact_id) = action.get("fact_id").and_then(Value::as_i64) else {
                skipped += 1;
                continue;
            };
            match delete_fact(state, fact_id).await {
                Ok(true) => applied += 1,
                Ok(false) | Err(_) => skipped += 1,
            }
        }
        report.insert(
            "applied_counts".to_string(),
            json!({ "delete": applied, "skipped": skipped }),
        );
    }
    report.insert("actions".to_string(), Value::Array(actions));

    if options.apply {
        let repair = super::memory_api::repair_derived_memory(state)
            .await
            .map_err(|message| TraceDecayError::Config {
                message: format!("memory derived-state repair failed: {message}"),
            })?;
        report.insert(
            "derived_memory_repair".to_string(),
            serde_json::to_value(repair).map_err(|e| TraceDecayError::Config {
                message: format!("failed to serialize memory repair report: {e}"),
            })?,
        );
    } else {
        report.insert(
            "derived_memory_repair".to_string(),
            json!({ "status": "not_run_read_only_preview" }),
        );
    }

    if options.llm && options.llm_ops.is_none() {
        let clusters = build_clusters(state, options.max_clusters).await?;
        let mut allowed_ids: BTreeSet<i64> = cluster_fact_ids(&clusters);
        allowed_ids.extend(hygiene_candidate_fact_ids(&hygiene_candidates));
        let has_hygiene = !hygiene_candidate_fact_ids(&hygiene_candidates).is_empty();
        {
            // Plan half of the two-phase contract: hand the caller the exact
            // chat messages the Hermes wrapper sends to its auxiliary LLM.
            // Hygiene candidates ride along so the external LLM reviews them
            // through the same ops contract.
            let user_message = format!(
                "Review these candidate clusters and return ops as strict JSON.\n\n{}",
                Value::Object(Map::from_iter([
                    ("clusters".to_string(), Value::Array(clusters.clone())),
                    ("hygiene_candidates".to_string(), hygiene_candidates.clone()),
                ]))
            );
            report.insert(
                "llm_review".to_string(),
                json!({
                    "status": if clusters.is_empty() && !has_hygiene { "nothing_to_review" } else { "needs_llm_review" },
                    "clusters_reviewed": clusters.len(),
                    "clusters": clusters,
                    "hygiene_candidates": hygiene_candidates,
                    "allowed_fact_ids": allowed_ids,
                    "min_confidence": options.min_confidence,
                    "messages": [
                        { "role": "system", "content": CURATION_SYSTEM_PROMPT },
                        { "role": "user", "content": user_message },
                    ],
                    "next_step": "run the messages through an LLM and pass its {\"ops\": [...]} JSON back via: tracedecay memory curate --llm-ops <file> [--apply]",
                }),
            );
        }
    }

    Ok(Value::Object(report))
}

/// Fact ids referenced by the deterministic hygiene candidate set — these are
/// legal op targets for the evidence guard alongside cluster members.
fn hygiene_candidate_fact_ids(hygiene_candidates: &Value) -> BTreeSet<i64> {
    ["secret_like", "transient", "supersession"]
        .iter()
        .filter_map(|key| hygiene_candidates.get(*key).and_then(Value::as_array))
        .flatten()
        .filter_map(|entry| entry.get("fact_id").and_then(Value::as_i64))
        .collect()
}

/// All member fact ids across the reviewable clusters (the evidence guard).
fn cluster_fact_ids(clusters: &[Value]) -> BTreeSet<i64> {
    clusters
        .iter()
        .filter_map(|cluster| cluster.get("members").and_then(Value::as_array))
        .flatten()
        .filter_map(|member| member.get("fact_id").and_then(Value::as_i64))
        .collect()
}

/// Groups candidate similarity pairs into reviewable clusters (union-find
/// over shared fact ids), port of the Hermes wrapper's
/// `_build_curation_clusters`. Pairs are walked in descending-similarity
/// order so cluster caps keep the strongest candidates.
fn find(parent: &mut HashMap<i64, i64>, mut x: i64) -> i64 {
    while *parent.entry(x).or_insert(x) != x {
        let grandparent = parent[&parent[&x]];
        parent.insert(x, grandparent);
        x = grandparent;
    }
    x
}

async fn build_clusters(state: &DashboardState, max_clusters: usize) -> Result<Vec<Value>> {
    let computation =
        similarity_computation(state)
            .await
            .map_err(|message| TraceDecayError::Config {
                message: format!("similarity computation failed: {message}"),
            })?;

    let mut parent: HashMap<i64, i64> = HashMap::new();
    let mut kept_pairs: Vec<(i64, i64, Value)> = Vec::new();
    for pair in &computation.pairs {
        if !CURATION_CLUSTER_CLASSIFICATIONS.contains(&pair.classification) {
            continue;
        }
        let a_id = computation.facts[pair.a]
            .get("fact_id")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let b_id = computation.facts[pair.b]
            .get("fact_id")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if a_id == 0 || b_id == 0 {
            continue;
        }
        let ra = find(&mut parent, a_id);
        let rb = find(&mut parent, b_id);
        if ra != rb {
            parent.insert(rb, ra);
        }
        kept_pairs.push((
            a_id,
            b_id,
            json!({
                "a_id": a_id,
                "b_id": b_id,
                "similarity": pair.similarity,
                "classification": pair.classification,
            }),
        ));
    }

    // Group by root, preserving first-seen (strongest-pair) order.
    let mut order: Vec<i64> = Vec::new();
    let mut groups: HashMap<i64, (BTreeSet<i64>, Vec<Value>)> = HashMap::new();
    for (a_id, b_id, pair) in kept_pairs {
        let root = find(&mut parent, a_id);
        let entry = groups.entry(root).or_insert_with(|| {
            order.push(root);
            (BTreeSet::new(), Vec::new())
        });
        entry.0.insert(a_id);
        entry.0.insert(b_id);
        entry.1.push(pair);
    }

    let member_ids: BTreeSet<i64> = order
        .iter()
        .take(max_clusters)
        .filter_map(|root| groups.get(root))
        .flat_map(|(ids, _)| ids.iter().copied())
        .collect();
    let details = fact_details(state, &member_ids).await?;

    let mut clusters = Vec::new();
    for (index, root) in order.into_iter().enumerate() {
        if clusters.len() >= max_clusters {
            break;
        }
        let Some((fact_ids, pairs)) = groups.remove(&root) else {
            continue;
        };
        let members: Vec<Value> = fact_ids
            .iter()
            .map(|fact_id| {
                details
                    .get(fact_id)
                    .cloned()
                    .unwrap_or_else(|| json!({ "fact_id": fact_id }))
            })
            .collect();
        clusters.push(json!({
            "cluster_id": format!("cluster-{index:04}"),
            "members": members,
            "pairs": pairs,
        }));
    }
    Ok(clusters)
}

/// Full member rows (content + freshness signals) for the LLM payload — the
/// similarity cache only retains the metadata the dashboard pair view needs.
async fn fact_details(
    state: &DashboardState,
    fact_ids: &BTreeSet<i64>,
) -> Result<BTreeMap<i64, Value>> {
    let mut details = BTreeMap::new();
    if fact_ids.is_empty() {
        return Ok(details);
    }
    let ids: Vec<i64> = fact_ids.iter().copied().collect();
    let sql = format!(
        "SELECT fact_id, content, category, tags, trust_score, metadata, created_at, updated_at,
                access_count, last_recalled_at
         FROM memory_facts WHERE fact_id IN ({})",
        qmarks(ids.len())
    );
    let params: Vec<libsql::Value> = ids.into_iter().map(libsql::Value::Integer).collect();
    let rows = query_rows(&state.mem_conn, &sql, params)
        .await
        .map_err(|message| TraceDecayError::Config {
            message: format!("fact detail query failed: {message}"),
        })?;
    for row in rows {
        let row = normalize_fact_metadata(row);
        if let Some(fact_id) = row.get("fact_id").and_then(Value::as_i64) {
            details.insert(fact_id, row);
        }
    }
    Ok(details)
}

/// Splits LLM-proposed ops into (valid actionable ops, rejected ops) —
/// required fields, op vocabulary, confidence floor, and the evidence guard
/// (every referenced fact id must belong to a reviewed cluster). Port of the
/// Hermes wrapper's `_validate_llm_ops`; `keep` ops are valid but never
/// actionable.
fn validate_llm_ops(
    raw_ops: &[Value],
    allowed_ids: &BTreeSet<i64>,
    min_confidence: f64,
) -> (Vec<Value>, Vec<Value>) {
    const GROOMING_OPS: [&str; 5] = [
        "normalize_tags",
        "merge_entities",
        "add_alias",
        "link_facts",
        "repair_vector",
    ];
    let mut valid = Vec::new();
    let mut rejected = Vec::new();
    for raw in raw_ops {
        let Some(op_obj) = raw.as_object() else {
            rejected.push(json!({ "op": raw, "rejected_reason": "not an object" }));
            continue;
        };
        let op = op_obj.get("op").and_then(Value::as_str).unwrap_or("");
        let confidence = op_obj
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if op == "keep" {
            continue;
        }
        if op != "merge" && op != "delete" && !GROOMING_OPS.contains(&op) {
            rejected.push(reject(raw, &format!("unknown op '{op}'")));
            continue;
        }
        if uses_updated_at_as_truth_freshness(raw) {
            rejected.push(reject(
                raw,
                "updated_at is maintenance metadata; cite asserted_at, effective_at, observed_at, occurred_at, or created_at for truth freshness",
            ));
            continue;
        }
        if confidence < min_confidence {
            rejected.push(reject(raw, &format!("confidence {confidence} below floor")));
            continue;
        }
        if GROOMING_OPS.contains(&op) {
            let evidence_ids = op_obj
                .get("evidence_fact_ids")
                .and_then(Value::as_array)
                .map(|ids| ids.iter().filter_map(Value::as_i64).collect::<Vec<_>>())
                .unwrap_or_default();
            if evidence_ids.is_empty() || evidence_ids.iter().any(|id| !allowed_ids.contains(id)) {
                rejected.push(reject(
                    raw,
                    "grooming evidence_fact_ids were empty or outside reviewed evidence",
                ));
                continue;
            }
            let valid_shape = match op {
                "normalize_tags" => {
                    op_obj
                        .get("fact_id")
                        .and_then(Value::as_i64)
                        .is_some_and(|id| allowed_ids.contains(&id))
                        && op_obj.get("tags").and_then(Value::as_array).is_some()
                }
                "merge_entities" => {
                    op_obj
                        .get("winner_entity_id")
                        .and_then(Value::as_i64)
                        .is_some()
                        && op_obj
                            .get("loser_entity_ids")
                            .and_then(Value::as_array)
                            .is_some_and(|ids| {
                                !ids.is_empty() && ids.iter().all(|id| id.as_i64().is_some())
                            })
                }
                "add_alias" => {
                    op_obj.get("entity_id").and_then(Value::as_i64).is_some()
                        && op_obj
                            .get("alias")
                            .and_then(Value::as_str)
                            .is_some_and(|alias| !alias.trim().is_empty())
                }
                "link_facts" => {
                    let source_id = op_obj.get("source_fact_id").and_then(Value::as_i64);
                    let target_id = op_obj.get("target_fact_id").and_then(Value::as_i64);
                    source_id.is_some_and(|id| allowed_ids.contains(&id))
                        && target_id.is_some_and(|id| allowed_ids.contains(&id))
                        && source_id != target_id
                        && matches!(
                            op_obj.get("relation").and_then(Value::as_str),
                            Some("supports" | "contradicts" | "supersedes" | "derived_from")
                        )
                        && op_obj
                            .get("source")
                            .and_then(Value::as_str)
                            .is_some_and(|source| !source.trim().is_empty())
                }
                "repair_vector" => op_obj
                    .get("fact_id")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| allowed_ids.contains(&id)),
                _ => false,
            };
            if !valid_shape {
                rejected.push(reject(raw, "missing/invalid bounded grooming fields"));
                continue;
            }
            valid.push(raw.clone());
            continue;
        }
        if op == "delete" {
            let Some(fact_id) = op_obj.get("fact_id").and_then(Value::as_i64) else {
                rejected.push(reject(raw, "missing/invalid fact_id"));
                continue;
            };
            if !allowed_ids.contains(&fact_id) {
                rejected.push(reject(
                    raw,
                    &format!("fact_id {fact_id} was not in reviewed evidence"),
                ));
                continue;
            }
            valid.push(raw.clone());
            continue;
        }
        let Some(winner_id) = op_obj.get("winner_id").and_then(Value::as_i64) else {
            rejected.push(reject(raw, "missing/invalid winner_id/loser_ids"));
            continue;
        };
        let loser_ids: Vec<i64> = op_obj
            .get("loser_ids")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        if loser_ids.is_empty() || loser_ids.contains(&winner_id) {
            rejected.push(reject(raw, "empty loser_ids or winner among losers"));
            continue;
        }
        if !allowed_ids.contains(&winner_id) || loser_ids.iter().any(|id| !allowed_ids.contains(id))
        {
            rejected.push(reject(raw, "fact ids were not all in reviewed evidence"));
            continue;
        }
        valid.push(raw.clone());
    }
    (valid, rejected)
}

fn reject(raw: &Value, reason: &str) -> Value {
    let mut out = raw.as_object().cloned().unwrap_or_default();
    out.insert("rejected_reason".to_string(), json!(reason));
    Value::Object(out)
}

fn parse_grooming_ops(valid: &[Value]) -> Result<Vec<MemoryGroomingOperation>> {
    valid
        .iter()
        .filter(|op| {
            matches!(
                op.get("op").and_then(Value::as_str),
                Some(
                    "normalize_tags"
                        | "merge_entities"
                        | "add_alias"
                        | "link_facts"
                        | "repair_vector"
                )
            )
        })
        .map(|op| {
            serde_json::from_value(op.clone()).map_err(|e| TraceDecayError::Config {
                message: format!("invalid grooming operation after validation: {e}"),
            })
        })
        .collect()
}

async fn prevalidate_destructive_ops(state: &DashboardState, ops: &[Value]) -> Result<()> {
    let destructive_count = ops
        .iter()
        .filter(|op| {
            matches!(
                op.get("op").and_then(Value::as_str),
                Some("delete" | "merge")
            )
        })
        .count();
    let has_grooming = ops.iter().any(|op| {
        matches!(
            op.get("op").and_then(Value::as_str),
            Some(
                "normalize_tags" | "merge_entities" | "add_alias" | "link_facts" | "repair_vector"
            )
        )
    });
    if destructive_count > 1 || (destructive_count == 1 && has_grooming) {
        return Err(TraceDecayError::Config {
            message: "curation batches may contain one destructive operation or an atomic grooming batch, not both".to_string(),
        });
    }
    let mut mutation_targets = BTreeSet::new();
    let mut merge_winners = BTreeSet::new();
    let mut required_facts = BTreeSet::new();
    for op in ops {
        match op.get("op").and_then(Value::as_str) {
            Some("delete") => {
                let fact_id = op.get("fact_id").and_then(Value::as_i64).ok_or_else(|| {
                    TraceDecayError::Config {
                        message: "delete op lost fact_id after validation".to_string(),
                    }
                })?;
                if merge_winners.contains(&fact_id) || !mutation_targets.insert(fact_id) {
                    return Err(TraceDecayError::Config {
                        message: format!("fact {fact_id} is targeted by multiple destructive ops"),
                    });
                }
                required_facts.insert(fact_id);
            }
            Some("merge") => {
                let winner_id = op.get("winner_id").and_then(Value::as_i64).ok_or_else(|| {
                    TraceDecayError::Config {
                        message: "merge op lost winner_id after validation".to_string(),
                    }
                })?;
                if mutation_targets.contains(&winner_id) {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "fact {winner_id} has conflicting destructive batch roles"
                        ),
                    });
                }
                merge_winners.insert(winner_id);
                required_facts.insert(winner_id);
                for loser_id in op
                    .get("loser_ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_i64)
                {
                    if merge_winners.contains(&loser_id)
                        || !mutation_targets.insert(loser_id)
                        || loser_id == winner_id
                    {
                        return Err(TraceDecayError::Config {
                            message: format!(
                                "fact {loser_id} has conflicting destructive batch roles"
                            ),
                        });
                    }
                    required_facts.insert(loser_id);
                }
            }
            _ => {}
        }
    }
    for fact_id in required_facts {
        let mut rows = state
            .mem_conn
            .query(
                "SELECT 1 FROM memory_facts WHERE fact_id = ?1 LIMIT 1",
                libsql::params![fact_id],
            )
            .await
            .map_err(|e| TraceDecayError::Database {
                message: e.to_string(),
                operation: "prevalidate_destructive_ops".to_string(),
            })?;
        if rows
            .next()
            .await
            .map_err(|e| TraceDecayError::Database {
                message: e.to_string(),
                operation: "prevalidate_destructive_ops".to_string(),
            })?
            .is_none()
        {
            return Err(TraceDecayError::Config {
                message: format!("fact {fact_id} no longer exists; batch was not applied"),
            });
        }
    }
    Ok(())
}

fn uses_updated_at_as_truth_freshness(raw: &Value) -> bool {
    raw.get("reason")
        .and_then(Value::as_str)
        .is_some_and(|reason| {
            let normalized = reason.to_ascii_lowercase();
            normalized.contains("updated_at") || normalized.contains("updated at")
        })
        || raw
            .get("freshness_field")
            .and_then(Value::as_str)
            .is_some_and(|field| field == "updated_at")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn allowed(ids: &[i64]) -> BTreeSet<i64> {
        ids.iter().copied().collect()
    }

    #[tokio::test]
    async fn preview_is_read_only_while_apply_repairs_derived_memory() {
        let temp = tempfile::tempdir().unwrap();
        let memory_path = temp.path().join("user-memory.db");
        let authority =
            crate::db::DatabaseAuthority::acquire_test(&memory_path, "memory curation test")
                .unwrap();
        let (db, _) = Database::initialize(&memory_path, &authority)
            .await
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO memory_facts
                    (fact_id, content, category, trust_score, created_at, updated_at, source)
                 VALUES (1, 'Keep diagnostic previews read only', 'decision', 0.9, 1, 1, 'test')",
                (),
            )
            .await
            .unwrap();
        let options = MemoryCurateOptions {
            llm_ops: Some(json!({ "ops": [] })),
            ..MemoryCurateOptions::default()
        };

        let preview = run_user_memory_curate(
            &db,
            &memory_path,
            temp.path(),
            &temp.path().join("user-automation"),
            &options,
        )
        .await
        .unwrap();

        assert_eq!(
            preview["derived_memory_repair"]["status"],
            json!("not_run_read_only_preview")
        );
        assert_eq!(missing_vector_count(&db).await, 1);

        let applied = run_user_memory_curate(
            &db,
            &memory_path,
            temp.path(),
            &temp.path().join("user-automation"),
            &MemoryCurateOptions {
                apply: true,
                ..options
            },
        )
        .await
        .unwrap();

        assert_eq!(
            applied["derived_memory_repair"]["missing_vectors_repaired"],
            json!(1)
        );
        assert_eq!(missing_vector_count(&db).await, 0);
    }

    async fn missing_vector_count(db: &Database) -> i64 {
        db.conn()
            .query(
                "SELECT COUNT(*) FROM memory_facts WHERE hrr_vector IS NULL",
                (),
            )
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap()
            .get(0)
            .unwrap()
    }

    #[test]
    fn validate_keeps_are_silently_dropped() {
        let ops = vec![json!({ "op": "keep", "cluster_id": "cluster-0000", "confidence": 0.9 })];
        let (valid, rejected) = validate_llm_ops(&ops, &allowed(&[1, 2]), 0.5);
        assert!(valid.is_empty());
        assert!(rejected.is_empty());
    }

    #[test]
    fn validate_enforces_confidence_floor_and_evidence_guard() {
        let ops = vec![
            json!({ "op": "delete", "fact_id": 1, "confidence": 0.4 }),
            json!({ "op": "delete", "fact_id": 99, "confidence": 0.9 }),
            json!({ "op": "delete", "fact_id": 2, "confidence": 0.9 }),
        ];
        let (valid, rejected) = validate_llm_ops(&ops, &allowed(&[1, 2]), 0.5);
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0]["fact_id"], json!(2));
        assert_eq!(rejected.len(), 2);
        assert!(
            rejected[0]["rejected_reason"]
                .as_str()
                .unwrap()
                .contains("below floor")
        );
        assert!(
            rejected[1]["rejected_reason"]
                .as_str()
                .unwrap()
                .contains("not in reviewed evidence")
        );
    }

    #[test]
    fn validate_merge_requires_distinct_winner_and_losers() {
        let ops = vec![
            json!({ "op": "merge", "winner_id": 1, "loser_ids": [1], "confidence": 0.9 }),
            json!({ "op": "merge", "winner_id": 1, "loser_ids": [2], "confidence": 0.9 }),
            json!({ "op": "merge", "winner_id": 1, "loser_ids": [], "confidence": 0.9 }),
            json!({ "op": "rename", "confidence": 0.9 }),
        ];
        let (valid, rejected) = validate_llm_ops(&ops, &allowed(&[1, 2]), 0.5);
        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0]["winner_id"], json!(1));
        assert_eq!(rejected.len(), 3);
    }

    #[test]
    fn validate_llm_ops_allows_delete_and_merge_with_candidate_evidence() {
        let ops = vec![
            json!({
                "op": "delete",
                "fact_id": 3,
                "confidence": 0.8,
                "reason": "secret-like hygiene candidate confirmed"
            }),
            json!({
                "op": "merge",
                "winner_id": 1,
                "loser_ids": [2],
                "confidence": 0.9,
                "reason": "same durable claim"
            }),
        ];

        let (valid, rejected) = validate_llm_ops(&ops, &allowed(&[1, 2, 3]), 0.5);

        assert!(rejected.is_empty());
        assert_eq!(valid.len(), 2);
        assert_eq!(valid[0]["op"], "delete");
        assert_eq!(valid[1]["op"], "merge");
    }

    #[test]
    fn validate_rejects_updated_at_as_truth_freshness() {
        let ops = vec![
            json!({
                "op": "delete",
                "fact_id": 1,
                "confidence": 0.9,
                "reason": "fact 1 is stale because updated_at is older than fact 2"
            }),
            json!({
                "op": "delete",
                "fact_id": 2,
                "confidence": 0.9,
                "reason": "same subject and atomic claim; created_at shows fact 2 is stale"
            }),
        ];

        let (valid, rejected) = validate_llm_ops(&ops, &allowed(&[1, 2]), 0.5);

        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0]["fact_id"], json!(2));
        assert_eq!(rejected.len(), 1);
        assert!(
            rejected[0]["rejected_reason"]
                .as_str()
                .unwrap()
                .contains("updated_at is maintenance metadata")
        );
    }

    #[test]
    fn validate_accepts_only_bounded_grooming_with_reviewed_evidence() {
        let ops = vec![
            json!({
                "op": "link_facts",
                "source_fact_id": 1,
                "target_fact_id": 2,
                "relation": "supports",
                "source": "memory_curator",
                "evidence_fact_ids": [1, 2],
                "confidence": 0.9
            }),
            json!({
                "op": "link_facts",
                "source_fact_id": 1,
                "target_fact_id": 1,
                "relation": "invented_relation",
                "source": "memory_curator",
                "evidence_fact_ids": [1],
                "confidence": 0.9
            }),
            json!({
                "op": "add_alias",
                "entity_id": 4,
                "alias": "safe alias",
                "evidence_fact_ids": [99],
                "confidence": 0.9
            }),
        ];

        let (valid, rejected) = validate_llm_ops(&ops, &allowed(&[1, 2]), 0.5);

        assert_eq!(valid.len(), 1);
        assert_eq!(valid[0]["relation"], "supports");
        assert_eq!(rejected.len(), 2);
    }
}
