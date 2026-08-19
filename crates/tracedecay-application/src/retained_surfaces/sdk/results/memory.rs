use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{FactAssertionId, FactEventId, FactId, UtcMicros};

pub use crate::memory::{
    FactCommitOwnerV1, FactIdentitySourceResultV1, FactPayloadAccessV1, FactProjectionV1,
    FactSearchCursorV1, FactSearchGraphCoverageV1, FactSearchGraphDegradationV1, FactSearchHitV1,
    FactSearchScoresV1, FactStatusV1, FactTelemetryV1, FactV1,
};
use crate::retained_surfaces::FactFeedbackActionV1;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactCommitDispositionV1 {
    Committed,
    IdempotentReplay,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactCommitReceiptV1 {
    pub disposition: FactCommitDispositionV1,
    pub fact_id: FactId,
    pub owner: FactCommitOwnerV1,
    pub committed_event_ids: Vec<FactEventId>,
    pub last_event_id: FactEventId,
    pub active_assertion_id: Option<FactAssertionId>,
}

macro_rules! fact_search_result {
    ($name:ident) => {
        #[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub owner: FactCommitOwnerV1,
            pub hits: Vec<FactSearchHitV1>,
            pub next_after: Option<FactSearchCursorV1>,
            pub graph_coverage: FactSearchGraphCoverageV1,
        }
    };
}

