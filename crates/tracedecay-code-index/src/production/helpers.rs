use super::*;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1, ImportReexportScopeV1};
use tracedecay_domain::{
    CodeSourceOmissionReasonV1, EdgeAuthorityV1, RelationEdgeKindV1, SymbolOccurrenceId,
};

use crate::chunks::{
    CROSS_FILE_REFERENCE_BLOCKLIST, cross_file_reference_name_is_blocklisted, is_typescript_family,
    relation_target_kind_is_compatible, rust_qualified_name_is_ufcs_trait_impl,
    rust_type_path_alias_for_trait_impl_method, typescript_member_call_path,
};
use crate::lineage::LineageSymbolRecordV1;
use crate::production::module_resolution::{ModuleImportIndexV1, is_module_import_language};
use crate::production::resolution_view::{FileSymbolsByNameV1, ResolutionFileV1, SymbolsByNameV1};
use crate::production::typescript_resolution::{
    ImportBindingOutcomeV1, TypeScriptModuleIndexV1, unique_local_import,
};

pub(crate) struct StagedGenerationV1 {
    pub(crate) files: Vec<Arc<FileGenerationArtifactsV1>>,
    pub(crate) chunks: GenerationChunkManifestV1,
    pub(crate) symbols: GenerationSymbolIndexV1,
    pub(crate) clone_payloads_reused: u64,
    pub(crate) clone_payloads_computed: u64,
    pub(crate) clone_stale_invalidations: u64,
}

pub(crate) fn staged_generation(
    generation_id: CodeGenerationId,
    mut files: Vec<Arc<FileGenerationArtifactsV1>>,
) -> Result<StagedGenerationV1, CodeIndexProductionErrorV1> {
    files.sort_by(|left, right| {
        left.artifacts
            .chunks
            .document
            .file_occurrence_id
            .cmp(&right.artifacts.chunks.document.file_occurrence_id)
    });
    let chunks = {
        let _span = tracing::trace_span!("code_index.generation.aggregate_chunks").entered();
        GenerationChunkManifestV1::from_validated_files(
            generation_id.clone(),
            files
                .iter()
                .map(|file| file.artifacts.chunks.clone())
                .collect(),
        )
    }
    .map_err(CodeIndexProductionErrorV1::Increment)?;
    let symbols = {
        let _span = tracing::trace_span!("code_index.generation.aggregate_symbols").entered();
        GenerationSymbolIndexV1::new(
            generation_id,
            files
                .iter()
                .flat_map(|file| file.artifacts.symbols.clone())
                .collect(),
        )
    }
    .map_err(CodeIndexProductionErrorV1::Lineage)?;
    Ok(StagedGenerationV1 {
        files,
        chunks,
        symbols,
        clone_payloads_reused: 0,
        clone_payloads_computed: 0,
        clone_stale_invalidations: 0,
    })
}

/// Pin one descriptor registry to the languages this generation can actually
/// index. The same registry instance shape is used by intake, generation
/// sealing, and capability emission, so capability pins cannot disagree with
/// the generation's language revision set.
pub(crate) fn registry_for_snapshot(
    snapshot: &SanitizedCodeSnapshotV1,
) -> Result<StaticLanguageRegistry, CodeIndexProductionErrorV1> {
    let available = StaticLanguageRegistry::new();
    let mut languages = BTreeSet::new();
    for file in &snapshot.files {
        if file.disposition == SnapshotFileDispositionV1::Present {
            let language = file.language.clone().ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "present snapshot file has no declared language".to_owned(),
                )
            })?;
            languages.insert(language);
        }
    }
    if languages.is_empty() {
        return Err(CodeIndexInputErrorV1::NoExtractableFiles.into());
    }
    let mut descriptors = Vec::with_capacity(languages.len());
    for language in languages {
        let descriptor = available.descriptor(&language).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "present snapshot language has no compiled descriptor".to_owned(),
            )
        })?;
        descriptors.push(descriptor.clone());
    }
    StaticLanguageRegistry::try_from_descriptors(descriptors)
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
}

/// Whether a generation's language pins match the currently compiled
/// descriptors for every present file in its authenticated snapshot.
pub fn generation_language_revisions_are_current(
    manifest: &CodeGenerationManifestV1,
    snapshot: &SanitizedCodeSnapshotV1,
) -> bool {
    registry_for_snapshot(snapshot)
        .is_ok_and(|registry| generation_language_revisions_match(manifest, &registry))
}

pub(crate) fn captured_files(
    snapshot: &SanitizedCodeSnapshotV1,
    captured: Vec<CodeIndexCapturedFileV1>,
) -> Result<BTreeMap<FileOccurrenceId, CodeIndexCapturedFileV1>, CodeIndexInputErrorV1> {
    let present = snapshot
        .files
        .iter()
        .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
        .map(|file| (file.file_occurrence_id.clone(), file))
        .collect::<BTreeMap<_, _>>();
    let mut captured_files = BTreeMap::new();
    for captured in captured {
        let Some(file) = present.get(&captured.file_occurrence_id) else {
            return Err(CodeIndexInputErrorV1::UnexpectedCapturedFile);
        };
        if content_digest(&captured.sanitized_bytes) != file.content_digest {
            return Err(CodeIndexInputErrorV1::ContentDigestMismatch);
        }
        if captured_files
            .insert(captured.file_occurrence_id.clone(), captured)
            .is_some()
        {
            return Err(CodeIndexInputErrorV1::DuplicateCapturedFile);
        }
    }
    Ok(captured_files)
}

pub(crate) fn coverage_summary(
    snapshot: &SanitizedCodeSnapshotV1,
    files: &[Arc<FileGenerationArtifactsV1>],
) -> CoverageSummaryV1 {
    let mut coverage = snapshot_coverage(snapshot);
    for file in files {
        coverage = add_coverage(coverage, file_coverage(file));
    }
    coverage
}

/// What one file's extraction contributes to its generation's coverage.
pub(crate) fn file_coverage(file: &FileGenerationArtifactsV1) -> CoverageSummaryV1 {
    let mut coverage = CoverageSummaryV1 {
        ranges_unsupported: u64::try_from(
            file.extraction.error_ranges.len() + file.extraction.unsupported_ranges.len(),
        )
        .unwrap_or(u64::MAX),
        ..CoverageSummaryV1::default()
    };
    match &file.artifacts.chunks.document.eligibility {
        CodeSearchEligibilityV1::Eligible => {}
        CodeSearchEligibilityV1::Excluded { .. } => coverage.files_excluded += 1,
        CodeSearchEligibilityV1::Partial { .. } => coverage.files_partial += 1,
    }
    coverage
}

fn add_coverage(left: CoverageSummaryV1, right: CoverageSummaryV1) -> CoverageSummaryV1 {
    CoverageSummaryV1 {
        files_eligible: left.files_eligible.saturating_add(right.files_eligible),
        files_excluded: left.files_excluded.saturating_add(right.files_excluded),
        files_partial: left.files_partial.saturating_add(right.files_partial),
        files_unsupported: left
            .files_unsupported
            .saturating_add(right.files_unsupported),
        ranges_excluded: left.ranges_excluded.saturating_add(right.ranges_excluded),
        ranges_unsupported: left
            .ranges_unsupported
            .saturating_add(right.ranges_unsupported),
    }
}

/// `parent`'s coverage for a successor over `snapshot` that replaces each
/// file of `before` with its counterpart in `after`: coverage is a sum of
/// the snapshot's rows and every file's extraction, so it moves by the
/// replaced terms. A parent that does not contain the replaced terms is
/// refused rather than clamped.
pub(crate) fn successor_coverage<'a>(
    parent: CoverageSummaryV1,
    parent_snapshot: &SanitizedCodeSnapshotV1,
    snapshot: &SanitizedCodeSnapshotV1,
    before: impl Iterator<Item = &'a FileGenerationArtifactsV1>,
    after: impl Iterator<Item = &'a FileGenerationArtifactsV1>,
) -> Result<CoverageSummaryV1, CodeIndexProductionErrorV1> {
    let subtract = |left: CoverageSummaryV1, right: CoverageSummaryV1| {
        let field = |left: u64, right: u64| {
            left.checked_sub(right).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed parent coverage does not contain the files its successor replaces"
                        .to_owned(),
                )
            })
        };
        Ok::<_, CodeIndexProductionErrorV1>(CoverageSummaryV1 {
            files_eligible: field(left.files_eligible, right.files_eligible)?,
            files_excluded: field(left.files_excluded, right.files_excluded)?,
            files_partial: field(left.files_partial, right.files_partial)?,
            files_unsupported: field(left.files_unsupported, right.files_unsupported)?,
            ranges_excluded: field(left.ranges_excluded, right.ranges_excluded)?,
            ranges_unsupported: field(left.ranges_unsupported, right.ranges_unsupported)?,
        })
    };
    let mut files = subtract(parent, snapshot_coverage(parent_snapshot))?;
    for file in before {
        files = subtract(files, file_coverage(file))?;
    }
    for file in after {
        files = add_coverage(files, file_coverage(file));
    }
    Ok(add_coverage(snapshot_coverage(snapshot), files))
}

