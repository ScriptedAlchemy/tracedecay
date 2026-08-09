use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::{
    DomainError, FactAssertionId, FactEventId, FactId, FactOwnerV1, ProvenanceId, UtcMicros,
};

use super::super::queries::MAX_CURRENT_LIMIT;
use super::super::{
    FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_REASON_BYTES, validate_owned_fact_id,
};
use super::{ProjectMemoryFactAddCommandV1, ProjectMemoryFactMappingV1};

/// The only durable outcomes of an automatic fact apply. Candidate discovery
/// and in-flight work are owned by the automation run receipt, never this
/// terminal audit record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMemoryAutomaticFactStateV1 {
    Applied,
    Quarantined,
}

/// Automation evidence retained with the terminal apply receipt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMemoryAutomaticFactEvidenceV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    validation: Option<Value>,
}

impl ProjectMemoryAutomaticFactEvidenceV1 {
    pub fn new(
        evidence_hash: Option<String>,
        item: Option<Value>,
        validation: Option<Value>,
    ) -> FactStoreResult<Self> {
        let evidence = Self {
            evidence_hash,
            item,
            validation,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> FactStoreResult<()> {
        if self.evidence_hash.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > 160 || value.chars().any(char::is_control)
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "automatic fact evidence hash",
            }));
        }
        Ok(())
    }

    pub fn evidence_hash(&self) -> Option<&str> {
        self.evidence_hash.as_deref()
    }

    pub fn item(&self) -> Option<&Value> {
        self.item.as_ref()
    }

    pub fn validation(&self) -> Option<&Value> {
        self.validation.as_ref()
    }
}

/// The durable effect of one automatic apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectMemoryAutomaticFactEffectV1 {
    Applied {
        fact_id: FactId,
        mapping: ProjectMemoryFactMappingV1,
        assertion_id: FactAssertionId,
        event_id: FactEventId,
    },
    Quarantined {
        reason: String,
    },
}

impl ProjectMemoryAutomaticFactEffectV1 {
    fn validate(&self, owner: &FactOwnerV1) -> FactStoreResult<()> {
        match self {
            Self::Applied {
                fact_id, mapping, ..
            } => {
                validate_owned_fact_id(fact_id, owner)?;
                if mapping.owner() != owner || mapping.fact_id() != fact_id {
                    return Err(FactStoreError::FactMismatch);
                }
            }
            Self::Quarantined { reason } => {
                if reason.trim().is_empty() || reason.len() > MAX_PROJECT_MEMORY_REASON_BYTES {
                    return Err(FactStoreError::Contract(DomainError::NonCanonical {
                        field: "automatic fact quarantine reason",
                    }));
                }
            }
        }
        Ok(())
    }

    pub const fn state(&self) -> ProjectMemoryAutomaticFactStateV1 {
        match self {
            Self::Applied { .. } => ProjectMemoryAutomaticFactStateV1::Applied,
            Self::Quarantined { .. } => ProjectMemoryAutomaticFactStateV1::Quarantined,
        }
    }

    pub fn applied_fact_id(&self) -> Option<&FactId> {
        match self {
            Self::Applied { fact_id, .. } => Some(fact_id),
            Self::Quarantined { .. } => None,
        }
    }

    pub fn applied_mapping(&self) -> Option<&ProjectMemoryFactMappingV1> {
        match self {
            Self::Applied { mapping, .. } => Some(mapping),
            Self::Quarantined { .. } => None,
        }
    }

    pub fn applied_assertion_id(&self) -> Option<&FactAssertionId> {
        match self {
            Self::Applied { assertion_id, .. } => Some(assertion_id),
            Self::Quarantined { .. } => None,
        }
    }

    pub fn applied_event_id(&self) -> Option<&FactEventId> {
        match self {
            Self::Applied { event_id, .. } => Some(event_id),
            Self::Quarantined { .. } => None,
        }
    }

    pub fn quarantine_reason(&self) -> Option<&str> {
        match self {
            Self::Applied { .. } => None,
            Self::Quarantined { reason } => Some(reason),
        }
    }
}

/// Immutable terminal audit receipt for an automatic apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryAutomaticFactReceiptV1 {
    apply_id: ProvenanceId,
    owner: FactOwnerV1,
    state: ProjectMemoryAutomaticFactStateV1,
    request: ProjectMemoryFactAddCommandV1,
    evidence: ProjectMemoryAutomaticFactEvidenceV1,
    effect: ProjectMemoryAutomaticFactEffectV1,
    recorded_at: UtcMicros,
}

impl ProjectMemoryAutomaticFactReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        apply_id: ProvenanceId,
        owner: FactOwnerV1,
        state: ProjectMemoryAutomaticFactStateV1,
        request: ProjectMemoryFactAddCommandV1,
        evidence: ProjectMemoryAutomaticFactEvidenceV1,
        effect: ProjectMemoryAutomaticFactEffectV1,
        recorded_at: UtcMicros,
    ) -> FactStoreResult<Self> {
        apply_id.validate()?;
        owner.validate()?;
        if request.owner() != &owner {
            return Err(FactStoreError::OwnerMismatch);
        }
        evidence.validate()?;
        effect.validate(&owner)?;
        if effect.state() != state {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "automatic fact receipt state and effect",
            }));
        }
        Ok(Self {
            apply_id,
            owner,
            state,
            request,
            evidence,
            effect,
            recorded_at,
        })
    }

    pub fn apply_id(&self) -> &ProvenanceId {
        &self.apply_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub const fn state(&self) -> ProjectMemoryAutomaticFactStateV1 {
        self.state
    }

    pub fn request(&self) -> &ProjectMemoryFactAddCommandV1 {
        &self.request
    }

    pub fn automation_run_id(&self) -> Option<&str> {
        self.request.automation_run_id()
    }

    pub fn evidence(&self) -> &ProjectMemoryAutomaticFactEvidenceV1 {
        &self.evidence
    }

    pub fn effect(&self) -> &ProjectMemoryAutomaticFactEffectV1 {
        &self.effect
    }

    pub fn applied_fact_id(&self) -> Option<&FactId> {
        self.effect.applied_fact_id()
    }

    pub fn applied_mapping(&self) -> Option<&ProjectMemoryFactMappingV1> {
        self.effect.applied_mapping()
    }

    pub fn applied_assertion_id(&self) -> Option<&FactAssertionId> {
        self.effect.applied_assertion_id()
    }

    pub fn applied_event_id(&self) -> Option<&FactEventId> {
        self.effect.applied_event_id()
    }

    pub fn quarantine_reason(&self) -> Option<&str> {
        self.effect.quarantine_reason()
    }

    pub const fn recorded_at(&self) -> UtcMicros {
        self.recorded_at
    }
}

/// An apply disposition comes from the authority transaction or its replay
/// receipt, never from a caller-side pre-read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryAutomaticFactApplyDispositionV1 {
    Applied,
    AlreadyApplied,
    Quarantined,
}

/// Atomic automatic apply result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryAutomaticFactApplyResultV1 {
    receipt: ProjectMemoryAutomaticFactReceiptV1,
    disposition: ProjectMemoryAutomaticFactApplyDispositionV1,
}

impl ProjectMemoryAutomaticFactApplyResultV1 {
    pub fn new(
        receipt: ProjectMemoryAutomaticFactReceiptV1,
        disposition: ProjectMemoryAutomaticFactApplyDispositionV1,
    ) -> FactStoreResult<Self> {
        let valid = matches!(
            (receipt.state(), disposition),
            (
                ProjectMemoryAutomaticFactStateV1::Applied,
                ProjectMemoryAutomaticFactApplyDispositionV1::Applied
                    | ProjectMemoryAutomaticFactApplyDispositionV1::AlreadyApplied,
            ) | (
                ProjectMemoryAutomaticFactStateV1::Quarantined,
                ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined,
            )
        );
        if !valid {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "automatic fact apply result state",
            }));
        }
        Ok(Self {
            receipt,
            disposition,
        })
    }

    pub fn receipt(&self) -> &ProjectMemoryAutomaticFactReceiptV1 {
        &self.receipt
    }

    pub const fn disposition(&self) -> ProjectMemoryAutomaticFactApplyDispositionV1 {
        self.disposition
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryAutomaticFactReceiptPageV1 {
    owner: FactOwnerV1,
    receipts: Vec<ProjectMemoryAutomaticFactReceiptV1>,
    next_after_apply_id: Option<ProvenanceId>,
}

impl ProjectMemoryAutomaticFactReceiptPageV1 {
    pub fn new(
        owner: FactOwnerV1,
        receipts: Vec<ProjectMemoryAutomaticFactReceiptV1>,
        next_after_apply_id: Option<ProvenanceId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if receipts.len() > MAX_CURRENT_LIMIT {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: receipts.len(),
                max: MAX_CURRENT_LIMIT,
            });
        }
        let mut previous: Option<&ProvenanceId> = None;
        for receipt in &receipts {
            if receipt.owner() != &owner {
                return Err(FactStoreError::OwnerMismatch);
            }
            if previous.is_some_and(|value| value >= receipt.apply_id()) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "automatic fact receipt page order",
                }));
            }
            previous = Some(receipt.apply_id());
        }
        if let Some(cursor) = &next_after_apply_id {
            cursor.validate()?;
            if previous.is_some_and(|last| cursor <= last) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "automatic fact receipt page cursor",
                }));
            }
        }
        Ok(Self {
            owner,
            receipts,
            next_after_apply_id,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn receipts(&self) -> &[ProjectMemoryAutomaticFactReceiptV1] {
        &self.receipts
    }

    pub fn next_after_apply_id(&self) -> Option<&ProvenanceId> {
        self.next_after_apply_id.as_ref()
    }
}
