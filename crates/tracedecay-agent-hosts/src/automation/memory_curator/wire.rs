use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use tracedecay_domain::{Confidence, FactCategoryV1, FactEventId, FactId, FactRelationKindV1};
use tracedecay_store::ProjectMemoryFactUpdatePatchV1;
use tracedecay_usecases::memory::{
    ProjectMemoryCurationMutationTarget, ProjectMemoryCurationOperation,
    ProjectMemoryFactAddRequest,
};

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum CanonicalCurationWire {
    Add {
        content: String,
        category: FactCategoryV1,
        source_label: Option<String>,
        tags: Vec<String>,
        entities: Vec<String>,
        trust: Option<Confidence>,
        metadata: Value,
        evidence_facts: Vec<ExactFactWire>,
        confidence: Confidence,
        reason: String,
    },
    Update {
        target: ExactFactWire,
        content: Option<String>,
        category: Option<FactCategoryV1>,
        source_label: Option<String>,
        tags: Option<Vec<String>>,
        entities: Option<Vec<String>>,
        metadata: Option<Value>,
        trust: Option<Confidence>,
        evidence_facts: Vec<ExactFactWire>,
        confidence: Confidence,
        reason: String,
    },
    Merge {
        winner: ExactFactWire,
        losers: Vec<ExactFactWire>,
        merged_content: Option<String>,
        evidence_facts: Vec<ExactFactWire>,
        confidence: Confidence,
        reason: String,
    },
    Remove {
        target: ExactFactWire,
        evidence_facts: Vec<ExactFactWire>,
        confidence: Confidence,
        reason: String,
    },
    NormalizeTags {
        target: ExactFactWire,
        tags: Vec<String>,
        evidence_facts: Vec<ExactFactWire>,
        confidence: Confidence,
    },
    LinkFacts {
        source: ExactFactWire,
        target: ExactFactWire,
        relation: FactRelationKindV1,
        evidence_facts: Vec<ExactFactWire>,
        confidence: Confidence,
        source_label: String,
        metadata: Value,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExactFactWire {
    fact_id: FactId,
    expected_last_event_id: FactEventId,
}

impl ExactFactWire {
    fn matches(&self, allowed: &BTreeMap<FactId, FactEventId>) -> bool {
        allowed.get(&self.fact_id) == Some(&self.expected_last_event_id)
    }

    fn into_target(self) -> ProjectMemoryCurationMutationTarget {
        ProjectMemoryCurationMutationTarget::new(self.fact_id, self.expected_last_event_id)
    }
}

impl CanonicalCurationWire {
    pub(super) fn into_operation(self) -> Result<ProjectMemoryCurationOperation, String> {
        Ok(match self {
            Self::Add {
                content,
                category,
                source_label,
                tags,
                entities,
                trust,
                metadata,
                evidence_facts,
                confidence,
                reason,
            } => ProjectMemoryCurationOperation::Add {
                request: ProjectMemoryFactAddRequest {
                    content,
                    category,
                    source_label,
                    tags,
                    entities,
                    trust,
                    metadata,
                },
                evidence_facts: evidence_facts
                    .into_iter()
                    .map(ExactFactWire::into_target)
                    .collect(),
                confidence,
                reason,
            },
            Self::Update {
                target,
                content,
                category,
                source_label,
                tags,
                entities,
                metadata,
                trust,
                evidence_facts,
                confidence,
                reason,
            } => ProjectMemoryCurationOperation::Update {
                target: target.into_target(),
                patch: ProjectMemoryFactUpdatePatchV1::new(
                    content,
                    category,
                    source_label.map(Some),
                    tags,
                    entities,
                    metadata,
                    trust,
                )
                .map_err(|error| error.to_string())?,
                evidence_facts: evidence_facts
                    .into_iter()
                    .map(ExactFactWire::into_target)
                    .collect(),
                confidence,
                reason,
            },
            Self::Merge {
                winner,
                losers,
                merged_content,
                evidence_facts,
                confidence,
                reason,
            } => ProjectMemoryCurationOperation::Merge {
                winner: winner.into_target(),
                losers: losers.into_iter().map(ExactFactWire::into_target).collect(),
                merged_content,
                evidence_facts: evidence_facts
                    .into_iter()
                    .map(ExactFactWire::into_target)
                    .collect(),
                confidence,
                reason,
            },
            Self::Remove {
                target,
                evidence_facts,
                confidence,
                reason,
            } => ProjectMemoryCurationOperation::Remove {
                target: target.into_target(),
                evidence_facts: evidence_facts
                    .into_iter()
                    .map(ExactFactWire::into_target)
                    .collect(),
                confidence,
                reason,
            },
            Self::NormalizeTags {
                target,
                tags,
                evidence_facts,
                confidence,
            } => ProjectMemoryCurationOperation::NormalizeTags {
                target: target.into_target(),
                tags,
                evidence_facts: evidence_facts
                    .into_iter()
                    .map(ExactFactWire::into_target)
                    .collect(),
                confidence,
            },
            Self::LinkFacts {
                source,
                target,
                relation,
                evidence_facts,
                confidence,
                source_label,
                metadata,
            } => ProjectMemoryCurationOperation::LinkFacts {
                source: source.into_target(),
                target: target.into_target(),
                relation,
                evidence_facts: evidence_facts
                    .into_iter()
                    .map(ExactFactWire::into_target)
                    .collect(),
                confidence,
                source_label,
                metadata,
            },
        })
    }

    fn evidence_facts(&self) -> &[ExactFactWire] {
        match self {
            Self::Add { evidence_facts, .. }
            | Self::Update { evidence_facts, .. }
            | Self::Merge { evidence_facts, .. }
            | Self::Remove { evidence_facts, .. }
            | Self::NormalizeTags { evidence_facts, .. }
            | Self::LinkFacts { evidence_facts, .. } => evidence_facts,
        }
    }
}

pub(super) fn valid_curation_op(raw: &Value, allowed: &BTreeMap<FactId, FactEventId>) -> bool {
    let Ok(operation) = serde_json::from_value::<CanonicalCurationWire>(raw.clone()) else {
        return false;
    };
    if !valid_evidence(operation.evidence_facts(), allowed) {
        return false;
    }
    let exact_identity = match &operation {
        CanonicalCurationWire::Add {
            source_label,
            reason,
            ..
        } => source_label.as_deref().is_none_or(valid_source_label) && valid_reason(reason),
        CanonicalCurationWire::Update { target, reason, .. }
        | CanonicalCurationWire::Remove { target, reason, .. } => {
            target.matches(allowed) && valid_reason(reason)
        }
        CanonicalCurationWire::Merge {
            winner,
            losers,
            reason,
            ..
        } => {
            !losers.is_empty()
                && winner.matches(allowed)
                && losers.iter().all(|target| target.matches(allowed))
                && losers.iter().all(|target| target.fact_id != winner.fact_id)
                && losers.iter().enumerate().all(|(index, target)| {
                    !losers[..index]
                        .iter()
                        .any(|previous| previous.fact_id == target.fact_id)
                })
                && valid_reason(reason)
        }
        CanonicalCurationWire::NormalizeTags { target, tags, .. } => {
            target.matches(allowed)
                && tags.len() <= 256
                && tags.iter().all(|tag| valid_curation_text(tag))
        }
        CanonicalCurationWire::LinkFacts {
            source,
            target,
            source_label,
            ..
        } => {
            valid_source_label(source_label)
                && source.fact_id != target.fact_id
                && source.matches(allowed)
                && target.matches(allowed)
        }
    };
    exact_identity && operation.into_operation().is_ok()
}

pub(super) const MODEL_CONTRACT: &str = r#"Each ops item must match exactly one shape (no extra keys):
- add: {"op":"add","content":string,"category":"general"|"user_pref"|"project"|"tool"|"decision"|"code_area","source_label"?:string,"tags":string[],"entities":string[],"trust"?:number,"metadata":object,"evidence_facts":[reviewed_fact],"confidence":number,"reason":string}
- update: {"op":"update","target":reviewed_fact,"content"?:string,"category"?:"general"|"user_pref"|"project"|"tool"|"decision"|"code_area","source_label"?:string,"tags"?:string[],"entities"?:string[],"metadata"?:object,"trust"?:number,"evidence_facts":[reviewed_fact],"confidence":number,"reason":string}; include at least one patch field.
- merge: {"op":"merge","winner":reviewed_fact,"losers":[reviewed_fact],"merged_content"?:string,"evidence_facts":[reviewed_fact],"confidence":number,"reason":string}
- remove: {"op":"remove","target":reviewed_fact,"evidence_facts":[reviewed_fact],"confidence":number,"reason":string}
- normalize_tags: {"op":"normalize_tags","target":reviewed_fact,"tags":string[],"evidence_facts":[reviewed_fact],"confidence":number}
- link_facts: {"op":"link_facts","source":reviewed_fact,"target":reviewed_fact,"relation":"supports"|"contradicts"|"supersedes"|"derived_from","evidence_facts":[reviewed_fact],"confidence":number,"source_label":string,"metadata":object}
reviewed_fact is exactly {"fact_id":fact_id,"expected_last_event_id":event_id}. For every operation, confidence must be finite in [context.min_confidence,1]. Trust, when present, must be finite in [0,1]. evidence_facts must contain 1..=256 unique IDs, each represented by the exact reviewed_fact pair copied from the supplied fact. Every reviewed_fact pair must be copied from the same supplied fact. Merge losers must be nonempty, unique, and distinct from the winner. reason and source_label must be nonblank, at most 4096 bytes, and contain no control characters."#;

fn valid_evidence(facts: &[ExactFactWire], allowed: &BTreeMap<FactId, FactEventId>) -> bool {
    !facts.is_empty()
        && facts.len() <= 256
        && facts.iter().enumerate().all(|(index, fact)| {
            fact.matches(allowed)
                && !facts[..index]
                    .iter()
                    .any(|prior| prior.fact_id == fact.fact_id)
        })
}

fn valid_reason(reason: &str) -> bool {
    !reason.trim().is_empty() && reason.len() <= 4 * 1024 && !reason.chars().any(char::is_control)
}

fn valid_source_label(source_label: &str) -> bool {
    !source_label.trim().is_empty()
        && source_label.len() <= 4 * 1024
        && !source_label.chars().any(char::is_control)
}

fn valid_curation_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4 * 1024 && !value.chars().any(char::is_control)
}