/// What a snapshot's rows contribute to coverage, before any file extracts.
fn snapshot_coverage(snapshot: &SanitizedCodeSnapshotV1) -> CoverageSummaryV1 {
    let mut coverage = CoverageSummaryV1::default();
    for file in &snapshot.files {
        match &file.disposition {
            SnapshotFileDispositionV1::Present => coverage.files_eligible += 1,
            SnapshotFileDispositionV1::Ignored | SnapshotFileDispositionV1::Generated => {
                coverage.files_excluded += 1;
                coverage.ranges_excluded += 1;
            }
            SnapshotFileDispositionV1::Binary | SnapshotFileDispositionV1::UnsupportedLanguage => {
                coverage.files_unsupported += 1;
                coverage.ranges_unsupported += 1;
            }
            SnapshotFileDispositionV1::Deleted | SnapshotFileDispositionV1::Renamed => {}
        }
    }
    // Withheld sources already count through their `Ignored` row.
    coverage.files_unsupported += snapshot
        .omitted_sources
        .iter()
        .filter(|source| source.reason == CodeSourceOmissionReasonV1::UnrepresentablePath)
        .count() as u64;
    coverage
}

/// The projection request a generation seals for `changes`: an initial
/// replay of every chunk when the generation has no projected parent, and a
/// source edit over the parent's projection otherwise. A successor whose
/// projection key moved replays whole, so it is built without a parent.
pub(crate) fn projection_request(
    previous_projection_key: Option<ProjectionKeyV1>,
    target_projection_key: ProjectionKeyV1,
    changes: tracedecay_domain::ChangedCodeChunkSetV1,
) -> Result<ProjectionBatchRequestV1, CodeIndexProductionErrorV1> {
    let replay_reason = match &previous_projection_key {
        None => ProjectionReplayReasonV1::InitialProjection,
        Some(previous) if *previous == target_projection_key => {
            ProjectionReplayReasonV1::SourceEdit
        }
        Some(_) => {
            return Err(CodeIndexProductionErrorV1::Contract(
                "a source edit cannot change its projection key".to_owned(),
            ));
        }
    };
    let mut request = ProjectionBatchRequestV1 {
        request_digest: changes.manifest_digest.clone(),
        changes,
        previous_projection_key,
        target_projection_key,
        replay_reason,
    };
    request.request_digest = expected_request_digest(&request)
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    Ok(request)
}

pub(crate) fn edge_order(
    left: &CanonicalRelationEdgeV1,
    right: &CanonicalRelationEdgeV1,
) -> std::cmp::Ordering {
    (
        &left.from_occurrence,
        &left.to_occurrence,
        left.kind,
        left.evidence_span.start_byte,
        left.evidence_span.end_byte,
    )
        .cmp(&(
            &right.from_occurrence,
            &right.to_occurrence,
            right.kind,
            right.evidence_span.start_byte,
            right.evidence_span.end_byte,
        ))
}

pub(crate) fn collect_edge_evidence<T>(
    files: &[T],
) -> Result<
    (Vec<CanonicalRelationEdgeV1>, Vec<CodeIndexEdgeAbstentionV1>),
    CodeIndexProductionErrorV1,
>
where
    T: ResolutionFileV1,
{
    // Resolution's whole-set indexes are gone before the per-file copies are
    // made, and the one exact-capacity vector never regrows, so the peak is
    // the edges this returns plus one sort buffer.
    let cross_file = resolve_cross_file_references(files)?;
    Ok(edge_evidence(files, cross_file))
}

/// `files`' edge evidence: each file's own edges and `cross_file`, the
/// edges resolution derives between them.
pub(crate) fn edge_evidence<T>(
    files: &[T],
    cross_file: Vec<CanonicalRelationEdgeV1>,
) -> (Vec<CanonicalRelationEdgeV1>, Vec<CodeIndexEdgeAbstentionV1>)
where
    T: ResolutionFileV1,
{
    let per_file = files
        .iter()
        .map(|file| file.as_ref().artifacts.edges.len())
        .sum::<usize>();
    let mut edges = Vec::with_capacity(per_file.saturating_add(cross_file.len()));
    for file in files {
        edges.extend(file.as_ref().artifacts.edges.iter().cloned());
    }
    edges.extend(cross_file);
    edges.sort_by(edge_order);
    let mut abstentions = files
        .iter()
        .flat_map(|file| file.as_ref().artifacts.edge_abstentions.clone())
        .collect::<Vec<_>>();
    abstentions.sort();
    (edges, abstentions)
}

/// Resolve the retained per-file unresolved references against the whole
/// staged file set. Sealing is the first moment every file's symbols exist
/// together, so this is where cross-file call/use/implements edges are
/// derived; incremental generations re-run it over their full carried +
/// re-extracted file set, so an edge disappears with either endpoint.
///
/// Binding requires a qualified reference or parser-attested import path,
/// including Rust parent globs and workspace-crate public re-exports, plus
/// exactly one kind-compatible symbol; Python, Go, Java, and Ruby calls bind
/// through [`ModuleImportIndexV1`]. Other bare names have no cross-file
/// authority and stay unresolved. Bound edges carry the `NameResolved`
/// authority class, not `SyntaxExact`.
#[tracing::instrument(name = "code_index.seal.resolve", level = "trace", skip_all)]
pub(crate) fn resolve_cross_file_references<T>(
    files: &[T],
) -> Result<Vec<CanonicalRelationEdgeV1>, CodeIndexProductionErrorV1>
where
    T: ResolutionFileV1,
{
    #[cfg(test)]
    SEAL_REFERENCE_RESOLUTIONS.with(|resolutions| resolutions.set(resolutions.get() + 1));
    let by_simple_name = {
        let _span = tracing::trace_span!("code_index.seal.reference_index").entered();
        FileSymbolsByNameV1::new(files)
    };
    resolve_references(files, &by_simple_name, None)
}

/// The retained references a resolution pass decides: per file index, the
/// indices of its references to decide, ascending.
pub(crate) type ReferenceSelectionV1 = [(usize, Vec<usize>)];

/// Resolves only `selection`'s references against the whole file set whose
/// symbols `by_simple_name` indexes. Each reference binds exactly as
/// [`resolve_cross_file_references`] binds it; the result is the edges those
/// references contribute.
#[tracing::instrument(name = "code_index.seal.resolve_selected", level = "trace", skip_all)]
pub(crate) fn resolve_selected_cross_file_references<T>(
    files: &[T],
    by_simple_name: &dyn SymbolsByNameV1,
    selection: &ReferenceSelectionV1,
) -> Result<Vec<CanonicalRelationEdgeV1>, CodeIndexProductionErrorV1>
where
    T: ResolutionFileV1,
{
    resolve_references(files, by_simple_name, Some(selection))
}

/// `selection`'s references, or every retained reference, as `(file index,
/// reference)` in file and reference order.
pub(crate) fn selected_references<'f, T>(
    files: &'f [T],
    selection: Option<&ReferenceSelectionV1>,
) -> impl Iterator<Item = (usize, &'f CodeIndexUnresolvedReferenceV1)>
where
    T: ResolutionFileV1,
{
    let every = selection.is_none().then(|| {
        files.iter().enumerate().flat_map(|(index, file)| {
            file.as_ref()
                .artifacts
                .unresolved_references
                .iter()
                .map(move |reference| (index, reference))
        })
    });
    let selected = selection.into_iter().flatten().flat_map(|(index, picks)| {
        let references = &files[*index].as_ref().artifacts.unresolved_references;
        picks.iter().map(move |&pick| (*index, &references[pick]))
    });
    every.into_iter().flatten().chain(selected)
}

/// The module facts resolution consults beside symbols by name, each built
/// from the file set the first time a reference needs it: a file set whose
/// references are all Rust never reads a TypeScript manifest or a Python
/// module.
pub(crate) struct ResolutionModulesV1<'f, T> {
    files: &'f [T],
    rust: RustFileIndexV1,
    typescript: OnceLock<TypeScriptModuleIndexV1>,
    modules: OnceLock<ModuleImportIndexV1<'f, T>>,
}

impl<'f, T: ResolutionFileV1> ResolutionModulesV1<'f, T> {
    pub(crate) fn new(files: &'f [T]) -> Self {
        Self {
            files,
            rust: RustFileIndexV1::new(files),
            typescript: OnceLock::new(),
            modules: OnceLock::new(),
        }
    }

    fn typescript(&self) -> &TypeScriptModuleIndexV1 {
        self.typescript
            .get_or_init(|| TypeScriptModuleIndexV1::new(self.files))
    }

    fn modules(&self) -> &ModuleImportIndexV1<'f, T> {
        self.modules
            .get_or_init(|| ModuleImportIndexV1::new(self.files))
    }
}

