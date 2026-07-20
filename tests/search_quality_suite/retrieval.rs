//! The harness retrieval port: executes the frozen development workload
//! against the indexed corpus through the existing `tracedecay_search` /
//! `tracedecay_grep` tool behavior (public lib dispatch), maps hits back to
//! corpus document anchors, and emits a typed `EvidenceBatchV1`.
//!
//! No labels are consulted, no thresholds are asserted, and no quality claim
//! is made here: this packet emits evidence only. Metric computation over
//! the emitted batches lands with the locked-comparison packet.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use tracedecay::tracedecay::TraceDecay;

use crate::evaluation::{
    CandidateListV1, CorpusDocumentId, EvalCandidateAnchorV1, EvalCandidateV1, EvalQueryV1,
    EvalRunScopeV1, EvidenceBatchDigest, EvidenceBatchId, EvidenceBatchV1, FixtureManifestV1,
    QueryWorkloadV1, RetrieverLaneId, RunId,
};
use crate::support::call_tool;

/// Lane identities recorded in the emitted evidence. These name the existing
/// tool behaviors the harness wires against at this revision.
pub(crate) const SYMBOL_SEARCH_LANE: &str = "tracedecay_search";
pub(crate) const CONTENT_GREP_LANE: &str = "tracedecay_grep";

const ZERO_DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// Executes workload queries against one indexed corpus project.
pub(crate) struct HarnessRetriever<'a> {
    cg: &'a TraceDecay,
    /// Project-relative snapshot path -> corpus document identity.
    documents: BTreeMap<String, CorpusDocumentId>,
}

impl<'a> HarnessRetriever<'a> {
    pub(crate) fn new(cg: &'a TraceDecay, manifest: &FixtureManifestV1) -> Self {
        let documents = manifest
            .corpus
            .iter()
            .map(|document| {
                (
                    document
                        .snapshot_path
                        .strip_prefix("corpus/")
                        .unwrap_or(&document.snapshot_path)
                        .to_string(),
                    document.document_id.clone(),
                )
            })
            .collect();
        Self { cg, documents }
    }

    pub(crate) fn lanes() -> [RetrieverLaneId; 2] {
        [
            RetrieverLaneId::new(SYMBOL_SEARCH_LANE).expect("valid lane id"),
            RetrieverLaneId::new(CONTENT_GREP_LANE).expect("valid lane id"),
        ]
    }

    /// Executes the frozen development workload and returns the typed
    /// evidence batch with a self-verifying digest. Sealed-holdout queries
    /// are never executed (development scope).
    pub(crate) async fn run_development_workload(
        &self,
        workload: &QueryWorkloadV1,
        run_id: &RunId,
        batch_id: &EvidenceBatchId,
    ) -> EvidenceBatchV1 {
        let mut candidate_lists = Vec::new();
        for query in workload.development_queries() {
            for lane in Self::lanes() {
                candidate_lists.push(self.run_lane(query, &lane).await);
            }
        }
        let mut batch = EvidenceBatchV1 {
            batch_id: batch_id.clone(),
            run_id: run_id.clone(),
            scope: EvalRunScopeV1::Development,
            workload_digest: workload.digest.clone(),
            candidate_lists,
            holdout_receipts: Vec::new(),
            digest: EvidenceBatchDigest::new(ZERO_DIGEST).unwrap(),
        };
        batch.digest = batch.compute_digest().expect("batch digest computable");
        batch
    }

    async fn run_lane(&self, query: &EvalQueryV1, lane: &RetrieverLaneId) -> CandidateListV1 {
        let payload = match lane.as_str() {
            SYMBOL_SEARCH_LANE => {
                call_tool(
                    self.cg,
                    SYMBOL_SEARCH_LANE,
                    json!({"query": query.query_text.as_str(), "limit": 10, "format": "json"}),
                )
                .await
            }
            CONTENT_GREP_LANE => {
                call_tool(
                    self.cg,
                    CONTENT_GREP_LANE,
                    json!({
                        "pattern": query.query_text.as_str(),
                        "fixed_strings": true,
                        "case_sensitive": false,
                        "max_results": 20,
                        "format": "json",
                    }),
                )
                .await
            }
            other => panic!("unknown harness lane {other}"),
        };
        let candidates = self.map_candidates(&payload);
        let list = CandidateListV1 {
            query_id: query.query_id.clone(),
            lane: lane.clone(),
            candidates,
        };
        list.validate().expect("candidate list validates");
        list
    }

    /// Maps a tool payload to deduped corpus-document anchors, preserving
    /// the tool's rank order. Hits outside the corpus snapshot are dropped:
    /// the evaluation corpus is the committed snapshot, not the live repo.
    fn map_candidates(&self, payload: &Value) -> Vec<EvalCandidateV1> {
        let empty = Vec::new();
        let items: &Vec<Value> = if let Some(items) = payload.as_array() {
            items
        } else {
            payload["results"].as_array().unwrap_or(&empty)
        };
        let mut seen: BTreeMap<CorpusDocumentId, ()> = BTreeMap::new();
        let mut candidates = Vec::new();
        for item in items {
            let Some(file) = item["file"].as_str() else {
                continue;
            };
            let Some(document_id) = self.documents.get(file) else {
                continue;
            };
            if seen.insert(document_id.clone(), ()).is_some() {
                continue;
            }
            let symbol = item["name"]
                .as_str()
                .or_else(|| item["symbol"].as_str())
                .map(str::to_string);
            candidates.push(EvalCandidateV1 {
                anchor: EvalCandidateAnchorV1 {
                    document_id: document_id.clone(),
                    symbol,
                },
                ordinal_rank: candidates.len() as u32,
            });
        }
        candidates
    }
}
