//! Durable codec of the interactive catalog as a sealed read bundle artifact.
//!
//! Encoding happens at seal, from the manifest rows the seal already holds
//! ([`super::catalog::build_interactive_catalog_from_manifest`]). Decoding
//! happens at open, after the bundle envelope has verified the artifact's
//! content digest and its generation-identity binding, so the checks here are
//! structural defense in depth — a corrupt row is a typed `Corrupt`, never a
//! partially installed catalog.

use std::io::Write;

use serde::{Deserialize, Serialize};
use tracedecay_domain::{SanitizedCodeFileV1, SymbolOccurrenceId};
use tracedecay_graph_db::{GraphCancellation, GraphGenerationManifest};

use super::super::schema::{SYMBOL_LABEL, SYMBOL_RECORD_PROPERTY, deserialize_property, has_label};
use super::super::{CodeGraphProjectionError, CodeGraphSymbolBindingV1, validate_symbol_record};
use super::catalog::{build_interactive_catalog_from_manifest, check_cancelled};
use super::models::{CatalogSymbol, InteractiveCatalog};
use crate::chunks::CodeIndexImportEvidenceV1;
use crate::lineage::LineageSymbolRecordV1;

/// Bundle artifact name of the interactive catalog.
pub const INTERACTIVE_CATALOG_ARTIFACT_NAME: &str = "interactive-catalog";

const INTERACTIVE_CATALOG_ARTIFACT_FORMAT_V1: &str = "tracedecay.code-graph-interactive-catalog.v1";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogSymbolRowV1 {
    occurrence: SymbolOccurrenceId,
    binding: Option<CodeGraphSymbolBindingV1>,
    metadata: Option<LineageSymbolRecordV1>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InteractiveCatalogArtifactV1 {
    format: String,
    /// The graph generation this catalog was derived from, for a cheap
    /// self-description check on top of the envelope's identity binding.
    graph_generation: String,
    symbols: Vec<CatalogSymbolRowV1>,
    files: Vec<SanitizedCodeFileV1>,
    imports: Vec<CodeIndexImportEvidenceV1>,
}

/// Derives the interactive catalog from the sealed generation's manifest rows
/// and streams the catalog artifact into `out`. This is the seal-time half of
/// catalog-at-seal: one linear pass over rows already in RAM, instead of the
/// paged projection re-scan the open-time warm performs.
#[hotpath::measure]
pub fn write_interactive_catalog_artifact(
    manifest: &GraphGenerationManifest,
    out: &mut dyn Write,
    cancellation: &dyn GraphCancellation,
) -> Result<(), CodeGraphProjectionError> {
    let catalog = hotpath::measure_block!(
        "code_graph.catalog.seal_derive",
        build_interactive_catalog_from_manifest(manifest, cancellation)
    )?;
    check_cancelled(cancellation)?;
    // Symbol rows are emitted in entity-scan order, not occurrence order:
    // the catalog's per-name and per-file vectors preserve insertion order,
    // so the decoded catalog reproduces the warm scan's exact ordering only
    // if the rows replay in the same order the scan saw them.
    let mut symbols = Vec::with_capacity(catalog.symbols.len());
    for entity in &manifest.entities {
        check_cancelled(cancellation)?;
        if !has_label(entity, SYMBOL_LABEL) {
            continue;
        }
        let record: super::super::SymbolRecordV1 =
            deserialize_property(entity, SYMBOL_RECORD_PROPERTY)?;
        symbols.push(CatalogSymbolRowV1 {
            occurrence: record.occurrence,
            binding: record.binding,
            metadata: record.metadata,
        });
    }
    if symbols.len() != catalog.symbols.len() {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph catalog artifact symbol rows diverged from the derived catalog".to_owned(),
        ));
    }
    let artifact = InteractiveCatalogArtifactV1 {
        format: INTERACTIVE_CATALOG_ARTIFACT_FORMAT_V1.to_owned(),
        graph_generation: manifest.generation.as_str().to_owned(),
        symbols,
        files: catalog.files.values().cloned().collect(),
        imports: catalog.imports.clone(),
    };
    serde_json::to_writer(out, &artifact).map_err(|error| {
        CodeGraphProjectionError::Unavailable(format!(
            "failed to encode code graph interactive catalog artifact: {error}"
        ))
    })
}

/// Decodes a digest-verified catalog artifact back into the in-memory
/// catalog, revalidating structural invariants row by row.
#[hotpath::measure]
pub(super) fn decode_interactive_catalog_artifact(
    bytes: &[u8],
    expected_graph_generation: &str,
    cancellation: &dyn GraphCancellation,
) -> Result<InteractiveCatalog, CodeGraphProjectionError> {
    let artifact: InteractiveCatalogArtifactV1 =
        serde_json::from_slice(bytes).map_err(|error| {
            CodeGraphProjectionError::Corrupt(format!(
                "code graph interactive catalog artifact is corrupt: {error}"
            ))
        })?;
    if artifact.format != INTERACTIVE_CATALOG_ARTIFACT_FORMAT_V1 {
        return Err(CodeGraphProjectionError::Corrupt(format!(
            "code graph interactive catalog artifact format `{}` is not `{INTERACTIVE_CATALOG_ARTIFACT_FORMAT_V1}`",
            artifact.format
        )));
    }
    if artifact.graph_generation != expected_graph_generation {
        return Err(CodeGraphProjectionError::GenerationMismatch);
    }
    let mut catalog = InteractiveCatalog::empty();
    for file in artifact.files {
        check_cancelled(cancellation)?;
        file.validate()
            .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))?;
        let previous = catalog
            .by_logical_path
            .insert(file.logical_path.clone(), file.file_occurrence_id.clone());
        if previous.is_some_and(|existing| existing != file.file_occurrence_id) {
            return Err(CodeGraphProjectionError::Corrupt(format!(
                "code graph catalog artifact logical path `{}` is claimed by more than one file occurrence",
                file.logical_path
            )));
        }
        if catalog
            .files
            .insert(file.file_occurrence_id.clone(), file)
            .is_some()
        {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph catalog artifact contains a duplicate file row".to_owned(),
            ));
        }
    }
    for row in artifact.symbols {
        check_cancelled(cancellation)?;
        let record = super::super::SymbolRecordV1 {
            occurrence: row.occurrence,
            binding: row.binding,
            metadata: row.metadata,
        };
        validate_symbol_record(&record)?;
        if catalog.symbols.contains_key(&record.occurrence) {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph catalog artifact contains a duplicate symbol row".to_owned(),
            ));
        }
        catalog.insert(
            record.occurrence,
            CatalogSymbol {
                binding: record.binding,
                metadata: record.metadata,
            },
        );
    }
    let mut imports = artifact.imports;
    for import in &imports {
        check_cancelled(cancellation)?;
        import.validate().map_err(|error| {
            CodeGraphProjectionError::Corrupt(format!(
                "code graph catalog artifact import row is not canonical: {error}"
            ))
        })?;
        let file = catalog
            .files
            .get(&import.file_occurrence_id)
            .ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph catalog artifact import refers to a missing file occurrence"
                        .to_owned(),
                )
            })?;
        if file.logical_path != import.logical_path {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph catalog artifact import logical path does not match its file"
                    .to_owned(),
            ));
        }
    }
    imports.sort_by(super::catalog::canonical_import_order);
    catalog.imports = imports;
    Ok(catalog)
}
