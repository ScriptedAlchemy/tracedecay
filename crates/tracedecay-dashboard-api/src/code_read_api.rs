//! Dashboard HTTP reads for verified shared-code families and revision pairs.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_contracts::retrieval::{SimilarCoverageV1, SimilarMatchClassV1, SimilarResultV1};
use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_domain::{
    CodeGenerationId, ContentDigest, FileOccurrenceId, GitOidV1, RefId, SymbolOccurrenceId,
};

use super::read_model::{
    DashboardCoverageCompletenessV1, DashboardCoverageV1, DashboardDomainStateV1,
    DashboardEnvelopeV1, DashboardFreshnessV1, DashboardVersionV1, scope_from_state,
};
use super::util::{JsonQuery, coerce_limit};
use super::{DashboardHttpRequestControlV1, DashboardState, RequestControl};

const DEFAULT_FAMILY_RESULT_LIMIT: i64 = 100;
const MAX_FAMILY_RESULT_LIMIT: i64 = 1_000;
const MAX_FAMILY_WORK_LIMIT: i64 = MAX_FAMILY_RESULT_LIMIT + 1;

/// Work a shared-family page spends so serving can prove whether another page
/// exists. Serving subtracts one unit before paging
/// (`work_limit.saturating_sub(1)`), so a page of `result_limit` members
/// states a work budget of `result_limit + 1`. Callers own both budgets; this
/// is the page policy, not an alias of a single `limit`.
pub fn shared_family_page_work_limit(result_limit: usize) -> usize {
    result_limit.saturating_add(1)
}

#[derive(Clone, Debug)]
pub struct DashboardCodeReadControlV1 {
    pub deadline: Deadline,
    pub cancellation: CancellationSignal,
}

