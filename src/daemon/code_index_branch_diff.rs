//! Exact sealed-generation branch comparison for the daemon-owned MCP route.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use tracedecay_domain::{
    FreshnessVectorDigest, RetrievalRequest, RetrievalScope, RetrievalSnapshot, SingleRootScopeV1,
    TemporalModeV1, VectorWatermark, canonical_sha256,
};
use tracedecay_query::code_search;
use tracedecay_query::retrieval::{PreparedQueryBindingsV1, PreparedQueryErrorV1, PreparedQueryV1};

use super::{code_index_scheduler, project_open_owners, query_mcp_admission};

const MAX_CONCURRENT_BRANCH_DIFFS: usize = 1;
const MAX_BRANCH_DIFF_FILES_PER_GENERATION: usize = 1_024;
const MAX_BRANCH_DIFF_CHUNKS_PER_GENERATION: usize = 4_096;
const MAX_BRANCH_DIFF_SYMBOLS_PER_GENERATION: usize = 1_024;

type SymbolKey = (String, String, String, String);

#[derive(Serialize)]
#[serde(tag = "change", content = "symbol", rename_all = "snake_case")]
enum BranchDiffItemV1 {
    Added(code_search::CodeIndexBranchSymbolV1),
    Removed(code_search::CodeIndexBranchSymbolV1),
    Changed(code_search::CodeIndexBranchChangedSymbolV1),
}

#[derive(Clone, Copy)]
struct GenerationCountsV1 {
    files: usize,
    chunks: usize,
    symbols: usize,
}

fn generation_counts(
    generation: &crate::code_index::production::CodeIndexPublishedGenerationV1,
) -> GenerationCountsV1 {
    GenerationCountsV1 {
        files: generation.snapshot().files.len(),
        chunks: generation.chunks().chunks().len(),
        symbols: generation.symbols().symbols.len(),
    }
}

fn generation_bound_reason(
    counts: GenerationCountsV1,
) -> Option<code_search::CodeIndexBranchDiffPartialReasonV1> {
    if counts.files > MAX_BRANCH_DIFF_FILES_PER_GENERATION {
        Some(code_search::CodeIndexBranchDiffPartialReasonV1::GenerationFileLimit)
    } else if counts.chunks > MAX_BRANCH_DIFF_CHUNKS_PER_GENERATION {
        Some(code_search::CodeIndexBranchDiffPartialReasonV1::GenerationChunkLimit)
    } else if counts.symbols > MAX_BRANCH_DIFF_SYMBOLS_PER_GENERATION {
        Some(code_search::CodeIndexBranchDiffPartialReasonV1::GenerationSymbolLimit)
    } else {
        None
    }
}

fn unavailable(
    base_generation: Option<String>,
    head_generation: Option<String>,
    reason: code_search::CodeIndexSearchUnavailableReasonV1,
) -> code_search::CodeIndexBranchDiffOutcomeV1 {
    code_search::CodeIndexBranchDiffOutcomeV1::Unavailable(
        code_search::CodeIndexBranchDiffUnavailableV1 {
            base_generation,
            head_generation,
            reason,
        },
    )
}

fn symbol_key(symbol: &code_search::CodeIndexBranchSymbolV1) -> SymbolKey {
    (
        symbol.qualified_name.clone(),
        symbol.kind.clone(),
        symbol.file.clone(),
        symbol.symbol_identity.clone(),
    )
}

