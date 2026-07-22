use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_domain::UtcMicros;
use tracedecay_policy::authorization::SourceAuthorizationEvaluator;
use tracedecay_tool_catalog::SortContractId;

use crate::authorization::{AuthorizationPort, AuthorizationService};
use crate::context::RequestContext;
use crate::handlers::ApplicationOperation;
use crate::result::{
    ApplicationProblem, ApplicationResult, CoverageCompleteness, CoverageDomainState,
    EvidenceCoverage, EvidenceDomain, FreshnessState, Omission, OmissionReason,
    OperationBudgetUsage, PageState, RetrievalEvidence, RetryDirective, SafeDiagnostic,
    TemporalState,
};

use super::service::{evidence_envelope, problem_envelope};
use super::{RetrievalPortOutcome, RetrievalRequestMeta};

pub const MAX_SOURCE_READ_PATH_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceReadModeV1 {
    Full,
    Lines,
    Map,
    Signatures,
}

impl SourceReadModeV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lines => "lines",
            Self::Map => "map",
            Self::Signatures => "signatures",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceReadPrimitiveRequest {
    pub file: String,
    pub mode: SourceReadModeV1,
    pub lines: Option<String>,
    pub include_symbols: bool,
    pub meta: RetrievalRequestMeta,
}

impl SourceReadPrimitiveRequest {
    fn validate(&self) -> bool {
        let file_is_valid = !self.file.is_empty()
            && self.file.len() <= MAX_SOURCE_READ_PATH_BYTES
            && !self.file.contains('\0');
        let range_shape_is_valid = match self.mode {
            SourceReadModeV1::Lines => self.lines.is_some(),
            SourceReadModeV1::Full | SourceReadModeV1::Map | SourceReadModeV1::Signatures => {
                self.lines.is_none()
            }
        };
        file_is_valid && range_shape_is_valid && self.meta.page.cursor.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SourceReadResultV1 {
    pub file: String,
    pub mode: SourceReadModeV1,
    pub mtime_ns: u64,
    pub digest: String,
    pub token_count: usize,
    pub unchanged: bool,
    pub body: Option<String>,
    pub context: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SourceReadPortOutcome {
    Completed {
        result: SourceReadResultV1,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
    Partial {
        result: SourceReadResultV1,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
    Failed {
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
}

pub type SourceReadPortFuture<'a> =
    Pin<Box<dyn Future<Output = SourceReadPortOutcome> + Send + 'a>>;

#[derive(Clone, Copy, Debug)]
pub struct SourceReadPortContext<'a> {
    pub request: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
    pub observed_at: UtcMicros,
}

/// Async application port for compatibility-preserving source reads.
///
/// Implementations must delegate range parsing, rendering, and cache handling
/// to the existing source-read kernel.
pub trait SourceReadPrimitivePort {
    fn source_read<'a>(
        &'a self,
        context: SourceReadPortContext<'a>,
        request: &'a SourceReadPrimitiveRequest,
    ) -> SourceReadPortFuture<'a>;
}

pub struct SourceReadPrimitiveService<P, A, E> {
    port: P,
    authorization: AuthorizationService<A, E>,
    operation: ApplicationOperation,
}

impl<P, A, E> SourceReadPrimitiveService<P, A, E>
where
    P: SourceReadPrimitivePort,
    A: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    pub fn new(
        port: P,
        authorization: AuthorizationService<A, E>,
        operation: ApplicationOperation,
    ) -> Self {
        Self {
            port,
            authorization,
            operation,
        }
    }

    pub async fn execute(
        &self,
        context: &RequestContext,
        request: SourceReadPrimitiveRequest,
        observed_at: UtcMicros,
    ) -> ApplicationResult<SourceReadResultV1> {
        if !request.validate() {
            return problem_envelope(context, &self.operation, invalid_source_read_problem());
        }
        let admission = match self
            .authorization
            .admit(context, &self.operation, observed_at)
        {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, &self.operation, problem),
        };
        let outcome = self
            .port
            .source_read(
                SourceReadPortContext {
                    request: context,
                    operation: &self.operation,
                    observed_at,
                },
                &request,
            )
            .await;
        evidence_envelope(
            context,
            &self.operation,
            &self.authorization,
            &admission,
            source_read_evidence(outcome),
            observed_at,
        )
    }
}

fn invalid_source_read_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic::new(
            "application.source-read.invalid-request",
            "The source read request is invalid.",
        )
        .expect("static safe diagnostic is valid"),
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

fn source_read_evidence(
    outcome: SourceReadPortOutcome,
) -> RetrievalPortOutcome<SourceReadResultV1> {
    match outcome {
        SourceReadPortOutcome::Completed {
            result,
            finished_at,
            budget,
        } => RetrievalPortOutcome::Completed(retrieval_evidence(
            Some(result),
            finished_at,
            budget,
            CoverageCompleteness::Complete,
            None,
        )),
        SourceReadPortOutcome::Partial {
            result,
            finished_at,
            budget,
        } => RetrievalPortOutcome::Partial(retrieval_evidence(
            Some(result),
            finished_at,
            budget,
            CoverageCompleteness::Partial,
            Some(OmissionReason::Budget),
        )),
        SourceReadPortOutcome::Failed {
            finished_at,
            budget,
        } => RetrievalPortOutcome::Failed(retrieval_evidence(
            None,
            finished_at,
            budget,
            CoverageCompleteness::Unknown,
            Some(OmissionReason::Failed),
        )),
    }
}

fn retrieval_evidence(
    payload: Option<SourceReadResultV1>,
    finished_at: UtcMicros,
    budget: OperationBudgetUsage,
    completeness: CoverageCompleteness,
    omission: Option<OmissionReason>,
) -> RetrievalEvidence<SourceReadResultV1> {
    let returned = u64::from(payload.is_some());
    RetrievalEvidence {
        payload,
        temporal: TemporalState {
            requested_mode: tracedecay_domain::TemporalModeV1::Current,
            requested_at: finished_at,
            resolved_at: finished_at,
            source_generation: None,
            watermark_digest: None,
            freshness: if completeness == CoverageCompleteness::Complete {
                FreshnessState::Current
            } else {
                FreshnessState::Unknown
            },
        },
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Source],
            visited: Some(returned),
            eligible: Some(1),
            returned,
            completeness,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Source,
                completeness,
            }],
        },
        omissions: omission
            .map(|reason| Omission {
                domain: EvidenceDomain::Source,
                count: 0,
                reason,
            })
            .into_iter()
            .collect(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState {
            sort_contract_id: SortContractId::new("sort.application.source-read.v1")
                .expect("static sort contract id is valid"),
            sort_revision: 1,
            total: Some(1),
            returned,
            cursor: None,
            expires_at: None,
        },
        finished_at,
        budget,
        cancellation: None,
    }
}
