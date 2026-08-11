use serde_json::Value;
use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactCategoryV1, FactEventId, FactOwnerV1, FactPayloadV1,
    PayloadAccessState, ProvenanceId, SanitizationReceiptV1, SanitizerDispositionV1,
    canonical_sha256,
};

use super::super::super::{
    FactCommitReceipt, FactStoreError, FactStoreResult, MAX_PROJECT_MEMORY_REASON_BYTES,
    ProjectMemoryFactFeedbackActionV1,
};
use super::super::{
    ProjectMemoryFactIdV1, ProjectMemoryFactProjectionV1, validate_project_memory_text,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactAddMaterialV1 {
    owner: FactOwnerV1,
    content: String,
    category: FactCategoryV1,
    source_label: Option<String>,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
    sanitization_receipt: SanitizationReceiptV1,
    /// Durable automation identity. This is command metadata, deliberately
    /// separate from the fact payload metadata that passes through privacy
    /// sanitization.
    automation_run_id: Option<String>,
    default_trust: Confidence,
    actor: Option<ActorId>,
    input_digest: String,
}

impl ProjectMemoryFactAddMaterialV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        content: String,
        category: FactCategoryV1,
        source_label: Option<String>,
        mut tags: Vec<String>,
        mut entities: Vec<String>,
        mut metadata: Value,
        sanitization_receipt: SanitizationReceiptV1,
        automation_run_id: Option<String>,
        default_trust: Confidence,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        if let Some(run_id) = automation_run_id.as_deref() {
            validate_project_memory_text(run_id, "project memory fact automation run identity")?;
        }
        if let Some(object) = metadata.as_object_mut() {
            object.remove("automation_run_id");
        }
        if !matches!(
            sanitization_receipt.disposition(),
            SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted
        ) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact add sanitization disposition",
            }));
        }
        let payload_reference = FactPayloadV1::canonicalize_material(
            &content,
            category,
            &mut tags,
            &mut entities,
            &metadata,
            source_label.as_deref(),
        )?;
        if sanitization_receipt.payload() != Some(&payload_reference) {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "project memory fact add sanitization receipt",
            }));
        }
        let input_digest = project_memory_fact_add_input_digest(
            &owner,
            &content,
            category,
            source_label.as_deref(),
            &tags,
            &entities,
            &metadata,
            &sanitization_receipt,
            automation_run_id.as_deref(),
            default_trust,
            actor.as_ref(),
        )?;
        Ok(Self {
            owner,
            content,
            category,
            source_label,
            tags,
            entities,
            metadata,
            sanitization_receipt,
            automation_run_id,
            default_trust,
            actor,
            input_digest,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn category(&self) -> FactCategoryV1 {
        self.category
    }
    pub fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
    pub fn entities(&self) -> &[String] {
        &self.entities
    }
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }
    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        &self.sanitization_receipt
    }
    pub fn with_automation_run_id(mut self, run_id: String) -> FactStoreResult<Self> {
        validate_project_memory_text(&run_id, "project memory fact automation run identity")?;
        self.automation_run_id = Some(run_id);
        self.input_digest = project_memory_fact_add_input_digest(
            &self.owner,
            &self.content,
            self.category,
            self.source_label.as_deref(),
            &self.tags,
            &self.entities,
            &self.metadata,
            &self.sanitization_receipt,
            self.automation_run_id.as_deref(),
            self.default_trust,
            self.actor.as_ref(),
        )?;
        Ok(self)
    }
    pub fn automation_run_id(&self) -> Option<&str> {
        self.automation_run_id.as_deref()
    }
    pub fn default_trust(&self) -> Confidence {
        self.default_trust
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }

    pub fn input_digest(&self) -> &str {
        &self.input_digest
    }

    pub fn into_command(
        self,
        operation_id: ProvenanceId,
    ) -> FactStoreResult<ProjectMemoryFactAddCommandV1> {
        operation_id.validate()?;
        Ok(ProjectMemoryFactAddCommandV1 {
            material: self,
            operation_id,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn project_memory_fact_add_input_digest(
    owner: &FactOwnerV1,
    content: &str,
    category: FactCategoryV1,
    source_label: Option<&str>,
    tags: &[String],
    entities: &[String],
    metadata: &Value,
    sanitization_receipt: &SanitizationReceiptV1,
    automation_run_id: Option<&str>,
    default_trust: Confidence,
    actor: Option<&ActorId>,
) -> FactStoreResult<String> {
    let mut material = serde_json::json!({
        "owner": owner,
        "content": content,
        "category": category,
        "tags": tags,
        "entities": entities,
        "metadata": metadata,
        "sanitization_receipt": sanitization_receipt,
        "automation_run_id": automation_run_id,
        "default_trust": default_trust.as_f64(),
        "actor": actor.map(ActorId::as_str),
    });
    if let (Value::Object(material), Some(source_label)) = (&mut material, source_label) {
        material.insert(
            "source_label".to_owned(),
            Value::String(source_label.to_owned()),
        );
    }
    let digest = canonical_sha256(&("tracedecay.project-memory.fact-add-input.v1", material))?;
    digest
        .as_str()
        .strip_prefix("sha256:")
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact add input digest",
            })
        })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactAddCommandV1 {
    material: ProjectMemoryFactAddMaterialV1,
    operation_id: ProvenanceId,
}