pub(super) fn diff_symbols(
    base_generation: &str,
    base: Vec<code_search::CodeIndexBranchSymbolV1>,
    head_generation: &str,
    head: Vec<code_search::CodeIndexBranchSymbolV1>,
) -> code_search::CodeIndexBranchDiffCompletedV1 {
    let mut base = base
        .into_iter()
        .map(|symbol| (symbol_key(&symbol), symbol))
        .collect::<BTreeMap<_, _>>();
    let mut head = head
        .into_iter()
        .map(|symbol| (symbol_key(&symbol), symbol))
        .collect::<BTreeMap<_, _>>();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for key in base
        .keys()
        .chain(head.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
    {
        match (base.remove(&key), head.remove(&key)) {
            (None, Some(symbol)) => added.push(symbol),
            (Some(symbol), None) => removed.push(symbol),
            (Some(base), Some(head)) if base.content_digest != head.content_digest => {
                changed.push(code_search::CodeIndexBranchChangedSymbolV1 { base, head });
            }
            _ => {}
        }
    }
    let total_changes = added.len() + removed.len() + changed.len();
    code_search::CodeIndexBranchDiffCompletedV1 {
        base_generation: base_generation.to_owned(),
        head_generation: head_generation.to_owned(),
        total_changes,
        next_cursor: None,
        added,
        removed,
        changed,
    }
}

pub(super) fn generation_symbols(
    generation: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
    control: &code_index_scheduler::branch_generations::BranchGenerationReadControlV1,
) -> Result<
    Vec<code_search::CodeIndexBranchSymbolV1>,
    code_search::CodeIndexSearchUnavailableReasonV1,
> {
    let files = generation
        .snapshot()
        .files
        .iter()
        .map(|file| (file.file_occurrence_id.clone(), file.logical_path.as_str()))
        .collect::<BTreeMap<_, _>>();
    let mut symbol_files = BTreeMap::new();
    for chunk in generation.chunks().chunks() {
        if let Some(reason) = control.termination() {
            return Err(reason);
        }
        let Some(symbol) = chunk.anchor.symbol_occurrence_id.as_ref() else {
            continue;
        };
        let file = files
            .get(&chunk.anchor.file_occurrence_id)
            .ok_or(code_search::CodeIndexSearchUnavailableReasonV1::Internal)?;
        if symbol_files
            .insert(symbol.clone(), *file)
            .is_some_and(|prior| prior != *file)
        {
            return Err(code_search::CodeIndexSearchUnavailableReasonV1::Internal);
        }
    }
    let mut symbols = Vec::new();
    for symbol in &generation.symbols().symbols {
        if let Some(reason) = control.termination() {
            return Err(reason);
        }
        let file = symbol_files
            .get(&symbol.occurrence)
            .ok_or(code_search::CodeIndexSearchUnavailableReasonV1::Internal)?;
        if file_filter.is_some_and(|filter| !file.starts_with(filter) && *file != filter)
            || kind_filter.is_some_and(|filter| symbol.kind != filter)
        {
            continue;
        }
        symbols.push(code_search::CodeIndexBranchSymbolV1 {
            qualified_name: symbol.qualified_name.clone(),
            name: symbol
                .qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(symbol.qualified_name.as_str())
                .to_owned(),
            kind: symbol.kind.clone(),
            file: (*file).to_owned(),
            symbol_identity: symbol.identity.as_str().to_owned(),
            content_digest: symbol.content_digest.as_str().to_owned(),
        });
    }
    symbols.sort_by_key(symbol_key);
    Ok(symbols)
}

fn partial(
    base_generation: &str,
    head_generation: &str,
    base_counts: GenerationCountsV1,
    head_counts: GenerationCountsV1,
    reason: code_search::CodeIndexBranchDiffPartialReasonV1,
) -> code_search::CodeIndexBranchDiffOutcomeV1 {
    code_search::CodeIndexBranchDiffOutcomeV1::Partial(code_search::CodeIndexBranchDiffPartialV1 {
        base_generation: base_generation.to_owned(),
        head_generation: head_generation.to_owned(),
        reason,
        base_file_count: base_counts.files,
        head_file_count: head_counts.files,
        base_chunk_count: base_counts.chunks,
        head_chunk_count: head_counts.chunks,
        base_symbol_count: base_counts.symbols,
        head_symbol_count: head_counts.symbols,
        total_changes: None,
        next_cursor: None,
        added: Vec::new(),
        removed: Vec::new(),
        changed: Vec::new(),
    })
}

pub(super) fn bounded_diff(
    base: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    head: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
    control: &code_index_scheduler::branch_generations::BranchGenerationReadControlV1,
) -> Result<
    code_search::CodeIndexBranchDiffOutcomeV1,
    code_search::CodeIndexSearchUnavailableReasonV1,
> {
    if let Some(reason) = control.termination() {
        return Err(reason);
    }
    let base_id = base.manifest().generation_id.as_str();
    let head_id = head.manifest().generation_id.as_str();
    let base_counts = generation_counts(base);
    let head_counts = generation_counts(head);
    if let Some(reason) =
        generation_bound_reason(base_counts).or_else(|| generation_bound_reason(head_counts))
    {
        return Ok(partial(base_id, head_id, base_counts, head_counts, reason));
    }
    let base_symbols = generation_symbols(base, file_filter, kind_filter, control)?;
    let head_symbols = generation_symbols(head, file_filter, kind_filter, control)?;
    if let Some(reason) = control.termination() {
        return Err(reason);
    }
    Ok(code_search::CodeIndexBranchDiffOutcomeV1::Complete(
        diff_symbols(base_id, base_symbols, head_id, head_symbols),
    ))
}

async fn paginate_diff(
    schedulers: &code_index_scheduler::CodeIndexSchedulerRegistryV1,
    scope: &tracedecay_application::ResolvedScope,
    authority: &code_search::CodeIndexSearchAuthorityV1,
    request: &code_search::CodeIndexBranchDiffRequestV1,
    base: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    head: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    completed: code_search::CodeIndexBranchDiffCompletedV1,
) -> Result<
    code_search::CodeIndexBranchDiffCompletedV1,
    code_search::CodeIndexSearchUnavailableReasonV1,
> {
    let page_size = request
        .limit
        .min(code_search::CODE_INDEX_BRANCH_DIFF_MAX_RESULTS_V1);
    let page_size = u32::try_from(page_size)
        .ok()
        .filter(|size| *size > 0)
        .ok_or(code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest)?;
    let query_authority = schedulers
        .query_authority_for_scope(scope)
        .await
        .ok_or(code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable)?;
    let binding_digest = canonical_sha256(&(
        "tracedecay.branch-diff.query-binding.v1",
        request.base_reference.as_str(),
        request.base_revision.as_str(),
        request.base_tree.as_str(),
        request.head_reference.as_str(),
        request.head_revision.as_str(),
        request.head_tree.as_str(),
        request.file_filter.as_deref(),
        request.kind_filter.as_deref(),
        base.manifest().generation_id.as_str(),
        head.manifest().generation_id.as_str(),
    ))
    .map_err(|_| code_search::CodeIndexSearchUnavailableReasonV1::Internal)?;
    let retrieval_request = RetrievalRequest {
        principal: authority.principal.clone(),
        scope: RetrievalScope {
            privacy_domain: head.manifest().privacy_domain.clone(),
            root: SingleRootScopeV1 {
                repository: head.snapshot().repository.clone(),
                worktree: head.snapshot().worktree.clone(),
                reference: head.snapshot().reference.clone(),
            },
        },
        temporal_mode: TemporalModeV1::Current,
        snapshot: RetrievalSnapshot {
            watermarks: VectorWatermark::default(),
            freshness_digest: FreshnessVectorDigest::new(binding_digest.as_str())
                .map_err(|_| code_search::CodeIndexSearchUnavailableReasonV1::Internal)?,
            authorization_revision: authority.authorization_revision.clone(),
            captured_at: head.manifest().seal.sealed_at,
        },
        profile_id: query_authority.profile().profile_id.clone(),
        budget: query_authority.profile().retrieval_budget,
    };
    let prepared = PreparedQueryV1::prepare(
        query_authority,
        retrieval_request,
        request.cursor.as_deref(),
    )
    .map_err(map_prepared_query_error)?;
    let bindings = PreparedQueryBindingsV1::new(
        "code_index_branch_diff",
        scope.scope_digest.clone(),
        head.manifest().generation_id.clone(),
        binding_digest,
    )
    .map_err(map_prepared_query_error)?;
    let mut items = Vec::with_capacity(completed.total_changes);
    items.extend(completed.added.into_iter().map(BranchDiffItemV1::Added));
    items.extend(completed.removed.into_iter().map(BranchDiffItemV1::Removed));
    items.extend(completed.changed.into_iter().map(BranchDiffItemV1::Changed));
    let page = prepared
        .paginate(
            &bindings,
            items,
            page_size,
            tracedecay_application::now_micros(),
        )
        .map_err(map_prepared_query_error)?;
    let total_changes = usize::try_from(page.total)
        .map_err(|_| code_search::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable)?;
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    for item in page.items {
        match item {
            BranchDiffItemV1::Added(symbol) => added.push(symbol),
            BranchDiffItemV1::Removed(symbol) => removed.push(symbol),
            BranchDiffItemV1::Changed(symbol) => changed.push(symbol),
        }
    }
    Ok(code_search::CodeIndexBranchDiffCompletedV1 {
        base_generation: completed.base_generation,
        head_generation: completed.head_generation,
        total_changes,
        next_cursor: page.next_cursor,
        added,
        removed,
        changed,
    })
}

fn map_prepared_query_error(
    error: PreparedQueryErrorV1,
) -> code_search::CodeIndexSearchUnavailableReasonV1 {
    match error {
        PreparedQueryErrorV1::Invalid => {
            code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest
        }
        PreparedQueryErrorV1::Stale => {
            code_search::CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
        }
        PreparedQueryErrorV1::Unavailable => {
            code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable
        }
    }
}

pub(super) fn code_index_branch_diff_executor(
    schedulers: code_index_scheduler::CodeIndexSchedulerRegistryV1,
    project_id: tracedecay_domain::ProjectId,
    admission_provider: query_mcp_admission::QueryMcpReadAdmissionProviderV1,
) -> code_search::CodeIndexBranchDiffExecutor {
    let execution_admission = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_BRANCH_DIFFS));
    Arc::new(move |request| {
        let schedulers = schedulers.clone();
        let project_id = project_id.clone();
        let admission_provider = admission_provider.clone();
        let execution_admission = Arc::clone(&execution_admission);
        Box::pin(async move {
            let scope = match project_open_owners::resolved_scope_for_project(
                &request.project_root,
                &project_id,
            ) {
                Ok(scope) => scope,
                Err(_) => {
                    return unavailable(
                        None,
                        None,
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    );
                }
            };
            let admission = match admission_provider.admit_current(&scope) {
                Ok(admission) => admission,
                Err(_) => {
                    return unavailable(
                        None,
                        None,
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    );
                }
            };
            let authority = match admission.authorize(&scope, request.authority.as_ref()) {
                Ok(authority) => authority,
                Err(_) => {
                    return unavailable(
                        None,
                        None,
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    );
                }
            };
            let control = code_index_scheduler::branch_generations::BranchGenerationReadControlV1 {
                deadline: request.deadline.clone(),
                cancellation: request.cancellation.clone(),
            };
            if let Some(reason) = control.termination() {
                return unavailable(None, None, reason);
            }
            let _permit = match execution_admission.try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    return unavailable(
                        None,
                        None,
                        code_search::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable,
                    );
                }
            };
            let generations = match schedulers
                .generations_for_revisions(
                    &scope,
                    &request.base_reference,
                    &request.base_revision,
                    &request.base_tree,
                    &request.head_reference,
                    &request.head_revision,
                    &request.head_tree,
                    control.clone(),
                )
                .await
            {
                Ok(generations) => generations,
                Err(reason) => return unavailable(None, None, reason),
            };
            let base_id = generations
                .base
                .generation()
                .manifest()
                .generation_id
                .as_str()
                .to_owned();
            let head_id = generations
                .head
                .generation()
                .manifest()
                .generation_id
                .as_str()
                .to_owned();
            let outcome = match bounded_diff(
                generations.base.generation(),
                generations.head.generation(),
                request.file_filter.as_deref(),
                request.kind_filter.as_deref(),
                &control,
            ) {
                Ok(outcome) => outcome,
                Err(reason) => return unavailable(Some(base_id), Some(head_id), reason),
            };
            let outcome = match outcome {
                code_search::CodeIndexBranchDiffOutcomeV1::Complete(completed) => {
                    match paginate_diff(
                        &schedulers,
                        &scope,
                        &authority,
                        &request,
                        generations.base.generation(),
                        generations.head.generation(),
                        completed,
                    )
                    .await
                    {
                        Ok(completed) => {
                            code_search::CodeIndexBranchDiffOutcomeV1::Complete(completed)
                        }
                        Err(reason) => return unavailable(Some(base_id), Some(head_id), reason),
                    }
                }
                partial if request.cursor.is_none() => partial,
                _ => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest,
                    );
                }
            };
            let terminal_scope = match project_open_owners::resolved_scope_for_project(
                &request.project_root,
                &project_id,
            ) {
                Ok(terminal_scope) if terminal_scope == scope => terminal_scope,
                _ => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    );
                }
            };
            let terminal_admission = match admission_provider.admit_current(&terminal_scope) {
                Ok(admission) => admission,
                Err(_) => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    );
                }
            };
            if terminal_admission.search_authority() != authority
                || terminal_admission
                    .authorize(&terminal_scope, Some(&authority))
                    .is_err()
            {
                return unavailable(
                    Some(base_id),
                    Some(head_id),
                    code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                );
            }
            if let Some(reason) = control.termination() {
                return unavailable(Some(base_id), Some(head_id), reason);
            }
            outcome
        })
    })
}