fn resolve_references<T>(
    files: &[T],
    by_simple_name: &dyn SymbolsByNameV1,
    selection: Option<&ReferenceSelectionV1>,
) -> Result<Vec<CanonicalRelationEdgeV1>, CodeIndexProductionErrorV1>
where
    T: ResolutionFileV1,
{
    let workers = crate::parallelism::indexing_workers().max(1);

    {
        metrics::gauge!("code_index.seal.resolve.effective_workers").set(workers as f64);
        metrics::gauge!("code_index.seal.resolve.unresolved_references")
            .set(selected_references(files, selection).count() as f64);
    }
    let modules = {
        let _span = tracing::trace_span!("code_index.seal.module_index").entered();
        ResolutionModulesV1::new(files)
    };
    // Every file resolves against the same immutable whole-set index, so this
    // is one ordered fan-out over the indexing pool. Concatenating each file's
    // edges in file-index order reproduces the exact sequence the serial loop
    // pushed, so the stable sort and dedup below, and therefore every edge
    // digest downstream, do not depend on the width.
    let units = selection.map_or(files.len(), <[_]>::len);
    let per_file = collect_by_file_index_ordered(units, workers, &|unit| {
        let (index, picks) = match selection {
            Some(selection) => (selection[unit].0, Some(selection[unit].1.as_slice())),
            None => (unit, None),
        };
        resolve_one_file_cross_file_references(files, by_simple_name, &modules, index, picks)
    })?;
    drop(modules);
    let mut edges = Vec::with_capacity(per_file.iter().map(Vec::len).sum());
    for file_edges in per_file {
        edges.extend(file_edges);
    }
    {
        let _span = tracing::trace_span!("code_index.seal.edge_materialization").entered();
        {
            edges.sort_by(edge_order);
            edges.dedup();
        }
    };
    Ok(edges)
}

/// Retained call sites whose import binding names project code the seal
/// could not bind. TypeScript-family: a relative, aliased, or
/// workspace-package specifier that reaches no indexed file, or a module that
/// does not define the imported name (a default import, an `export { x }` of
/// a name the module neither declares nor imports from project code).
/// Python, Go, Java, and Ruby: see [`ModuleImportIndexV1::call_gaps`].
/// These are the sites `callers` and `file_dependents` must disclose as gaps;
/// an import of an external dependency is not one of them.
/// `selection`, when given, limits the sites decided to its references.
pub(crate) fn unresolved_import_calls<T>(
    files: &[T],
    by_simple_name: &dyn SymbolsByNameV1,
    selection: Option<&ReferenceSelectionV1>,
) -> Vec<CodeIndexUnresolvedReferenceV1>
where
    T: ResolutionFileV1,
{
    let modules = ResolutionModulesV1::new(files);
    let selected = || selected_references(files, selection);
    let mut unresolved =
        if selected().any(|(index, _)| is_module_import_language(files[index].language())) {
            modules.modules().call_gaps(selected())
        } else {
            Vec::new()
        };
    let mut receiver_gaps = HashSet::new();
    for (index, reference) in selected() {
        if !is_typescript_family(files[index].language())
            || reference.kind != RelationEdgeKindV1::Calls
            || reference.reference_name.contains("::")
        {
            continue;
        }
        let file = files[index].as_ref();
        match typescript_import_call_outcome(
            files,
            by_simple_name,
            modules.typescript(),
            file,
            reference,
        ) {
            Some(ImportBindingOutcomeV1::Unresolved) => unresolved.push(reference.clone()),
            // A member call no import names is a receiver call nothing binds.
            // It can only be a missing edge to a project symbol of that name,
            // and one site per caller and name is the whole disclosure.
            None => {
                if let Some((_, member)) = reference.reference_name.rsplit_once('.')
                    && by_simple_name.get(member).is_some()
                    && receiver_gaps.insert((&reference.from_occurrence, member))
                {
                    unresolved.push(reference.clone());
                }
            }
            Some(_) => {}
        }
    }
    unresolved
}

/// How a TypeScript-family call binds through the file's import of its
/// callee: a bare imported name, or a member path read from an imported
/// module namespace. `None` when no unique local import names the callee.
fn typescript_import_call_outcome<'a, T>(
    files: &'a [T],
    by_simple_name: &'a dyn SymbolsByNameV1,
    typescript_modules: &TypeScriptModuleIndexV1,
    file: &FileGenerationArtifactsV1,
    reference: &CodeIndexUnresolvedReferenceV1,
) -> Option<ImportBindingOutcomeV1<'a>>
where
    T: ResolutionFileV1,
{
    if !reference.reference_name.contains('.') {
        let binding = unique_import(file, &reference.reference_name, reference.kind)?;
        return Some(typescript_modules.resolve_import_binding(
            files,
            by_simple_name,
            binding,
            reference.kind,
        ));
    }
    if reference.kind != RelationEdgeKindV1::Calls {
        return None;
    }
    let (head, members) = typescript_member_call_path(&reference.reference_name)?;
    let binding = unique_import(file, head, reference.kind)?;
    Some(typescript_modules.resolve_member_call(
        files,
        by_simple_name,
        binding,
        members,
        reference.kind,
    ))
}

/// One ordered, panic-contained fan-out over file indices on the indexing pool.
///
/// Results come back in index order whatever the completion order was, so a
/// caller may concatenate them and keep the sequence a serial loop produced.
fn collect_by_file_index_ordered<R>(
    count: usize,
    workers: usize,
    operation: &(dyn Fn(usize) -> R + Send + Sync),
) -> Result<Vec<R>, CodeIndexProductionErrorV1>
where
    R: Send,
{
    if count < 2 || workers < 2 {
        return Ok((0..count).map(operation).collect());
    }
    crate::parallelism::install(|| {
        (0..count)
            .into_par_iter()
            .map(|index| {
                crate::parallelism::with_background_cpu_permit(|| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(index)))
                        .map_err(|payload| {
                            crate::parallelism::CodeIndexParallelismErrorV1::from_panic_payload(
                                index, &*payload,
                            )
                        })
                })
            })
            // Collecting every unit before short-circuiting keeps the reported
            // failure the lowest-index one, as the serial loop's would be.
            .collect::<Vec<_>>()
    })
    .map_err(CodeIndexProductionErrorV1::from)?
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .map_err(CodeIndexProductionErrorV1::from)
}

/// Resolve one file's retained unresolved references against the whole staged
/// file set.
///
/// The memo is file-local on purpose. `ResolvedReferenceCacheV1` is keyed by
/// source-file index, so a shared map could never serve another file's entry.
/// A per-file resolution therefore decides exactly what the whole-repository
/// serial loop decided.
fn resolve_one_file_cross_file_references<T>(
    files: &[T],
    by_simple_name: &dyn SymbolsByNameV1,
    modules: &ResolutionModulesV1<'_, T>,
    index: usize,
    picks: Option<&[usize]>,
) -> Vec<CanonicalRelationEdgeV1>
where
    T: ResolutionFileV1,
{
    // A module-rule binding may land in the referencing file itself
    // (`Util.normalize` inside `Util`); the per-file pass never saw it.
    let same_file_binds = is_module_import_language(files[index].language());
    let mut resolved_references = ResolvedReferenceCacheV1::new();
    let mut edges = Vec::new();
    let references = &files[index].as_ref().artifacts.unresolved_references;
    let every = picks.is_none().then(|| references.iter());
    let picked = picks.into_iter().flatten().map(|&pick| &references[pick]);
    for reference in every.into_iter().flatten().chain(picked) {
        let cache_key = (
            index,
            reference.reference_name.as_str(),
            reference.kind,
            reference.argument_count,
        );
        let resolved = if let Some(resolved) = resolved_references.get(&cache_key) {
            resolved.clone()
        } else {
            let resolved = {
                let _span =
                    tracing::trace_span!("code_index.seal.reference_candidate_lookup").entered();
                resolve_cross_file_reference(files, by_simple_name, modules, index, reference)
            };
            resolved_references.insert(cache_key, resolved.clone());
            resolved
        };
        let Some((target_index, targets)) = resolved else {
            continue;
        };
        if target_index == index && !same_file_binds {
            continue;
        }
        edges.extend(targets.into_iter().map(|target| CanonicalRelationEdgeV1 {
            from_occurrence: reference.from_occurrence.clone(),
            to_occurrence: target,
            kind: reference.kind,
            authority: EdgeAuthorityV1::NameResolved,
            evidence_span: reference.evidence_span,
        }));
    }
    edges
}