impl ProjectMemoryFactAddCommandV1 {
    pub fn owner(&self) -> &FactOwnerV1 {
        self.material.owner()
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn content(&self) -> &str {
        self.material.content()
    }
    pub fn category(&self) -> FactCategoryV1 {
        self.material.category()
    }
    pub fn source_label(&self) -> Option<&str> {
        self.material.source_label()
    }
    pub fn tags(&self) -> &[String] {
        self.material.tags()
    }
    pub fn entities(&self) -> &[String] {
        self.material.entities()
    }
    pub fn metadata(&self) -> &Value {
        self.material.metadata()
    }
    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        self.material.sanitization_receipt()
    }
    pub fn automation_run_id(&self) -> Option<&str> {
        self.material.automation_run_id()
    }
    pub fn default_trust(&self) -> Confidence {
        self.material.default_trust()
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.material.actor()
    }
    pub fn input_digest(&self) -> &str {
        self.material.input_digest()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactUpdatePatchV1 {
    content: Option<String>,
    category: Option<FactCategoryV1>,
    source_label: Option<Option<String>>,
    tags: Option<Vec<String>>,
    entities: Option<Vec<String>>,
    metadata: Option<Value>,
    trust: Option<Confidence>,
}

impl ProjectMemoryFactUpdatePatchV1 {
    pub fn new(
        content: Option<String>,
        category: Option<FactCategoryV1>,
        source_label: Option<Option<String>>,
        tags: Option<Vec<String>>,
        entities: Option<Vec<String>>,
        metadata: Option<Value>,
        trust: Option<Confidence>,
    ) -> FactStoreResult<Self> {
        if content.is_none()
            && category.is_none()
            && source_label.is_none()
            && tags.is_none()
            && entities.is_none()
            && metadata.is_none()
            && trust.is_none()
        {
            return Err(FactStoreError::Contract(DomainError::Empty {
                field: "project memory fact update patch",
            }));
        }
        if content
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact update content",
            }));
        }
        if source_label.as_ref().is_some_and(|value| {
            value.as_ref().is_some_and(|source_label| {
                source_label.trim().is_empty()
                    || source_label.len() > MAX_PROJECT_MEMORY_REASON_BYTES
            })
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact update source label",
            }));
        }
        Ok(Self {
            content,
            category,
            source_label,
            tags,
            entities,
            metadata,
            trust,
        })
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }
    pub fn source_label(&self) -> Option<Option<&str>> {
        self.source_label.as_ref().map(|value| value.as_deref())
    }
    pub fn tags(&self) -> Option<&[String]> {
        self.tags.as_deref()
    }
    pub fn entities(&self) -> Option<&[String]> {
        self.entities.as_deref()
    }
    pub fn metadata(&self) -> Option<&Value> {
        self.metadata.as_ref()
    }
    pub fn trust(&self) -> Option<Confidence> {
        self.trust
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactUpdateCommandV1 {
    target: ProjectMemoryFactIdV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    patch: ProjectMemoryFactUpdatePatchV1,
    actor: Option<ActorId>,
}

impl ProjectMemoryFactUpdateCommandV1 {
    pub fn new(
        target: ProjectMemoryFactIdV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        patch: ProjectMemoryFactUpdatePatchV1,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            patch,
            actor,
        })
    }

    pub fn target(&self) -> &ProjectMemoryFactIdV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn patch(&self) -> &ProjectMemoryFactUpdatePatchV1 {
        &self.patch
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactRemoveCommandV1 {
    target: ProjectMemoryFactIdV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
}

impl ProjectMemoryFactRemoveCommandV1 {
    pub fn new(
        target: ProjectMemoryFactIdV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            actor,
        })
    }

    pub fn target(&self) -> &ProjectMemoryFactIdV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactFeedbackCommandV1 {
    target: ProjectMemoryFactIdV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    action: ProjectMemoryFactFeedbackActionV1,
    actor: Option<ActorId>,
    source_label: Option<String>,
    reason: Option<String>,
}

