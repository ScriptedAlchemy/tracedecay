//! Interactive code-graph projection mount for dashboard integration fixtures.
//!
//! Dashboard neighbor reads take their adjacency exclusively from the retained
//! interactive code-graph projection: in production the daemon's code-index
//! scheduler activates a published generation and hands the dashboard graph
//! read authority a resolver for the retained store. A fixture that seeds the
//! relational `nodes`/`edges` tables performs only half of that — the
//! relational half — so without this bridge every `/neighbors` read answers
//! its typed unavailable envelope ("interactive code graph reads require the
//! daemon-owned scheduler bridge") no matter what the fixture seeded.
//!
//! This module closes the fixture's half by projecting the seeded relational
//! graph into a hermetic in-memory verified generation with the same shape an
//! activated production generation has — file snapshot, symbol index with
//! qualified names and kinds, canonical relation edges — and exposing it
//! through the very resolver type the daemon injects. Adjacency still comes
//! from a verified projection; only the *provenance* of the generation is the
//! fixture rather than an extraction run.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionStore, HermeticCodeGraphProjectionStore,
};
use tracedecay_code_index::lineage::{GenerationSymbolIndexV1, LineageSymbolRecordV1};
use tracedecay_domain::{
    BoundedSanitizedText, CanonicalRelationEdgeV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkAnchorV1, CodeSearchChunkGrainV1, CodeSearchChunkV1, ContentDigest,
    EdgeAuthorityV1, EdgeKind, FileOccurrenceId, LanguageDescriptorRevision, LanguageId,
    PolicyRevisionId, RelationEdgeKindV1, SanitizedCodeFileV1, SanitizerRevision,
    SensitivityDecision, SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan,
    SymbolOccurrenceId,
};
use tracedecay_graph_db::NeverCancelled;

use crate::errors::{Result, TraceDecayError};
use crate::tracedecay::TraceDecay;
use crate::types::{Edge, Node};

/// Generation the fixture projection publishes under. It is a fixture
/// identity, never a published production generation id.
const FIXTURE_GENERATION: &str = "generation.dashboard-fixture.1";

fn fixture_error(operation: &str, error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("dashboard graph projection fixture {operation}: {error}"),
    }
}

/// Stable synthetic digest. Projection records require a digest-shaped
/// identity; the fixture derives one from the value it stands for so two
/// distinct symbols never collide.
fn fixture_digest<T>(kind: &str, value: &str) -> std::result::Result<T, String>
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Display,
{
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    let encoded = hasher
        .finalize()
        .iter()
        .fold(String::from("sha256:"), |mut encoded, byte| {
            use std::fmt::Write;
            let _ = write!(encoded, "{byte:02x}");
            encoded
        });
    T::try_from(encoded).map_err(|error| error.to_string())
}

/// Relational edge kinds the code-graph projection can carry. Kinds outside
/// this set have no canonical relation form, so the fixture drops them rather
/// than inventing one — exactly as extraction would abstain.
fn relation_kind(kind: EdgeKind) -> Option<RelationEdgeKindV1> {
    match kind {
        EdgeKind::Calls => Some(RelationEdgeKindV1::Calls),
        EdgeKind::Uses => Some(RelationEdgeKindV1::Uses),
        EdgeKind::TypeOf => Some(RelationEdgeKindV1::TypeOf),
        EdgeKind::Contains => Some(RelationEdgeKindV1::Contains),
        EdgeKind::Implements => Some(RelationEdgeKindV1::Implements),
        EdgeKind::Extends => Some(RelationEdgeKindV1::Extends),
        EdgeKind::Annotates => Some(RelationEdgeKindV1::Annotates),
        EdgeKind::Returns | EdgeKind::DerivesMacro | EdgeKind::Receives => None,
    }
}

/// File occurrence identity for one repository-relative path.
fn file_occurrence(path: &str) -> std::result::Result<FileOccurrenceId, String> {
    FileOccurrenceId::try_from(format!("file:{path}")).map_err(|error| error.to_string())
}

fn language_for(path: &str) -> Option<LanguageId> {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)?;
    let language = match extension {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "tsx",
        "js" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "go" => "go",
        _ => return None,
    };
    LanguageId::new(language).ok()
}

struct ProjectionInputs {
    files: Vec<SanitizedCodeFileV1>,
    chunks: Vec<CodeSearchChunkV1>,
    symbols: Vec<LineageSymbolRecordV1>,
    edges: Vec<CanonicalRelationEdgeV1>,
}