#[cfg(test)]
thread_local! {
    static SEAL_REFERENCE_RESOLUTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn take_seal_reference_resolutions() -> usize {
    SEAL_REFERENCE_RESOLUTIONS.with(|resolutions| resolutions.replace(0))
}

type ResolvedReferenceCacheV1<'a> = HashMap<
    (usize, &'a str, RelationEdgeKindV1, Option<u32>),
    Option<(usize, Vec<SymbolOccurrenceId>)>,
>;

fn resolve_cross_file_reference<T>(
    files: &[T],
    by_simple_name: &dyn SymbolsByNameV1,
    modules: &ResolutionModulesV1<'_, T>,
    index: usize,
    reference: &CodeIndexUnresolvedReferenceV1,
) -> Option<(usize, Vec<SymbolOccurrenceId>)>
where
    T: ResolutionFileV1,
{
    let rust = &modules.rust;
    let file = files[index].as_ref();
    // These languages bind one exact module member through their own import
    // and package rules, never by name matching.
    if is_module_import_language(file.extraction.language.as_str()) {
        return match modules.modules().call_outcome(index, reference)? {
            ImportBindingOutcomeV1::Bound(target_index, symbol) => {
                Some((target_index, vec![symbol.occurrence.clone()]))
            }
            ImportBindingOutcomeV1::External
            | ImportBindingOutcomeV1::Unresolved
            | ImportBindingOutcomeV1::ValueMember => None,
        };
    }
    if file.extraction.language.as_str() == "rust"
        && reference.kind == RelationEdgeKindV1::Calls
        && reference.reference_name.contains('.')
    {
        return None;
    }
    let qualified = reference.reference_name.contains("::");
    // A TypeScript-family import binds one exact module and one exact name,
    // so it resolves through module resolution rather than name matching;
    // the ubiquity blocklist guards only name-only binding.
    if !qualified
        && is_typescript_family(file.extraction.language.as_str())
        && let Some(outcome) = typescript_import_call_outcome(
            files,
            by_simple_name,
            modules.typescript(),
            file,
            reference,
        )
    {
        return match outcome {
            ImportBindingOutcomeV1::Bound(target_index, symbol) if target_index != index => {
                Some((target_index, vec![symbol.occurrence.clone()]))
            }
            ImportBindingOutcomeV1::Bound(..)
            | ImportBindingOutcomeV1::External
            | ImportBindingOutcomeV1::Unresolved
            | ImportBindingOutcomeV1::ValueMember => None,
        };
    }
    let import = (!qualified)
        .then(|| unique_import(file, &reference.reference_name, reference.kind))
        .flatten();
    let has_rust_glob = file.extraction.language.as_str() == "rust"
        && file.artifacts.imports.iter().any(|binding| binding.is_glob);
    if !qualified && import.is_none() && !has_rust_glob {
        return None;
    }
    let simple_name = import
        .and_then(|binding| binding.imported_name.as_deref())
        .or_else(|| reference.reference_name.rsplit("::").next())
        .unwrap_or(reference.reference_name.as_str());
    let is_rust = file.extraction.language.as_str() == "rust";
    let owner_attested = qualified
        && is_rust
        && (CROSS_FILE_REFERENCE_BLOCKLIST.contains(&simple_name)
            || cross_file_reference_name_is_blocklisted(&reference.reference_name, false))
        && rust_qualified_owner_is_project_attested(rust, file, &reference.reference_name);
    // Retention already narrows names, but carried artifacts outlive policy
    // revisions; apply the current blocklist to every retained reference.
    if simple_name.is_empty()
        || (import.is_some() && CROSS_FILE_REFERENCE_BLOCKLIST.contains(&simple_name))
        || cross_file_reference_name_is_blocklisted(&reference.reference_name, owner_attested)
    {
        return None;
    }
    let candidates = by_simple_name.get(simple_name)?;
    if !qualified
        && import.is_none()
        && has_rust_glob
        && candidates.iter().any(|(candidate_index, symbol)| {
            *candidate_index == index
                && relation_target_kind_is_compatible(reference.kind, &symbol.kind)
        })
    {
        return None;
    }
    let source_path = &file.authority.logical_path;
    let crate_qualified = reference.reference_name.strip_prefix("crate::");
    // A blocklisted member (`new`, `read`, `spawn`) is exempt from the
    // blocklist only behind an owner this file attests as project code;
    // `fs::read` through `use std::fs` keeps the member's verdict.
    if qualified
        && is_rust
        && CROSS_FILE_REFERENCE_BLOCKLIST.contains(&simple_name)
        && !owner_attested
    {
        return None;
    }
    // The walk's target-independent half is computed once per reference, not
    // once per candidate: `new` alone has thousands of candidates.
    let walk = (qualified && is_rust)
        .then(|| RustQualifiedWalkV1::new(files, rust, index, &reference.reference_name))
        .flatten();
    let compatible = candidates
        .iter()
        .filter(|(candidate_index, symbol)| {
            let target = RustSymbolTargetV1 {
                index: *candidate_index,
                symbol,
            };
            files[*candidate_index].language() == file.extraction.language.as_str()
                && relation_target_kind_is_compatible(reference.kind, &symbol.kind)
                && match import {
                    None => {
                        let direct = match crate_qualified {
                            None => {
                                (has_rust_glob
                                    && rust_parent_glob_import_matches(
                                        files,
                                        rust,
                                        index,
                                        &reference.reference_name,
                                        reference.kind,
                                        target,
                                    ))
                                    // Rust `::` paths bind only through the
                                    // hop-by-hop walk below; a bare file-stem
                                    // match would bind `fs::read` to any crate's
                                    // `fs.rs`.
                                    || (!is_rust
                                        && file_qualified_name_matches(
                                            &reference.reference_name,
                                            files[*candidate_index].logical_path(),
                                            &symbol.qualified_name,
                                        ))
                            }
                            Some(crate_path) => rust_crate_qualified_name_matches(
                                crate_path,
                                source_path,
                                files[*candidate_index].logical_path(),
                                &symbol.qualified_name,
                            ),
                        };
                        direct
                            || walk
                                .as_ref()
                                .is_some_and(|walk| walk.matches(files, rust, target))
                    }
                    Some(binding) => match binding.module_kind {
                        ImportModuleKindV1::ProjectRelative => project_import_matches(
                            binding,
                            &binding.logical_path,
                            files[*candidate_index].logical_path(),
                            &symbol.qualified_name,
                        ),
                        ImportModuleKindV1::BareModule
                            if file.extraction.language.as_str() == "rust" =>
                        {
                            rust_bare_import_matches(files, rust, index, binding, target, "")
                        }
                        ImportModuleKindV1::BareModule => false,
                    },
                }
        })
        .collect::<Vec<_>>();
    // A type-path call may match both an inherent `Type::method` and one or
    // more `<Type as Trait>::method` aliases; Rust prefers the inherent, so
    // keep a unique non-UFCS hit when aliases also matched.
    let compatible = match compatible.as_slice() {
        [] => return None,
        [_] => compatible,
        // `#[cfg]` variants of one Rust definition: one file, one qualified
        // name, one kind. The call binds that identity at every site.
        [(first_index, first), rest @ ..]
            if is_rust
                && rest.iter().all(|(index, symbol)| {
                    index == first_index
                        && symbol.qualified_name == first.qualified_name
                        && symbol.kind == first.kind
                }) =>
        {
            compatible
        }
        many => {
            let inherent = many
                .iter()
                .copied()
                .filter(|(_, symbol)| {
                    !rust_qualified_name_is_ufcs_trait_impl(&symbol.qualified_name)
                })
                .collect::<Vec<_>>();
            match inherent.as_slice() {
                [_] => inherent,
                _ => return None,
            }
        }
    };
    let (target_index, _) = compatible.first()?;
    if *target_index == index {
        return None;
    }
    Some((
        *target_index,
        compatible
            .iter()
            .map(|(_, symbol)| symbol.occurrence.clone())
            .collect(),
    ))
}

fn unique_import<'a>(
    file: &'a FileGenerationArtifactsV1,
    local_name: &str,
    relation: RelationEdgeKindV1,
) -> Option<&'a CodeIndexImportEvidenceV1> {
    if is_typescript_family(file.extraction.language.as_str()) {
        // `export { x } from` forwards without binding `x` locally; Rust's
        // `pub use` does both, so only TypeScript filters forwarding rows.
        return unique_local_import(file, local_name, relation);
    }
    let mut matches = file.artifacts.imports.iter().filter(|binding| {
        binding.local_name.as_deref() == Some(local_name)
            && match relation {
                RelationEdgeKindV1::Calls => binding.namespace == ImportNamespaceV1::Value,
                RelationEdgeKindV1::Implements
                | RelationEdgeKindV1::Extends
                | RelationEdgeKindV1::TypeOf => {
                    file.extraction.language.as_str() == "rust"
                        || binding.namespace == ImportNamespaceV1::Type
                }
                _ => true,
            }
    });
    let binding = matches.next()?;
    matches.next().is_none().then_some(binding)
}

#[derive(Clone, Copy)]
struct RustSymbolTargetV1<'a> {
    index: usize,
    symbol: &'a LineageSymbolRecordV1,
}

struct RustFileIndexV1 {
    modules: BTreeMap<(String, String), Option<usize>>,
    crate_roots: BTreeMap<String, Option<usize>>,
}

impl RustFileIndexV1 {
    fn new<T>(files: &[T]) -> Self
    where
        T: ResolutionFileV1,
    {
        let mut modules = BTreeMap::new();
        let mut crate_roots = BTreeMap::new();
        // Cargo manifests are found by path without scanning the set per
        // crate root.
        let by_path = files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.logical_path(), index))
            .collect::<HashMap<_, _>>();
        for (index, file) in files.iter().enumerate() {
            if file.language() != "rust" {
                continue;
            }
            let logical_path = file.logical_path();
            let Some(source_root) = rust_source_root(logical_path) else {
                continue;
            };
            let Some(relative) = logical_path
                .strip_prefix(source_root)
                .and_then(|path| path.strip_prefix('/'))
            else {
                continue;
            };
            if let Some(module) = rust_file_module(relative) {
                insert_unique_index(
                    &mut modules,
                    (source_root.to_owned(), module.to_owned()),
                    index,
                );
            }
            if relative == "lib.rs"
                && let Some(crate_name) = rust_crate_name(files, &by_path, source_root)
            {
                insert_unique_index(&mut crate_roots, crate_name, index);
            }
        }
        Self {
            modules,
            crate_roots,
        }
    }

    fn module(&self, source_path: &str, module: &str) -> Option<usize> {
        let source_root = rust_source_root(source_path)?;
        self.modules
            .get(&(source_root.to_owned(), module.to_owned()))
            .copied()
            .flatten()
    }

    fn crate_root(&self, crate_name: &str) -> Option<usize> {
        self.crate_roots.get(crate_name).copied().flatten()
    }
}

