//! A refresh's code graph as a delta over the sealed generation it replaces.
//!
//! Sealing records one content-addressed output page per snapshot file after
//! canonical resolution. A refresh compares those descriptors with the cold
//! base, reads only pages whose output changed, and reads an unchanged target
//! page only when a new relation needs its endpoint row. Page ownership makes
//! every hide local, including placeholders shared by several source files.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use tracedecay_domain::{FileOccurrenceId, SymbolOccurrenceId};
use tracedecay_graph_db::{
    GraphDbError, GraphEntityId, GraphLayeredRowSpill, GraphProjectionIdentity,
    GraphProjectorRevision, GraphRelationId, LayeredGraphGeneration,
};

use super::builder::{
    CodeGraphRowBatch, CodeGraphRowContext, edge_relation_id, emit_code_graph_rows,
    emit_persisted_code_graph_page, file_symbol_relation_id, group_unresolved_calls,
};
use super::schema::{file_entity_id, file_import_relation_id_with, import_entity_id};
use super::{
    CURRENT_GENERATION_ENTITY, CodeGraphProjectionError, SealedCodeGraphRowsError, SymbolRecordV1,
    code_graph_generation_id, code_graph_manifest_identity, current_generation_entity, projection,
    symbol_entity, symbol_entity_id,
};
use crate::production::{
    CodeGraphPageDescriptorV1, CodeGraphPageStoreV1, FileCodeGraphPageStoreV1,
    PersistedCodeGraphPageV1, SealedCodeGraphPageStoreV1, SealedGenerationFileWindowsV1,
    SealedGenerationSegmentReaderV1,
};

/// A refresh whose files changed since its base exceed this share, one in
/// this many, seals cold and becomes the next base.
const LAYERED_MAX_CHANGED_FILE_SHARE_DENOMINATOR: usize = 8;

/// What a layered build did, beside the rows it sealed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CodeGraphLayeredReportV1 {
    /// File segments decoded because the base did not carry them.
    pub reextracted_files: usize,
    /// Child files whose resolution inputs came from the base.
    pub reused_files: usize,
    /// Base files the child dropped or changed.
    pub removed_files: usize,
    /// Retained references cross-file resolution re-decided.
    pub resolved_references: usize,
    /// `(entities, relations)` the delta container encodes.
    pub delta_rows: (usize, usize),
}

/// Why a refresh declined to layer over its base; it seals cold instead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodeGraphLayeredDeclineV1 {
    /// The base's inputs were recorded for another projector or revision.
    BaseInputsRevision,
    /// Files changed since the base exceed the share a delta may carry.
    ChangedFileShare { changed: usize, files: usize },
}

/// A layered graph generation and how it was built.
pub struct CodeGraphLayeredBuildV1 {
    pub generation: LayeredGraphGeneration,
    pub report: CodeGraphLayeredReportV1,
}

