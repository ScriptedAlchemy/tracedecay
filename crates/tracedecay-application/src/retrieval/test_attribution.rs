use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use crate::context::RequestContext;
use crate::handlers::ApplicationOperation;
use crate::result::{OpaqueCursor, OperationBudgetUsage};

use super::RetrievalRequestMeta;

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AffectedFileTestsPrimitiveRequest {
    pub files: Vec<String>,
    pub maximum_depth: usize,
    pub filter: Option<String>,
    pub meta: RetrievalRequestMeta,
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