fn rust_crate_name<T>(
    files: &[T],
    by_path: &HashMap<&str, usize>,
    source_root: &str,
) -> Option<String>
where
    T: ResolutionFileV1,
{
    let manifest = Path::new(source_root)
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join("Cargo.toml");
    let manifest = manifest.to_str()?;
    by_path
        .get(manifest)
        .map(|index| &files[*index])
        .and_then(|file| {
            file.as_ref().artifacts.symbols.iter().find(|symbol| {
                symbol.simple_name == "name" && symbol.qualified_name.ends_with("::package::name")
            })
        })
        .and_then(|symbol| symbol.signature.as_deref())
        .and_then(|signature| toml::from_str::<toml::Value>(signature).ok())
        .and_then(|pair| {
            pair.get("name")
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            Path::new(source_root)
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(str::to_owned)
        })
        .map(|name| name.replace('-', "_"))
}

fn insert_unique_index<K: Ord>(index: &mut BTreeMap<K, Option<usize>>, key: K, value: usize) {
    index
        .entry(key)
        .and_modify(|slot| *slot = None)
        .or_insert(Some(value));
}

fn rust_parent_glob_import_matches<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    source_index: usize,
    local_name: &str,
    relation: RelationEdgeKindV1,
    target: RustSymbolTargetV1<'_>,
) -> bool
where
    T: ResolutionFileV1,
{
    let source = files[source_index].as_ref();
    source
        .artifacts
        .imports
        .iter()
        .filter(|binding| {
            binding.is_glob
                && (binding.module_specifier == "super"
                    || binding.module_specifier.starts_with("super::"))
        })
        .filter_map(|binding| {
            rust_relative_module(&binding.module_specifier, &source.authority.logical_path)
        })
        .filter_map(|module| rust.module(&source.authority.logical_path, &module))
        .filter_map(|scope_index| unique_import(files[scope_index].as_ref(), local_name, relation))
        .any(|binding| match binding.module_kind {
            ImportModuleKindV1::ProjectRelative => project_import_matches(
                binding,
                &binding.logical_path,
                files[target.index].logical_path(),
                &target.symbol.qualified_name,
            ),
            ImportModuleKindV1::BareModule => {
                rust_bare_import_matches(files, rust, source_index, binding, target, "")
            }
        })
}

fn rust_bare_import_matches<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    access_index: usize,
    binding: &CodeIndexImportEvidenceV1,
    target: RustSymbolTargetV1<'_>,
    member: &str,
) -> bool
where
    T: ResolutionFileV1,
{
    let mut chain = Vec::new();
    rust_bare_import_chain(files, rust, access_index, binding, member, &mut chain);
    rust_export_chain_matches(files, rust, &chain, target)
}

/// Where a qualified Rust path starts once its head segment is expanded.
enum RustPathOriginV1 {
    /// A path inside the referencing file's own crate, as module segments
    /// from that crate's root.
    InCrate,
    /// A path into another workspace crate, from that crate's root file.
    Crate { root_index: usize },
}

/// Whether the qualified reference `reference_name` names `target` when its
/// path is walked segment by segment: `Type::member` through the file's
/// import of `Type`, `krate::module::Type::member` through the workspace
/// crate's root and its (re-)exports, `crate::`/`self::`/`super::` paths
/// through this crate's module files, and a bare module path relative to the
/// referencing module. Every hop is parser-attested (a module file, an
/// import row, or a symbol whose qualified name equals the walked path), so
/// a path that does not exist in the staged file set never binds.
fn rust_qualified_path_matches<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    index: usize,
    reference_name: &str,
    target: RustSymbolTargetV1<'_>,
) -> bool
where
    T: ResolutionFileV1,
{
    RustQualifiedWalkV1::new(files, rust, index, reference_name)
        .is_some_and(|walk| walk.matches(files, rust, target))
}

/// The target-independent half of [`rust_qualified_path_matches`]: the head
/// expansion and, for every module split of one reference, the chain of
/// export hops the walk visits, built once and then matched against each
/// candidate.
struct RustQualifiedWalkV1 {
    /// Each split treats `path[..k]` as modules, `path[k]` as the exported
    /// name, and the rest as the `::member` path below it, so both a method
    /// on a re-exported type and a free function in a nested module are
    /// covered. Splits whose module prefix names no file are dropped.
    chains: Vec<Vec<RustExportHopV1>>,
}

impl RustQualifiedWalkV1 {
    fn new<T>(
        files: &[T],
        rust: &RustFileIndexV1,
        index: usize,
        reference_name: &str,
    ) -> Option<Self>
    where
        T: ResolutionFileV1,
    {
        let file = files[index].as_ref();
        let segments = reference_name.split("::").collect::<Vec<_>>();
        if segments.len() < 2
            || segments
                .iter()
                .any(|segment| segment.is_empty() || segment.contains('<'))
        {
            return None;
        }
        let (origin, path) = rust_expand_path_head(rust, file, &segments)?;
        // A path into another workspace crate binds only public targets.
        let (origin_index, requires_public) = match origin {
            RustPathOriginV1::InCrate => (index, false),
            RustPathOriginV1::Crate { root_index } => (root_index, true),
        };
        let root_path = files[origin_index].logical_path();
        let chains = (0..path.len())
            .filter_map(|k| {
                let scope_index = rust.module(root_path, &path[..k].join("/"))?;
                let member = path[k + 1..]
                    .iter()
                    .map(|segment| format!("::{segment}"))
                    .collect::<String>();
                let mut chain = Vec::new();
                rust_export_chain(
                    files,
                    rust,
                    origin_index,
                    scope_index,
                    index,
                    &path[k],
                    &member,
                    requires_public,
                    &mut BTreeSet::new(),
                    &mut chain,
                );
                Some(chain)
            })
            .collect();
        Some(Self { chains })
    }

    fn matches<T>(
        &self,
        files: &[T],
        rust: &RustFileIndexV1,
        target: RustSymbolTargetV1<'_>,
    ) -> bool
    where
        T: ResolutionFileV1,
    {
        self.chains
            .iter()
            .any(|chain| rust_export_chain_matches(files, rust, chain, target))
    }
}

/// Expands the head of a qualified path into its origin and the remaining
/// segments: `crate`/`self`/`super` prefixes become module segments of this
/// crate, an imported name becomes the path it was imported from, a
/// workspace crate name becomes that crate's root, and any other name is a
/// module beside the referencing one. A path whose head is not attested by
/// any of those is `None`.
fn rust_expand_path_head(
    rust: &RustFileIndexV1,
    file: &FileGenerationArtifactsV1,
    segments: &[&str],
) -> Option<(RustPathOriginV1, Vec<String>)> {
    let source_path = file.authority.logical_path.as_str();
    let head = segments[0];
    if matches!(head, "crate" | "self" | "super") {
        return rust_relative_path(source_path, segments)
            .map(|path| (RustPathOriginV1::InCrate, path));
    }
    if let Some(binding) = unique_named_import(file, head) {
        let imported_name = binding.imported_name.as_deref()?;
        let mut expanded = binding
            .module_specifier
            .split("::")
            .map(str::to_owned)
            .collect::<Vec<_>>();
        expanded.push(imported_name.to_owned());
        expanded.extend(segments[1..].iter().map(|segment| (*segment).to_owned()));
        let expanded_segments = expanded.iter().map(String::as_str).collect::<Vec<_>>();
        let head = expanded_segments[0];
        if matches!(head, "crate" | "self" | "super") {
            return rust_relative_path(source_path, &expanded_segments)
                .map(|path| (RustPathOriginV1::InCrate, path));
        }
        let root_index = rust.crate_root(head)?;
        return Some((
            RustPathOriginV1::Crate { root_index },
            expanded[1..].to_vec(),
        ));
    }
    if let Some(root_index) = rust.crate_root(head) {
        return Some((
            RustPathOriginV1::Crate { root_index },
            segments[1..]
                .iter()
                .map(|segment| (*segment).to_owned())
                .collect(),
        ));
    }
    let mut relative = vec!["self"];
    relative.extend_from_slice(segments);
    rust_relative_path(source_path, &relative).map(|path| (RustPathOriginV1::InCrate, path))
}

/// Whether the head of the qualified Rust path `reference_name` is attested
/// as project code by `file`'s own evidence: a `crate`/`self`/`super` path,
/// an import whose path leads into this crate or a staged workspace crate,
/// a workspace crate name, a module file beside the referencing module, or
/// a type this file defines. `fs` from `use std::fs` is none of those, so
/// `fs::read` is judged by its member, while `ignore::WalkBuilder::new`
/// behind a workspace crate is not.
fn rust_qualified_owner_is_project_attested(
    rust: &RustFileIndexV1,
    file: &FileGenerationArtifactsV1,
    reference_name: &str,
) -> bool {
    let Some((head, _)) = reference_name.split_once("::") else {
        return false;
    };
    if matches!(head, "crate" | "self" | "super") {
        return true;
    }
    if let Some(binding) = unique_named_import(file, head) {
        let import_head = binding
            .module_specifier
            .split("::")
            .next()
            .unwrap_or_default();
        return matches!(import_head, "crate" | "self" | "super")
            || rust.crate_root(import_head).is_some();
    }
    if rust.crate_root(head).is_some() {
        return true;
    }
    let source_path = file.authority.logical_path.as_str();
    rust_relative_module(&format!("self::{head}"), source_path)
        .is_some_and(|module| rust.module(source_path, &module).is_some())
        || file.artifacts.symbols.iter().any(|symbol| {
            symbol.simple_name == head
                && relation_target_kind_is_compatible(RelationEdgeKindV1::TypeOf, &symbol.kind)
        })
}

