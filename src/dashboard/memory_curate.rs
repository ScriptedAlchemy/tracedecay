//! `tokensave memory curate` — dashboard-free curation core.
//!
//! Footprint-Ladder rung 2 for Hermes: the similarity-dedup curate (and the
//! LLM-review tier's plan/apply halves) run directly against the project
//! memory store, so a cron job can call the CLI without the dashboard server
//! or its Hermes wrapper.
//!
//! The LLM tier mirrors the LCM summarizer's two-phase `needs_summary` →
//! `provided` contract: this binary never calls an LLM itself. `--llm` emits
//! a `llm_review` request (clusters + chat messages, ported from the Hermes
//! wrapper's `/curation/llm-plan`); the caller runs the one-shot review with
//! whatever LLM it owns and feeds the strict-JSON ops back through
//! `--llm-ops`, which validates them against freshly recomputed clusters
//! (the evidence guard) and applies them through the canonical store paths.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::{json, Map, Value};
use tokio::sync::RwLock;

use super::memory_api::{
    apply_delete_op, apply_merge_op, build_delete_plan, delete_fact, similarity_computation,
};
use super::util::{qmarks, query_rows};
use super::{token_count, DashboardState};
use crate::errors::{Result, TokenSaveError};
use crate::tokensave::TokenSave;

pub const CURATION_DEFAULT_MAX_CLUSTERS: usize = 12;
pub const CURATION_DEFAULT_MIN_CONFIDENCE: f64 = 0.5;
const CURATION_CLUSTER_CLASSIFICATIONS: [&str; 2] = ["likely_duplicate", "merge_candidate"];

