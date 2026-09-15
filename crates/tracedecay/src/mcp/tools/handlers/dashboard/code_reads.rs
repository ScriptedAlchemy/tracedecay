use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tracedecay_code_index_runtime::code_index_branch_diff::{
    CodeIndexRevisionFileV1, CodeIndexRevisionPairRequestV1, CodeIndexRevisionPairV1,
};
use tracedecay_code_index_runtime::code_index_scheduler::branch_generations::BranchGenerationReadControlV1;
use tracedecay_contracts::ResolvedScope;
use tracedecay_contracts::branch_snapshots::{
    LocalBranchReadControlV1, LocalBranchRevisionV1, LocalBranchSnapshotErrorV1,
};
use tracedecay_contracts::retrieval::{
    SimilarCoverageV1, SimilarFamilyV1, SimilarMatchClassV1, SimilarOccurrenceV1, SimilarResultV1,
};
use tracedecay_dashboard_api::code_read_api::{
    DashboardCodeReadErrorV1, DashboardCodeReadPortV1, DashboardRevisionPairReadFuture,
    DashboardRevisionPairRequestV1, DashboardRevisionSelectionV1, DashboardSharedFamilyReadFuture,
    DashboardSharedFamilyRequestV1, RevisionPairChangeV1, RevisionPairFileDispositionV1,
    RevisionPairFileRegionV1, RevisionPairFileV1, RevisionPairRevisionV1,
    RevisionPairSymbolRegionV1, RevisionPairSymbolV1, RevisionPairUnionLayoutV1,
};
use tracedecay_domain::{
    CodeGenerationId, FileIdentityDigest, GitOidV1, SnapshotFileDispositionV1, SymbolIdentityDigest,
};
use tracedecay_query::code_search::{
    CodeIndexBranchSymbolV1, CodeIndexSearchAuthorityV1, CodeIndexSearchUnavailableReasonV1,
    CodeIndexSimilarCompletedV1, CodeIndexSimilarExecutor, CodeIndexSimilarOutcomeV1,
    CodeIndexSimilarRequestV1, CodeIndexSimilarTargetV1,
};
use tracedecay_query::retrieval::lexical::CloneArtifactCursorV1;

#[derive(Clone)]
pub(super) struct DashboardCodeReadAdapter {
    project_root: PathBuf,
    scope: ResolvedScope,
    search_authority: CodeIndexSearchAuthorityV1,
    similar_executor: CodeIndexSimilarExecutor,
    invocation_service: tracedecay_daemon_service::DaemonInvocationService,
}

impl DashboardCodeReadAdapter {
    pub(super) fn new(
        project_root: PathBuf,
        scope: ResolvedScope,
        search_authority: CodeIndexSearchAuthorityV1,
        similar_executor: CodeIndexSimilarExecutor,
        invocation_service: tracedecay_daemon_service::DaemonInvocationService,
    ) -> Self {
        Self {
            project_root,
            scope,
            search_authority,
            similar_executor,
            invocation_service,
        }
    }
}