/// The crate-root-relative module segments of a `crate::`/`self::`/`super::`
/// path from `source_path`, followed by the path's remaining segments.
fn rust_relative_path(source_path: &str, segments: &[&str]) -> Option<Vec<String>> {
    let prefix_len = segments
        .iter()
        .take_while(|segment| matches!(**segment, "crate" | "self" | "super"))
        .count();
    let module = rust_relative_module(&segments[..prefix_len].join("::"), source_path)?;
    let mut path = module
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    path.extend(
        segments[prefix_len..]
            .iter()
            .map(|segment| (*segment).to_owned()),
    );
    (!path.is_empty()).then_some(path)
}

/// The single non-glob import binding `local_name` in `file`, whatever its
/// namespace: a path head may be a type, a module, or a value.
fn unique_named_import<'a>(
    file: &'a FileGenerationArtifactsV1,
    local_name: &str,
) -> Option<&'a CodeIndexImportEvidenceV1> {
    let mut matches =
        file.artifacts.imports.iter().filter(|binding| {
            !binding.is_glob && binding.local_name.as_deref() == Some(local_name)
        });
    let binding = matches.next()?;
    matches.next().is_none().then_some(binding)
}

fn rust_reexport_visible<T>(
    files: &[T],
    binding: &CodeIndexImportEvidenceV1,
    access_index: usize,
    scope_index: usize,
) -> bool
where
    T: ResolutionFileV1,
{
    if binding.is_public {
        return true;
    }
    let Some(reexport_scope) = binding.reexport_scope.as_ref() else {
        return false;
    };
    let access_path = files[access_index].logical_path();
    let scope_path = files[scope_index].logical_path();
    let (Some(access_root), Some(scope_root)) =
        (rust_source_root(access_path), rust_source_root(scope_path))
    else {
        return false;
    };
    if access_root != scope_root {
        return false;
    }
    if *reexport_scope == ImportReexportScopeV1::Crate {
        return true;
    }
    let Some(access_module) = access_path
        .strip_prefix(access_root)
        .and_then(|path| path.strip_prefix('/'))
        .and_then(rust_file_module)
    else {
        return false;
    };
    let Some(scope_module) = scope_path
        .strip_prefix(scope_root)
        .and_then(|path| path.strip_prefix('/'))
        .and_then(rust_file_module)
    else {
        return false;
    };
    let visible_module = match reexport_scope {
        ImportReexportScopeV1::Crate => Some(String::new()),
        ImportReexportScopeV1::SelfModule => Some(scope_module.to_owned()),
        ImportReexportScopeV1::Super => Some(
            scope_module
                .rsplit_once('/')
                .map_or("", |(parent, _)| parent)
                .to_owned(),
        ),
        ImportReexportScopeV1::Module(module) => Some(module.clone()),
    };
    visible_module.is_some_and(|module| rust_module_contains(&module, access_module))
}

fn rust_module_contains(container: &str, candidate: &str) -> bool {
    container.is_empty()
        || candidate == container
        || candidate
            .strip_prefix(container)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

/// One hop of an export walk: the target-independent state at which every
/// candidate is tested. `qualified` is the crate-relative path the hop names
/// directly; `import_qualified` is the path a `crate::`/`self::`/`super::`
/// re-export at this hop names, member appended. `origin_index` is the file
/// whose Cargo source root anchors both comparisons.
struct RustExportHopV1 {
    origin_index: usize,
    scope_index: usize,
    scope_module: String,
    exported_name: String,
    member: String,
    qualified: String,
    import_qualified: Option<String>,
    /// Reached through a workspace crate's re-export, which binds only public
    /// targets; every later hop of the chain inherits the requirement.
    requires_public: bool,
}

/// Whether `exported_name` in the scope file `scope_index` reaches `target`,
/// directly or through re-exports visible to `access_index`. `member` is the
/// `::segment` suffix below the exported name (`::build` for a method on a
/// re-exported type; empty for the export itself). `origin_index` is the
/// file whose Cargo source root anchors every qualified-name comparison.
#[allow(clippy::too_many_arguments)]
fn rust_export_resolves_to_target<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    origin_index: usize,
    scope_index: usize,
    access_index: usize,
    exported_name: &str,
    target: RustSymbolTargetV1<'_>,
    member: &str,
) -> bool
where
    T: ResolutionFileV1,
{
    let mut chain = Vec::new();
    rust_export_chain(
        files,
        rust,
        origin_index,
        scope_index,
        access_index,
        exported_name,
        member,
        false,
        &mut BTreeSet::new(),
        &mut chain,
    );
    rust_export_chain_matches(files, rust, &chain, target)
}

/// The hops `exported_name` in `scope_index` walks through re-exports visible
/// to `access_index`, appended to `chain` in walk order: this scope, then the
/// unique visible import named `exported_name` leads either into another
/// module of this crate (the walk continues with the same origin) or into a
/// workspace crate's root (the walk continues from that root and binds only
/// public targets). A scope revisited on one path ends it.
#[allow(clippy::too_many_arguments)]
fn rust_export_chain<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    origin_index: usize,
    scope_index: usize,
    access_index: usize,
    exported_name: &str,
    member: &str,
    requires_public: bool,
    visited: &mut BTreeSet<(usize, String)>,
    chain: &mut Vec<RustExportHopV1>,
) where
    T: ResolutionFileV1,
{
    if !visited.insert((scope_index, format!("{exported_name}{member}"))) {
        return;
    }
    let scope_path = files[scope_index].logical_path();
    let Some(source_root) = rust_source_root(scope_path) else {
        return;
    };
    let Some(relative_scope) = scope_path
        .strip_prefix(source_root)
        .and_then(|path| path.strip_prefix('/'))
    else {
        return;
    };
    let Some(scope_module) = rust_file_module(relative_scope) else {
        return;
    };
    let mut hop = RustExportHopV1 {
        origin_index,
        scope_index,
        scope_module: scope_module.to_owned(),
        exported_name: exported_name.to_owned(),
        member: member.to_owned(),
        qualified: format!(
            "{}{member}",
            rust_scope_qualified_name(scope_module, exported_name)
        ),
        import_qualified: None,
        requires_public,
    };
    let mut bindings = files[scope_index]
        .as_ref()
        .artifacts
        .imports
        .iter()
        .filter(|binding| {
            binding.local_name.as_deref() == Some(exported_name)
                && rust_reexport_visible(files, binding, access_index, scope_index)
        });
    let binding = match (bindings.next(), bindings.next()) {
        (Some(binding), None) => binding,
        _ => {
            chain.push(hop);
            return;
        }
    };
    match binding.module_kind {
        ImportModuleKindV1::BareModule => {
            chain.push(hop);
            rust_bare_import_chain(files, rust, access_index, binding, member, chain);
        }
        ImportModuleKindV1::ProjectRelative => {
            let Some(imported_name) = binding.imported_name.as_deref() else {
                chain.push(hop);
                return;
            };
            let Some(qualified) =
                rust_import_qualified_name(&binding.module_specifier, imported_name, scope_path)
            else {
                chain.push(hop);
                return;
            };
            hop.import_qualified = Some(format!("{qualified}{member}"));
            chain.push(hop);
            let (module, imported_name) = qualified.rsplit_once("::").unwrap_or(("", &qualified));
            let Some(next_scope) = rust.module(scope_path, &module.replace("::", "/")) else {
                return;
            };
            rust_export_chain(
                files,
                rust,
                origin_index,
                next_scope,
                access_index,
                imported_name,
                member,
                requires_public,
                visited,
                chain,
            );
        }
    }
}

/// The hops a workspace-crate import (`use krate::module::Name`) walks from
/// that crate's root: its `Name` export, then whatever that re-exports. Such
/// a walk binds only public targets and starts a fresh revisit set, so a
/// cross-crate re-export cycle is the crate authors' problem, as before.
fn rust_bare_import_chain<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    access_index: usize,
    binding: &CodeIndexImportEvidenceV1,
    member: &str,
    chain: &mut Vec<RustExportHopV1>,
) where
    T: ResolutionFileV1,
{
    let Some(imported_name) = binding.imported_name.as_deref() else {
        return;
    };
    let mut module = binding.module_specifier.split("::");
    let Some(crate_name) = module.next() else {
        return;
    };
    let module = module.collect::<Vec<_>>().join("/");
    let Some(root_index) = rust.crate_root(crate_name) else {
        return;
    };
    let scope_index = if module.is_empty() {
        root_index
    } else {
        let root_path = files[root_index].logical_path();
        let Some(index) = rust.module(root_path, &module) else {
            return;
        };
        index
    };
    rust_export_chain(
        files,
        rust,
        root_index,
        scope_index,
        access_index,
        imported_name,
        member,
        true,
        &mut BTreeSet::new(),
        chain,
    );
}

