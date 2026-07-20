use serde::{Deserialize, Serialize};
use tracedecay_tool_catalog::{SchemaId, SchemaRef};

use crate::context::{RequestId, ResolvedScope};
use crate::error::ApplicationContractError;

use super::{ApplicationProblem, EffectResult, EvidencePacket, PreviewResult};

/// Versioned schema identity for an application result contract.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResultContractRef {
    schema_id: SchemaId,
    schema_revision: u32,
}

impl ResultContractRef {
    pub fn new(
        schema_id: SchemaId,
        schema_revision: u32,
    ) -> Result<Self, ApplicationContractError> {
        if schema_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "result schema revision",
            });
        }
        Ok(Self {
            schema_id,
            schema_revision,
        })
    }

    pub fn from_schema(schema: &SchemaRef) -> Self {
        Self {
            schema_id: schema.schema_id().clone(),
            schema_revision: schema.revision(),
        }
    }

    pub fn schema_id(&self) -> &SchemaId {
        &self.schema_id
    }

    pub const fn schema_revision(&self) -> u32 {
        self.schema_revision
    }
}

/// Canonical outcome family for an admitted application operation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "value")]
pub enum ApplicationOutcome<T> {
    Evidence(EvidencePacket<T>),
    Preview(PreviewResult<T>),
    Effect(EffectResult<T>),
}

/// Successful application result with a stable contract, request, and scope.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationEnvelope<T> {
    pub contract: ResultContractRef,
    pub request_id: RequestId,
    pub scope: ResolvedScope,
    pub outcome: ApplicationOutcome<T>,
}

impl<T> ApplicationEnvelope<T> {
    pub fn evidence(
        contract: ResultContractRef,
        request_id: RequestId,
        scope: ResolvedScope,
        packet: EvidencePacket<T>,
    ) -> Self {
        Self {
            contract,
            request_id,
            scope,
            outcome: ApplicationOutcome::Evidence(packet),
        }
    }

    pub fn preview(
        contract: ResultContractRef,
        request_id: RequestId,
        scope: ResolvedScope,
        preview: PreviewResult<T>,
    ) -> Self {
        Self {
            contract,
            request_id,
            scope,
            outcome: ApplicationOutcome::Preview(preview),
        }
    }

    pub fn effect(
        contract: ResultContractRef,
        request_id: RequestId,
        scope: ResolvedScope,
        effect: EffectResult<T>,
    ) -> Self {
        Self {
            contract,
            request_id,
            scope,
            outcome: ApplicationOutcome::Effect(effect),
        }
    }
}

/// Stable pre-admission failure envelope. Admitted terminal failures stay in
/// their evidence, preview, or effect receipt instead.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProblemEnvelope {
    pub contract: ResultContractRef,
    pub request_id: RequestId,
    pub problem: ApplicationProblem,
}

impl ApplicationProblemEnvelope {
    pub fn new(
        contract: ResultContractRef,
        request_id: RequestId,
        problem: ApplicationProblem,
    ) -> Self {
        Self {
            contract,
            request_id,
            problem,
        }
    }
}

pub type ApplicationResult<T> = Result<ApplicationEnvelope<T>, ApplicationProblemEnvelope>;