impl DashboardCodeReadPortV1 for DashboardCodeReadAdapter {
    fn shared_family(
        &self,
        request: DashboardSharedFamilyRequestV1,
    ) -> DashboardSharedFamilyReadFuture<'_> {
        Box::pin(async move {
            let cursor = request
                .cursor
                .as_deref()
                .map(CloneArtifactCursorV1::decode)
                .transpose()
                .map_err(|_| DashboardCodeReadErrorV1::InvalidRequest)?;
            let match_class = match request.match_class {
                SimilarMatchClassV1::ConservativeExact => {
                    tracedecay_code_index::clones::CloneNormalizationClassV1::Conservative
                }
                SimilarMatchClassV1::RenameNormalizedExact => {
                    tracedecay_code_index::clones::CloneNormalizationClassV1::Rename
                }
            };
            let outcome = (self.similar_executor)(CodeIndexSimilarRequestV1 {
                project_root: self.project_root.clone(),
                target: CodeIndexSimilarTargetV1::SymbolOccurrence(request.symbol_occurrence_id),
                match_classes: vec![match_class],
                result_limit: request.limit,
                work_limit: request.limit.saturating_add(1),
                cursor,
                authority: Some(self.search_authority.clone()),
                deadline: Some(request.control.deadline),
                cancellation: Some(request.control.cancellation),
            })
            .await;
            match outcome {
                CodeIndexSimilarOutcomeV1::Complete(result) => {
                    shared_family_result(&self.scope, *result)
                }
                CodeIndexSimilarOutcomeV1::NotFound => Err(DashboardCodeReadErrorV1::NotFound),
                CodeIndexSimilarOutcomeV1::Unavailable(reason) => Err(map_unavailable(reason)),
            }
        })
    }

    fn revision_pair(
        &self,
        request: DashboardRevisionPairRequestV1,
    ) -> DashboardRevisionPairReadFuture<'_> {
        Box::pin(async move {
            let control = LocalBranchReadControlV1 {
                max_refs: 1,
                after: None,
                deadline: Some(request.control.deadline.clone()),
                cancellation: Some(request.control.cancellation.clone()),
            };
            let base = resolve_revision(&self.project_root, &request.base, &control)?;
            let head = resolve_revision(&self.project_root, &request.head, &control)?;
            let revisions = ResolvedRevisionPairV1 {
                base: ResolvedRevisionV1 {
                    selection: request.base,
                    tree: base.tree,
                },
                head: ResolvedRevisionV1 {
                    selection: request.head,
                    tree: head.tree,
                },
            };
            let pair = self
                .invocation_service
                .code_index_revision_pair_layout_inputs(
                    &self.scope,
                    CodeIndexRevisionPairRequestV1 {
                        base_reference: revisions.base.selection.reference.clone(),
                        base_revision: revisions.base.selection.revision.clone(),
                        base_tree: revisions.base.tree.clone(),
                        head_reference: revisions.head.selection.reference.clone(),
                        head_revision: revisions.head.selection.revision.clone(),
                        head_tree: revisions.head.tree.clone(),
                        file_filter: request.file_filter,
                        kind_filter: request.kind_filter,
                        control: BranchGenerationReadControlV1 {
                            deadline: Some(request.control.deadline),
                            cancellation: Some(request.control.cancellation),
                        },
                    },
                )
                .await
                .map_err(map_unavailable)?;
            revision_pair_layout(revisions, pair)
        })
    }
}

fn resolve_revision(
    project_root: &Path,
    selection: &DashboardRevisionSelectionV1,
    control: &LocalBranchReadControlV1,
) -> Result<LocalBranchRevisionV1, DashboardCodeReadErrorV1> {
    let branch = selection
        .reference
        .as_str()
        .strip_prefix("refs/heads/")
        .ok_or(DashboardCodeReadErrorV1::InvalidRequest)?;
    let revision = tracedecay_query::native_git::local_branch_revision_controlled(
        project_root,
        branch,
        control,
    )
    .map_err(map_branch_error)?;
    if revision.commit != selection.revision {
        return Err(DashboardCodeReadErrorV1::RevisionChanged);
    }
    Ok(revision)
}

fn map_branch_error(error: LocalBranchSnapshotErrorV1) -> DashboardCodeReadErrorV1 {
    match error {
        LocalBranchSnapshotErrorV1::InvalidReference { .. }
        | LocalBranchSnapshotErrorV1::InvalidLimit => DashboardCodeReadErrorV1::InvalidRequest,
        LocalBranchSnapshotErrorV1::NotFound { .. }
        | LocalBranchSnapshotErrorV1::RepositoryUnavailable
        | LocalBranchSnapshotErrorV1::ReferenceUnavailable { .. }
        | LocalBranchSnapshotErrorV1::EnumerationUnavailable => {
            DashboardCodeReadErrorV1::GenerationUnavailable
        }
        LocalBranchSnapshotErrorV1::CapacityExceeded { .. } => {
            DashboardCodeReadErrorV1::CapacityUnavailable
        }
        LocalBranchSnapshotErrorV1::Cancelled => DashboardCodeReadErrorV1::Cancelled,
        LocalBranchSnapshotErrorV1::TimedOut => DashboardCodeReadErrorV1::TimedOut,
    }
}