/// Whether any hop of `chain` names `target`. A hop that requires a public
/// target ends the chain for a non-public one.
fn rust_export_chain_matches<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    chain: &[RustExportHopV1],
    target: RustSymbolTargetV1<'_>,
) -> bool
where
    T: ResolutionFileV1,
{
    let target_path = files[target.index].logical_path();
    for hop in chain {
        if hop.requires_public && target.symbol.visibility != "public" {
            return false;
        }
        let root_path = files[hop.origin_index].logical_path();
        if (target.index == hop.scope_index
            && rust_crate_qualified_name_matches(
                &hop.qualified,
                root_path,
                target_path,
                &target.symbol.qualified_name,
            ))
            || rust_inherent_method_owned_by_scope_type(
                files,
                rust,
                hop.origin_index,
                hop.scope_index,
                &hop.scope_module,
                &hop.exported_name,
                &hop.member,
                target,
            )
            || hop.import_qualified.as_deref().is_some_and(|qualified| {
                rust_crate_qualified_name_matches(
                    qualified,
                    root_path,
                    target_path,
                    &target.symbol.qualified_name,
                )
            })
        {
            return true;
        }
    }
    false
}

/// The crate-relative path of `exported_name` defined in the module file
/// `scope_module` (`""` for the crate root).
fn rust_scope_qualified_name(scope_module: &str, exported_name: &str) -> String {
    if scope_module.is_empty() {
        exported_name.to_owned()
    } else {
        format!("{}::{exported_name}", scope_module.replace('/', "::"))
    }
}

/// Whether the `crate::`/`self::`/`super::` import `binding`, read from the
/// file at `scope_path`, names `target` (with `member` appended): directly
/// by the imported path, or through the public re-exports of the module the
/// path leads into.
#[allow(clippy::too_many_arguments)]
fn rust_project_import_resolves_to_target<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    origin_index: usize,
    access_index: usize,
    scope_path: &str,
    binding: &CodeIndexImportEvidenceV1,
    target: RustSymbolTargetV1<'_>,
    member: &str,
) -> bool
where
    T: ResolutionFileV1,
{
    let Some(imported_name) = binding.imported_name.as_deref() else {
        return false;
    };
    let Some(qualified) =
        rust_import_qualified_name(&binding.module_specifier, imported_name, scope_path)
    else {
        return false;
    };
    let root_path = files[origin_index].logical_path();
    if rust_crate_qualified_name_matches(
        &format!("{qualified}{member}"),
        root_path,
        files[target.index].logical_path(),
        &target.symbol.qualified_name,
    ) {
        return true;
    }
    let (module, imported_name) = qualified.rsplit_once("::").unwrap_or(("", &qualified));
    let Some(next_scope) = rust.module(scope_path, &module.replace("::", "/")) else {
        return false;
    };
    rust_export_resolves_to_target(
        files,
        rust,
        origin_index,
        next_scope,
        access_index,
        imported_name,
        target,
        member,
    )
}

fn project_import_matches(
    binding: &CodeIndexImportEvidenceV1,
    source_path: &str,
    target_path: &str,
    target_qualified_name: &str,
) -> bool {
    if binding.module_specifier == "crate"
        || binding.module_specifier.starts_with("crate::")
        || binding.module_specifier == "self"
        || binding.module_specifier.starts_with("self::")
        || binding.module_specifier == "super"
        || binding.module_specifier.starts_with("super::")
    {
        let Some(imported_name) = binding.imported_name.as_deref() else {
            return false;
        };
        let Some(qualified) =
            rust_import_qualified_name(&binding.module_specifier, imported_name, source_path)
        else {
            return false;
        };
        return rust_crate_qualified_name_matches(
            &qualified,
            source_path,
            target_path,
            target_qualified_name,
        );
    }
    let Some(parent) = Path::new(&binding.logical_path).parent() else {
        return false;
    };
    let Some(module) = normalize_project_path(&parent.join(&binding.module_specifier)) else {
        return false;
    };
    let target = Path::new(target_path);
    module_file_matches(&module, target)
        || (target.parent() == Some(module.as_path())
            && target.file_stem().is_some_and(|stem| stem == "index"))
}

fn rust_import_qualified_name(
    module_specifier: &str,
    imported_name: &str,
    source_path: &str,
) -> Option<String> {
    let module = rust_relative_module(module_specifier, source_path)?;
    Some(if module.is_empty() {
        imported_name.to_owned()
    } else {
        format!("{}::{imported_name}", module.replace('/', "::"))
    })
}

fn rust_relative_module(module_specifier: &str, source_path: &str) -> Option<String> {
    let module = if module_specifier == "crate" {
        Vec::new()
    } else if let Some(module) = module_specifier.strip_prefix("crate::") {
        module.split("::").collect()
    } else {
        let source_root = rust_source_root(source_path)?;
        let relative = source_path.strip_prefix(source_root)?.strip_prefix('/')?;
        let source_module = rust_file_module(relative)?;
        let mut module = source_module
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let mut relative = module_specifier.split("::");
        let mut may_ascend = match relative.next()? {
            "self" => false,
            "super" => {
                module.pop()?;
                true
            }
            _ => return None,
        };
        for part in relative {
            if part == "super" && may_ascend {
                module.pop()?;
            } else if matches!(part, "self" | "super") {
                return None;
            } else if !part.is_empty() {
                may_ascend = false;
                module.push(part);
            }
        }
        module
    };
    Some(module.join("/"))
}

fn module_file_matches(module: &Path, target: &Path) -> bool {
    if target == module {
        return true;
    }
    let same_stem = target.parent() == module.parent() && target.file_stem() == module.file_stem();
    if !same_stem {
        return false;
    }
    match module.extension().and_then(|extension| extension.to_str()) {
        None => true,
        Some("js" | "jsx" | "mjs" | "cjs") => target
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "ts" | "tsx" | "mts" | "cts")),
        Some(_) => false,
    }
}

fn normalize_project_path(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir if normalized.pop() => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

fn file_qualified_name_matches(
    reference_path: &str,
    target_path: &str,
    target_qualified_name: &str,
) -> bool {
    let Some(symbol_path) = target_qualified_name
        .strip_prefix(target_path)
        .and_then(|path| path.strip_prefix("::"))
    else {
        return false;
    };
    let Some(file_stem) = target_path
        .rsplit('/')
        .next()
        .and_then(|file| file.rsplit_once('.').map(|(stem, _)| stem))
    else {
        return false;
    };
    reference_path
        .strip_prefix(file_stem)
        .and_then(|path| path.strip_prefix("::"))
        == Some(symbol_path)
}

/// A `Type::method` whose owning type is defined in `scope_index` (the
/// module file `scope_module`, `""` for the crate root) may live in any
/// other file of the same crate, either an inherent `impl Type` or a unique
/// `impl Trait for Type` whose UFCS name still answers the type-path call.
/// Validate the type at scope, match the method by its file-relative
/// `Type::method` path (including the UFCS alias), and require the `impl`
/// file to bind `Type` to that same definition: it is the defining file, or
/// it defines no `Type` of its own and imports the type, by name or through
/// a glob of a module that exports it. A same-named type in another module
/// therefore never lends its methods to the scope's type.
#[allow(clippy::too_many_arguments)]
fn rust_inherent_method_owned_by_scope_type<T>(
    files: &[T],
    rust: &RustFileIndexV1,
    origin_index: usize,
    scope_index: usize,
    scope_module: &str,
    exported_name: &str,
    member: &str,
    target: RustSymbolTargetV1<'_>,
) -> bool
where
    T: ResolutionFileV1,
{
    if member.is_empty() {
        return false;
    }
    let root_path = files[origin_index].logical_path();
    // The member's own identity settles nearly every candidate (another
    // crate, another member name) without scanning either file's rows.
    let Some(impl_owner) = rust_inherent_method_owner(
        exported_name,
        member,
        root_path,
        files[target.index].logical_path(),
        &target.symbol.qualified_name,
    ) else {
        return false;
    };
    if impl_owner.rsplit("::").next() != Some(exported_name) {
        return false;
    }
    let impl_file = files[target.index].as_ref();
    if !rust_method_belongs_to_type_impl(impl_file, target.symbol) {
        return false;
    }
    let scope = files[scope_index].as_ref();
    let scope_qualified_type = rust_scope_qualified_name(scope_module, exported_name);
    let Some(scope_type) = scope.artifacts.symbols.iter().find(|symbol| {
        relation_target_kind_is_compatible(RelationEdgeKindV1::TypeOf, &symbol.kind)
            && rust_crate_qualified_name_matches(
                &scope_qualified_type,
                root_path,
                &scope.authority.logical_path,
                &symbol.qualified_name,
            )
    }) else {
        return false;
    };
    let shadowed = impl_file.artifacts.symbols.iter().any(|symbol| {
        symbol.simple_name == exported_name
            && relation_target_kind_is_compatible(RelationEdgeKindV1::TypeOf, &symbol.kind)
    });
    if shadowed {
        return false;
    }
    let type_target = RustSymbolTargetV1 {
        index: scope_index,
        symbol: scope_type,
    };
    if impl_owner.contains("::") {
        return rust_qualified_path_matches(files, rust, target.index, impl_owner, type_target);
    }
    if target.index == scope_index {
        return true;
    }
    let impl_path = impl_file.authority.logical_path.as_str();
    if let Some(binding) = unique_named_import(impl_file, exported_name) {
        return binding.module_kind == ImportModuleKindV1::ProjectRelative
            && rust_project_import_resolves_to_target(
                files,
                rust,
                origin_index,
                target.index,
                impl_path,
                binding,
                type_target,
                "",
            );
    }
    let glob_modules = impl_file
        .artifacts
        .imports
        .iter()
        .filter(|binding| {
            binding.is_glob && binding.module_kind == ImportModuleKindV1::ProjectRelative
        })
        .filter_map(|binding| rust_relative_module(&binding.module_specifier, impl_path))
        .collect::<Vec<_>>();
    glob_modules.iter().any(|module| {
        rust.module(impl_path, module).is_some_and(|glob_scope| {
            rust_export_resolves_to_target(
                files,
                rust,
                origin_index,
                glob_scope,
                target.index,
                exported_name,
                type_target,
                "",
            )
        })
    })
}

fn rust_method_belongs_to_type_impl(
    file: &FileGenerationArtifactsV1,
    method: &LineageSymbolRecordV1,
) -> bool {
    file.artifacts
        .edges
        .iter()
        .filter(|edge| {
            edge.kind == RelationEdgeKindV1::Contains && edge.to_occurrence == method.occurrence
        })
        .filter_map(|edge| {
            file.artifacts
                .symbols
                .iter()
                .find(|symbol| symbol.occurrence == edge.from_occurrence)
        })
        .any(|owner| owner.kind == "impl")
}

/// File-relative method identity for an `impl Type` or `impl Trait for Type`
/// block's `Type::method` (UFCS definitions keep a type-path alias). Same
/// crate as `source_path`, in whichever module file holds the `impl`.
/// Methods of an `impl` nested in an inline module are not matched: their
/// `Type` is bound by that module's own imports, which file import rows do
/// not attest.
fn rust_inherent_method_owner<'a>(
    type_name: &str,
    member: &str,
    source_path: &str,
    target_path: &str,
    target_qualified_name: &'a str,
) -> Option<&'a str> {
    let source_root = rust_source_root(source_path)?;
    if rust_source_root(target_path) != Some(source_root) {
        return None;
    }
    let relative_file = target_path
        .strip_prefix(source_root)
        .and_then(|path| path.strip_prefix('/'))?;
    let source_file = source_path
        .strip_prefix(source_root)
        .and_then(|path| path.strip_prefix('/'))?;
    if source_file.starts_with("bin/") || relative_file.starts_with("bin/") {
        return None;
    }
    if matches!(
        (source_file, relative_file),
        ("lib.rs", "main.rs") | ("main.rs", "lib.rs")
    ) {
        return None;
    }
    let symbol_path = target_qualified_name
        .strip_prefix(target_path)
        .and_then(|path| path.strip_prefix("::"))?;
    let (target_owner, target_member) = symbol_path.rsplit_once("::")?;
    let member = member.strip_prefix("::")?;
    if target_member != member {
        return None;
    }
    let owner =
        rust_ufcs_impl_type_name(target_owner).or_else(|| nominal_rust_impl_owner(target_owner))?;
    (owner.rsplit("::").next() == Some(type_name)).then_some(owner)
}

