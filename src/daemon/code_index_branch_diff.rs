//! Exact sealed-generation branch comparison for the daemon-owned MCP route.

use std::collections::BTreeMap;
use std::sync::Arc;

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

type SymbolKey = tracedecay_domain::SymbolIdentityDigest;

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

fn generation_exceeds_bound(counts: GenerationCountsV1) -> bool {
    counts.files > MAX_BRANCH_DIFF_FILES_PER_GENERATION
        || counts.chunks > MAX_BRANCH_DIFF_CHUNKS_PER_GENERATION
        || counts.symbols > MAX_BRANCH_DIFF_SYMBOLS_PER_GENERATION
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
    symbol.symbol_identity.clone()
}

pub(super) fn diff_symbols(
    base_generation: &str,
    base: Vec<code_search::CodeIndexBranchSymbolV1>,
    head_generation: &str,
    head: Vec<code_search::CodeIndexBranchSymbolV1>,
) -> Result<
    code_search::CodeIndexBranchDiffCompletedV1,
    code_search::CodeIndexSearchUnavailableReasonV1,
> {
    let collect = |symbols: Vec<code_search::CodeIndexBranchSymbolV1>| {
        let mut collected = BTreeMap::new();
        for symbol in symbols {
            if collected.insert(symbol_key(&symbol), symbol).is_some() {
                return Err(code_search::CodeIndexSearchUnavailableReasonV1::Internal);
            }
        }
        Ok(collected)
    };
    let mut base = collect(base)?;
    let mut head = collect(head)?;
    let mut changes = Vec::new();
    for key in base
        .keys()
        .chain(head.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
    {
        match (base.remove(&key), head.remove(&key)) {
            (None, Some(symbol)) => {
                changes.push(code_search::CodeIndexBranchChangeV1::Added { symbol });
            }
            (Some(symbol), None) => {
                changes.push(code_search::CodeIndexBranchChangeV1::Removed { symbol });
            }
            (Some(base), Some(head)) if base.content_digest != head.content_digest => {
                changes.push(code_search::CodeIndexBranchChangeV1::Changed { base, head });
            }
            _ => {}
        }
    }
    Ok(code_search::CodeIndexBranchDiffCompletedV1 {
        base_generation: base_generation.to_owned(),
        head_generation: head_generation.to_owned(),
        total_changes: changes.len(),
        changes,
    })
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
            .insert(
                symbol.clone(),
                (chunk.anchor.file_occurrence_id.clone(), *file),
            )
            .is_some_and(|prior| prior != (chunk.anchor.file_occurrence_id.clone(), *file))
        {
            return Err(code_search::CodeIndexSearchUnavailableReasonV1::Internal);
        }
    }
    let mut symbols = Vec::new();
    for symbol in &generation.symbols().symbols {
        if let Some(reason) = control.termination() {
            return Err(reason);
        }
        let (file_occurrence_id, file) = symbol_files
            .get(&symbol.occurrence)
            .ok_or(code_search::CodeIndexSearchUnavailableReasonV1::Internal)?;
        if file_filter.is_some_and(|filter| !file.starts_with(filter) && *file != filter)
            || kind_filter.is_some_and(|filter| symbol.kind != filter)
        {
            continue;
        }
        symbols.push(code_search::CodeIndexBranchSymbolV1 {
            symbol_identity: symbol.identity.clone(),
            symbol_occurrence_id: symbol.occurrence.clone(),
            file_identity: symbol.file_identity.clone(),
            file_occurrence_id: file_occurrence_id.clone(),
            qualified_name: symbol.qualified_name.clone(),
            name: symbol
                .qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(symbol.qualified_name.as_str())
                .to_owned(),
            kind: symbol.kind.clone(),
            file: (*file).to_owned(),
            content_digest: symbol.content_digest.as_str().to_owned(),
        });
    }
    symbols.sort_by_key(symbol_key);
    Ok(symbols)
}

fn page_diff_changes(
    base_generation: &str,
    head_generation: &str,
    changes: Vec<code_search::CodeIndexBranchChangeV1>,
    total_changes: usize,
    next_cursor: Option<String>,
) -> code_search::CodeIndexBranchDiffOutcomeV1 {
    match next_cursor {
        Some(next_cursor) => code_search::CodeIndexBranchDiffOutcomeV1::Partial(
            code_search::CodeIndexBranchDiffPartialV1 {
                base_generation: base_generation.to_owned(),
                head_generation: head_generation.to_owned(),
                reason: code_search::CodeIndexBranchDiffPartialReasonV1::ResultLimit,
                total_changes,
                changes,
                next_cursor,
            },
        ),
        None => code_search::CodeIndexBranchDiffOutcomeV1::Complete(
            code_search::CodeIndexBranchDiffCompletedV1 {
                base_generation: base_generation.to_owned(),
                head_generation: head_generation.to_owned(),
                total_changes,
                changes,
            },
        ),
    }
}