fn map_unavailable(reason: CodeIndexSearchUnavailableReasonV1) -> DashboardCodeReadErrorV1 {
    match reason {
        CodeIndexSearchUnavailableReasonV1::CapabilityUnavailable
        | CodeIndexSearchUnavailableReasonV1::AuthorityUnavailable
        | CodeIndexSearchUnavailableReasonV1::LinkedWorktreeDisabled => {
            DashboardCodeReadErrorV1::AuthorityUnavailable
        }
        CodeIndexSearchUnavailableReasonV1::Cancelled => DashboardCodeReadErrorV1::Cancelled,
        CodeIndexSearchUnavailableReasonV1::TimedOut => DashboardCodeReadErrorV1::TimedOut,
        CodeIndexSearchUnavailableReasonV1::CapacityUnavailable => {
            DashboardCodeReadErrorV1::CapacityUnavailable
        }
        CodeIndexSearchUnavailableReasonV1::GenerationUnavailable
        | CodeIndexSearchUnavailableReasonV1::GenerationUnverified => {
            DashboardCodeReadErrorV1::GenerationUnavailable
        }
        CodeIndexSearchUnavailableReasonV1::InvalidRequest => {
            DashboardCodeReadErrorV1::InvalidRequest
        }
        CodeIndexSearchUnavailableReasonV1::CorruptionResetRequired => {
            DashboardCodeReadErrorV1::CorruptionResetRequired
        }
        CodeIndexSearchUnavailableReasonV1::Internal => DashboardCodeReadErrorV1::Internal,
    }
}

fn shared_family_result(
    scope: &ResolvedScope,
    result: CodeIndexSimilarCompletedV1,
) -> Result<SimilarResultV1, DashboardCodeReadErrorV1> {
    if result.source.occurrence.project_id != scope.project_id
        || result.source.occurrence.repository_id != scope.repository_id
    {
        return Err(DashboardCodeReadErrorV1::NotFound);
    }
    let source = similar_occurrence(&result.source.occurrence);
    let representative_payload_digest = result.source.payload.payload_digest.clone();
    let mut complete = true;
    let families = result
        .exact_groups
        .into_iter()
        .map(|group| {
            let match_class = match group.key.class {
                tracedecay_code_index::clones::CloneNormalizationClassV1::Conservative => {
                    SimilarMatchClassV1::ConservativeExact
                }
                tracedecay_code_index::clones::CloneNormalizationClassV1::Rename => {
                    SimilarMatchClassV1::RenameNormalizedExact
                }
            };
            let unfiltered_member_count = group.members.len();
            let members = group
                .members
                .into_iter()
                .filter(|member| {
                    member.occurrence.project_id == scope.project_id
                        && member.occurrence.repository_id == scope.repository_id
                })
                .map(|member| similar_occurrence(&member.occurrence))
                .collect::<Vec<_>>();
            let family_complete = group.complete && members.len() == unfiltered_member_count;
            complete &= family_complete;
            let next_cursor = group
                .next_cursor
                .as_ref()
                .map(CloneArtifactCursorV1::encode)
                .transpose()
                .map_err(|_| DashboardCodeReadErrorV1::Internal)?;
            Ok(SimilarFamilyV1 {
                match_class,
                normalization_revision: group.key.normalization_revision,
                family_digest: group.key.digest,
                representative_payload_digest: representative_payload_digest.clone(),
                member_count: members.len(),
                members,
                complete: family_complete,
                next_cursor,
            })
        })
        .collect::<Result<Vec<_>, DashboardCodeReadErrorV1>>()?;
    let coverage = match result.source.occurrence.eligibility {
        tracedecay_code_index::clones::CloneBodyEligibilityV1::Eligible if complete => {
            SimilarCoverageV1::Complete
        }
        tracedecay_code_index::clones::CloneBodyEligibilityV1::Eligible => {
            SimilarCoverageV1::Partial
        }
        tracedecay_code_index::clones::CloneBodyEligibilityV1::ExcludedTooSmall {
            minimum_tokens,
        } => SimilarCoverageV1::ExcludedTooSmall { minimum_tokens },
        tracedecay_code_index::clones::CloneBodyEligibilityV1::ExcludedIncompleteTokenization => {
            SimilarCoverageV1::ExcludedIncompleteTokenization
        }
    };
    Ok(SimilarResultV1 {
        source_generation: source.source_generation.clone(),
        source,
        families,
        coverage,
    })
}