fn rust_ufcs_impl_type_name(owner: &str) -> Option<&str> {
    let body = owner.strip_prefix('<')?.strip_suffix('>')?;
    let mut depth = 0_i32;
    for (index, character) in body.char_indices() {
        match character {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ if depth == 0 && body[index..].starts_with(" as ") => {
                let type_name = body[..index].trim();
                return (!type_name.is_empty()).then_some(type_name);
            }
            _ => {}
        }
    }
    None
}

fn nominal_rust_impl_owner(owner: &str) -> Option<&str> {
    if rust_ufcs_impl_type_name(owner).is_some() {
        return None;
    }
    match owner.find('<') {
        Some(generic_start) if owner.ends_with('>') => Some(&owner[..generic_start]),
        Some(_) => None,
        None => Some(owner),
    }
}

/// Map an extracted Rust symbol back to the path used by a `crate::...`
/// reference. Standard Cargo source roots scope the match, so equal module
/// paths in sibling workspace crates cannot cross-bind.
fn rust_crate_qualified_name_matches(
    reference_path: &str,
    source_path: &str,
    target_path: &str,
    target_qualified_name: &str,
) -> bool {
    let Some(source_root) = rust_source_root(source_path) else {
        return false;
    };
    if rust_source_root(target_path) != Some(source_root) {
        return false;
    }
    let Some(symbol_path) = target_qualified_name
        .strip_prefix(target_path)
        .and_then(|path| path.strip_prefix("::"))
    else {
        return false;
    };
    let Some(relative_file) = target_path
        .strip_prefix(source_root)
        .and_then(|path| path.strip_prefix('/'))
    else {
        return false;
    };
    let Some(source_file) = source_path
        .strip_prefix(source_root)
        .and_then(|path| path.strip_prefix('/'))
    else {
        return false;
    };
    if source_file.starts_with("bin/") || relative_file.starts_with("bin/") {
        return false;
    }
    if matches!(
        (source_file, relative_file),
        ("lib.rs", "main.rs") | ("main.rs", "lib.rs")
    ) {
        return false;
    }
    let Some(module) = rust_file_module(relative_file) else {
        return false;
    };
    let path_matches = |path: &str| {
        if module.is_empty() {
            reference_path == path
        } else {
            reference_path == path
                || reference_path == format!("{}::{path}", module.replace('/', "::"))
        }
    };
    if path_matches(symbol_path) {
        return true;
    }
    // `<Type as Trait>::method` also answers a type-path call `Type::method`
    // (and the type's final path segment when the UFCS type is crate-qualified).
    let Some(alias) = rust_type_path_alias_for_trait_impl_method(symbol_path) else {
        return false;
    };
    if path_matches(&alias) {
        return true;
    }
    let Some((type_name, method)) = alias.rsplit_once("::") else {
        return false;
    };
    let Some(simple) = type_name.rsplit("::").next() else {
        return false;
    };
    simple != type_name && path_matches(&format!("{simple}::{method}"))
}

fn rust_file_module(relative_file: &str) -> Option<&str> {
    match relative_file {
        "lib.rs" | "main.rs" => Some(""),
        path if path.ends_with("/mod.rs") => path.strip_suffix("/mod.rs"),
        path if path.ends_with(".rs") => path.strip_suffix(".rs"),
        _ => None,
    }
}

fn rust_source_root(path: &str) -> Option<&str> {
    if path.starts_with("src/") {
        return Some("src");
    }
    let marker = path.rfind("/src/")?;
    Some(&path[..marker + "/src".len()])
}

#[cfg(test)]
mod tests {
    use super::{
        file_qualified_name_matches, rust_crate_qualified_name_matches, rust_import_qualified_name,
    };
    use tracedecay_code_extraction::{LanguageExtractor, RustExtractor};

    #[test]
    fn rust_crate_qualified_names_stay_inside_their_cargo_source_root() {
        let call = RustExtractor
            .extract_artifact(
                "src/alpha/mod.rs",
                "pub fn run() -> i32 { crate::beta::run() }",
            )
            .result;
        let target = RustExtractor
            .extract_artifact("src/beta/mod.rs", "pub fn run() -> i32 { 1 }")
            .result;
        assert!(
            call.unresolved_refs
                .iter()
                .any(|reference| reference.reference_name == "crate::beta::run")
        );
        assert!(
            target
                .nodes
                .iter()
                .any(|node| node.qualified_name == "src/beta/mod.rs::run")
        );
        assert!(rust_crate_qualified_name_matches(
            "outer::beta::run",
            "src/alpha/mod.rs",
            "src/outer/beta/mod.rs",
            "src/outer/beta/mod.rs::run",
        ));
        assert!(!rust_crate_qualified_name_matches(
            "beta::run",
            "crates/one/src/alpha/mod.rs",
            "crates/two/src/beta/mod.rs",
            "crates/two/src/beta/mod.rs::run",
        ));
        assert!(!rust_crate_qualified_name_matches(
            "beta::run",
            "src/bin/tool.rs",
            "src/beta/mod.rs",
            "src/beta/mod.rs::run",
        ));
        assert!(!rust_crate_qualified_name_matches(
            "run",
            "src/main.rs",
            "src/lib.rs",
            "src/lib.rs::run",
        ));
        assert!(file_qualified_name_matches(
            "right::Base",
            "src/right.ts",
            "src/right.ts::Base",
        ));
        assert!(!file_qualified_name_matches(
            "other::Base",
            "src/right.ts",
            "src/right.ts::Base",
        ));
    }

    #[test]
    fn rust_relative_imports_use_the_source_module() {
        assert_eq!(
            rust_import_qualified_name(
                "self::read::nested",
                "Detail",
                "crates/tracedecay-contracts/src/feedback/mod.rs",
            )
            .as_deref(),
            Some("feedback::read::nested::Detail")
        );
        assert_eq!(
            rust_import_qualified_name(
                "super::shared",
                "Item",
                "crates/tracedecay-contracts/src/feedback/read.rs",
            )
            .as_deref(),
            Some("feedback::shared::Item")
        );
    }
}
