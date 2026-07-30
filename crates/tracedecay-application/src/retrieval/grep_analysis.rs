use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use crate::context::{RequestAdmission, RequestContext};
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::{CoverageCompleteness, OpaqueCursor};

pub const MAX_GREP_RESULTS_V1: u32 = 200;
pub const MAX_GREP_CONTEXT_LINES_V1: u32 = 3;
pub const MAX_ANALYSIS_RESULTS_V1: u32 = 100;
pub const MAX_REDUNDANCY_PAIRS_V1: u32 = 500;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveWindowV1 {
    pub limit: u32,
    pub cursor: Option<OpaqueCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GrepRequestV1 {
    pub pattern: String,
    pub fixed_strings: bool,
    pub case_sensitive: bool,
    pub path_glob: Option<String>,
    pub context_lines: u32,
    pub window: PrimitiveWindowV1,
}

impl GrepRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_nonempty_pattern(&self.pattern, "grep pattern", false)?;
        if self.context_lines > MAX_GREP_CONTEXT_LINES_V1
            || self.window.limit == 0
            || self.window.limit > MAX_GREP_RESULTS_V1
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "grep request bounds",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GrepHitV1 {
    pub file: String,
    pub line: u32,
    pub text: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub symbol: Option<String>,
    pub node_id: Option<String>,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GrepResultV1 {
    pub matches: Vec<GrepHitV1>,
    pub truncated: bool,
    pub files_scanned: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AstGrepRequestV1 {
    pub pattern: String,
    pub lang: Option<String>,
    pub path_glob: Option<String>,
    pub window: PrimitiveWindowV1,
}

impl AstGrepRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_nonempty_pattern(&self.pattern, "AST grep pattern", true)?;
        if self.window.limit == 0 || self.window.limit > MAX_GREP_RESULTS_V1 {
            return Err(ApplicationContractError::InvalidRange {
                field: "AST grep result limit",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AstGrepHitV1 {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub lang: String,
    #[serde(rename = "match")]
    pub matched_text: String,
    pub line_text: String,
    pub symbol: Option<String>,
    pub node_id: Option<String>,
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AstGrepResultV1 {
    pub matches: Vec<AstGrepHitV1>,
    pub truncated: bool,
    pub files_scanned: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComplexityRequestV1 {
    pub node_kind: Option<String>,
    pub path: Option<String>,
    pub window: PrimitiveWindowV1,
}

impl ComplexityRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.window.limit > MAX_ANALYSIS_RESULTS_V1 {
            return Err(ApplicationContractError::InvalidRange {
                field: "complexity result limit",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComplexityItemV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub lines: u32,
    pub cyclomatic_complexity: u32,
    pub branches: u32,
    pub loops: u32,
    pub returns: u32,
    pub max_nesting: u32,
    pub unsafe_blocks: u32,
    pub unchecked_calls: u32,
    pub assertions: u32,
    pub fan_out: u64,
    pub fan_in: u64,
    pub score: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComplexityResultV1 {
    pub formula: String,
    pub note: String,
    pub result_count: u64,
    pub ranking: Vec<ComplexityItemV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RedundancyRequestV1 {
    pub path: Option<String>,
    pub min_lines: u32,
    pub max_pairs: u32,
    pub similarity_threshold: f64,
    pub include_naming_only: bool,
    pub include_generated_paths: bool,
    pub cursor: Option<OpaqueCursor>,
}

impl RedundancyRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.max_pairs > MAX_REDUNDANCY_PAIRS_V1
            || !self.similarity_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.similarity_threshold)
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "redundancy request bounds",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedundancyNodeV1 {
    pub file: String,
    pub line: u32,
    pub name: String,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RedundancySignalsV1 {
    pub ast_match: bool,
    pub cfg_match: bool,
    pub call_seq_match: bool,
    pub shingle_jaccard: f64,
    pub body_vector_cosine: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_vector_cosine: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_distance_micros: Option<i64>,
    pub generic_helper_downranked: bool,
    pub body_tokens: [u64; 2],
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RedundancyPairV1 {
    pub similarity: f64,
    pub ranking_score: f64,
    pub severity: String,
    pub overlap_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<String>,
    pub a: RedundancyNodeV1,
    pub b: RedundancyNodeV1,
    pub signals: RedundancySignalsV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticRedundancyGenerationV1 {
    pub vector_generation: String,
    pub source_generation: String,
    pub projection_key: String,
    pub scope_digest: String,
    pub accepted_profile_digest: String,
    pub calibration_profile_id: String,
    pub calibration_digest: String,
    pub redundancy_profile_digest: String,
    pub maximum_distance_micros: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedundancyGroupV1 {
    pub size: u64,
    pub nodes: Vec<RedundancyNodeV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RedundancyThresholdsV1 {
    pub min_lines: u32,
    pub similarity_threshold: f64,
    pub include_naming_only: bool,
    pub include_generated_paths: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RedundancyResultV1 {
    pub candidates: u64,
    pub scanned: u64,
    pub skipped_for_size: u64,
    pub pair_count: u64,
    pub pairs: Vec<RedundancyPairV1>,
    pub groups: Vec<RedundancyGroupV1>,
    pub groups_scope: String,
    pub ranked_by: String,
    pub scope: String,
    pub thresholds: RedundancyThresholdsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_generation: Option<SemanticRedundancyGenerationV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DependencyDepthRequestV1 {
    pub path: Option<String>,
    pub window: PrimitiveWindowV1,
}

impl DependencyDepthRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.window.limit > MAX_ANALYSIS_RESULTS_V1 {
            return Err(ApplicationContractError::InvalidRange {
                field: "dependency-depth result limit",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DependencyDepthChainV1 {
    pub file: String,
    pub depth: u64,
    pub chain: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DependencyDepthResultV1 {
    pub max_depth: u64,
    pub ideal_depth: u64,
    pub depth_score: f64,
    pub chains: Vec<DependencyDepthChainV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveCoverageV1 {
    pub completeness: CoverageCompleteness,
    pub visited: Option<u64>,
    pub eligible: Option<u64>,
    pub returned: u64,
    pub unsupported_languages: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PrimitivePageV1<T> {
    pub payload: T,
    pub coverage: PrimitiveCoverageV1,
    pub continuation: Option<OpaqueCursor>,
    pub finished_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum GrepAnalysisProblemV1 {
    Denied,
    Cancelled,
    TimedOut,
    InvalidRequest(String),
    AuthorityFailed(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum PrimitiveOutcomeV1<T> {
    Completed(PrimitivePageV1<T>),
    Partial(PrimitivePageV1<T>),
    Cancelled,
    TimedOut,
    Failed(GrepAnalysisProblemV1),
}

pub struct PrimitivePortContextV1<'a> {
    pub request: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
    pub scope_prefix: Option<&'a str>,
    pub observed_at: UtcMicros,
}

pub type PrimitiveFutureV1<'a, T> =
    Pin<Box<dyn Future<Output = PrimitiveOutcomeV1<T>> + Send + 'a>>;

pub trait LexicalGrepAuthorityV1 {
    fn grep<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a GrepRequestV1,
    ) -> PrimitiveFutureV1<'a, GrepResultV1>;
}

pub trait AstGrepAuthorityV1 {
    fn ast_grep<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a AstGrepRequestV1,
    ) -> PrimitiveFutureV1<'a, AstGrepResultV1>;
}

pub trait ComplexityAuthorityV1 {
    fn complexity<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a ComplexityRequestV1,
    ) -> PrimitiveFutureV1<'a, ComplexityResultV1>;
}

pub trait RedundancyAuthorityV1 {
    fn redundancy<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a RedundancyRequestV1,
    ) -> PrimitiveFutureV1<'a, RedundancyResultV1>;
}

pub trait DependencyDepthAuthorityV1 {
    fn dependency_depth<'a>(
        &'a self,
        context: &'a PrimitivePortContextV1<'a>,
        request: &'a DependencyDepthRequestV1,
    ) -> PrimitiveFutureV1<'a, DependencyDepthResultV1>;
}

pub struct GrepAnalysisOperationsV1<L, A, C, R, D> {
    lexical: L,
    ast: A,
    complexity: C,
    redundancy: R,
    dependency_depth: D,
}

impl<L, A, C, R, D> GrepAnalysisOperationsV1<L, A, C, R, D> {
    pub fn new(lexical: L, ast: A, complexity: C, redundancy: R, dependency_depth: D) -> Self {
        Self {
            lexical,
            ast,
            complexity,
            redundancy,
            dependency_depth,
        }
    }
}

impl<L, A, C, R, D> GrepAnalysisOperationsV1<L, A, C, R, D>
where
    L: LexicalGrepAuthorityV1,
    A: AstGrepAuthorityV1,
    C: ComplexityAuthorityV1,
    R: RedundancyAuthorityV1,
    D: DependencyDepthAuthorityV1,
{
    pub async fn grep(
        &self,
        context: &PrimitivePortContextV1<'_>,
        request: &GrepRequestV1,
    ) -> PrimitiveOutcomeV1<GrepResultV1> {
        if let Err(problem) = admit(context, || request.validate()) {
            return problem;
        }
        self.lexical.grep(context, request).await
    }

    pub async fn ast_grep(
        &self,
        context: &PrimitivePortContextV1<'_>,
        request: &AstGrepRequestV1,
    ) -> PrimitiveOutcomeV1<AstGrepResultV1> {
        if let Err(problem) = admit(context, || request.validate()) {
            return problem;
        }
        self.ast.ast_grep(context, request).await
    }

    pub async fn complexity(
        &self,
        context: &PrimitivePortContextV1<'_>,
        request: &ComplexityRequestV1,
    ) -> PrimitiveOutcomeV1<ComplexityResultV1> {
        if let Err(problem) = admit(context, || request.validate()) {
            return problem;
        }
        self.complexity.complexity(context, request).await
    }

    pub async fn redundancy(
        &self,
        context: &PrimitivePortContextV1<'_>,
        request: &RedundancyRequestV1,
    ) -> PrimitiveOutcomeV1<RedundancyResultV1> {
        if let Err(problem) = admit(context, || request.validate()) {
            return problem;
        }
        self.redundancy.redundancy(context, request).await
    }

    pub async fn dependency_depth(
        &self,
        context: &PrimitivePortContextV1<'_>,
        request: &DependencyDepthRequestV1,
    ) -> PrimitiveOutcomeV1<DependencyDepthResultV1> {
        if let Err(problem) = admit(context, || request.validate()) {
            return problem;
        }
        self.dependency_depth
            .dependency_depth(context, request)
            .await
    }
}

fn admit<T>(
    context: &PrimitivePortContextV1<'_>,
    validate: impl FnOnce() -> Result<(), ApplicationContractError>,
) -> Result<(), PrimitiveOutcomeV1<T>> {
    if context.request.validate().is_err()
        || !context.request.allows(
            context.operation.capability_id(),
            context.operation.use_case_id(),
        )
    {
        return Err(PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::Denied));
    }
    match context.request.admission_at(context.observed_at) {
        RequestAdmission::Cancelled => return Err(PrimitiveOutcomeV1::Cancelled),
        RequestAdmission::TimedOut => return Err(PrimitiveOutcomeV1::TimedOut),
        RequestAdmission::Admitted => {}
    }
    validate().map_err(|error| {
        PrimitiveOutcomeV1::Failed(GrepAnalysisProblemV1::InvalidRequest(error.to_string()))
    })
}

fn validate_nonempty_pattern(
    value: &str,
    field: &'static str,
    trim: bool,
) -> Result<(), ApplicationContractError> {
    if value.is_empty() || (trim && value.trim().is_empty()) {
        return Err(ApplicationContractError::InvalidIdentifier { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grep_request_json_round_trips_and_rejects_invalid_bounds() {
        let ok: GrepRequestV1 = serde_json::from_value(serde_json::json!({
            "pattern": "HostCapabilityStateV1",
            "fixed_strings": true,
            "case_sensitive": true,
            "path_glob": "crates/**/*.rs",
            "context_lines": 1,
            "window": { "limit": 25, "cursor": null }
        }))
        .unwrap();
        assert_eq!(ok.pattern, "HostCapabilityStateV1");
        assert_eq!(ok.window.limit, 25);
        ok.validate().unwrap();

        let zero_limit: GrepRequestV1 = serde_json::from_value(serde_json::json!({
            "pattern": "x",
            "fixed_strings": false,
            "case_sensitive": false,
            "path_glob": null,
            "context_lines": 0,
            "window": { "limit": 0, "cursor": null }
        }))
        .unwrap();
        assert!(matches!(
            zero_limit.validate(),
            Err(ApplicationContractError::InvalidRange { .. })
        ));

        let over_context: GrepRequestV1 = serde_json::from_value(serde_json::json!({
            "pattern": "x",
            "fixed_strings": false,
            "case_sensitive": false,
            "path_glob": null,
            "context_lines": MAX_GREP_CONTEXT_LINES_V1 + 1,
            "window": { "limit": 10, "cursor": null }
        }))
        .unwrap();
        assert!(matches!(
            over_context.validate(),
            Err(ApplicationContractError::InvalidRange { .. })
        ));
    }

    #[test]
    fn analysis_request_json_validation_covers_complexity_redundancy_and_depth() {
        let complexity: ComplexityRequestV1 = serde_json::from_value(serde_json::json!({
            "node_kind": "function",
            "path": "src/lib.rs",
            "window": { "limit": 10, "cursor": null }
        }))
        .unwrap();
        complexity.validate().unwrap();
        let over_complexity = ComplexityRequestV1 {
            window: PrimitiveWindowV1 {
                limit: MAX_ANALYSIS_RESULTS_V1 + 1,
                cursor: None,
            },
            ..complexity
        };
        assert!(matches!(
            over_complexity.validate(),
            Err(ApplicationContractError::InvalidRange { .. })
        ));

        let redundancy: RedundancyRequestV1 = serde_json::from_value(serde_json::json!({
            "path": null,
            "min_lines": 8,
            "max_pairs": 20,
            "similarity_threshold": 0.6,
            "include_naming_only": false,
            "include_generated_paths": false,
            "cursor": null
        }))
        .unwrap();
        redundancy.validate().unwrap();
        let bad_threshold = RedundancyRequestV1 {
            similarity_threshold: 1.5,
            ..redundancy
        };
        assert!(matches!(
            bad_threshold.validate(),
            Err(ApplicationContractError::InvalidRange { .. })
        ));

        let depth: DependencyDepthRequestV1 = serde_json::from_value(serde_json::json!({
            "path": "crates/tracedecay-application",
            "window": { "limit": 5, "cursor": null }
        }))
        .unwrap();
        depth.validate().unwrap();
        assert_eq!(depth.path.as_deref(), Some("crates/tracedecay-application"));
    }

    #[test]
    fn ast_grep_request_json_requires_nonempty_pattern() {
        let ok: AstGrepRequestV1 = serde_json::from_value(serde_json::json!({
            "pattern": "fn $NAME($$$ARGS) { $$$BODY }",
            "lang": "rust",
            "path_glob": null,
            "window": { "limit": 50, "cursor": null }
        }))
        .unwrap();
        ok.validate().unwrap();

        let blank: AstGrepRequestV1 = serde_json::from_value(serde_json::json!({
            "pattern": "   ",
            "lang": null,
            "path_glob": null,
            "window": { "limit": 1, "cursor": null }
        }))
        .unwrap();
        assert!(matches!(
            blank.validate(),
            Err(ApplicationContractError::InvalidIdentifier { .. })
        ));
    }
}