fn similar_occurrence(
    occurrence: &tracedecay_code_index::clones::CloneBodyOccurrenceV1,
) -> SimilarOccurrenceV1 {
    SimilarOccurrenceV1 {
        project_id: occurrence.project_id.clone(),
        repository_id: occurrence.repository_id.clone(),
        worktree_id: occurrence.worktree_id.clone(),
        source_generation: occurrence.source_generation.clone(),
        snapshot_digest: occurrence.snapshot_digest.clone(),
        symbol_occurrence_id: occurrence.symbol_occurrence_id.clone(),
        path: occurrence.path.clone(),
        body_span: occurrence.body_span,
    }
}

struct PairSlot<T> {
    base: Option<T>,
    head: Option<T>,
}

struct ResolvedRevisionV1 {
    selection: DashboardRevisionSelectionV1,
    tree: GitOidV1,
}

struct ResolvedRevisionPairV1 {
    base: ResolvedRevisionV1,
    head: ResolvedRevisionV1,
}

fn insert_side<K: Ord, T>(
    slots: &mut BTreeMap<K, PairSlot<T>>,
    key: K,
    value: T,
    base: bool,
) -> Result<(), DashboardCodeReadErrorV1> {
    let slot = slots.entry(key).or_insert_with(|| PairSlot {
        base: None,
        head: None,
    });
    let side = if base { &mut slot.base } else { &mut slot.head };
    if side.replace(value).is_some() {
        return Err(DashboardCodeReadErrorV1::Internal);
    }
    Ok(())
}

fn revision_pair_layout(
    revisions: ResolvedRevisionPairV1,
    pair: CodeIndexRevisionPairV1,
) -> Result<RevisionPairUnionLayoutV1, DashboardCodeReadErrorV1> {
    let base_generation = CodeGenerationId::new(pair.base.generation.clone())
        .map_err(|_| DashboardCodeReadErrorV1::Internal)?;
    let head_generation = CodeGenerationId::new(pair.head.generation.clone())
        .map_err(|_| DashboardCodeReadErrorV1::Internal)?;
    let files = revision_file_regions(
        pair.base.files,
        pair.head.files,
        &pair.base.symbols,
        &pair.head.symbols,
    )?;
    let symbols = revision_symbol_regions(pair.base.symbols, pair.head.symbols)?;
    Ok(RevisionPairUnionLayoutV1 {
        base: revision_descriptor(revisions.base, base_generation),
        head: revision_descriptor(revisions.head, head_generation),
        files,
        symbols,
    })
}

fn revision_file_regions(
    base_files: Vec<CodeIndexRevisionFileV1>,
    head_files: Vec<CodeIndexRevisionFileV1>,
    base_symbols: &[CodeIndexBranchSymbolV1],
    head_symbols: &[CodeIndexBranchSymbolV1],
) -> Result<Vec<RevisionPairFileRegionV1>, DashboardCodeReadErrorV1> {
    let mut file_symbols: BTreeMap<FileIdentityDigest, PairSlot<Vec<SymbolIdentityDigest>>> =
        BTreeMap::new();
    for (base, symbols) in [(true, base_symbols), (false, head_symbols)] {
        let mut grouped: BTreeMap<FileIdentityDigest, Vec<SymbolIdentityDigest>> = BTreeMap::new();
        for symbol in symbols {
            grouped
                .entry(symbol.file_identity.clone())
                .or_default()
                .push(symbol.symbol_identity.clone());
        }
        for (file, mut symbols) in grouped {
            symbols.sort();
            symbols.dedup();
            insert_side(&mut file_symbols, file, symbols, base)?;
        }
    }
    let mut files = BTreeMap::new();
    for file in base_files {
        insert_side(&mut files, file.file_identity.clone(), file, true)?;
    }
    for file in head_files {
        insert_side(&mut files, file.file_identity.clone(), file, false)?;
    }
    let files = files
        .into_iter()
        .map(|(file_identity, slot)| {
            let symbols = file_symbols.remove(&file_identity);
            let base_symbols = symbols
                .as_ref()
                .and_then(|symbols| symbols.base.clone())
                .unwrap_or_default();
            let head_symbols = symbols
                .as_ref()
                .and_then(|symbols| symbols.head.clone())
                .unwrap_or_default();
            let change = pair_change(
                slot.base.as_ref().map(|file| {
                    (
                        file.path.as_str(),
                        &file.content_digest,
                        file.disposition,
                        &base_symbols,
                    )
                }),
                slot.head.as_ref().map(|file| {
                    (
                        file.path.as_str(),
                        &file.content_digest,
                        file.disposition,
                        &head_symbols,
                    )
                }),
            )?;
            Ok(RevisionPairFileRegionV1 {
                file_identity: file_identity.as_str().to_owned(),
                change,
                base: slot.base.map(|file| revision_file(file, base_symbols)),
                head: slot.head.map(|file| revision_file(file, head_symbols)),
            })
        })
        .collect::<Result<Vec<_>, DashboardCodeReadErrorV1>>()?;
    if !file_symbols.is_empty() {
        return Err(DashboardCodeReadErrorV1::Internal);
    }
    Ok(files)
}

