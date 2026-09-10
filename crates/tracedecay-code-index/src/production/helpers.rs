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
            let crate_qualified = reference.reference_name.strip_prefix("crate::");
            // Carried artifacts outlive policy revisions, so reapply the
            // unqualified-name blocklist at every seal.
            if simple_name.is_empty()
                || (crate_qualified.is_none()
                    && CROSS_FILE_REFERENCE_BLOCKLIST.contains(&simple_name))
            {
                continue;
            }
            let Some(candidates) = by_simple_name.get(simple_name) else {
                continue;
            };
            let source_path = &file.as_ref().authority.logical_path;
            let mut compatible = candidates.iter().filter(|(candidate_index, symbol)| {
                relation_target_kind_is_compatible(reference.kind, &symbol.kind)
                    && crate_qualified.map_or_else(
                        || {
                            !reference.reference_name.contains("::")
                                || file_qualified_name_matches(
                                    &reference.reference_name,
                                    &files[*candidate_index].as_ref().authority.logical_path,
                                    &symbol.qualified_name,
                                )
                        },
                        |qualified| {
                            rust_crate_qualified_name_matches(
                                qualified,
                                source_path,
                                &files[*candidate_index].as_ref().authority.logical_path,
                                &symbol.qualified_name,
                            )
                        },
                    )
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
    let module = match relative_file {
        "lib.rs" | "main.rs" => "",
        path if path.ends_with("/mod.rs") => path.strip_suffix("/mod.rs").unwrap_or_default(),
        path if path.ends_with(".rs") => path.strip_suffix(".rs").unwrap_or_default(),
        _ => return false,
    };
    if module.is_empty() {
        reference_path == symbol_path
    } else {
        reference_path == format!("{}::{symbol_path}", module.replace('/', "::"))
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
    use super::{file_qualified_name_matches, rust_crate_qualified_name_matches};
    use tracedecay_code_extraction::{LanguageExtractor, RustExtractor};

    #[test]
    fn rust_crate_qualified_names_stay_inside_their_cargo_source_root() {
        let call = RustExtractor.extract(
            "src/alpha/mod.rs",
            "pub fn run() -> i32 { crate::beta::run() }",
        );
        let target = RustExtractor.extract("src/beta/mod.rs", "pub fn run() -> i32 { 1 }");
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
}
