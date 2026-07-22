use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;
use tracedecay_policy::authorization::SourceAuthorizationEvaluator;
use tracedecay_tool_catalog::SortContractId;

use crate::authorization::{AuthorizationPort, AuthorizationService};
use crate::context::RequestContext;
use crate::handlers::ApplicationOperation;
use crate::result::{
    ApplicationProblem, ApplicationResult, CoverageCompleteness, CoverageDomainState,
    EvidenceCoverage, EvidenceDomain, FreshnessState, Omission, OmissionReason, OpaqueCursor,
    OperationBudgetUsage, PageState, RetrievalEvidence, RetryDirective, SafeDiagnostic,
    TemporalState,
};

use super::service::{evidence_envelope, problem_envelope};
use super::{RetrievalPortOutcome, RetrievalRequestMeta};

pub const MAX_TEST_PRIMITIVE_FILES: usize = 256;
pub const MAX_TEST_PRIMITIVE_DEPTH: usize = 10;
pub const MAX_TEST_FILTER_BYTES: usize = 1_024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestMapPrimitiveRequest {
    pub file: Option<String>,
    pub node_id: Option<String>,
    pub meta: RetrievalRequestMeta,
}

impl TestMapPrimitiveRequest {
    fn validate(&self) -> bool {
        self.file.is_some() ^ self.node_id.is_some()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AffectedFileTestsPrimitiveRequest {
    pub files: Vec<String>,
    pub maximum_depth: usize,
    pub filter: Option<String>,
    pub meta: RetrievalRequestMeta,
}

impl AffectedFileTestsPrimitiveRequest {
    fn validate(&self) -> bool {
        !self.files.is_empty()
            && self.files.len() <= MAX_TEST_PRIMITIVE_FILES
            && self.maximum_depth <= MAX_TEST_PRIMITIVE_DEPTH
            && self
                .filter
                .as_ref()
                .is_none_or(|filter| filter.len() <= MAX_TEST_FILTER_BYTES)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestReferenceV1 {
    pub test_name: String,
    pub test_file: String,
    pub test_line: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestMapCoverageV1 {
    pub source_name: String,
    pub source_id: String,
    pub source_file: String,
    pub source_line: usize,
    pub tests: Vec<TestReferenceV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UncoveredSourceV1 {
    pub id: String,
    pub name: String,
    pub file: String,
    pub line: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestMapPrimitiveResultV1 {
    pub covered_symbols: usize,
    pub uncovered_symbols: usize,
    pub test_files: Vec<String>,
    pub coverage: Vec<TestMapCoverageV1>,
    pub uncovered: Vec<UncoveredSourceV1>,
    pub total: Option<u64>,
    pub next_cursor: Option<OpaqueCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RankedAffectedTestV1 {
    pub path: String,
    pub rank: usize,
    pub distance: usize,
    pub proximity: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AffectedFileTestsPrimitiveResultV1 {
    pub changed_files: Vec<String>,
    pub affected_tests: Vec<String>,
    pub ranked_tests: Vec<RankedAffectedTestV1>,
    pub recommended_tests: Vec<String>,
    pub total: Option<u64>,
    pub next_cursor: Option<OpaqueCursor>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TestPrimitivePortOutcome<T> {
    Completed {
        result: T,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
    Partial {
        result: T,
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
    Failed {
        finished_at: UtcMicros,
        budget: OperationBudgetUsage,
    },
}

pub type TestPrimitivePortFuture<'a, T> =
    Pin<Box<dyn Future<Output = TestPrimitivePortOutcome<T>> + Send + 'a>>;

#[derive(Clone, Copy, Debug)]
pub struct TestPrimitivePortContext<'a> {
    pub request: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
    pub observed_at: UtcMicros,
}

/// Async application port for test-map and changed-file affected-test reads.
///
/// Implementations delegate matching, dependency traversal, and continuation
/// to the established test-map and graph-query authorities.
pub trait TestPrimitivePort {
    fn test_map<'a>(
        &'a self,
        context: TestPrimitivePortContext<'a>,
        request: &'a TestMapPrimitiveRequest,
    ) -> TestPrimitivePortFuture<'a, TestMapPrimitiveResultV1>;

    fn affected_file_tests<'a>(
        &'a self,
        context: TestPrimitivePortContext<'a>,
        request: &'a AffectedFileTestsPrimitiveRequest,
    ) -> TestPrimitivePortFuture<'a, AffectedFileTestsPrimitiveResultV1>;
}

#[derive(Clone, Debug)]
pub struct TestPrimitiveOperations {
    pub test_map: ApplicationOperation,
    pub affected_file_tests: ApplicationOperation,
}

pub struct TestPrimitiveService<P, A, E> {
    port: P,
    authorization: AuthorizationService<A, E>,
    operations: TestPrimitiveOperations,
}

impl<P, A, E> TestPrimitiveService<P, A, E>
where
    P: TestPrimitivePort,
    A: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    pub fn new(
        port: P,
        authorization: AuthorizationService<A, E>,
        operations: TestPrimitiveOperations,
    ) -> Self {
        Self {
            port,
            authorization,
            operations,
        }
    }

    pub async fn test_map(
        &self,
        context: &RequestContext,
        request: TestMapPrimitiveRequest,
        observed_at: UtcMicros,
    ) -> ApplicationResult<TestMapPrimitiveResultV1> {
        if !request.validate() {
            return problem_envelope(
                context,
                &self.operations.test_map,
                invalid_test_primitive_problem(),
            );
        }
        let operation = &self.operations.test_map;
        let admission = match self.authorization.admit(context, operation, observed_at) {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        let outcome = self
            .port
            .test_map(
                TestPrimitivePortContext {
                    request: context,
                    operation,
                    observed_at,
                },
                &request,
            )
            .await;
        evidence_envelope(
            context,
            operation,
            &self.authorization,
            &admission,
            test_evidence(outcome, test_map_page),
            observed_at,
        )
    }

    pub async fn affected_file_tests(
        &self,
        context: &RequestContext,
        request: AffectedFileTestsPrimitiveRequest,
        observed_at: UtcMicros,
    ) -> ApplicationResult<AffectedFileTestsPrimitiveResultV1> {
        if !request.validate() {
            return problem_envelope(
                context,
                &self.operations.affected_file_tests,
                invalid_test_primitive_problem(),
            );
        }
        let operation = &self.operations.affected_file_tests;
        let admission = match self.authorization.admit(context, operation, observed_at) {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        let outcome = self
            .port
            .affected_file_tests(
                TestPrimitivePortContext {
                    request: context,
                    operation,
                    observed_at,
                },
                &request,
            )
            .await;
        evidence_envelope(
            context,
            operation,
            &self.authorization,
            &admission,
            test_evidence(outcome, affected_tests_page),
            observed_at,
        )
    }
}

fn invalid_test_primitive_problem() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic::new(
            "application.test-primitive.invalid-request",
            "The test attribution request is invalid.",
        )
        .expect("static safe diagnostic is valid"),
        retry: RetryDirective::Never,
        legal_actions: Vec::new(),
    }
}

/// Page projection: (returned, total, cursor) extracted from a payload.
type TestPageProjectionFn<T> = fn(&T) -> (u64, Option<u64>, Option<OpaqueCursor>);

fn test_evidence<T>(
    outcome: TestPrimitivePortOutcome<T>,
    page: TestPageProjectionFn<T>,
) -> RetrievalPortOutcome<T> {
    match outcome {
        TestPrimitivePortOutcome::Completed {
            result,
            finished_at,
            budget,
        } => RetrievalPortOutcome::Completed(retrieval_evidence(
            Some(result),
            finished_at,
            budget,
            CoverageCompleteness::Complete,
            None,
            page,
        )),
        TestPrimitivePortOutcome::Partial {
            result,
            finished_at,
            budget,
        } => RetrievalPortOutcome::Partial(retrieval_evidence(
            Some(result),
            finished_at,
            budget,
            CoverageCompleteness::Partial,
            Some(OmissionReason::Budget),
            page,
        )),
        TestPrimitivePortOutcome::Failed {
            finished_at,
            budget,
        } => RetrievalPortOutcome::Failed(retrieval_evidence(
            None,
            finished_at,
            budget,
            CoverageCompleteness::Unknown,
            Some(OmissionReason::Failed),
            page,
        )),
    }
}

fn retrieval_evidence<T>(
    payload: Option<T>,
    finished_at: UtcMicros,
    budget: OperationBudgetUsage,
    completeness: CoverageCompleteness,
    omission: Option<OmissionReason>,
    page: TestPageProjectionFn<T>,
) -> RetrievalEvidence<T> {
    let (returned, total, cursor) = payload.as_ref().map_or((0, None, None), page);
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
            requested_domains: vec![EvidenceDomain::Test],
            visited: total.or(Some(returned)),
            eligible: total,
            returned,
            completeness,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Test,
                completeness,
            }],
        },
        omissions: omission
            .map(|reason| Omission {
                domain: EvidenceDomain::Test,
                count: 0,
                reason,
            })
            .into_iter()
            .collect(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState {
            sort_contract_id: SortContractId::new("sort.application.test-attribution.v1")
                .expect("static sort contract id is valid"),
            sort_revision: 1,
            total,
            returned,
            cursor,
            expires_at: None,
        },
        finished_at,
        budget,
        cancellation: None,
    }
}

fn test_map_page(result: &TestMapPrimitiveResultV1) -> (u64, Option<u64>, Option<OpaqueCursor>) {
    (
        result.coverage.len() as u64 + result.uncovered.len() as u64,
        result.total,
        result.next_cursor.clone(),
    )
}

fn affected_tests_page(
    result: &AffectedFileTestsPrimitiveResultV1,
) -> (u64, Option<u64>, Option<OpaqueCursor>) {
    (
        result.affected_tests.len() as u64,
        result.total,
        result.next_cursor.clone(),
    )
}