/// Verbatim port of the Hermes wrapper's `_CURATION_SYSTEM_PROMPT`
/// (dashboard/hermes-wrapper/plugin_api.py), itself adapted from the
/// `holographic_plus` curator's one-shot LLM review tier.
const CURATION_SYSTEM_PROMPT: &str =
    "You are a memory hygiene engine for an AI agent's long-term fact store. \
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
after the same-subject / same-claim conflict is established (created_at \
is the freshness signal; updated_at is maintenance metadata). If the \
facts describe an EVOLUTION over time (a preference pivot, not a true \
contradiction, e.g. 'used React' then 'switched to Vue'), emit a merge \
whose merged_content is ONE time-aware fact built strictly from the \
cluster's own text. Distinct contexts that merely look similar are NOT \
contradictions - leave them with \"keep\".\n\n\
There is NO archive: delete and merge losers are removed permanently, \
so prefer \"keep\" whenever unsure.\n\n\
Return JSON of shape: {\"ops\": [ ... ]}. Each op MUST include: \
cluster_id (string, from the input), op (one of merge, delete, keep), \
confidence (0.0-1.0), and reason (short string). Use op \"keep\" for \
reviewed clusters that need no change; do not omit keep reviews.\n\
Per-op required fields:\n\
  merge: {\"winner_id\": <id>, \"loser_ids\": [<id>, ...]} and optional \
\"merged_content\" (string) when the winner's text should be replaced \
by a consolidated fact.\n\
  delete: {\"fact_id\": <id>}\n\
Only reference fact ids that appear in the input clusters. \
Return ONLY the JSON object.";

/// Options for one `tokensave memory curate` run.
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

/// Minimal dashboard state over the project memory store — no LCM store,
/// savings DB, or token-count cache warmup (those belong to the server).
async fn cli_state(cg: &TokenSave) -> DashboardState {
    // Same vector/bank repair the dashboard runs before serving similarity.
    if let Err(err) = cg.memory_status().await {
        eprintln!("Warning: memory repair failed: {err}");
    }
    DashboardState {
        mem_conn: cg.dashboard_connection(),
        mem_db_path: cg.dashboard_db_path().display().to_string(),
        lcm_conn: None,
        lcm_db_path: String::new(),
        lcm_scope: "project_local",
        savings_db: None,
        savings_db_path: String::new(),
        project_root: cg.project_root().to_path_buf(),
        curate_preview: Arc::new(RwLock::new(None)),
        token_counts: Arc::new(token_count::TokenCountCache::new()),
    }
}

/// Runs the curate verb and returns the JSON report printed by the CLI.
pub async fn run_memory_curate(cg: &TokenSave, options: &MemoryCurateOptions) -> Result<Value> {
    let state = cli_state(cg).await;
    let mut report = Map::new();
    report.insert("mode".to_string(), json!("similarity_dedup"));
    report.insert("dry_run".to_string(), json!(!options.apply));

    // Externally produced LLM ops are validated and applied FIRST: they were
    // planned against the current store, and running the similarity-dedup
    // deletions beforehand would invalidate the very clusters the ops
    // reference (their fact ids would already be gone).
    if let Some(provided) = options.llm_ops.as_ref() {
        let clusters = build_clusters(&state, options.max_clusters).await?;
        let allowed_ids: BTreeSet<i64> = cluster_fact_ids(&clusters);
        let raw_ops = provided
            .get("ops")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| TokenSaveError::Config {
                message: "--llm-ops payload must be a JSON object with an `ops` array".to_string(),
            })?;
        let (valid, rejected) = validate_llm_ops(&raw_ops, &allowed_ids, options.min_confidence);
        let mut llm_report = Map::new();
        llm_report.insert("clusters_reviewed".to_string(), json!(clusters.len()));
        llm_report.insert("rejected_ops".to_string(), Value::Array(rejected));
        if options.apply {
            let mut results = Vec::new();
            let mut applied = 0i64;
            for op in &valid {
                let (result, ok) = match op.get("op").and_then(Value::as_str) {
                    Some("delete") => apply_delete_op(&state, op).await,
                    Some("merge") => apply_merge_op(&state, op).await,
                    _ => (json!({ "status": "error", "error": "unknown op" }), false),
                };
                if ok {
                    applied += 1;
                }
                results.push(result);
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

    // Similarity-dedup tier (the dashboard's `/curate` semantics), planned
    // on the post-LLM-ops store state.
    let (actions, counts, total) =
        build_delete_plan(&state)
            .await
            .map_err(|message| TokenSaveError::Config {
                message: format!("curation analysis failed: {message}"),
            })?;
    report.insert("counts".to_string(), Value::Object(counts));
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
            match delete_fact(&state, fact_id).await {
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

    if options.llm && options.llm_ops.is_none() {
        let clusters = build_clusters(&state, options.max_clusters).await?;
        let allowed_ids: BTreeSet<i64> = cluster_fact_ids(&clusters);
        {
            // Plan half of the two-phase contract: hand the caller the exact
            // chat messages the Hermes wrapper sends to its auxiliary LLM.
            let user_message = format!(
                "Review these candidate clusters and return ops as strict JSON.\n\n{}",
                Value::Object(Map::from_iter([(
                    "clusters".to_string(),
                    Value::Array(clusters.clone()),
                )]))
            );
            report.insert(
                "llm_review".to_string(),
                json!({
                    "status": if clusters.is_empty() { "nothing_to_review" } else { "needs_llm_review" },
                    "clusters_reviewed": clusters.len(),
                    "clusters": clusters,
                    "allowed_fact_ids": allowed_ids,
                    "min_confidence": options.min_confidence,
                    "messages": [
                        { "role": "system", "content": CURATION_SYSTEM_PROMPT },
                        { "role": "user", "content": user_message },
                    ],
                    "next_step": "run the messages through an LLM and pass its {\"ops\": [...]} JSON back via: tokensave memory curate --llm-ops <file> [--apply]",
                }),
            );
        }
    }

    Ok(Value::Object(report))
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
            .map_err(|message| TokenSaveError::Config {
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
        "SELECT fact_id, content, category, tags, trust_score, created_at, updated_at
         FROM memory_facts WHERE fact_id IN ({})",
        qmarks(ids.len())
    );
    let params: Vec<libsql::Value> = ids.into_iter().map(libsql::Value::Integer).collect();
    let rows = query_rows(&state.mem_conn, &sql, params)
        .await
        .map_err(|message| TokenSaveError::Config {
            message: format!("fact detail query failed: {message}"),
        })?;
    for row in rows {
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
        if op != "merge" && op != "delete" {
            rejected.push(reject(raw, &format!("unknown op '{op}'")));
            continue;
        }
        if confidence < min_confidence {
            rejected.push(reject(raw, &format!("confidence {confidence} below floor")));
            continue;
        }
        if op == "delete" {
            let Some(fact_id) = op_obj.get("fact_id").and_then(Value::as_i64) else {
                rejected.push(reject(raw, "missing/invalid fact_id"));
                continue;
            };
            if !allowed_ids.contains(&fact_id) {
                rejected.push(reject(raw, "fact_id not in reviewed clusters"));
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
            rejected.push(reject(raw, "fact ids not in reviewed clusters"));
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn allowed(ids: &[i64]) -> BTreeSet<i64> {
        ids.iter().copied().collect()
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
        assert!(rejected[0]["rejected_reason"]
            .as_str()
            .unwrap()
            .contains("below floor"));
        assert!(rejected[1]["rejected_reason"]
            .as_str()
            .unwrap()
            .contains("not in reviewed clusters"));
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
}
