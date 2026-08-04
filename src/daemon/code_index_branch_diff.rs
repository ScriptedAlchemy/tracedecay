//! Exact sealed-generation branch comparison for the daemon-owned MCP route.

use std::collections::BTreeMap;
use std::sync::Arc;

use tracedecay_query::code_search;

use super::{code_index_scheduler, project_open_owners, query_mcp_admission};

const MAX_CONCURRENT_BRANCH_DIFFS: usize = 1;

type SymbolKey = (String, String, String);

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
    code_search::CodeIndexBranchDiffCompletedV1 {
        base_generation: base_generation.to_owned(),
        head_generation: head_generation.to_owned(),
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
            content_digest: symbol.content_digest.as_str().to_owned(),
        });
    }
    symbols.sort_by_key(symbol_key);
    Ok(symbols)
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
                deadline: request.deadline,
                cancellation: request.cancellation,
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
                    &request.base_revision,
                    &request.head_revision,
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
            let base = match generation_symbols(
                generations.base.generation(),
                request.file_filter.as_deref(),
                request.kind_filter.as_deref(),
                &control,
            ) {
                Ok(symbols) => symbols,
                Err(reason) => return unavailable(Some(base_id), Some(head_id), reason),
            };
            let head = match generation_symbols(
                generations.head.generation(),
                request.file_filter.as_deref(),
                request.kind_filter.as_deref(),
                &control,
            ) {
                Ok(symbols) => symbols,
                Err(reason) => return unavailable(Some(base_id), Some(head_id), reason),
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
            code_search::CodeIndexBranchDiffOutcomeV1::Complete(diff_symbols(
                &base_id, base, &head_id, head,
            ))
        })
    })
}

#[cfg(test)]
mod tests {
    use tracedecay_query::code_search::CodeIndexBranchSymbolV1;

    use super::diff_symbols;

    fn symbol(qualified_name: &str, file: &str, content_digest: &str) -> CodeIndexBranchSymbolV1 {
        CodeIndexBranchSymbolV1 {
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
}