impl ProjectMemoryFactFeedbackCommandV1 {
    pub fn new(
        target: ProjectMemoryFactIdV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        action: ProjectMemoryFactFeedbackActionV1,
        actor: Option<ActorId>,
        source_label: Option<String>,
        reason: Option<String>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        if source_label.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_PROJECT_MEMORY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact feedback source label",
            }));
        }
        if reason.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_PROJECT_MEMORY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact feedback reason",
            }));
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            action,
            actor,
            source_label,
            reason,
        })
    }

    pub fn target(&self) -> &ProjectMemoryFactIdV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn action(&self) -> ProjectMemoryFactFeedbackActionV1 {
        self.action
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
    pub fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactAddDispositionV1 {
    Added,
    NearDuplicate,
    PossibleConflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactAddOutcomeV1 {
    fact: ProjectMemoryFactProjectionV1,
    disposition: ProjectMemoryFactAddDispositionV1,
    closest_fact_id: Option<ProjectMemoryFactIdV1>,
    similarity_millionths: Option<u32>,
    commit_receipt: Option<FactCommitReceipt>,
    commit_replayed: bool,
}

impl ProjectMemoryFactAddOutcomeV1 {
    pub fn added(
        fact: ProjectMemoryFactProjectionV1,
        commit_receipt: FactCommitReceipt,
        commit_replayed: bool,
    ) -> FactStoreResult<Self> {
        if fact.owner() != commit_receipt.owner() || fact.fact_id() != commit_receipt.fact_id() {
            return Err(FactStoreError::InvalidCommitReceipt);
        }
        Ok(Self {
            fact,
            disposition: ProjectMemoryFactAddDispositionV1::Added,
            closest_fact_id: None,
            similarity_millionths: None,
            commit_receipt: Some(commit_receipt),
            commit_replayed,
        })
    }

    pub fn normalized_duplicate(
        fact: ProjectMemoryFactProjectionV1,
        closest_fact_id: ProjectMemoryFactIdV1,
    ) -> FactStoreResult<Self> {
        if fact.owner() != closest_fact_id.owner() || fact.fact_id() != closest_fact_id.fact_id() {
            return Err(FactStoreError::FactMismatch);
        }
        Ok(Self {
            fact,
            disposition: ProjectMemoryFactAddDispositionV1::NearDuplicate,
            closest_fact_id: Some(closest_fact_id),
            similarity_millionths: Some(1_000_000),
            commit_receipt: None,
            commit_replayed: false,
        })
    }

    pub fn semantic_near_duplicate(
        fact: ProjectMemoryFactProjectionV1,
        closest_fact_id: ProjectMemoryFactIdV1,
        similarity_millionths: u32,
        commit_receipt: FactCommitReceipt,
        commit_replayed: bool,
    ) -> FactStoreResult<Self> {
        Self::committed_comparison(
            fact,
            ProjectMemoryFactAddDispositionV1::NearDuplicate,
            closest_fact_id,
            similarity_millionths,
            commit_receipt,
            commit_replayed,
        )
    }

    pub fn possible_conflict(
        fact: ProjectMemoryFactProjectionV1,
        closest_fact_id: ProjectMemoryFactIdV1,
        similarity_millionths: u32,
        commit_receipt: FactCommitReceipt,
        commit_replayed: bool,
    ) -> FactStoreResult<Self> {
        Self::committed_comparison(
            fact,
            ProjectMemoryFactAddDispositionV1::PossibleConflict,
            closest_fact_id,
            similarity_millionths,
            commit_receipt,
            commit_replayed,
        )
    }

    fn committed_comparison(
        fact: ProjectMemoryFactProjectionV1,
        disposition: ProjectMemoryFactAddDispositionV1,
        closest_fact_id: ProjectMemoryFactIdV1,
        similarity_millionths: u32,
        commit_receipt: FactCommitReceipt,
        commit_replayed: bool,
    ) -> FactStoreResult<Self> {
        if fact.owner() != commit_receipt.owner()
            || fact.fact_id() != commit_receipt.fact_id()
            || fact.owner() != closest_fact_id.owner()
            || fact.fact_id() == closest_fact_id.fact_id()
            || similarity_millionths > 1_000_000
        {
            return Err(FactStoreError::InvalidCommitReceipt);
        }
        Ok(Self {
            fact,
            disposition,
            closest_fact_id: Some(closest_fact_id),
            similarity_millionths: Some(similarity_millionths),
            commit_receipt: Some(commit_receipt),
            commit_replayed,
        })
    }

    pub fn fact(&self) -> &ProjectMemoryFactProjectionV1 {
        &self.fact
    }
    pub fn disposition(&self) -> ProjectMemoryFactAddDispositionV1 {
        self.disposition
    }
    pub fn closest_fact_id(&self) -> Option<&ProjectMemoryFactIdV1> {
        self.closest_fact_id.as_ref()
    }
    pub fn similarity_millionths(&self) -> Option<u32> {
        self.similarity_millionths
    }
    pub fn commit_receipt(&self) -> Option<&FactCommitReceipt> {
        self.commit_receipt.as_ref()
    }
    pub fn commit_replayed(&self) -> bool {
        self.commit_replayed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactUpdateOutcomeV1 {
    fact: ProjectMemoryFactProjectionV1,
    trust_delta_millionths: i32,
    commit_receipt: FactCommitReceipt,
    commit_replayed: bool,
}

impl ProjectMemoryFactUpdateOutcomeV1 {
    pub fn committed(
        fact: ProjectMemoryFactProjectionV1,
        trust_delta_millionths: i32,
        commit_receipt: FactCommitReceipt,
        commit_replayed: bool,
    ) -> FactStoreResult<Self> {
        if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "project memory fact update trust delta",
            }));
        }
        if fact.owner() != commit_receipt.owner() || fact.fact_id() != commit_receipt.fact_id() {
            return Err(FactStoreError::InvalidCommitReceipt);
        }
        Ok(Self {
            fact,
            trust_delta_millionths,
            commit_receipt,
            commit_replayed,
        })
    }

    pub fn fact(&self) -> &ProjectMemoryFactProjectionV1 {
        &self.fact
    }
    pub fn trust_delta_millionths(&self) -> i32 {
        self.trust_delta_millionths
    }
    pub fn commit_receipt(&self) -> &FactCommitReceipt {
        &self.commit_receipt
    }
    pub fn commit_replayed(&self) -> bool {
        self.commit_replayed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactRemoveOutcomeV1 {
    /// `None` only for the idempotent no-op disposition: the target never
    /// resolved to a stored fact inside this transaction (never added, or
    /// concurrently removed just before this attempt), so there is no
    /// projection to report. `removed` and `remaining_fact_count` remain
    /// meaningful in that case.
    fact: Option<ProjectMemoryFactProjectionV1>,
    removed: bool,
    remaining_fact_count: u64,
    commit_receipt: Option<FactCommitReceipt>,
    commit_replayed: bool,
}

impl ProjectMemoryFactRemoveOutcomeV1 {
    pub fn removed(
        fact: ProjectMemoryFactProjectionV1,
        remaining_fact_count: u64,
        commit_receipt: FactCommitReceipt,
        commit_replayed: bool,
    ) -> FactStoreResult<Self> {
        if fact.owner() != commit_receipt.owner() || fact.fact_id() != commit_receipt.fact_id() {
            return Err(FactStoreError::InvalidCommitReceipt);
        }
        if !matches!(
            &fact,
            ProjectMemoryFactProjectionV1::Unavailable(fact)
                if fact.payload_access() == PayloadAccessState::Deleted
        ) {
            return Err(FactStoreError::PayloadAccessMismatch);
        }
        Ok(Self {
            fact: Some(fact),
            removed: true,
            remaining_fact_count,
            commit_receipt: Some(commit_receipt),
            commit_replayed,
        })
    }

    pub fn already_removed(
        fact: ProjectMemoryFactProjectionV1,
        remaining_fact_count: u64,
    ) -> FactStoreResult<Self> {
        if !matches!(
            &fact,
            ProjectMemoryFactProjectionV1::Unavailable(fact)
                if fact.payload_access() == PayloadAccessState::Deleted
        ) {
            return Err(FactStoreError::PayloadAccessMismatch);
        }
        Ok(Self {
            fact: Some(fact),
            removed: false,
            remaining_fact_count,
            commit_receipt: None,
            commit_replayed: false,
        })
    }

    /// Idempotent no-op outcome for a remove target that never resolved to a
    /// stored fact within the authority's single remove transaction.
    /// `removed()` is always `false` here, matching the pre-existing
    /// idempotent-success contract for removing an already-absent fact.
    pub fn not_found(remaining_fact_count: u64) -> Self {
        Self {
            fact: None,
            removed: false,
            remaining_fact_count,
            commit_receipt: None,
            commit_replayed: false,
        }
    }

    pub fn fact(&self) -> Option<&ProjectMemoryFactProjectionV1> {
        self.fact.as_ref()
    }
    pub fn was_removed(&self) -> bool {
        self.removed
    }
    pub fn remaining_fact_count(&self) -> u64 {
        self.remaining_fact_count
    }
    pub fn commit_receipt(&self) -> Option<&FactCommitReceipt> {
        self.commit_receipt.as_ref()
    }
    pub fn commit_replayed(&self) -> bool {
        self.commit_replayed
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactFeedbackOutcomeV1 {
    fact: ProjectMemoryFactProjectionV1,
    event_id: FactEventId,
    old_trust: Confidence,
    new_trust: Confidence,
    trust_delta_millionths: i32,
    helpful_count: u64,
    unhelpful_count: u64,
    commit_receipt: FactCommitReceipt,
    commit_replayed: bool,
}

impl ProjectMemoryFactFeedbackOutcomeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn committed(
        fact: ProjectMemoryFactProjectionV1,
        event_id: FactEventId,
        old_trust: Confidence,
        new_trust: Confidence,
        trust_delta_millionths: i32,
        helpful_count: u64,
        unhelpful_count: u64,
        commit_receipt: FactCommitReceipt,
        commit_replayed: bool,
    ) -> FactStoreResult<Self> {
        event_id.validate()?;
        validate_feedback_trust_delta(old_trust, new_trust, trust_delta_millionths)?;
        if fact.owner() != commit_receipt.owner()
            || fact.fact_id() != commit_receipt.fact_id()
            || &event_id != commit_receipt.last_event_id()
        {
            return Err(FactStoreError::InvalidCommitReceipt);
        }
        Ok(Self {
            fact,
            event_id,
            old_trust,
            new_trust,
            trust_delta_millionths,
            helpful_count,
            unhelpful_count,
            commit_receipt,
            commit_replayed,
        })
    }

    pub fn fact(&self) -> &ProjectMemoryFactProjectionV1 {
        &self.fact
    }
    pub fn event_id(&self) -> &FactEventId {
        &self.event_id
    }
    pub fn old_trust(&self) -> Confidence {
        self.old_trust
    }
    pub fn new_trust(&self) -> Confidence {
        self.new_trust
    }
    pub fn trust_delta_millionths(&self) -> i32 {
        self.trust_delta_millionths
    }
    pub fn helpful_count(&self) -> u64 {
        self.helpful_count
    }
    pub fn unhelpful_count(&self) -> u64 {
        self.unhelpful_count
    }
    pub fn commit_receipt(&self) -> &FactCommitReceipt {
        &self.commit_receipt
    }
    pub fn commit_replayed(&self) -> bool {
        self.commit_replayed
    }
}

fn validate_feedback_trust_delta(
    old_trust: Confidence,
    new_trust: Confidence,
    trust_delta_millionths: i32,
) -> FactStoreResult<()> {
    let expected = ((new_trust.as_f64() - old_trust.as_f64()) * 1_000_000.0).round() as i32;
    if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths)
        || trust_delta_millionths != expected
    {
        return Err(FactStoreError::Contract(DomainError::NonCanonical {
            field: "project memory fact feedback trust delta",
        }));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_trust_delta_must_match_the_bound_transition() {
        let old = Confidence::new(0.5).unwrap();
        let new = Confidence::new(0.6).unwrap();
        assert!(validate_feedback_trust_delta(old, new, 100_000).is_ok());
        assert!(validate_feedback_trust_delta(old, new, -100_000).is_err());
    }
}
