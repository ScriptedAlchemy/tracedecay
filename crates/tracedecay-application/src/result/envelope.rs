use serde::{Deserialize, Serialize};
use tracedecay_tool_catalog::{SchemaId, SchemaRef};

use crate::context::{RequestId, ResolvedScope};
use crate::error::ApplicationContractError;

use super::{
    ApplicationProblem, ApplicationProblemKind, CancellationStage, EffectResult, EvidenceCoverage,
    EvidencePacket, LegalAction, PreviewResult, ProblemOwningLayer, ProblemTerminality,
    RetryDirective, RetryScope, SafeDiagnostic,
};

pub const APPLICATION_PROBLEM_REVISION: u32 = 1;
pub const MAX_PROBLEM_DETAILS: usize = 8;
pub const MAX_RETRY_AFTER_MILLIS: u64 = 24 * 60 * 60 * 1_000;

/// Canonical delay stamped whenever a problem carries
/// `RetryDirective::AfterDelay` and its producer supplied no explicit figure.
/// Matches the saturation backoff the operation-event stream already uses.
pub const DEFAULT_RETRY_AFTER_MILLIS: u64 = 250;

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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "value")]
pub enum ApplicationOutcome<T> {
    Evidence(EvidencePacket<T>),
    Preview(PreviewResult<T>),
    Effect(EffectResult<T>),
}

/// Successful application result with a stable contract, request, and scope.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

/// Stable application problem record shared verbatim by every adapter.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProblemRecord {
    pub revision: u32,
    pub kind: ApplicationProblemKind,
    pub code: String,
    pub message: String,
    pub diagnostic: Option<SafeDiagnostic>,
    pub owning_layer: ProblemOwningLayer,
    pub terminality: ProblemTerminality,
    pub retryable: bool,
    pub retry: RetryDirective,
    pub retry_scope: Option<RetryScope>,
    pub retry_after_millis: Option<u64>,
    pub cancellation_stage: Option<CancellationStage>,
    pub request_id: RequestId,
    pub trace_id: RequestId,
    pub details: Vec<SafeDiagnostic>,
    pub legal_actions: Vec<LegalAction>,
    pub coverage: Option<EvidenceCoverage>,
    #[serde(skip)]
    source: ApplicationProblem,
}

impl ApplicationProblemRecord {
    fn new(request_id: RequestId, source: ApplicationProblem) -> Self {
        let retry = source.retry();
        let kind = source.kind();
        let retry_scope = match retry {
            RetryDirective::Never => None,
            RetryDirective::SameRequest | RetryDirective::AfterDelay => {
                Some(RetryScope::SameRequest)
            }
            RetryDirective::AfterRevalidate => Some(RetryScope::FreshRequest),
            RetryDirective::AfterReconcile => Some(RetryScope::SameOperation),
        };
        let diagnostic = source.diagnostic().cloned();
        let code = diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code.clone())
            .unwrap_or_else(|| source.canonical_code().to_owned());
        Self {
            revision: APPLICATION_PROBLEM_REVISION,
            kind,
            code,
            message: source.safe_message().to_owned(),
            diagnostic,
            owning_layer: ProblemOwningLayer::Application,
            terminality: ProblemTerminality::PreAdmission,
            retryable: retry != RetryDirective::Never,
            retry,
            retry_scope,
            // An `after_delay` directive promises a delay: the serialized
            // contract (enforced by every generated SDK client) rejects the
            // directive with a null delay, so the canonical default fills it
            // here at the single construction authority. Callers that know a
            // better figure override via `with_retry_after_millis`.
            retry_after_millis: (retry == RetryDirective::AfterDelay)
                .then_some(DEFAULT_RETRY_AFTER_MILLIS),
            cancellation_stage: matches!(
                kind,
                ApplicationProblemKind::Cancelled | ApplicationProblemKind::TimedOut
            )
            .then_some(CancellationStage::BeforeAdmission),
            trace_id: request_id.clone(),
            request_id,
            details: Vec::new(),
            legal_actions: source.legal_actions().to_vec(),
            coverage: None,
            source,
        }
    }

    pub fn kind(&self) -> ApplicationProblemKind {
        self.kind
    }

    pub fn is_pre_admission(&self) -> bool {
        self.terminality == ProblemTerminality::PreAdmission
    }

    pub fn source(&self) -> &ApplicationProblem {
        &self.source
    }

    pub fn into_source(self) -> ApplicationProblem {
        self.source
    }
}

/// Stable pre-admission failure envelope. Admitted terminal failures stay in
/// their evidence, preview, or effect receipt instead.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplicationProblemEnvelope {
    pub contract: ResultContractRef,
    pub request_id: RequestId,
    // Boxed: the problem record dominates the envelope's size, and this
    // envelope is the Err variant of every application result.
    pub problem: Box<ApplicationProblemRecord>,
}

impl ApplicationProblemEnvelope {
    pub fn new(
        contract: ResultContractRef,
        request_id: RequestId,
        problem: ApplicationProblem,
    ) -> Self {
        let record = ApplicationProblemRecord::new(request_id.clone(), problem);
        Self {
            contract,
            request_id,
            problem: Box::new(record),
        }
    }

    pub fn with_owning_layer(mut self, owning_layer: ProblemOwningLayer) -> Self {
        self.problem.owning_layer = owning_layer;
        self
    }

    pub fn with_retry_after_millis(
        mut self,
        retry_after_millis: Option<u64>,
    ) -> Result<Self, ApplicationContractError> {
        if retry_after_millis.is_some_and(|delay| delay > MAX_RETRY_AFTER_MILLIS)
            || (retry_after_millis.is_some() && !self.problem.retryable)
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "problem retry delay",
            });
        }
        self.problem.retry_after_millis = retry_after_millis;
        Ok(self)
    }

    pub fn with_coverage(
        mut self,
        coverage: EvidenceCoverage,
    ) -> Result<Self, ApplicationContractError> {
        coverage.validate()?;
        self.problem.coverage = Some(coverage);
        Ok(self)
    }
}

pub type ApplicationResult<T> = Result<ApplicationEnvelope<T>, ApplicationProblemEnvelope>;
