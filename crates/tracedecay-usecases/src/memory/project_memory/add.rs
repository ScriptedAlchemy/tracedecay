//! Canonical project-memory add preflight and execution.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::{
    ActorId, Confidence, FactCategoryV1, FactOwnerV1, ProvenanceId, canonical_sha256,
};
use tracedecay_runtime_core::memory::trust::DEFAULT_TRUST;
use tracedecay_store::{
    FactWriteControl, ProjectMemoryFactAddCommandV1, ProjectMemoryFactAddMaterialV1,
    ProjectMemoryFactAddOutcomeV1, ProjectMemoryFactStore,
};

use super::super::MemoryApplication;
use super::super::context::{MemoryOperationContext, validate_operation_component};
use super::super::error::{MemoryApplicationError, MemoryMutationError, settle_authority_result};
use super::super::sanitize::{SanitizedAddFactRequest, sanitize_add_fact_request};
use super::validate_project_memory_add_outcome;

/// Transport-neutral input accepted before privacy sanitization.
///
/// Transport adapters own their wire DTOs. This single use-case request owns
/// the boundary between unsanitized user intent and the canonical store
/// command, so callers cannot accidentally bypass payload sanitization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMemoryFactAddRequest {
    pub content: String,
    pub category: FactCategoryV1,
    pub source_label: Option<String>,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub trust: Option<Confidence>,
    pub metadata: Value,
}

/// Result of the privacy boundary that precedes the canonical add authority.
///
/// Secret-like input is rejected before the store or its write control is
/// consulted. Applied requests embed the store's canonical result without
/// copying fact identity or commit-receipt fields into a second DTO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactAddRequestOutcome {
    RejectedSecretLike,
    Applied(ProjectMemoryFactAddOutcomeV1),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProjectMemoryFactAddEffectDispositionV1 {
    Ready,
    RejectedSecretLike,
}

/// Privacy-safe canonical material used to settle the retained add effect.
///
/// Accepted requests bind the store-owned input digest. Refusals bind only a
/// one-way digest of the rejected request, so secret-like input is never
/// exposed to retained-effect serialization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProjectMemoryFactAddEffectMaterialV1 {
    disposition: ProjectMemoryFactAddEffectDispositionV1,
    canonical_digest: String,
}

impl ProjectMemoryFactAddEffectMaterialV1 {
    fn ready(canonical_digest: String) -> Self {
        Self {
            disposition: ProjectMemoryFactAddEffectDispositionV1::Ready,
            canonical_digest,
        }
    }

    fn rejected_secret_like(canonical_digest: String) -> Self {
        Self {
            disposition: ProjectMemoryFactAddEffectDispositionV1::RejectedSecretLike,
            canonical_digest,
        }
    }

    pub fn canonical_digest(&self) -> &str {
        &self.canonical_digest
    }
}

/// One canonical add preflight shared by retained-effect identity and store
/// command execution.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProjectMemoryFactAddPreflight {
    #[non_exhaustive]
    RejectedSecretLike {
        effect_material: ProjectMemoryFactAddEffectMaterialV1,
        operation_id: ProvenanceId,
    },
    #[non_exhaustive]
    Ready {
        effect_material: ProjectMemoryFactAddEffectMaterialV1,
        command: ProjectMemoryFactAddCommandV1,
    },
}

impl ProjectMemoryFactAddPreflight {
    pub fn effect_material(&self) -> &ProjectMemoryFactAddEffectMaterialV1 {
        match self {
            Self::RejectedSecretLike {
                effect_material, ..
            }
            | Self::Ready {
                effect_material, ..
            } => effect_material,
        }
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        match self {
            Self::RejectedSecretLike { operation_id, .. } => operation_id,
            Self::Ready { command, .. } => command.operation_id(),
        }
    }

    pub fn command(&self) -> Option<&ProjectMemoryFactAddCommandV1> {
        match self {
            Self::RejectedSecretLike { .. } => None,
            Self::Ready { command, .. } => Some(command),
        }
    }

    pub fn into_command(self) -> Option<ProjectMemoryFactAddCommandV1> {
        match self {
            Self::RejectedSecretLike { .. } => None,
            Self::Ready { command, .. } => Some(command),
        }
    }
}

/// Converts an automation item without manufacturing a second identity. The
/// deterministic operation identity makes repeated processing of the same
/// run/apply identity idempotent at the authority boundary.
pub fn automatic_fact_add_command(
    owner: FactOwnerV1,
    request: ProjectMemoryFactAddRequest,
    run_id: &str,
    apply_id: &str,
    actor: Option<ActorId>,
) -> Result<ProjectMemoryFactAddCommandV1, MemoryApplicationError> {
    owner.validate()?;
    validate_operation_component(run_id, "automatic fact run identity")?;
    validate_operation_component(apply_id, "automatic fact apply identity")?;
    let context = MemoryOperationContext::from_request_id(
        &owner,
        "automatic-fact",
        &format!("{run_id}:{apply_id}"),
        actor,
    )?;
    let Some(request) = sanitize_add_fact_request(request)? else {
        return Err(MemoryApplicationError::InvalidInput {
            invariant: "automatic fact declined by memory privacy sanitizer",
        });
    };
    fact_add_material(
        owner,
        request,
        context.actor().cloned(),
        Some(run_id.to_owned()),
    )?
    .into_command(context.operation_id().clone())
    .map_err(MemoryApplicationError::Store)
}

