use super::*;

use tracedecay_domain::EdgeAuthorityV1;

use crate::chunks::{CROSS_FILE_REFERENCE_BLOCKLIST, relation_target_kind_is_compatible};
use crate::lineage::LineageSymbolRecordV1;

pub(crate) struct StagedGenerationV1 {
    pub(crate) files: Vec<Arc<FileGenerationArtifactsV1>>,
    pub(crate) chunks: GenerationChunkManifestV1,
    pub(crate) symbols: GenerationSymbolIndexV1,
    pub(crate) lineage: Vec<SymbolLineageCandidateV1>,
}

pub(crate) fn staged_generation(
    generation_id: CodeGenerationId,
    mut files: Vec<Arc<FileGenerationArtifactsV1>>,
    lineage: Vec<SymbolLineageCandidateV1>,
) -> Result<StagedGenerationV1, CodeIndexProductionErrorV1> {
    files.sort_by(|left, right| {
        left.artifacts
            .chunks
            .document
            .file_occurrence_id
            .cmp(&right.artifacts.chunks.document.file_occurrence_id)
    });
    let chunks = hotpath::measure_block!(
        "code_index.generation.aggregate_chunks",
        GenerationChunkManifestV1::new(
            generation_id.clone(),
            files
                .iter()
                .map(|file| file.artifacts.chunks.clone())
                .collect(),
        )
    )
    .map_err(CodeIndexProductionErrorV1::Increment)?;
    let symbols = hotpath::measure_block!(
        "code_index.generation.aggregate_symbols",
        GenerationSymbolIndexV1::new(
            generation_id,
            files
                .iter()
                .flat_map(|file| file.artifacts.symbols.clone())
                .collect(),
        )
    )
    .map_err(CodeIndexProductionErrorV1::Lineage)?;
    Ok(StagedGenerationV1 {
        files,
        chunks,
        symbols,
        lineage,
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
    for file in files {
        coverage.ranges_unsupported += u64::try_from(
            file.extraction.error_ranges.len() + file.extraction.unsupported_ranges.len(),
        )
        .unwrap_or(u64::MAX);
        match &file.artifacts.chunks.document.eligibility {
            CodeSearchEligibilityV1::Eligible => {}
            CodeSearchEligibilityV1::Excluded { .. } => coverage.files_excluded += 1,
            CodeSearchEligibilityV1::Partial { .. } => coverage.files_partial += 1,
            CodeSearchEligibilityV1::Unsupported { .. } => coverage.files_unsupported += 1,
        }
    }
    coverage
}

pub(crate) fn projection_request(
    active: Option<&CodeIndexPublishedGenerationV1>,
    increment: Option<&crate::generations::GenerationIncrementPlanV1>,
    target_projection_key: ProjectionKeyV1,
    changes: tracedecay_domain::ChangedCodeChunkSetV1,
) -> Result<ProjectionBatchRequestV1, CodeIndexProductionErrorV1> {
    let previous_projection_key =
        active.map(|active| active.projection.request().target_projection_key.clone());
    let replay_reason = match (active, increment) {
        (None, _) => ProjectionReplayReasonV1::InitialProjection,
        (_, Some(increment)) if increment.is_full_rebuild() => {
            ProjectionReplayReasonV1::FullRebuildIncompatible
        }
        (Some(_), _) if previous_projection_key.as_ref() != Some(&target_projection_key) => {
            ProjectionReplayReasonV1::ProjectionProfileChange
        }
        _ => ProjectionReplayReasonV1::SourceEdit,
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
) -> (Vec<CanonicalRelationEdgeV1>, Vec<CodeIndexEdgeAbstentionV1>)
where
    T: AsRef<FileGenerationArtifactsV1>,
{
    let mut edges = files
        .iter()
        .flat_map(|file| file.as_ref().artifacts.edges.clone())
        .collect::<Vec<_>>();
    edges.extend(resolve_cross_file_references(files));
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
/// Binding is deliberately conservative, mirroring the same-file binder:
/// a reference binds only when exactly one kind-compatible symbol matches
/// its simple name across the generation, and never when its own file also
/// defines a compatible candidate (local ambiguity or suppression owns those).
/// Everything else stays truthfully unresolved. Bound edges carry the
/// `NameResolved` authority class, not `SyntaxExact`.
fn resolve_cross_file_references<T>(files: &[T]) -> Vec<CanonicalRelationEdgeV1>
where
    T: AsRef<FileGenerationArtifactsV1>,
{
    let mut by_simple_name: BTreeMap<&str, Vec<(usize, &LineageSymbolRecordV1)>> = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        for symbol in &file.as_ref().artifacts.symbols {
            by_simple_name
                .entry(symbol.simple_name.as_str())
                .or_default()
                .push((index, symbol));
        }
    }
    let mut edges = Vec::new();
    for (index, file) in files.iter().enumerate() {
        for reference in &file.as_ref().artifacts.unresolved_references {
            let simple_name = reference
                .reference_name
                .rsplit("::")
                .next()
                .unwrap_or(reference.reference_name.as_str());
            // Retention already narrows names, but carried artifacts outlive
            // policy revisions; the blocklist is a resolution rule, so apply
            // it to every retained reference regardless of when it was sealed.
            if simple_name.is_empty() || CROSS_FILE_REFERENCE_BLOCKLIST.contains(&simple_name) {
                continue;
            }
            let Some(candidates) = by_simple_name.get(simple_name) else {
                continue;
            };
            let mut compatible = candidates.iter().filter(|(_, symbol)| {
                relation_target_kind_is_compatible(reference.kind, &symbol.kind)
            });
            let Some((first_index, target)) = compatible.next() else {
                continue;
            };
            if *first_index == index || compatible.next().is_some() {
                continue;
            }
            edges.push(CanonicalRelationEdgeV1 {
                from_occurrence: reference.from_occurrence.clone(),
                to_occurrence: target.occurrence.clone(),
                kind: reference.kind,
                authority: EdgeAuthorityV1::NameResolved,
                evidence_span: reference.evidence_span,
            });
        }
    }
    edges.sort_by(edge_order);
    edges.dedup();
    edges
}