fn revision_symbol_regions(
    base_symbols: Vec<CodeIndexBranchSymbolV1>,
    head_symbols: Vec<CodeIndexBranchSymbolV1>,
) -> Result<Vec<RevisionPairSymbolRegionV1>, DashboardCodeReadErrorV1> {
    let mut symbols = BTreeMap::new();
    for symbol in base_symbols {
        insert_side(&mut symbols, symbol.symbol_identity.clone(), symbol, true)?;
    }
    for symbol in head_symbols {
        insert_side(&mut symbols, symbol.symbol_identity.clone(), symbol, false)?;
    }
    symbols
        .into_iter()
        .map(|(symbol_identity, slot)| {
            let change = pair_change(
                slot.base.as_ref().map(symbol_comparison),
                slot.head.as_ref().map(symbol_comparison),
            )?;
            Ok(RevisionPairSymbolRegionV1 {
                symbol_identity: symbol_identity.as_str().to_owned(),
                change,
                base: slot.base.map(revision_symbol),
                head: slot.head.map(revision_symbol),
            })
        })
        .collect()
}

fn revision_descriptor(
    revision: ResolvedRevisionV1,
    generation: CodeGenerationId,
) -> RevisionPairRevisionV1 {
    RevisionPairRevisionV1 {
        reference: revision.selection.reference,
        revision: revision.selection.revision,
        tree: revision.tree,
        generation,
    }
}

fn pair_change<T: PartialEq>(
    base: Option<T>,
    head: Option<T>,
) -> Result<RevisionPairChangeV1, DashboardCodeReadErrorV1> {
    match (base, head) {
        (None, Some(_)) => Ok(RevisionPairChangeV1::Added),
        (Some(_), None) => Ok(RevisionPairChangeV1::Removed),
        (Some(base), Some(head)) if base == head => Ok(RevisionPairChangeV1::Unchanged),
        (Some(_), Some(_)) => Ok(RevisionPairChangeV1::Changed),
        (None, None) => Err(DashboardCodeReadErrorV1::Internal),
    }
}

fn revision_file(
    file: CodeIndexRevisionFileV1,
    symbol_identities: Vec<SymbolIdentityDigest>,
) -> RevisionPairFileV1 {
    RevisionPairFileV1 {
        file_occurrence_id: file.file_occurrence_id,
        path: file.path,
        content_digest: file.content_digest,
        disposition: file_disposition(file.disposition),
        symbol_identities: symbol_identities
            .into_iter()
            .map(|identity| identity.as_str().to_owned())
            .collect(),
    }
}

fn file_disposition(disposition: SnapshotFileDispositionV1) -> RevisionPairFileDispositionV1 {
    match disposition {
        SnapshotFileDispositionV1::Present => RevisionPairFileDispositionV1::Present,
        SnapshotFileDispositionV1::Deleted => RevisionPairFileDispositionV1::Deleted,
        SnapshotFileDispositionV1::Renamed => RevisionPairFileDispositionV1::Renamed,
        SnapshotFileDispositionV1::Ignored => RevisionPairFileDispositionV1::Ignored,
        SnapshotFileDispositionV1::Binary => RevisionPairFileDispositionV1::Binary,
        SnapshotFileDispositionV1::Generated => RevisionPairFileDispositionV1::Generated,
        SnapshotFileDispositionV1::UnsupportedLanguage => {
            RevisionPairFileDispositionV1::UnsupportedLanguage
        }
    }
}

fn symbol_comparison(
    symbol: &CodeIndexBranchSymbolV1,
) -> (&FileIdentityDigest, &str, &str, &str, &str) {
    (
        &symbol.file_identity,
        symbol.qualified_name.as_str(),
        symbol.name.as_str(),
        symbol.kind.as_str(),
        symbol.content_digest.as_str(),
    )
}