/// Projects the seeded relational graph into the inputs one verified code
/// graph generation is built from. The relational node id *is* the symbol
/// occurrence id, so the served id-space stays identical on both sides of the
/// projection and neighbor hydration resolves exactly.
fn project(
    generation: &CodeGenerationId,
    nodes: &[Node],
    edges: &[Edge],
) -> std::result::Result<ProjectionInputs, String> {
    let mut files: BTreeMap<FileOccurrenceId, SanitizedCodeFileV1> = BTreeMap::new();
    let mut chunks = Vec::with_capacity(nodes.len());
    let mut symbols = Vec::with_capacity(nodes.len());
    let mut known: BTreeSet<SymbolOccurrenceId> = BTreeSet::new();

    for (ordinal, node) in nodes.iter().enumerate() {
        let occurrence = SymbolOccurrenceId::try_from(node.id.clone())
            .map_err(|error| format!("node id {:?} is not an occurrence id: {error}", node.id))?;
        let file = file_occurrence(&node.file_path)?;
        files.entry(file.clone()).or_insert(SanitizedCodeFileV1 {
            file_occurrence_id: file.clone(),
            logical_path: node.file_path.clone(),
            language: language_for(&node.file_path),
            content_digest: fixture_digest::<ContentDigest>("file", &node.file_path)?,
            disposition: SnapshotFileDispositionV1::Present,
        });
        let qualified_name = if node.qualified_name.trim().is_empty() {
            node.name.clone()
        } else {
            node.qualified_name.clone()
        };
        symbols.push(LineageSymbolRecordV1 {
            occurrence: occurrence.clone(),
            identity: fixture_digest("symbol-identity", &node.id)?,
            qualified_name,
            kind: node.kind.as_str().to_owned(),
            file_identity: fixture_digest("file-identity", &node.file_path)?,
            content_digest: fixture_digest("symbol-content", &node.id)?,
        });
        chunks.push(CodeSearchChunkV1 {
            id: format!("chunk:{}", node.id)
                .try_into()
                .map_err(|error: tracedecay_domain::DomainError| error.to_string())?,
            anchor: CodeSearchChunkAnchorV1 {
                generation_id: generation.clone(),
                file_occurrence_id: file,
                symbol_occurrence_id: Some(occurrence.clone()),
                parent_chunk_id: None,
                source_span: SourceSpan {
                    start_byte: 0,
                    end_byte: 1,
                },
                grain: CodeSearchChunkGrainV1::SymbolBody,
                ordinal: u32::try_from(ordinal).map_err(|error| error.to_string())?,
            },
            content_digest: fixture_digest("chunk", &node.id)?,
            language_descriptor_revision: LanguageDescriptorRevision::new("language.fixture.v1")
                .map_err(|error| error.to_string())?,
            chunker_revision: ChunkerRevision::new("chunker.fixture.v1")
                .map_err(|error| error.to_string())?,
            sanitizer_revision: SanitizerRevision::new("sanitizer.fixture.v1")
                .map_err(|error| error.to_string())?,
            sensitivity: SensitivityDecision {
                level: SensitivityLevelV1::Public,
                policy_revision: PolicyRevisionId::new("policy.fixture.v1")
                    .map_err(|error| error.to_string())?,
            },
            exact_terms: Vec::new(),
            subtokens: Vec::new(),
            sanitized_text: BoundedSanitizedText::new("fixture")
                .map_err(|error| error.to_string())?,
        });
        known.insert(occurrence);
    }

    let mut relations = Vec::with_capacity(edges.len());
    for edge in edges {
        let Some(kind) = relation_kind(edge.kind) else {
            continue;
        };
        let from =
            SymbolOccurrenceId::try_from(edge.source.clone()).map_err(|error| error.to_string())?;
        let to =
            SymbolOccurrenceId::try_from(edge.target.clone()).map_err(|error| error.to_string())?;
        // An edge whose endpoints the node index does not carry has no
        // projected symbol to attach to; extraction abstains from the same
        // shape rather than minting a symbol for it.
        if !known.contains(&from) || !known.contains(&to) {
            continue;
        }
        let start = u64::from(edge.line.unwrap_or(0));
        relations.push(CanonicalRelationEdgeV1 {
            from_occurrence: from,
            to_occurrence: to,
            kind,
            authority: EdgeAuthorityV1::SyntaxExact,
            evidence_span: SourceSpan {
                start_byte: start,
                end_byte: start.saturating_add(1),
            },
        });
    }

    Ok(ProjectionInputs {
        files: files.into_values().collect(),
        chunks,
        symbols,
        edges: relations,
    })
}

/// Builds the retained interactive projection store over the graph a fixture
/// has already seeded, and returns the resolver the dashboard graph read
/// authority takes. Call it after every seeding step: the published
/// generation is immutable, exactly as an activated one is.
pub(crate) async fn interactive_resolver_for_test(
    cg: &TraceDecay,
) -> Result<crate::mcp::server::DashboardGraphInteractiveResolver> {
    let nodes = cg.db().get_all_nodes().await?;
    let edges = cg.db().get_all_edges().await?;
    let generation = CodeGenerationId::new(FIXTURE_GENERATION)
        .map_err(|error| fixture_error("generation identity", error))?;
    let inputs =
        project(&generation, &nodes, &edges).map_err(|error| fixture_error("projection", error))?;
    let symbols = GenerationSymbolIndexV1::new(generation.clone(), inputs.symbols)
        .map_err(|error| fixture_error("symbol index", error))?;
    let cancellation =
        tracedecay_application::CancellationSignal::active("cancellation.dashboard-graph-fixture")
            .map_err(|error| fixture_error("cancellation token", error))?;
    let hermetic = HermeticCodeGraphProjectionStore::memory(&cancellation)
        .map_err(|error| fixture_error("hermetic store", error))?;
    hermetic
        .publish_indexed_with_cancellation(
            &generation,
            &inputs.edges,
            &inputs.chunks,
            &inputs.files,
            &symbols,
            Arc::new(NeverCancelled),
        )
        .map_err(|error| fixture_error("publish", error))?;
    let store: Arc<CodeGraphProjectionStore> = Arc::new(
        hermetic
            .verified_store(&generation)
            .map_err(|error| fixture_error("verified store", error))?,
    );
    Ok(Arc::new(move |_root: std::path::PathBuf| {
        let store = Arc::clone(&store);
        Box::pin(async move { Some(store) }) as crate::mcp::server::DashboardGraphInteractiveFuture
    }))
}