pub(super) fn bounded_diff(
    base: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    head: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    file_filter: Option<&str>,
    kind_filter: Option<&str>,
    _limit: usize,
    control: &code_index_scheduler::branch_generations::BranchGenerationReadControlV1,
) -> Result<
    code_search::CodeIndexBranchDiffCompletedV1,
    code_search::CodeIndexSearchUnavailableReasonV1,
> {
    if let Some(reason) = control.termination() {
        return Err(reason);
    }
    let base_id = base.manifest().generation_id.as_str();
    let head_id = head.manifest().generation_id.as_str();
    let base_counts = generation_counts(base);
    let head_counts = generation_counts(head);
    if generation_exceeds_bound(base_counts) || generation_exceeds_bound(head_counts) {
        return Err(code_search::CodeIndexSearchUnavailableReasonV1::CapacityUnavailable);
    }
    let base_symbols = generation_symbols(base, file_filter, kind_filter, control)?;
    let head_symbols = generation_symbols(head, file_filter, kind_filter, control)?;
    if let Some(reason) = control.termination() {
        return Err(reason);
    }
    diff_symbols(base_id, base_symbols, head_id, head_symbols)
}

fn prepared_error_reason(
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

fn branch_diff_scope_digest(
    scope: &tracedecay_application::ResolvedScope,
    request: &code_search::CodeIndexBranchDiffRequestV1,
    base_generation: &tracedecay_domain::CodeGenerationId,
    head_generation: &tracedecay_domain::CodeGenerationId,
) -> Result<tracedecay_domain::ManifestDigest, tracedecay_domain::DomainError> {
    canonical_sha256(&(
        "tracedecay.code-index-branch-diff.scope.v1",
        &scope.repository_id,
        &scope.worktree_id,
        &request.base_reference,
        &request.base_revision,
        &request.base_tree,
        base_generation,
        &request.head_reference,
        &request.head_revision,
        &request.head_tree,
        head_generation,
    ))
}

fn branch_diff_query_binding_digest(
    request: &code_search::CodeIndexBranchDiffRequestV1,
) -> Result<tracedecay_domain::ManifestDigest, tracedecay_domain::DomainError> {
    canonical_sha256(&(
        "tracedecay.code-index-branch-diff.query.v1",
        request.file_filter.as_deref(),
        request.kind_filter.as_deref(),
        "symbol_identity_ascending.v1",
    ))
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
            let completed = match bounded_diff(
                generations.base.generation(),
                generations.head.generation(),
                request.file_filter.as_deref(),
                request.kind_filter.as_deref(),
                request.limit,
                &control,
            ) {
                Ok(completed) => completed,
                Err(reason) => return unavailable(Some(base_id), Some(head_id), reason),
            };
            let query_authority = match schedulers.query_authority_for_scope(&scope).await {
                Some(authority) => authority,
                None => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable,
                    );
                }
            };
            let freshness_digest = match canonical_sha256(&(
                "tracedecay.code-index-branch-diff.freshness.v1",
                generations
                    .base
                    .generation()
                    .manifest()
                    .snapshot_digest
                    .as_str(),
                generations
                    .head
                    .generation()
                    .manifest()
                    .snapshot_digest
                    .as_str(),
            )) {
                Ok(digest) => digest,
                Err(_) => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                    );
                }
            };
            let freshness = match FreshnessVectorDigest::new(freshness_digest.as_str()) {
                Ok(freshness) => freshness,
                Err(_) => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                    );
                }
            };
            let retrieval_request = RetrievalRequest {
                principal: authority.principal.clone(),
                scope: RetrievalScope {
                    privacy_domain: generations
                        .head
                        .generation()
                        .manifest()
                        .privacy_domain
                        .clone(),
                    root: SingleRootScopeV1 {
                        repository: scope.repository_id.clone(),
                        worktree: Some(scope.worktree_id.clone()),
                        reference: Some(request.head_reference.clone()),
                    },
                },
                temporal_mode: TemporalModeV1::Current,
                snapshot: RetrievalSnapshot {
                    watermarks: VectorWatermark::default(),
                    freshness_digest: freshness,
                    authorization_revision: authority.authorization_revision.clone(),
                    captured_at: generations
                        .base
                        .generation()
                        .manifest()
                        .seal
                        .sealed_at
                        .max(generations.head.generation().manifest().seal.sealed_at),
                },
                profile_id: query_authority.profile().profile_id.clone(),
                budget: query_authority.profile().retrieval_budget,
            };
            let scope_digest = match branch_diff_scope_digest(
                &scope,
                &request,
                &generations.base.generation().manifest().generation_id,
                &generations.head.generation().manifest().generation_id,
            ) {
                Ok(digest) => digest,
                Err(_) => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                    );
                }
            };
            let query_binding_digest = match branch_diff_query_binding_digest(&request) {
                Ok(digest) => digest,
                Err(_) => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                    );
                }
            };
            let bindings = match PreparedQueryBindingsV1::new(
                "code_index_branch_diff.v1",
                scope_digest,
                generations
                    .head
                    .generation()
                    .manifest()
                    .generation_id
                    .clone(),
                query_binding_digest,
            ) {
                Ok(bindings) => bindings,
                Err(error) => {
                    return unavailable(Some(base_id), Some(head_id), prepared_error_reason(error));
                }
            };
            let prepared = match PreparedQueryV1::prepare(
                Arc::clone(&query_authority),
                retrieval_request,
                request.cursor.as_deref(),
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    return unavailable(Some(base_id), Some(head_id), prepared_error_reason(error));
                }
            };
            let page_size = match u32::try_from(
                request
                    .limit
                    .min(code_search::CODE_INDEX_BRANCH_DIFF_MAX_RESULTS_V1),
            ) {
                Ok(page_size) if page_size > 0 => page_size,
                _ => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::InvalidRequest,
                    );
                }
            };
            let page = match prepared.paginate(
                &bindings,
                completed.changes,
                page_size,
                tracedecay_application::clock::now_micros(),
            ) {
                Ok(page) => page,
                Err(error) => {
                    return unavailable(Some(base_id), Some(head_id), prepared_error_reason(error));
                }
            };
            let total_changes = match usize::try_from(page.total) {
                Ok(total) => total,
                Err(_) => {
                    return unavailable(
                        Some(base_id),
                        Some(head_id),
                        code_search::CodeIndexSearchUnavailableReasonV1::Internal,
                    );
                }
            };
            let outcome = page_diff_changes(
                &base_id,
                &head_id,
                page.items,
                total_changes,
                page.next_cursor,
            );
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
    use tracedecay_query::code_search::{self, CodeIndexBranchSymbolV1};

    use super::{
        branch_diff_query_binding_digest, branch_diff_scope_digest, diff_symbols, page_diff_changes,
    };

    fn symbol(
        identity: u64,
        occurrence: &str,
        qualified_name: &str,
        file: &str,
        content_digest: &str,
    ) -> CodeIndexBranchSymbolV1 {
        CodeIndexBranchSymbolV1 {
            symbol_identity: tracedecay_domain::SymbolIdentityDigest::new(format!(
                "sha256:{identity:064x}"
            ))
            .expect("symbol identity"),
            symbol_occurrence_id: tracedecay_domain::SymbolOccurrenceId::new(occurrence)
                .expect("symbol occurrence"),
            file_identity: tracedecay_domain::FileIdentityDigest::new(format!(
                "sha256:{:064x}",
                10
            ))
            .expect("file identity"),
            file_occurrence_id: tracedecay_domain::FileOccurrenceId::new("file.fixture")
                .expect("file occurrence"),
            qualified_name: qualified_name.to_owned(),
            name: qualified_name
                .rsplit("::")
                .next()
                .unwrap_or(qualified_name)
                .to_owned(),
            kind: "function".to_owned(),
            file: file.to_owned(),
            content_digest: content_digest.to_owned(),
        }
    }

    #[test]
    fn diff_is_deterministic_and_distinguishes_added_removed_and_changed_symbols() {
        let base = vec![
            symbol(
                1,
                "symbol.base.changed",
                "crate::changed",
                "src/lib.rs",
                "sha256:base",
            ),
            symbol(
                2,
                "symbol.base.removed",
                "crate::removed",
                "src/old.rs",
                "sha256:removed",
            ),
        ];
        let head = vec![
            symbol(
                3,
                "symbol.head.added",
                "crate::added",
                "src/new.rs",
                "sha256:added",
            ),
            symbol(
                1,
                "symbol.head.changed",
                "crate::changed",
                "src/lib.rs",
                "sha256:head",
            ),
        ];

        let completed =
            diff_symbols("generation.base", base, "generation.head", head).expect("diff");

        assert!(matches!(
            completed.changes.as_slice(),
            [
                code_search::CodeIndexBranchChangeV1::Changed { base, head },
                code_search::CodeIndexBranchChangeV1::Removed { symbol: removed },
                code_search::CodeIndexBranchChangeV1::Added { symbol: added },
            ] if base.content_digest == "sha256:base"
                && head.content_digest == "sha256:head"
                && removed.qualified_name == "crate::removed"
                && added.qualified_name == "crate::added"
        ));
        assert_eq!(completed.total_changes, 3);
        assert_eq!(completed.base_generation, "generation.base");
        assert_eq!(completed.head_generation, "generation.head");
    }

    #[test]
    fn result_limit_returns_a_deterministic_typed_partial() {
        let head = (0..300)
            .map(|index| {
                symbol(
                    u64::try_from(index).expect("fixture index"),
                    &format!("symbol.head.{index:03}"),
                    &format!("crate::added_{index:03}"),
                    "src/lib.rs",
                    &format!("sha256:{index:064x}"),
                )
            })
            .collect();
        let completed =
            diff_symbols("generation.base", Vec::new(), "generation.head", head).expect("diff");
        let outcome = page_diff_changes(
            &completed.base_generation,
            &completed.head_generation,
            completed.changes.into_iter().take(10).collect(),
            completed.total_changes,
            Some("cursor.authenticated".to_owned()),
        );

        assert!(matches!(
            outcome,
            code_search::CodeIndexBranchDiffOutcomeV1::Partial(
                code_search::CodeIndexBranchDiffPartialV1 {
                    reason: code_search::CodeIndexBranchDiffPartialReasonV1::ResultLimit,
                    total_changes: 300,
                    changes,
                    next_cursor,
                    ..
                }
            ) if changes.len() == 10 && next_cursor == "cursor.authenticated"
        ));
    }

    #[test]
    fn result_page_retains_authenticated_continuation_and_merged_order() {
        let changes = vec![
            code_search::CodeIndexBranchChangeV1::Removed {
                symbol: symbol(
                    1,
                    "symbol.base.removed",
                    "crate::removed",
                    "src/a.rs",
                    "sha256:removed",
                ),
            },
            code_search::CodeIndexBranchChangeV1::Added {
                symbol: symbol(
                    2,
                    "symbol.head.added",
                    "crate::added",
                    "src/b.rs",
                    "sha256:added",
                ),
            },
        ];

        let outcome = page_diff_changes(
            "generation.base",
            "generation.head",
            changes.clone(),
            3,
            Some("cursor.authenticated".to_owned()),
        );

        assert!(matches!(
            outcome,
            code_search::CodeIndexBranchDiffOutcomeV1::Partial(
                code_search::CodeIndexBranchDiffPartialV1 {
                    total_changes: 3,
                    changes: page,
                    next_cursor,
                    ..
                }
            ) if page == changes && next_cursor == "cursor.authenticated"
        ));
    }

    #[test]
    fn same_named_symbol_occurrences_are_not_overwritten() {
        let base = vec![
            symbol(
                1,
                "symbol.base.duplicate-a",
                "crate::duplicate",
                "src/lib.rs",
                "sha256:base-a",
            ),
            symbol(
                2,
                "symbol.base.duplicate-b",
                "crate::duplicate",
                "src/lib.rs",
                "sha256:base-b",
            ),
        ];
        let head = vec![
            symbol(
                1,
                "symbol.head.duplicate-a",
                "crate::duplicate",
                "src/lib.rs",
                "sha256:head-a",
            ),
            symbol(
                2,
                "symbol.head.duplicate-b",
                "crate::duplicate",
                "src/lib.rs",
                "sha256:head-b",
            ),
        ];

        let completed =
            diff_symbols("generation.base", base, "generation.head", head).expect("diff");

        assert_eq!(
            completed.changes.len(),
            2,
            "stable occurrence identity must preserve same-name definitions"
        );
    }

    #[test]
    fn duplicate_stable_symbol_identity_is_typed_corruption() {
        let duplicated = vec![
            symbol(
                1,
                "symbol.base.duplicate-a",
                "crate::duplicate",
                "src/a.rs",
                "sha256:a",
            ),
            symbol(
                1,
                "symbol.base.duplicate-b",
                "crate::duplicate",
                "src/b.rs",
                "sha256:b",
            ),
        ];

        assert_eq!(
            diff_symbols("generation.base", duplicated, "generation.head", Vec::new(),),
            Err(code_search::CodeIndexSearchUnavailableReasonV1::Internal)
        );
    }

    #[test]
    fn continuation_binding_covers_exact_refs_commits_trees_generations_and_filters() {
        let scope = tracedecay_application::ResolvedScope::new(
            tracedecay_domain::ProjectId::new("project.diff-binding").expect("project"),
            tracedecay_domain::RepositoryId::new("repository.diff-binding").expect("repository"),
            tracedecay_domain::WorktreeId::new("worktree.diff-binding").expect("worktree"),
            Some(tracedecay_domain::RefId::new("refs/heads/head").expect("scope reference")),
        )
        .expect("scope");
        let request = code_search::CodeIndexBranchDiffRequestV1 {
            project_root: std::path::PathBuf::from("/project"),
            base_reference: tracedecay_domain::RefId::new("refs/heads/base")
                .expect("base reference"),
            base_revision: tracedecay_domain::GitOidV1::new("1".repeat(40)).expect("base revision"),
            base_tree: tracedecay_domain::GitOidV1::new("2".repeat(40)).expect("base tree"),
            head_reference: tracedecay_domain::RefId::new("refs/heads/head")
                .expect("head reference"),
            head_revision: tracedecay_domain::GitOidV1::new("3".repeat(40)).expect("head revision"),
            head_tree: tracedecay_domain::GitOidV1::new("4".repeat(40)).expect("head tree"),
            file_filter: Some("src".to_owned()),
            kind_filter: Some("function".to_owned()),
            limit: 10,
            cursor: None,
            authority: None,
            deadline: None,
            cancellation: None,
        };
        let base_generation =
            tracedecay_domain::CodeGenerationId::new("generation.diff.base").expect("base gen");
        let head_generation =
            tracedecay_domain::CodeGenerationId::new("generation.diff.head").expect("head gen");
        let expected =
            branch_diff_scope_digest(&scope, &request, &base_generation, &head_generation)
                .expect("scope digest");

        let mut mutations = Vec::new();
        let mut changed = request.clone();
        changed.base_reference =
            tracedecay_domain::RefId::new("refs/heads/other-base").expect("other base");
        mutations.push(changed);
        let mut changed = request.clone();
        changed.base_revision =
            tracedecay_domain::GitOidV1::new("5".repeat(40)).expect("other base commit");
        mutations.push(changed);
        let mut changed = request.clone();
        changed.base_tree =
            tracedecay_domain::GitOidV1::new("6".repeat(40)).expect("other base tree");
        mutations.push(changed);
        let mut changed = request.clone();
        changed.head_reference =
            tracedecay_domain::RefId::new("refs/heads/other-head").expect("other head");
        mutations.push(changed);
        let mut changed = request.clone();
        changed.head_revision =
            tracedecay_domain::GitOidV1::new("7".repeat(40)).expect("other head commit");
        mutations.push(changed);
        let mut changed = request.clone();
        changed.head_tree =
            tracedecay_domain::GitOidV1::new("8".repeat(40)).expect("other head tree");
        mutations.push(changed);
        for changed in mutations {
            assert_ne!(
                branch_diff_scope_digest(&scope, &changed, &base_generation, &head_generation,)
                    .expect("changed scope digest"),
                expected,
            );
        }
        assert_ne!(
            branch_diff_scope_digest(
                &scope,
                &request,
                &tracedecay_domain::CodeGenerationId::new("generation.diff.other-base")
                    .expect("other base gen"),
                &head_generation,
            )
            .expect("changed base generation digest"),
            expected,
        );
        assert_ne!(
            branch_diff_scope_digest(
                &scope,
                &request,
                &base_generation,
                &tracedecay_domain::CodeGenerationId::new("generation.diff.other-head")
                    .expect("other head gen"),
            )
            .expect("changed head generation digest"),
            expected,
        );
        let expected_query =
            branch_diff_query_binding_digest(&request).expect("query binding digest");
        let mut changed_filter = request;
        changed_filter.kind_filter = Some("struct".to_owned());
        assert_ne!(
            branch_diff_query_binding_digest(&changed_filter).expect("changed query digest"),
            expected_query,
        );
    }
}