fact_search_result!(FactStoreSearchResultV1);
fact_search_result!(FactStoreProbeResultV1);
fact_search_result!(FactStoreRelatedResultV1);
fact_search_result!(FactStoreReasonResultV1);

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactContradictionV1 {
    pub existing_fact: FactV1,
    pub new_content: String,
    pub score_millionths: u32,
    pub why: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreContradictResultV1 {
    pub owner: FactCommitOwnerV1,
    pub contradictions: Vec<FactContradictionV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactStoreAddCommitV1 {
    Added {
        fact: FactProjectionV1,
        commit: FactCommitReceiptV1,
    },
    NearDuplicate {
        fact: FactProjectionV1,
        closest_fact_id: FactId,
        similarity_millionths: u32,
        commit: FactCommitReceiptV1,
    },
    PossibleConflict {
        fact: FactProjectionV1,
        closest_fact_id: FactId,
        similarity_millionths: u32,
        commit: FactCommitReceiptV1,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactStoreAddResultV1 {
    SecretRejected,
    NormalizedDuplicate {
        fact: FactProjectionV1,
        closest_fact_id: FactId,
    },
    Committed {
        result: FactStoreAddCommitV1,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactFeedbackDetailsAvailabilityV1 {
    Available,
    Redacted,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrustHistoryEntryV1 {
    pub event_id: FactEventId,
    pub occurred_at: UtcMicros,
    pub action: FactFeedbackActionV1,
    pub old_trust_millionths: u32,
    pub new_trust_millionths: u32,
    pub source_label: Option<String>,
    pub reason: Option<String>,
    pub details_availability: FactFeedbackDetailsAvailabilityV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreGetResultV1 {
    pub fact: FactProjectionV1,
    pub trust_history: Vec<TrustHistoryEntryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreUpdateResultV1 {
    pub fact: FactProjectionV1,
    pub trust_delta_millionths: i32,
    pub commit: FactCommitReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactStoreRemoveResultV1 {
    Removed {
        fact: FactProjectionV1,
        remaining_fact_count: u64,
        commit: FactCommitReceiptV1,
    },
    AlreadyRemoved {
        fact: FactProjectionV1,
        remaining_fact_count: u64,
    },
    NotFound {
        remaining_fact_count: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactStoreListResultV1 {
    pub owner: FactCommitOwnerV1,
    pub facts: Vec<FactProjectionV1>,
    pub next_after_fact_id: Option<FactId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactFeedbackV1 {
    pub event_id: FactEventId,
    pub fact_id: FactId,
    pub action: FactFeedbackActionV1,
    pub old_trust_millionths: u32,
    pub new_trust_millionths: u32,
    pub trust_delta_millionths: i32,
    pub helpful_count: u64,
    pub unhelpful_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FactFeedbackResultV1 {
    pub fact: FactProjectionV1,
    pub feedback: FactFeedbackV1,
    pub commit: FactCommitReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryAlgebraV1 {
    pub name: String,
    pub hrr_dim: u64,
    pub estimated_capacity: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryFeedbackFunnelV1 {
    pub retrieval_count_total: u64,
    pub access_count_total: u64,
    pub retrieved_fact_count: u64,
    pub rated_fact_count: u64,
    pub feedback_total: u64,
    pub seen_to_feedback_ratio: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryStatusV1 {
    pub owner: FactCommitOwnerV1,
    pub fact_count: u64,
    pub entity_count: u64,
    pub algebra: MemoryAlgebraV1,
    pub trust_0_025_count: u64,
    pub trust_025_050_count: u64,
    pub trust_050_075_count: u64,
    pub trust_075_100_count: u64,
    pub below_default_recall_threshold_count: u64,
    pub helpful_count: u64,
    pub unhelpful_count: u64,
    pub feedback_funnel: MemoryFeedbackFunnelV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryStatusResultV1 {
    pub memory: MemoryStatusV1,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        FactCommitReceiptV1, FactStoreAddCommitV1, FactStoreAddResultV1,
        FactStoreContradictResultV1, FactStoreSearchResultV1, MemoryStatusResultV1,
    };

    fn canonical_projection() -> serde_json::Value {
        json!({
            "kind": "available",
            "fact": {
                "owner": {"kind": "profile"},
                "fact_id": "fact.test",
                "content": "remember",
                "category": "general",
                "tags": [],
                "entities": [],
                "trust_score_millionths": 500_000,
                "source": {"kind": "application", "operation_id": "operation.test"},
                "source_label": null,
                "active_assertion_id": "assertion.active",
                "last_event_id": "event.created",
                "projected_as_of": 1,
                "telemetry": {
                    "retrieval_count": 0,
                    "access_count": 0,
                    "helpful_count": 0,
                    "unhelpful_count": 0,
                    "created_at": 1,
                    "updated_at": 1,
                    "last_retrieved_at": null,
                    "last_recalled_at": null,
                    "last_feedback_at": null
                },
                "metadata": {}
            }
        })
    }

    fn canonical_receipt() -> serde_json::Value {
        json!({
            "disposition": "committed",
            "fact_id": "fact.test",
            "owner": {"kind": "profile"},
            "committed_event_ids": ["event.created"],
            "last_event_id": "event.created",
            "active_assertion_id": "assertion.active"
        })
    }

    #[test]
    fn fact_commit_receipt_rejects_synthetic_and_numeric_identity_fields() {
        let receipt = json!({
            "disposition": "committed",
            "fact_id": "fact.test",
            "owner": {"kind": "project", "project_id": "project.alpha"},
            "committed_event_ids": ["event.created"],
            "last_event_id": "event.created",
            "active_assertion_id": "assertion.active"
        });
        serde_json::from_value::<FactCommitReceiptV1>(receipt.clone())
            .expect("canonical commit receipt");

        let mut synthetic = receipt.clone();
        synthetic["expected_last_event_id"] = json!("event.previous");
        assert!(serde_json::from_value::<FactCommitReceiptV1>(synthetic).is_err());

        let mut numeric = receipt;
        numeric["fact_id"] = json!(41);
        assert!(serde_json::from_value::<FactCommitReceiptV1>(numeric).is_err());
    }

    #[test]
    fn fact_add_result_has_only_finite_outcomes() {
        serde_json::from_value::<FactStoreAddResultV1>(json!({
            "outcome": "secret_rejected"
        }))
        .expect("secret rejection is a truthful no-write outcome");
        assert!(
            serde_json::from_value::<FactStoreAddResultV1>(json!({
                "count": 0,
                "fact": null,
                "diff": "rejected_secret_like",
                "closest_fact_id": null,
                "similarity": null,
                "reason": "secret-like",
                "mutation": null
            }))
            .is_err()
        );
    }

    #[test]
    fn committed_add_disposition_makes_comparison_fields_structural() {
        let added = json!({
            "disposition": "added",
            "fact": canonical_projection(),
            "commit": canonical_receipt()
        });
        serde_json::from_value::<FactStoreAddCommitV1>(added.clone())
            .expect("added commit has no comparison fields");

        let mut invalid_added = added;
        invalid_added["closest_fact_id"] = json!("fact.closest");
        assert!(serde_json::from_value::<FactStoreAddCommitV1>(invalid_added).is_err());

        assert!(
            serde_json::from_value::<FactStoreAddCommitV1>(json!({
                "disposition": "near_duplicate",
                "fact": canonical_projection(),
                "commit": canonical_receipt()
            }))
            .is_err()
        );
        serde_json::from_value::<FactStoreAddCommitV1>(json!({
            "disposition": "near_duplicate",
            "fact": canonical_projection(),
            "closest_fact_id": "fact.closest",
            "similarity_millionths": 900_000,
            "commit": canonical_receipt()
        }))
        .expect("semantic near-duplicate commit requires its comparison");
    }

    #[test]
    fn fact_search_page_requires_typed_graph_coverage() {
        let page = json!({
            "owner": {"kind": "project", "project_id": "project.alpha"},
            "hits": [],
            "next_after": null,
            "graph_coverage": {"kind": "not_mounted"}
        });
        serde_json::from_value::<FactStoreSearchResultV1>(page.clone())
            .expect("canonical search page");

        let mut missing_coverage = page;
        missing_coverage
            .as_object_mut()
            .expect("page is an object")
            .remove("graph_coverage");
        assert!(serde_json::from_value::<FactStoreSearchResultV1>(missing_coverage).is_err());
    }

    #[test]
    fn bounded_contradiction_result_rejects_false_continuations() {
        let result = json!({
            "owner": {"kind": "profile"},
            "contradictions": []
        });
        serde_json::from_value::<FactStoreContradictResultV1>(result.clone())
            .expect("canonical bounded contradiction result");

        for field in ["next_after", "next_cursor", "cursor"] {
            let mut paginated = result.clone();
            paginated[field] = json!("cursor.test");
            assert!(serde_json::from_value::<FactStoreContradictResultV1>(paginated).is_err());
        }
    }

    #[test]
    fn memory_status_rejects_unknown_fields() {
        let status = json!({
            "memory": {
                "owner": {"kind": "profile"},
                "fact_count": 0,
                "entity_count": 0,
                "algebra": {
                    "name": "amari_fhrr",
                    "hrr_dim": 2048,
                    "estimated_capacity": 1024
                },
                "trust_0_025_count": 0,
                "trust_025_050_count": 0,
                "trust_050_075_count": 0,
                "trust_075_100_count": 0,
                "below_default_recall_threshold_count": 0,
                "helpful_count": 0,
                "unhelpful_count": 0,
                "feedback_funnel": {
                    "retrieval_count_total": 0,
                    "access_count_total": 0,
                    "retrieved_fact_count": 0,
                    "rated_fact_count": 0,
                    "feedback_total": 0,
                    "seen_to_feedback_ratio": null
                }
            }
        });
        serde_json::from_value::<MemoryStatusResultV1>(status.clone())
            .expect("canonical memory status");

        let mut unknown = status;
        unknown["memory"]["unknown"] = json!(true);
        assert!(serde_json::from_value::<MemoryStatusResultV1>(unknown).is_err());
    }
}
