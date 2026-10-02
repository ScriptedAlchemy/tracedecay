//! A refresh's code graph as a delta over the sealed generation it replaces.
//!
//! Sealing records one content-addressed output page per snapshot file after
//! canonical resolution. A refresh compares those descriptors with the cold
//! base and reads only pages whose output changed. Exact endpoint rows for
//! relations into unchanged pages come from the graph base's point-readable
//! canonical row sidecar. Page ownership makes every hide local, including
//! placeholders shared by several source files.

use std::collections::BTreeSet;

use tracedecay_graph_db::{
    GraphDbError, GraphEntityId, GraphLayeredRowSpill, GraphProjectionIdentity,
    GraphProjectorRevision, GraphRelationId, LayeredGraphGeneration,
};

use super::builder::{edge_relation_id, emit_persisted_code_graph_page, file_symbol_relation_id};
use super::schema::{file_entity_id, file_import_relation_id_with, import_entity_id};
use super::{
    CURRENT_GENERATION_ENTITY, CodeGraphProjectionError, SealedCodeGraphRowsError,
    code_graph_generation_id, code_graph_manifest_identity, current_generation_entity, projection,
    symbol_entity_id,
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
    let changed_files = plan.changed.len();
    let child_files = plan.child_files;
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
    changed: Vec<ChangedPageV1>,
    child_files: usize,
}

struct ChangedPageV1 {
    child: Option<CodeGraphPageDescriptorV1>,
    base: Option<CodeGraphPageDescriptorV1>,
}

fn require_canonical_pages(
    pages: &[CodeGraphPageDescriptorV1],
) -> Result<(), CodeGraphProjectionError> {
    if pages
        .windows(2)
        .any(|pair| pair[0].logical_path >= pair[1].logical_path)
    {
        return Err(CodeGraphProjectionError::Contract(
            "code graph pages are not in canonical logical-path order".to_owned(),
        ));
    }
    Ok(())
}

fn changed_page_plan(
    child: &[CodeGraphPageDescriptorV1],
    base: &[CodeGraphPageDescriptorV1],
) -> Result<ChangedPagePlanV1, CodeGraphProjectionError> {
    require_canonical_pages(child)?;
    require_canonical_pages(base)?;
    let (mut child_position, mut base_position) = (0, 0);
    let mut changed = Vec::new();
    while child_position < child.len() || base_position < base.len() {
        match (child.get(child_position), base.get(base_position)) {
            (Some(child_page), Some(base_page)) => {
                match child_page.logical_path.cmp(&base_page.logical_path) {
                    std::cmp::Ordering::Less => {
                        changed.push(ChangedPageV1 {
                            child: Some(child_page.clone()),
                            base: None,
                        });
                        child_position += 1;
                    }
                    std::cmp::Ordering::Greater => {
                        changed.push(ChangedPageV1 {
                            child: None,
                            base: Some(base_page.clone()),
                        });
                        base_position += 1;
                    }
                    std::cmp::Ordering::Equal => {
                        if child_page.page_digest != base_page.page_digest {
                            changed.push(ChangedPageV1 {
                                child: Some(child_page.clone()),
                                base: Some(base_page.clone()),
                            });
                        }
                        child_position += 1;
                        base_position += 1;
                    }
                }
            }
            (Some(child_page), None) => {
                changed.push(ChangedPageV1 {
                    child: Some(child_page.clone()),
                    base: None,
                });
                child_position += 1;
            }
            (None, Some(base_page)) => {
                changed.push(ChangedPageV1 {
                    child: None,
                    base: Some(base_page.clone()),
                });
                base_position += 1;
            }
            (None, None) => break,
        }
    }
    Ok(ChangedPagePlanV1 {
        changed,
        child_files: child.len(),
    })
}

struct MaterializedPageDeltaV1 {
    base: Option<PersistedCodeGraphPageV1>,
    child: Option<PersistedCodeGraphPageV1>,
}

fn for_each_changed_page(
    plan: &ChangedPagePlanV1,
    child: &mut impl CodeGraphPageStoreV1,
    base: &mut impl CodeGraphPageStoreV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    mut visit: impl FnMut(MaterializedPageDeltaV1) -> Result<(), SealedCodeGraphRowsError>,
) -> Result<(), SealedCodeGraphRowsError> {
    for changed in &plan.changed {
        check()?;
        visit(MaterializedPageDeltaV1 {
            base: changed
                .base
                .as_ref()
                .map(|descriptor| base.read_page(descriptor))
                .transpose()?,
            child: changed
                .child
                .as_ref()
                .map(|descriptor| child.read_page(descriptor))
                .transpose()?,
        })?;
    }
    Ok(())
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
    for_each_changed_page(plan, child, base, check, |changed| {
        if let Some(page) = changed.base {
            let (entities, relations) = owned_page_rows(&page)?;
            spill.hide(entities, relations);
        }
        if let Some(page) = changed.child {
            let rows =
                emit_persisted_code_graph_page(projection_identity, generation, &page, check)?;
            spill.push_batch(rows.entities, rows.relations, check)?;
        }
        Ok(())
    })?;
    let missing_endpoints = spill.missing_endpoints();
    spill.copy_base_endpoints(missing_endpoints, check)?;
    reseal_generation_marker(spill, generation, check)?;
    let reextracted_files = plan
        .changed
        .iter()
        .filter(|page| page.child.is_some())
        .count();
    Ok(CodeGraphLayeredReportV1 {
        reextracted_files,
        reused_files: plan.child_files.saturating_sub(reextracted_files),
        removed_files: plan
            .changed
            .iter()
            .filter(|page| page.base.is_some())
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
        base_descriptors.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
        child_descriptors.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
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
        for_each_changed_page(&plan, &mut child, &mut base, &|| Ok(()), |_| Ok(()))
            .expect("materialize changed pages");
        let expected_child = plan
            .changed
            .iter()
            .filter(|page| page.child.is_some())
            .count();
        let expected_base = plan
            .changed
            .iter()
            .filter(|page| page.base.is_some())
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
