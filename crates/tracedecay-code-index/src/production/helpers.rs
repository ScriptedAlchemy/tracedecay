use super::*;

use std::path::{Component, Path, PathBuf};

use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_domain::{EdgeAuthorityV1, RelationEdgeKindV1};

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
/// Binding requires a qualified reference or parser-attested import path,
/// including Rust parent globs and workspace-crate public re-exports, plus
/// exactly one kind-compatible symbol. Other bare names have no cross-file
/// authority and stay unresolved. Bound edges carry the `NameResolved`
/// authority class, not `SyntaxExact`.
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
            let import = if reference.reference_name.contains("::") {
                None
            } else {
                unique_import(file.as_ref(), &reference.reference_name, reference.kind)
            };
            let has_rust_glob = file.as_ref().extraction.language.as_str() == "rust"
                && file
                    .as_ref()
                    .artifacts
                    .imports
                    .iter()
                    .any(|binding| binding.is_glob);
            if !reference.reference_name.contains("::") && import.is_none() && !has_rust_glob {
                continue;
            }
            let simple_name = import
                .and_then(|binding| binding.imported_name.as_deref())
                .or_else(|| reference.reference_name.rsplit("::").next())
                .unwrap_or(reference.reference_name.as_str());
            let crate_qualified = reference.reference_name.strip_prefix("crate::");
            // Retention already narrows names, but carried artifacts outlive
            // policy revisions; the blocklist is a resolution rule, so apply
            // it to every retained reference regardless of when it was sealed.
            if simple_name.is_empty() || CROSS_FILE_REFERENCE_BLOCKLIST.contains(&simple_name) {
                continue;
            }
            let Some(candidates) = by_simple_name.get(simple_name) else {
                continue;
            };
            let source_path = &file.as_ref().authority.logical_path;
            let mut compatible = candidates.iter().filter(|(candidate_index, symbol)| {
                files[*candidate_index].as_ref().extraction.language
                    == file.as_ref().extraction.language
                    && relation_target_kind_is_compatible(reference.kind, &symbol.kind)
                    && import.map_or_else(
                        || {
                            crate_qualified.map_or_else(
                                || {
                                    if has_rust_glob
                                        && rust_parent_glob_import_matches(
                                            files,
                                            index,
                                            &reference.reference_name,
                                            reference.kind,
                                            *candidate_index,
                                            symbol,
                                        )
                                    {
                                        return true;
                                    }
                                    file_qualified_name_matches(
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
                        },
                        |binding| match binding.module_kind {
                            ImportModuleKindV1::ProjectRelative => project_import_matches(
                                binding,
                                &binding.logical_path,
                                &files[*candidate_index].as_ref().authority.logical_path,
                                &symbol.qualified_name,
                            ),
                            ImportModuleKindV1::BareModule
                                if file.as_ref().extraction.language.as_str() == "rust" =>
                            {
                                rust_bare_import_matches(files, binding, *candidate_index, symbol)
                            }
                            ImportModuleKindV1::BareModule => false,
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

fn unique_import<'a>(
    file: &'a FileGenerationArtifactsV1,
    local_name: &str,
    relation: RelationEdgeKindV1,
) -> Option<&'a CodeIndexImportEvidenceV1> {
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

fn rust_parent_glob_import_matches<T>(
    files: &[T],
    source_index: usize,
    local_name: &str,
    relation: RelationEdgeKindV1,
    target_index: usize,
    target: &LineageSymbolRecordV1,
) -> bool
where
    T: AsRef<FileGenerationArtifactsV1>,
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
        .filter_map(|module| rust_module_file_index(files, &source.authority.logical_path, &module))
        .filter_map(|scope_index| unique_import(files[scope_index].as_ref(), local_name, relation))
        .any(|binding| match binding.module_kind {
            ImportModuleKindV1::ProjectRelative => project_import_matches(
                binding,
                &binding.logical_path,
                &files[target_index].as_ref().authority.logical_path,
                &target.qualified_name,
            ),
            ImportModuleKindV1::BareModule => {
                rust_bare_import_matches(files, binding, target_index, target)
            }
        })
}

fn rust_bare_import_matches<T>(
    files: &[T],
    binding: &CodeIndexImportEvidenceV1,
    target_index: usize,
    target: &LineageSymbolRecordV1,
) -> bool
where
    T: AsRef<FileGenerationArtifactsV1>,
{
    if target.visibility != "public" {
        return false;
    }
    let Some(imported_name) = binding.imported_name.as_deref() else {
        return false;
    };
    let mut module = binding.module_specifier.split("::");
    let Some(crate_name) = module.next() else {
        return false;
    };
    let module = module.collect::<Vec<_>>().join("/");
    let Some(root_index) = rust_crate_root_file_index(files, crate_name) else {
        return false;
    };
    let scope_index = if module.is_empty() {
        root_index
    } else {
        let root_path = &files[root_index].as_ref().authority.logical_path;
        let Some(index) = rust_module_file_index(files, root_path, &module) else {
            return false;
        };
        index
    };
    let mut visited = BTreeSet::new();
    rust_export_resolves_to_target(
        files,
        root_index,
        scope_index,
        imported_name,
        target_index,
        target,
        &mut visited,
    )
}

fn rust_export_resolves_to_target<T>(
    files: &[T],
    root_index: usize,
    scope_index: usize,
    exported_name: &str,
    target_index: usize,
    target: &LineageSymbolRecordV1,
    visited: &mut BTreeSet<(usize, String)>,
) -> bool
where
    T: AsRef<FileGenerationArtifactsV1>,
{
    if !visited.insert((scope_index, exported_name.to_owned())) {
        return false;
    }
    let root_path = &files[root_index].as_ref().authority.logical_path;
    let scope_path = &files[scope_index].as_ref().authority.logical_path;
    let Some(source_root) = rust_source_root(scope_path) else {
        return false;
    };
    let Some(relative_scope) = scope_path
        .strip_prefix(source_root)
        .and_then(|path| path.strip_prefix('/'))
    else {
        return false;
    };
    let Some(scope_module) = rust_file_module(relative_scope) else {
        return false;
    };
    let qualified = if scope_module.is_empty() {
        exported_name.to_owned()
    } else {
        format!("{}::{exported_name}", scope_module.replace('/', "::"))
    };
    if target_index == scope_index
        && rust_crate_qualified_name_matches(
            &qualified,
            root_path,
            &files[target_index].as_ref().authority.logical_path,
            &target.qualified_name,
        )
    {
        return true;
    }

    let mut bindings = files[scope_index]
        .as_ref()
        .artifacts
        .imports
        .iter()
        .filter(|binding| {
            binding.is_public && binding.local_name.as_deref() == Some(exported_name)
        });
    let Some(binding) = bindings.next() else {
        return false;
    };
    if bindings.next().is_some() {
        return false;
    }
    match binding.module_kind {
        ImportModuleKindV1::BareModule => {
            rust_bare_import_matches(files, binding, target_index, target)
        }
        ImportModuleKindV1::ProjectRelative => {
            let Some(imported_name) = binding.imported_name.as_deref() else {
                return false;
            };
            let Some(qualified) =
                rust_import_qualified_name(&binding.module_specifier, imported_name, scope_path)
            else {
                return false;
            };
            if rust_crate_qualified_name_matches(
                &qualified,
                root_path,
                &files[target_index].as_ref().authority.logical_path,
                &target.qualified_name,
            ) {
                return true;
            }
            let Some((module, imported_name)) = qualified.rsplit_once("::") else {
                return false;
            };
            let Some(next_scope) =
                rust_module_file_index(files, scope_path, &module.replace("::", "/"))
            else {
                return false;
            };
            rust_export_resolves_to_target(
                files,
                root_index,
                next_scope,
                imported_name,
                target_index,
                target,
                visited,
            )
        }
    }
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

fn rust_module_file_index<T>(files: &[T], source_path: &str, module: &str) -> Option<usize>
where
    T: AsRef<FileGenerationArtifactsV1>,
{
    let source_root = rust_source_root(source_path)?;
    let mut matches = files.iter().enumerate().filter_map(|(index, file)| {
        let file = file.as_ref();
        (file.extraction.language.as_str() == "rust"
            && rust_source_root(&file.authority.logical_path) == Some(source_root))
        .then_some(())?;
        let relative = file
            .authority
            .logical_path
            .strip_prefix(source_root)?
            .strip_prefix('/')?;
        (rust_file_module(relative) == Some(module)).then_some(index)
    });
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
}

fn rust_crate_root_file_index<T>(files: &[T], crate_name: &str) -> Option<usize>
where
    T: AsRef<FileGenerationArtifactsV1>,
{
    let mut matches = files.iter().enumerate().filter_map(|(index, file)| {
        let file = file.as_ref();
        let source_root = rust_source_root(&file.authority.logical_path)?;
        let relative = file
            .authority
            .logical_path
            .strip_prefix(source_root)?
            .strip_prefix('/')?;
        if relative != "lib.rs" {
            return None;
        }
        let directory = Path::new(source_root).parent()?.file_name()?.to_str()?;
        (directory.replace('-', "_") == crate_name).then_some(index)
    });
    let index = matches.next()?;
    matches.next().is_none().then_some(index)
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
    if module.is_empty() {
        reference_path == symbol_path
    } else {
        reference_path == format!("{}::{symbol_path}", module.replace('/', "::"))
    }
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