#[cfg(test)]
mod tests {
    use tracedecay_query::code_search::CodeIndexBranchSymbolV1;

    use super::diff_symbols;

    fn symbol_with_identity(
        qualified_name: &str,
        file: &str,
        identity: &str,
        content_digest: &str,
    ) -> CodeIndexBranchSymbolV1 {
        CodeIndexBranchSymbolV1 {
            qualified_name: qualified_name.to_owned(),
            name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name)
                .to_owned(),
            kind: "function".to_owned(),
            file: file.to_owned(),
            symbol_identity: identity.to_owned(),
            content_digest: content_digest.to_owned(),
        }
    }

    fn symbol(qualified_name: &str, file: &str, content_digest: &str) -> CodeIndexBranchSymbolV1 {
        symbol_with_identity(qualified_name, file, qualified_name, content_digest)
    }

    #[test]
    fn diff_is_deterministic_and_distinguishes_added_removed_and_changed_symbols() {
        let base = vec![
            symbol("crate::changed", "src/lib.rs", "sha256:base"),
            symbol("crate::removed", "src/old.rs", "sha256:removed"),
        ];
        let head = vec![
            symbol("crate::added", "src/new.rs", "sha256:added"),
            symbol("crate::changed", "src/lib.rs", "sha256:head"),
        ];

        let completed = diff_symbols("generation.base", base, "generation.head", head);

        assert_eq!(
            completed
                .added
                .iter()
                .map(|symbol| symbol.qualified_name.as_str())
                .collect::<Vec<_>>(),
            ["crate::added"]
        );
        assert_eq!(
            completed
                .removed
                .iter()
                .map(|symbol| symbol.qualified_name.as_str())
                .collect::<Vec<_>>(),
            ["crate::removed"]
        );
        assert_eq!(completed.changed.len(), 1);
        assert_eq!(completed.changed[0].base.content_digest, "sha256:base");
        assert_eq!(completed.changed[0].head.content_digest, "sha256:head");
        assert_eq!(completed.base_generation, "generation.base");
        assert_eq!(completed.head_generation, "generation.head");
    }

    #[test]
    fn materialized_diff_retains_the_bounded_candidate_set_for_authenticated_paging() {
        let head = (0..300)
            .map(|index| {
                symbol(
                    &format!("crate::added_{index:03}"),
                    "src/lib.rs",
                    &format!("sha256:{index:064x}"),
                )
            })
            .collect();
        let completed = diff_symbols("generation.base", Vec::new(), "generation.head", head);
        assert_eq!(completed.total_changes, 300);
        assert_eq!(completed.added.len(), 300);
        assert!(completed.removed.is_empty());
        assert!(completed.changed.is_empty());
    }

    #[test]
    fn same_name_occurrences_are_not_overwritten_during_diff() {
        let base = vec![
            symbol_with_identity(
                "crate::duplicate",
                "src/lib.rs",
                "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
            symbol_with_identity(
                "crate::duplicate",
                "src/lib.rs",
                "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ];
        let head = vec![
            symbol_with_identity(
                "crate::duplicate",
                "src/lib.rs",
                "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            ),
            symbol_with_identity(
                "crate::duplicate",
                "src/lib.rs",
                "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        ];

        let completed = diff_symbols("generation.base", base, "generation.head", head);

        assert_eq!(completed.changed.len(), 1);
        assert_eq!(
            completed.changed[0].base.symbol_identity,
            "sha256:1111111111111111111111111111111111111111111111111111111111111111"
        );
        assert!(completed.added.is_empty());
        assert!(completed.removed.is_empty());
    }
}