impl From<&DashboardHttpRequestControlV1> for DashboardCodeReadControlV1 {
    fn from(control: &DashboardHttpRequestControlV1) -> Self {
        Self {
            deadline: control.deadline(),
            cancellation: control.cancellation().clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DashboardSharedFamilyRequestV1 {
    pub symbol_occurrence_id: SymbolOccurrenceId,
    pub match_class: SimilarMatchClassV1,
    /// Members returned on this page. Not a `limit` alias.
    pub result_limit: usize,
    /// Search work this page may spend. Independent of [`Self::result_limit`].
    pub work_limit: usize,
    pub cursor: Option<String>,
    pub control: DashboardCodeReadControlV1,
}

#[derive(Clone, Debug)]
pub struct DashboardRevisionSelectionV1 {
    pub reference: RefId,
    pub revision: GitOidV1,
}

#[derive(Clone, Debug)]
pub struct DashboardRevisionPairRequestV1 {
    pub base: DashboardRevisionSelectionV1,
    pub head: DashboardRevisionSelectionV1,
    pub file_filter: Option<String>,
    pub kind_filter: Option<String>,
    pub control: DashboardCodeReadControlV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DashboardCodeReadErrorV1 {
    NotFound,
    InvalidRequest,
    RevisionChanged,
    AuthorityUnavailable,
    GenerationUnavailable,
    CapacityUnavailable,
    CorruptionResetRequired,
    Cancelled,
    TimedOut,
    Internal,
}

impl DashboardCodeReadErrorV1 {
    const fn reason(self) -> &'static str {
        match self {
            Self::NotFound => "selected_source_not_found",
            Self::InvalidRequest => "invalid_request",
            Self::RevisionChanged => "selected_revision_changed",
            Self::AuthorityUnavailable => "code_read_authority_unavailable",
            Self::GenerationUnavailable => "code_generation_unavailable",
            Self::CapacityUnavailable => "code_read_capacity_unavailable",
            Self::CorruptionResetRequired => "code_index_reset_required",
            Self::Cancelled => "request_cancelled",
            Self::TimedOut => "request_timed_out",
            Self::Internal => "code_read_failed",
        }
    }
}

pub type DashboardSharedFamilyReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SimilarResultV1, DashboardCodeReadErrorV1>> + Send + 'a>>;

pub type DashboardRevisionPairReadFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<RevisionPairUnionLayoutV1, DashboardCodeReadErrorV1>>
            + Send
            + 'a,
    >,
>;

pub trait DashboardCodeReadPortV1: Send + Sync {
    fn shared_family<'a>(
        &'a self,
        request: DashboardSharedFamilyRequestV1,
    ) -> DashboardSharedFamilyReadFuture<'a>;

    fn revision_pair<'a>(
        &'a self,
        request: DashboardRevisionPairRequestV1,
    ) -> DashboardRevisionPairReadFuture<'a>;
}

pub type DashboardCodeReadAuthorityV1 = Arc<dyn DashboardCodeReadPortV1>;

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionPairChangeV1 {
    Unchanged,
    Added,
    Removed,
    Changed,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionPairFileDispositionV1 {
    Present,
    Deleted,
    Renamed,
    Ignored,
    Binary,
    Generated,
    UnsupportedLanguage,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionPairRevisionV1 {
    pub reference: RefId,
    pub revision: GitOidV1,
    pub tree: GitOidV1,
    pub generation: CodeGenerationId,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionPairFileV1 {
    pub file_occurrence_id: FileOccurrenceId,
    pub path: String,
    pub content_digest: ContentDigest,
    pub disposition: RevisionPairFileDispositionV1,
    pub symbol_identities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionPairFileRegionV1 {
    pub file_identity: String,
    pub change: RevisionPairChangeV1,
    pub base: Option<RevisionPairFileV1>,
    pub head: Option<RevisionPairFileV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionPairSymbolV1 {
    pub symbol_occurrence_id: SymbolOccurrenceId,
    pub file_identity: String,
    pub file_occurrence_id: FileOccurrenceId,
    pub qualified_name: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub content_digest: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionPairSymbolRegionV1 {
    pub symbol_identity: String,
    pub change: RevisionPairChangeV1,
    pub base: Option<RevisionPairSymbolV1>,
    pub head: Option<RevisionPairSymbolV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionPairUnionLayoutV1 {
    pub base: RevisionPairRevisionV1,
    pub head: RevisionPairRevisionV1,
    pub files: Vec<RevisionPairFileRegionV1>,
    pub symbols: Vec<RevisionPairSymbolRegionV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedFamilyParamsV1 {
    symbol_occurrence_id: SymbolOccurrenceId,
    match_class: SimilarMatchClassV1,
    result_limit: Option<i64>,
    work_limit: Option<i64>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
pub struct RevisionPairParamsV1 {
    base: String,
    base_revision: String,
    head: String,
    head_revision: String,
    file: Option<String>,
    kind: Option<String>,
}

/// `GET /api/plugins/graph/shared-code/family`
pub async fn shared_family(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    JsonQuery(params): JsonQuery<SharedFamilyParamsV1>,
) -> Json<DashboardEnvelopeV1<Option<SimilarResultV1>>> {
    let Some(authority) = state.code_read_authority.as_ref() else {
        return code_read_unavailable(&state);
    };
    let Some((result_limit, work_limit)) =
        shared_family_budgets(params.result_limit, params.work_limit)
    else {
        return code_read_failed(&state, DashboardCodeReadErrorV1::InvalidRequest);
    };
    let result = authority
        .shared_family(DashboardSharedFamilyRequestV1 {
            symbol_occurrence_id: params.symbol_occurrence_id,
            match_class: params.match_class,
            result_limit,
            work_limit,
            cursor: params.cursor,
            control: DashboardCodeReadControlV1::from(&control),
        })
        .await;
    match result {
        Ok(result) => family_response(&state, result),
        Err(error) => code_read_failed(&state, error),
    }
}

/// `GET /api/plugins/graph/compare/union-layout`
pub async fn revision_pair(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
    JsonQuery(params): JsonQuery<RevisionPairParamsV1>,
) -> Json<DashboardEnvelopeV1<Option<RevisionPairUnionLayoutV1>>> {
    let Some(authority) = state.code_read_authority.as_ref() else {
        return code_read_unavailable(&state);
    };
    let request = revision_pair_request(params, &control);
    let result = match request {
        Ok(request) => authority.revision_pair(request).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(result) => revision_pair_response(&state, result),
        Err(error) => code_read_failed(&state, error),
    }
}

fn revision_pair_request(
    params: RevisionPairParamsV1,
    control: &DashboardHttpRequestControlV1,
) -> Result<DashboardRevisionPairRequestV1, DashboardCodeReadErrorV1> {
    let selection = |branch: String,
                     revision: String|
     -> Result<DashboardRevisionSelectionV1, DashboardCodeReadErrorV1> {
        let reference = RefId::new(format!("refs/heads/{branch}"))
            .map_err(|_| DashboardCodeReadErrorV1::InvalidRequest)?;
        let revision =
            GitOidV1::new(revision).map_err(|_| DashboardCodeReadErrorV1::InvalidRequest)?;
        Ok(DashboardRevisionSelectionV1 {
            reference,
            revision,
        })
    };
    Ok(DashboardRevisionPairRequestV1 {
        base: selection(params.base, params.base_revision)?,
        head: selection(params.head, params.head_revision)?,
        file_filter: params.file,
        kind_filter: params.kind,
        control: DashboardCodeReadControlV1::from(control),
    })
}

fn family_response(
    state: &DashboardState,
    result: SimilarResultV1,
) -> Json<DashboardEnvelopeV1<Option<SimilarResultV1>>> {
    let graph_version = result.source_generation.as_str().to_owned();
    let envelope = match result.coverage {
        SimilarCoverageV1::Complete if result.families.is_empty() => {
            DashboardEnvelopeV1::complete_zero_findings(
                scope_from_state(state),
                complete_family_coverage(false, false),
                Some(result),
            )
        }
        SimilarCoverageV1::Complete => DashboardEnvelopeV1::ready(
            scope_from_state(state),
            complete_family_coverage(true, false),
            Some(result),
        ),
        SimilarCoverageV1::Partial => {
            let mut coverage = DashboardCoverageV1::unknown();
            coverage
                .omission_reasons
                .push("shared_family_result_limit".to_owned());
            DashboardEnvelopeV1::new(
                scope_from_state(state),
                DashboardDomainStateV1::Partial,
                coverage,
                DashboardFreshnessV1::fresh_now(),
                Some(result),
            )
        }
        SimilarCoverageV1::ExcludedTooSmall { .. }
        | SimilarCoverageV1::ExcludedIncompleteTokenization => {
            DashboardEnvelopeV1::complete_zero_findings(
                scope_from_state(state),
                complete_family_coverage(false, true),
                Some(result),
            )
        }
    };
    Json(envelope.with_version(graph_version_value(graph_version)))
}

fn revision_pair_response(
    state: &DashboardState,
    result: RevisionPairUnionLayoutV1,
) -> Json<DashboardEnvelopeV1<Option<RevisionPairUnionLayoutV1>>> {
    let coverage = DashboardCoverageV1::complete(result.files.len() as u64, "file regions");
    let envelope = if result.files.is_empty() {
        DashboardEnvelopeV1::complete_zero_findings(scope_from_state(state), coverage, Some(result))
    } else {
        DashboardEnvelopeV1::ready(scope_from_state(state), coverage, Some(result))
    };
    Json(envelope)
}

fn code_read_unavailable<T>(state: &DashboardState) -> Json<DashboardEnvelopeV1<Option<T>>> {
    Json(DashboardEnvelopeV1::unsupported(
        scope_from_state(state),
        None,
    ))
}

fn code_read_failed<T>(
    state: &DashboardState,
    error: DashboardCodeReadErrorV1,
) -> Json<DashboardEnvelopeV1<Option<T>>> {
    let scope = scope_from_state(state);
    let envelope = match error {
        DashboardCodeReadErrorV1::NotFound => {
            DashboardEnvelopeV1::error(scope, None, error.reason())
        }
        DashboardCodeReadErrorV1::RevisionChanged => {
            let mut coverage = DashboardCoverageV1::unknown();
            coverage.omission_reasons.push(error.reason().to_owned());
            DashboardEnvelopeV1::stale(scope, coverage, None)
        }
        DashboardCodeReadErrorV1::Cancelled => DashboardEnvelopeV1::new(
            scope,
            DashboardDomainStateV1::Cancelled,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            None,
        ),
        DashboardCodeReadErrorV1::TimedOut => DashboardEnvelopeV1::new(
            scope,
            DashboardDomainStateV1::TimedOut,
            DashboardCoverageV1::unknown(),
            DashboardFreshnessV1::unknown(),
            None,
        ),
        DashboardCodeReadErrorV1::AuthorityUnavailable
        | DashboardCodeReadErrorV1::GenerationUnavailable
        | DashboardCodeReadErrorV1::CapacityUnavailable => {
            DashboardEnvelopeV1::unavailable(scope, None, error.reason())
        }
        DashboardCodeReadErrorV1::InvalidRequest
        | DashboardCodeReadErrorV1::CorruptionResetRequired
        | DashboardCodeReadErrorV1::Internal => {
            DashboardEnvelopeV1::error(scope, None, error.reason())
        }
    };
    Json(envelope)
}

fn complete_family_coverage(matched: bool, excluded: bool) -> DashboardCoverageV1 {
    DashboardCoverageV1 {
        completeness: DashboardCoverageCompletenessV1::Complete,
        eligible: Some(1),
        examined: Some(1),
        matched: Some(u64::from(matched)),
        excluded: Some(u64::from(excluded)),
        omitted: Some(0),
        unknown: Some(0),
        denominator: Some(1),
        unit: Some("selected source".to_owned()),
        omission_reasons: Vec::new(),
    }
}

fn shared_family_budgets(
    result_limit: Option<i64>,
    work_limit: Option<i64>,
) -> Option<(usize, usize)> {
    let result_limit = usize::try_from(coerce_limit(
        result_limit,
        DEFAULT_FAMILY_RESULT_LIMIT,
        MAX_FAMILY_RESULT_LIMIT,
    ))
    .ok()?;
    let default_work = i64::try_from(shared_family_page_work_limit(result_limit)).ok()?;
    let work_limit = usize::try_from(coerce_limit(
        work_limit,
        default_work,
        MAX_FAMILY_WORK_LIMIT,
    ))
    .ok()?;
    Some((result_limit, work_limit))
}

fn graph_version_value(generation: String) -> DashboardVersionV1 {
    DashboardVersionV1 {
        entity_version: None,
        graph_version: Some(generation),
    }
}

#[cfg(test)]
mod tests {
    use axum::extract::Query;
    use axum::http::Uri;

    use super::{SharedFamilyParamsV1, shared_family_budgets, shared_family_page_work_limit};

    #[test]
    fn shared_family_page_states_both_budgets() {
        assert_eq!(shared_family_budgets(Some(10), Some(11)), Some((10, 11)));
        assert_eq!(
            shared_family_budgets(Some(10), None),
            Some((10, shared_family_page_work_limit(10)))
        );
        assert_eq!(shared_family_budgets(None, None), Some((100, 101)));
    }

    #[test]
    fn shared_family_query_rejects_retired_limit_name() {
        let current: Uri = "/api/plugins/graph/shared-code/family?symbol_occurrence_id=symbol.shared.source&match_class=conservative_exact&result_limit=10&work_limit=11"
            .parse()
            .expect("current uri");
        let Query(params) = Query::<SharedFamilyParamsV1>::try_from_uri(&current)
            .expect("canonical budgets decode");
        assert_eq!(params.result_limit, Some(10));
        assert_eq!(params.work_limit, Some(11));

        let retired: Uri = "/api/plugins/graph/shared-code/family?symbol_occurrence_id=symbol.shared.source&match_class=conservative_exact&limit=10"
            .parse()
            .expect("retired uri");
        assert!(
            Query::<SharedFamilyParamsV1>::try_from_uri(&retired).is_err(),
            "limit is not a page-size alias"
        );
    }
}