/// Builds a sealed code generation's graph as a delta over `spill`'s base.
///
/// A decline when the base carries no resolution inputs this projector can
/// read, or the refresh changed too much of it to stay a delta; a cold build
/// answers both.
#[hotpath::measure(label = "code_index.graph.build_layered_rows")]
pub fn build_layered_code_graph_rows(
    projection_identity: GraphProjectionIdentity,
    source: &SealedGenerationFileWindowsV1,
    read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
    projector_revision: &GraphProjectorRevision,
    mut spill: GraphLayeredRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Result<CodeGraphLayeredBuildV1, CodeGraphLayeredDeclineV1>, SealedCodeGraphRowsError> {
    check()?;
    if projection_identity.projection != projection()? {
        return Err(CodeGraphProjectionError::Contract(
            "code graph projection identity uses a foreign projector".to_owned(),
        )
        .into());
    }
    let generation = source.generation_id().clone();
    generation
        .validate()
        .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
    let Some(mut base) =
        FileCodeGraphPageStoreV1::open(&spill.base_attachment(), projector_revision.as_str())?
    else {
        return Ok(Err(CodeGraphLayeredDeclineV1::BaseInputsRevision));
    };
    if code_graph_generation_id(base.generation(), projector_revision)?
        != *spill.base().generation()
    {
        return Err(CodeGraphProjectionError::Contract(
            "layered base inputs belong to a different graph generation".to_owned(),
        )
        .into());
    }
    let mut child = SealedCodeGraphPageStoreV1::new(source, read_segment);
    let plan = changed_page_plan(child.pages(), base.pages())?;
    // Every layered generation is a delta since the same cold base, so the
    // delta grows with each refresh until a cold build replaces the base.
    let changed_files = plan.changed_paths.len();
    let child_files = child.pages().len();
    if changed_files.saturating_mul(LAYERED_MAX_CHANGED_FILE_SHARE_DENOMINATOR) > child_files {
        return Ok(Err(CodeGraphLayeredDeclineV1::ChangedFileShare {
            changed: changed_files,
            files: child_files,
        }));
    }
    let report = hotpath::measure_block!(
        "code_index.graph.build_layered_rows.emit",
        emit_page_delta(
            &projection_identity,
            &generation,
            &plan,
            &mut child,
            &mut base,
            &mut spill,
            check,
        )
    )?;
    let identity =
        code_graph_manifest_identity(projection_identity, &generation, projector_revision)?;
    let generation = hotpath::measure_block!(
        "code_index.graph.build_layered_rows.finish",
        spill.finish(identity, check)
    )?;
    let report = CodeGraphLayeredReportV1 {
        delta_rows: generation.delta_row_counts(),
        ..report
    };
    #[cfg(feature = "hotpath")]
    {
        hotpath::gauge!("code_index.graph.layered.delta_entities").inc(report.delta_rows.0 as u64);
        hotpath::gauge!("code_index.graph.layered.delta_relations").inc(report.delta_rows.1 as u64);
        hotpath::gauge!("code_index.graph.layered.files_reused").inc(report.reused_files as u64);
    }
    Ok(Ok(CodeGraphLayeredBuildV1 { generation, report }))
}

struct ChangedPagePlanV1 {
    child: BTreeMap<String, CodeGraphPageDescriptorV1>,
    base: BTreeMap<String, CodeGraphPageDescriptorV1>,
    changed_paths: BTreeSet<String>,
}

fn pages_by_path(
    pages: &[CodeGraphPageDescriptorV1],
) -> Result<BTreeMap<String, CodeGraphPageDescriptorV1>, CodeGraphProjectionError> {
    let mut by_path = BTreeMap::new();
    for page in pages {
        if by_path
            .insert(page.logical_path.clone(), page.clone())
            .is_some()
        {
            return Err(CodeGraphProjectionError::Contract(
                "code graph page store repeats a logical path".to_owned(),
            ));
        }
    }
    Ok(by_path)
}

fn changed_page_plan(
    child: &[CodeGraphPageDescriptorV1],
    base: &[CodeGraphPageDescriptorV1],
) -> Result<ChangedPagePlanV1, CodeGraphProjectionError> {
    let child = pages_by_path(child)?;
    let base = pages_by_path(base)?;
    let changed_paths = child
        .keys()
        .chain(base.keys())
        .filter(|path| {
            child.get(*path).map(|page| &page.page_digest)
                != base.get(*path).map(|page| &page.page_digest)
        })
        .cloned()
        .collect();
    Ok(ChangedPagePlanV1 {
        child,
        base,
        changed_paths,
    })
}

struct MaterializedPageDeltaV1 {
    base: Option<PersistedCodeGraphPageV1>,
    child: Option<PersistedCodeGraphPageV1>,
}

fn materialize_changed_pages(
    plan: &ChangedPagePlanV1,
    child: &mut impl CodeGraphPageStoreV1,
    base: &mut impl CodeGraphPageStoreV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<MaterializedPageDeltaV1>, SealedCodeGraphRowsError> {
    let mut pages = Vec::with_capacity(plan.changed_paths.len());
    for path in &plan.changed_paths {
        check()?;
        pages.push(MaterializedPageDeltaV1 {
            base: plan
                .base
                .get(path)
                .map(|descriptor| base.read_page(descriptor))
                .transpose()?,
            child: plan
                .child
                .get(path)
                .map(|descriptor| child.read_page(descriptor))
                .transpose()?,
        });
    }
    Ok(pages)
}

fn emit_page_delta(
    projection_identity: &GraphProjectionIdentity,
    generation: &tracedecay_domain::CodeGenerationId,
    plan: &ChangedPagePlanV1,
    child: &mut impl CodeGraphPageStoreV1,
    base: &mut impl CodeGraphPageStoreV1,
    spill: &mut GraphLayeredRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<CodeGraphLayeredReportV1, SealedCodeGraphRowsError> {
    let mut child_pages = Vec::new();
    for changed in materialize_changed_pages(plan, child, base, check)? {
        if let Some(page) = changed.base {
            let (entities, relations) = owned_page_rows(&page)?;
            spill.hide(entities, relations);
        }
        if let Some(page) = changed.child {
            let rows =
                emit_persisted_code_graph_page(projection_identity, generation, &page, check)?;
            spill.push_batch(rows.entities, rows.relations, check)?;
            child_pages.push(page);
        }
    }
    emit_page_endpoint_stubs(
        projection_identity,
        generation,
        plan,
        child,
        spill,
        &child_pages,
        check,
    )?;
    reseal_generation_marker(spill, generation, check)?;
    Ok(CodeGraphLayeredReportV1 {
        reextracted_files: plan
            .changed_paths
            .iter()
            .filter(|path| plan.child.contains_key(*path))
            .count(),
        reused_files: plan.child.len().saturating_sub(
            plan.changed_paths
                .iter()
                .filter(|path| plan.child.contains_key(*path))
                .count(),
        ),
        removed_files: plan
            .changed_paths
            .iter()
            .filter(|path| plan.base.contains_key(*path))
            .count(),
        resolved_references: 0,
        delta_rows: (0, 0),
    })
}

fn owned_page_rows(
    page: &PersistedCodeGraphPageV1,
) -> Result<(Vec<GraphEntityId>, Vec<GraphRelationId>), CodeGraphProjectionError> {
    let mut entities = vec![file_entity_id(&page.file.file_occurrence_id)?];
    let mut relations = Vec::new();
    for import in &page.imports {
        let identity = import_entity_id(import)?;
        relations.push(file_import_relation_id_with(import, &identity)?);
        entities.push(identity);
    }
    for occurrence in page
        .bindings
        .keys()
        .chain(page.symbols.iter().map(|symbol| &symbol.occurrence))
        .chain(&page.owned_placeholders)
        .collect::<BTreeSet<_>>()
    {
        entities.push(symbol_entity_id(occurrence)?);
    }
    for (occurrence, binding) in &page.bindings {
        relations.push(file_symbol_relation_id(binding, occurrence)?);
    }
    for edge in &page.edges {
        relations.push(edge_relation_id(edge)?);
    }
    Ok((entities, relations))
}

fn emit_page_endpoint_stubs(
    projection_identity: &GraphProjectionIdentity,
    generation: &tracedecay_domain::CodeGenerationId,
    plan: &ChangedPagePlanV1,
    child: &mut impl CodeGraphPageStoreV1,
    spill: &mut GraphLayeredRowSpill,
    changed_pages: &[PersistedCodeGraphPageV1],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), SealedCodeGraphRowsError> {
    let missing = spill
        .missing_endpoints()
        .into_iter()
        .collect::<BTreeSet<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    let mut wanted_by_file = BTreeMap::<FileOccurrenceId, BTreeSet<SymbolOccurrenceId>>::new();
    for page in changed_pages {
        for edge in &page.edges {
            check()?;
            for occurrence in [&edge.from_occurrence, &edge.to_occurrence] {
                if !missing.contains(&symbol_entity_id(occurrence)?) {
                    continue;
                }
                let owner = page.target_files.get(occurrence).ok_or_else(|| {
                    CodeGraphProjectionError::Contract(
                        "changed code graph relation has no endpoint page".to_owned(),
                    )
                })?;
                wanted_by_file
                    .entry(owner.clone())
                    .or_default()
                    .insert(occurrence.clone());
            }
        }
    }
    let child_by_occurrence = plan
        .child
        .values()
        .map(|page| (page.file_occurrence_id.clone(), page))
        .collect::<BTreeMap<_, _>>();
    for (owner, wanted) in wanted_by_file {
        let descriptor = child_by_occurrence.get(&owner).ok_or_else(|| {
            CodeGraphProjectionError::Contract(
                "code graph relation endpoint page is outside the child".to_owned(),
            )
        })?;
        let page = child.read_page(descriptor)?;
        let files = BTreeMap::from([(&page.file.file_occurrence_id, &page.file)]);
        let bound = page
            .bindings
            .keys()
            .chain(page.symbols.iter().map(|symbol| &symbol.occurrence))
            .cloned()
            .collect::<HashSet<_>>();
        let unresolved_by_source = group_unresolved_calls(&page.unresolved_calls, check)?;
        let symbols = page
            .symbols
            .iter()
            .filter(|symbol| wanted.contains(&symbol.occurrence))
            .cloned()
            .collect::<Vec<_>>();
        let bindings = page
            .bindings
            .iter()
            .filter(|(occurrence, _)| wanted.contains(*occurrence))
            .map(|(occurrence, binding)| (occurrence.clone(), binding.clone()))
            .collect::<BTreeMap<_, _>>();
        let rows = emit_code_graph_rows(
            &CodeGraphRowContext {
                projection: projection_identity,
                generation,
                files: Some(&files),
                bound: &bound,
                unresolved_by_source: &unresolved_by_source,
            },
            &CodeGraphRowBatch {
                files: &[],
                imports: &[],
                chunks: &[],
                symbols: &symbols,
                edges: &[],
                bindings: Some(&bindings),
            },
            check,
        )?;
        let mut entities = rows.entities;
        for occurrence in wanted
            .iter()
            .filter(|occurrence| page.owned_placeholders.contains(*occurrence))
        {
            entities.push(symbol_entity(
                symbol_entity_id(occurrence)?,
                SymbolRecordV1 {
                    occurrence: occurrence.clone(),
                    binding: None,
                    metadata: None,
                    unresolved_calls: unresolved_by_source
                        .get(occurrence)
                        .cloned()
                        .unwrap_or_default(),
                },
            )?);
        }
        spill.push_batch(entities, Vec::new(), check)?;
    }
    Ok(())
}

/// Replaces the generation marker, which counts every entity, itself
/// included.
fn reseal_generation_marker(
    spill: &mut GraphLayeredRowSpill,
    generation: &tracedecay_domain::CodeGenerationId,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), SealedCodeGraphRowsError> {
    spill.hide([GraphEntityId::new(CURRENT_GENERATION_ENTITY)?], Vec::new());
    let projection_node_count = spill.entity_count(check)?.checked_add(1).ok_or_else(|| {
        CodeGraphProjectionError::Contract("code graph projection node count overflowed".to_owned())
    })?;
    spill.push_batch(
        vec![current_generation_entity(
            generation,
            projection_node_count,
        )?],
        Vec::new(),
        check,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use tracedecay_domain::{
        CodeGenerationId, ContentDigest, FileOccurrenceId, LanguageId, ManifestDigest,
        SanitizedCodeFileV1, SnapshotFileDispositionV1,
    };

    use super::*;
    use crate::production::CodeIndexProductionErrorV1;

    struct MemoryPageStore {
        generation: CodeGenerationId,
        descriptors: Vec<CodeGraphPageDescriptorV1>,
        pages: BTreeMap<String, PersistedCodeGraphPageV1>,
    }

    impl CodeGraphPageStoreV1 for MemoryPageStore {
        fn generation(&self) -> &CodeGenerationId {
            &self.generation
        }

        fn pages(&self) -> &[CodeGraphPageDescriptorV1] {
            &self.descriptors
        }

        fn read_page(
            &mut self,
            descriptor: &CodeGraphPageDescriptorV1,
        ) -> Result<PersistedCodeGraphPageV1, CodeIndexProductionErrorV1> {
            self.pages
                .get(&descriptor.logical_path)
                .cloned()
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "counting graph page store has no requested page".to_owned(),
                    )
                })
        }
    }

    struct CountingPageStore {
        inner: MemoryPageStore,
        readable: BTreeSet<String>,
        reads: Vec<String>,
    }

    impl CodeGraphPageStoreV1 for CountingPageStore {
        fn generation(&self) -> &CodeGenerationId {
            self.inner.generation()
        }

        fn pages(&self) -> &[CodeGraphPageDescriptorV1] {
            self.inner.pages()
        }

        fn read_page(
            &mut self,
            descriptor: &CodeGraphPageDescriptorV1,
        ) -> Result<PersistedCodeGraphPageV1, CodeIndexProductionErrorV1> {
            if !self.readable.contains(&descriptor.logical_path) {
                return Err(CodeIndexProductionErrorV1::Contract(format!(
                    "layered refresh touched unchanged page {}",
                    descriptor.logical_path
                )));
            }
            self.reads.push(descriptor.logical_path.clone());
            self.inner.read_page(descriptor)
        }
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
            .expect("fixture digest")
    }

    fn page(
        path: &str,
        ordinal: usize,
        page_digest: ManifestDigest,
    ) -> (CodeGraphPageDescriptorV1, PersistedCodeGraphPageV1) {
        let occurrence =
            FileOccurrenceId::new(format!("file.fixture.{ordinal}")).expect("file occurrence");
        let descriptor = CodeGraphPageDescriptorV1 {
            file_key: u32::try_from(ordinal).expect("fixture file key"),
            file_occurrence_id: occurrence.clone(),
            logical_path: path.to_owned(),
            page_digest,
            size_bytes: 1,
        };
        let page = PersistedCodeGraphPageV1 {
            file: SanitizedCodeFileV1 {
                file_occurrence_id: occurrence,
                logical_path: path.to_owned(),
                language: Some(LanguageId::new("rust").expect("language")),
                content_digest: ContentDigest::new(format!("sha256:{}", "a".repeat(64)))
                    .expect("content digest"),
                disposition: SnapshotFileDispositionV1::Present,
            },
            imports: Vec::new(),
            symbols: Vec::new(),
            edges: Vec::new(),
            bindings: BTreeMap::new(),
            unresolved_calls: Vec::new(),
            target_files: BTreeMap::new(),
            placeholder_targets: BTreeSet::new(),
            owned_placeholders: BTreeSet::new(),
        };
        (descriptor, page)
    }

    fn stores(unrelated: usize) -> (CountingPageStore, CountingPageStore) {
        let mut base_descriptors = Vec::new();
        let mut child_descriptors = Vec::new();
        let mut base_pages = BTreeMap::new();
        let mut child_pages = BTreeMap::new();
        let changed_path = "src/changed.rs";
        let (base_changed, base_page) = page(changed_path, 0, digest('b'));
        let (child_changed, child_page) = page(changed_path, 0, digest('c'));
        base_descriptors.push(base_changed);
        child_descriptors.push(child_changed);
        base_pages.insert(changed_path.to_owned(), base_page);
        child_pages.insert(changed_path.to_owned(), child_page);
        for ordinal in 1..=unrelated {
            let path = format!("src/unrelated-{ordinal}.rs");
            let (descriptor, stored) = page(&path, ordinal, digest('d'));
            base_descriptors.push(descriptor.clone());
            child_descriptors.push(descriptor);
            base_pages.insert(path.clone(), stored.clone());
            child_pages.insert(path, stored);
        }
        let generation =
            CodeGenerationId::new("generation.v1.fixture.00000001.aaaaaaaa").expect("generation");
        let readable = BTreeSet::from([changed_path.to_owned()]);
        (
            CountingPageStore {
                inner: MemoryPageStore {
                    generation: generation.clone(),
                    descriptors: child_descriptors,
                    pages: child_pages,
                },
                readable: readable.clone(),
                reads: Vec::new(),
            },
            CountingPageStore {
                inner: MemoryPageStore {
                    generation,
                    descriptors: base_descriptors,
                    pages: base_pages,
                },
                readable,
                reads: Vec::new(),
            },
        )
    }

    fn changed_reads(unrelated: usize) -> (Vec<String>, Vec<String>) {
        let (mut child, mut base) = stores(unrelated);
        let plan = changed_page_plan(child.pages(), base.pages()).expect("changed page plan");
        materialize_changed_pages(&plan, &mut child, &mut base, &|| Ok(()))
            .expect("materialize changed pages");
        let expected_child = plan
            .changed_paths
            .iter()
            .filter(|path| plan.child.contains_key(*path))
            .count();
        let expected_base = plan
            .changed_paths
            .iter()
            .filter(|path| plan.base.contains_key(*path))
            .count();
        assert_eq!(child.reads.len(), expected_child);
        assert_eq!(base.reads.len(), expected_base);
        (child.reads, base.reads)
    }

    #[test]
    fn layered_refresh_materializes_only_changed_output_pages() {
        let small = changed_reads(4);
        let with_more_unrelated_pages = changed_reads(40);
        assert_eq!(small, with_more_unrelated_pages);
    }
}
