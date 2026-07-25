//! Generation-bound semantic-vector adapter for redundancy classification.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use tracedecay_domain::{CodeGenerationId, CodeSearchChunkGrainV1, EmbeddingMetricV1};

use crate::code_index::production::CodeIndexPublishedGenerationV1;
use crate::db::Database;
use crate::store::vector_generations::DatabaseVectorGenerationStoreV1;

use super::project_semantic_source_generation;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticRedundancyVectorV1 {
    pub file_path: String,
    pub qualified_name: String,
    pub values: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticRedundancyGenerationV1 {
    pub vector_generation: String,
    pub source_generation: String,
    pub projection_key: String,
    pub vectors: Vec<SemanticRedundancyVectorV1>,
}

struct RetainedProjectGenerationsV1 {
    latest: CodeGenerationId,
    generations: BTreeMap<CodeGenerationId, CodeIndexPublishedGenerationV1>,
}

fn retained_generations() -> &'static Mutex<BTreeMap<PathBuf, RetainedProjectGenerationsV1>> {
    static GENERATIONS: OnceLock<Mutex<BTreeMap<PathBuf, RetainedProjectGenerationsV1>>> =
        OnceLock::new();
    GENERATIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Retain the immutable code-generation bindings needed to interpret active
/// vector rows. The semantic schedule hook calls this before enqueueing a
/// generation; reads still admit only the atomically current vector/source
/// identity.
pub(crate) fn register_project_semantic_redundancy_generation(
    project_root: PathBuf,
    generation: CodeIndexPublishedGenerationV1,
) {
    let current = project_semantic_source_generation(&project_root);
    let incoming = generation.manifest().generation_id.clone();
    let mut retained = retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let project = retained
        .entry(project_root)
        .or_insert_with(|| RetainedProjectGenerationsV1 {
            latest: incoming.clone(),
            generations: BTreeMap::new(),
        });
    project.latest = incoming.clone();
    project
        .generations
        .retain(|source, _| current.as_ref() == Some(source) || source == &incoming);
    project.generations.insert(incoming, generation);
}

pub(crate) fn unregister_project_semantic_redundancy_generation(project_root: &Path) {
    retained_generations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(project_root);
}

/// Read only a complete active cosine generation whose source identity equals
/// the semantic runtime's atomically current pointer.
pub(crate) async fn project_semantic_redundancy_generation(
    project_root: &Path,
    database: &Database,
) -> Option<SemanticRedundancyGenerationV1> {
    let source_generation = project_semantic_source_generation(project_root)?;
    let code = {
        let retained = retained_generations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = retained.get(project_root)?;
        if project.latest != source_generation {
            return None;
        }
        project.generations.get(&source_generation).cloned()?
    };
    let vectors = DatabaseVectorGenerationStoreV1::read_active_generation(database)
        .await
        .ok()??;
    if vectors.source_generation() != &source_generation
        || vectors.embedding_key().embedding_key().metric != EmbeddingMetricV1::Cosine
    {
        return None;
    }

    let chunks = code
        .chunks()
        .chunks()
        .iter()
        .map(|chunk| (&chunk.id, chunk))
        .collect::<HashMap<_, _>>();
    let files = code
        .snapshot()
        .files
        .iter()
        .map(|file| (&file.file_occurrence_id, file.logical_path.as_str()))
        .collect::<HashMap<_, _>>();
    let symbols = code
        .symbols()
        .symbols
        .iter()
        .map(|symbol| (&symbol.occurrence, symbol.qualified_name.as_str()))
        .collect::<HashMap<_, _>>();
    let mut admitted = Vec::new();
    for (chunk_id, vector) in vectors.vectors() {
        let chunk = chunks.get(chunk_id)?;
        if &vector.source_generation != &source_generation
            || &vector.projection_key != vectors.projection_key()
            || &vector.chunk_digest != &chunk.content_digest
        {
            return None;
        }
        if !matches!(
            chunk.anchor.grain,
            CodeSearchChunkGrainV1::SymbolBody | CodeSearchChunkGrainV1::SymbolMember
        ) {
            continue;
        }
        let symbol = chunk.anchor.symbol_occurrence_id.as_ref()?;
        admitted.push(SemanticRedundancyVectorV1 {
            file_path: (*files.get(&chunk.anchor.file_occurrence_id)?).to_owned(),
            qualified_name: (*symbols.get(symbol)?).to_owned(),
            values: vector.values.clone(),
        });
    }
    Some(SemanticRedundancyGenerationV1 {
        vector_generation: vectors.generation_id().as_digest().as_str().to_owned(),
        source_generation: source_generation.as_str().to_owned(),
        projection_key: vectors.projection_key().profile_digest.as_str().to_owned(),
        vectors: admitted,
    })
}