fn revision_symbol(symbol: CodeIndexBranchSymbolV1) -> RevisionPairSymbolV1 {
    RevisionPairSymbolV1 {
        symbol_occurrence_id: symbol.symbol_occurrence_id,
        file_identity: symbol.file_identity.as_str().to_owned(),
        file_occurrence_id: symbol.file_occurrence_id,
        qualified_name: symbol.qualified_name,
        name: symbol.name,
        kind: symbol.kind,
        file: symbol.file,
        content_digest: symbol.content_digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_code_index_runtime::code_index_branch_diff::CodeIndexRevisionSnapshotV1;
    use tracedecay_domain::{FileOccurrenceId, SymbolOccurrenceId};

    fn digest<T>(byte: char) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }

    fn file(byte: char, path: &str, content: char) -> CodeIndexRevisionFileV1 {
        CodeIndexRevisionFileV1 {
            file_identity: digest(byte),
            file_occurrence_id: FileOccurrenceId::new(format!("file.{byte}")).expect("file"),
            path: path.to_owned(),
            content_digest: digest(content),
            disposition: SnapshotFileDispositionV1::Present,
        }
    }

    fn symbol(byte: char, file: char, content: &str) -> CodeIndexBranchSymbolV1 {
        CodeIndexBranchSymbolV1 {
            symbol_identity: digest(byte),
            symbol_occurrence_id: SymbolOccurrenceId::new(format!("symbol.{byte}"))
                .expect("symbol"),
            file_identity: digest(file),
            file_occurrence_id: FileOccurrenceId::new(format!("file.{file}")).expect("file"),
            qualified_name: format!("crate::{byte}"),
            name: byte.to_string(),
            kind: "function".to_owned(),
            file: format!("src/{file}.rs"),
            content_digest: content.to_owned(),
        }
    }

    #[test]
    fn union_layout_keeps_both_sides_in_one_stable_identity_order() {
        let pair = CodeIndexRevisionPairV1 {
            base: CodeIndexRevisionSnapshotV1 {
                generation: "generation.base".to_owned(),
                files: vec![file('a', "src/a.rs", '1'), file('b', "src/b.rs", '2')],
                symbols: vec![symbol('a', 'a', "same"), symbol('b', 'b', "base")],
            },
            head: CodeIndexRevisionSnapshotV1 {
                generation: "generation.head".to_owned(),
                files: vec![file('a', "src/a.rs", '1'), file('c', "src/c.rs", '3')],
                symbols: vec![symbol('a', 'a', "same"), symbol('c', 'c', "head")],
            },
        };

        let layout = revision_pair_layout(
            ResolvedRevisionPairV1 {
                base: ResolvedRevisionV1 {
                    selection: DashboardRevisionSelectionV1 {
                        reference: tracedecay_domain::RefId::new("refs/heads/main")
                            .expect("base ref"),
                        revision: GitOidV1::new("1".repeat(40)).expect("base revision"),
                    },
                    tree: GitOidV1::new("2".repeat(40)).expect("base tree"),
                },
                head: ResolvedRevisionV1 {
                    selection: DashboardRevisionSelectionV1 {
                        reference: tracedecay_domain::RefId::new("refs/heads/feature")
                            .expect("head ref"),
                        revision: GitOidV1::new("3".repeat(40)).expect("head revision"),
                    },
                    tree: GitOidV1::new("4".repeat(40)).expect("head tree"),
                },
            },
            pair,
        )
        .expect("union layout");

        assert_eq!(
            layout
                .files
                .iter()
                .map(|region| region.change)
                .collect::<Vec<_>>(),
            [
                RevisionPairChangeV1::Unchanged,
                RevisionPairChangeV1::Removed,
                RevisionPairChangeV1::Added,
            ]
        );
        assert_eq!(
            layout
                .symbols
                .iter()
                .map(|region| region.change)
                .collect::<Vec<_>>(),
            [
                RevisionPairChangeV1::Unchanged,
                RevisionPairChangeV1::Removed,
                RevisionPairChangeV1::Added,
            ]
        );
        assert!(layout.files[1].head.is_none());
        assert!(layout.files[2].base.is_none());
        assert_eq!(
            layout.files[0]
                .base
                .as_ref()
                .map(|file| file.symbol_identities.len()),
            Some(1)
        );
    }
}