fn fact_add_material(
    owner: FactOwnerV1,
    request: SanitizedAddFactRequest,
    actor: Option<ActorId>,
    automation_run_id: Option<String>,
) -> Result<ProjectMemoryFactAddMaterialV1, MemoryApplicationError> {
    let (request, sanitization_receipt) = request.into_parts();
    let trust = request
        .trust
        .unwrap_or(Confidence::new(DEFAULT_TRUST).map_err(|_| {
            MemoryApplicationError::InvalidInput {
                invariant: "default trust must be between 0.0 and 1.0",
            }
        })?);
    ProjectMemoryFactAddMaterialV1::new(
        owner,
        request.content,
        request.category,
        request.source_label,
        request.tags,
        request.entities,
        request.metadata,
        sanitization_receipt,
        automation_run_id,
        trust,
        actor,
    )
    .map_err(MemoryApplicationError::Store)
}

fn rejected_add_effect_material(
    request: &ProjectMemoryFactAddRequest,
) -> Result<ProjectMemoryFactAddEffectMaterialV1, MemoryApplicationError> {
    let digest = canonical_sha256(&("tracedecay.project-memory.fact-add-rejected.v1", request))
        .map_err(|_| MemoryApplicationError::InvalidInput {
            invariant: "project-memory rejected add identity",
        })?;
    Ok(ProjectMemoryFactAddEffectMaterialV1::rejected_secret_like(
        digest.as_str().to_owned(),
    ))
}

impl<A: ProjectMemoryFactStore> MemoryApplication<A> {
    /// Canonicalizes one retained add before any external effect identity is
    /// prepared. The accepted path and store command share the store-owned
    /// input digest; the refusal path contains no command or raw secret.
    pub fn preflight_project_memory_fact_add(
        &self,
        request: ProjectMemoryFactAddRequest,
        actor: Option<ActorId>,
    ) -> Result<ProjectMemoryFactAddPreflight, MemoryApplicationError> {
        let rejected_effect_material = rejected_add_effect_material(&request)?;
        let Some(request) = sanitize_add_fact_request(request)? else {
            let context = MemoryOperationContext::from_logical_effect(
                &self.owner,
                "add",
                &rejected_effect_material,
                actor,
            )?;
            return Ok(ProjectMemoryFactAddPreflight::RejectedSecretLike {
                effect_material: rejected_effect_material,
                operation_id: context.operation_id().clone(),
            });
        };
        let material = fact_add_material(self.owner.clone(), request, actor.clone(), None)?;
        let effect_material =
            ProjectMemoryFactAddEffectMaterialV1::ready(material.input_digest().to_owned());
        let context = MemoryOperationContext::from_logical_effect(
            &self.owner,
            "add",
            &effect_material,
            actor,
        )?;
        let command = material
            .into_command(context.operation_id().clone())
            .map_err(MemoryApplicationError::Store)?;
        Ok(ProjectMemoryFactAddPreflight::Ready {
            effect_material,
            command,
        })
    }

    pub async fn add_project_memory_fact(
        &self,
        request: ProjectMemoryFactAddCommandV1,
        write_control: &FactWriteControl,
    ) -> Result<ProjectMemoryFactAddOutcomeV1, MemoryMutationError<ProjectMemoryFactAddOutcomeV1>>
    {
        self.ensure_owner(request.owner())?;
        let outcome = self
            .authority
            .add_project_memory_fact(request, write_control)
            .await
            .map_err(MemoryApplicationError::from)?;
        settle_authority_result(outcome, |outcome| {
            validate_project_memory_add_outcome(&self.owner, outcome)
        })
    }

    /// Consumes a canonical add preflight. Privacy refusals are truthful
    /// no-write outcomes and committed authority failures retain their exact
    /// outcome for external partial-effect settlement.
    pub async fn add_preflighted_project_memory_fact(
        &self,
        preflight: ProjectMemoryFactAddPreflight,
        write_control: &FactWriteControl,
    ) -> Result<
        ProjectMemoryFactAddRequestOutcome,
        MemoryMutationError<ProjectMemoryFactAddRequestOutcome>,
    > {
        let Some(command) = preflight.into_command() else {
            return Ok(ProjectMemoryFactAddRequestOutcome::RejectedSecretLike);
        };
        self.add_project_memory_fact(command, write_control)
            .await
            .map(ProjectMemoryFactAddRequestOutcome::Applied)
            .map_err(|error| {
                error.map_authority_result(ProjectMemoryFactAddRequestOutcome::Applied)
            })
    }
}
