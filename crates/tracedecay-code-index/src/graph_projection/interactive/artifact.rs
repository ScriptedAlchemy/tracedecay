//! Durable codec of the interactive catalog as a sealed read bundle artifact.
//!
//! Encoding happens at seal directly from the manifest rows the seal already
//! holds. Decoding happens at open, after the bundle envelope has verified the
//! artifact's content digest and its generation-identity binding, so the
//! checks here are structural defense in depth — a corrupt row is a typed
//! `Corrupt`, never a partially installed catalog.

use std::collections::BTreeMap;
use std::io::Write;

use serde::ser::{Error as _, SerializeSeq};
use serde::{Deserialize, Serialize, Serializer};
use tracedecay_domain::{FileOccurrenceId, SanitizedCodeFileV1, SymbolOccurrenceId};
use tracedecay_graph_db::{
    GraphCancellation, GraphGenerationManifest, MAX_VERIFIED_GENERATION_RELATIONS,
};

use super::super::schema::{
    FILE_IMPORT_EDGE_KIND, FILE_LABEL, FILE_RECORD_PROPERTY, IMPORT_LABEL, IMPORT_RECORD_PROPERTY,
    SYMBOL_LABEL, SYMBOL_RECORD_PROPERTY, deserialize_property, file_entity_id,
    file_import_relation_id, has_label, import_entity_id,
};
use super::super::{CodeGraphProjectionError, CodeGraphSymbolBindingV1, validate_symbol_record};
use super::catalog::{canonical_import_order, check_cancelled};
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

struct ValidatedCatalogArtifactRowsV1 {
    symbol_count: usize,
    files: Vec<SanitizedCodeFileV1>,
    imports: Vec<CodeIndexImportEvidenceV1>,
}

struct CatalogSymbolRowsV1<'a> {
    manifest: &'a GraphGenerationManifest,
    cancellation: &'a dyn GraphCancellation,
    count: usize,
}

impl Serialize for CatalogSymbolRowsV1<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.count))?;
        for entity in &self.manifest.entities {
            if !has_label(entity, SYMBOL_LABEL) {
                continue;
            }
            if self.cancellation.is_cancelled() {
                return Err(S::Error::custom("interactive catalog artifact cancelled"));
            }
            let record: super::super::SymbolRecordV1 =
                deserialize_property(entity, SYMBOL_RECORD_PROPERTY).map_err(S::Error::custom)?;
            sequence.serialize_element(&CatalogSymbolRowV1 {
                occurrence: record.occurrence,
                binding: record.binding,
                metadata: record.metadata,
            })?;
        }
        sequence.end()
    }
}

struct CancellableRowsV1<'a, T> {
    rows: &'a [T],
    cancellation: &'a dyn GraphCancellation,
}

impl<T> Serialize for CancellableRowsV1<'_, T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.rows.len()))?;
        for row in self.rows {
            if self.cancellation.is_cancelled() {
                return Err(S::Error::custom("interactive catalog artifact cancelled"));
            }
            sequence.serialize_element(row)?;
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct InteractiveCatalogArtifactViewV1<'a> {
    format: &'static str,
    graph_generation: &'a str,
    symbols: CatalogSymbolRowsV1<'a>,
    files: CancellableRowsV1<'a, SanitizedCodeFileV1>,
    imports: CancellableRowsV1<'a, CodeIndexImportEvidenceV1>,
}

fn validate_catalog_artifact_rows(
    manifest: &GraphGenerationManifest,
    cancellation: &dyn GraphCancellation,
) -> Result<ValidatedCatalogArtifactRowsV1, CodeGraphProjectionError> {
    let mut symbol_count = 0_usize;
    let mut files = BTreeMap::<FileOccurrenceId, SanitizedCodeFileV1>::new();
    let mut files_by_logical_path = BTreeMap::<String, FileOccurrenceId>::new();
    let mut imports = Vec::new();
    let mut expected_import_links = BTreeMap::new();

    for entity in &manifest.entities {
        check_cancelled(cancellation)?;
        if has_label(entity, FILE_LABEL) {
            let record: SanitizedCodeFileV1 = deserialize_property(entity, FILE_RECORD_PROPERTY)?;
            record
                .validate()
                .map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))?;
            if file_entity_id(&record.file_occurrence_id)? != entity.identity {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph file identity does not match its payload".to_owned(),
                ));
            }
            let previous = files_by_logical_path.insert(
                record.logical_path.clone(),
                record.file_occurrence_id.clone(),
            );
            if previous.is_some_and(|existing| existing != record.file_occurrence_id) {
                return Err(CodeGraphProjectionError::Corrupt(format!(
                    "code graph logical path `{}` is claimed by more than one file occurrence",
                    record.logical_path
                )));
            }
            if files
                .insert(record.file_occurrence_id.clone(), record)
                .is_some()
            {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph contains a duplicate file entity".to_owned(),
                ));
            }
        }
        if has_label(entity, SYMBOL_LABEL) {
            let record: super::super::SymbolRecordV1 =
                deserialize_property(entity, SYMBOL_RECORD_PROPERTY)?;
            validate_symbol_record(&record)?;
            if super::super::symbol_entity_id(&record.occurrence)? != entity.identity {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph symbol identity does not match its payload".to_owned(),
                ));
            }
            symbol_count = symbol_count.checked_add(1).ok_or_else(|| {
                CodeGraphProjectionError::Corrupt(
                    "code graph interactive symbol count overflowed".to_owned(),
                )
            })?;
        }
        if has_label(entity, IMPORT_LABEL) {
            let record: CodeIndexImportEvidenceV1 =
                deserialize_property(entity, IMPORT_RECORD_PROPERTY)?;
            record.validate().map_err(|error| {
                CodeGraphProjectionError::Corrupt(format!(
                    "code graph import row is not canonical: {error}"
                ))
            })?;
            if import_entity_id(&record)? != entity.identity {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph import identity does not match its payload".to_owned(),
                ));
            }
            let expected = (
                file_import_relation_id(&record)?,
                file_entity_id(&record.file_occurrence_id)?,
            );
            if expected_import_links
                .insert(entity.identity.clone(), expected)
                .is_some()
            {
                return Err(CodeGraphProjectionError::Corrupt(
                    "code graph contains a duplicate import entity".to_owned(),
                ));
            }
            imports.push(record);
        }
    }
    for import in &imports {
        let file = files.get(&import.file_occurrence_id).ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(
                "code graph import refers to a missing file occurrence".to_owned(),
            )
        })?;
        if file.logical_path != import.logical_path {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph import logical path does not match its file occurrence".to_owned(),
            ));
        }
    }

    let mut scanned_relations = 0_usize;
    for relation in &manifest.relations {
        check_cancelled(cancellation)?;
        scanned_relations = scanned_relations.checked_add(1).ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(
                "code graph interactive relation scan overflowed".to_owned(),
            )
        })?;
        if scanned_relations > MAX_VERIFIED_GENERATION_RELATIONS {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph interactive scan exceeded the verified relation ceiling".to_owned(),
            ));
        }
        if relation.kind.as_str() != FILE_IMPORT_EDGE_KIND {
            continue;
        }
        let Some((expected_identity, expected_file)) =
            expected_import_links.remove(&relation.to.identity)
        else {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph file-import relation targets a non-import entity".to_owned(),
            ));
        };
        if relation.from.identity != expected_file
            || relation.identity != expected_identity
            || !relation.properties.is_empty()
        {
            return Err(CodeGraphProjectionError::Corrupt(
                "code graph import file link is not canonical".to_owned(),
            ));
        }
    }
    if !expected_import_links.is_empty() {
        return Err(CodeGraphProjectionError::Corrupt(
            "code graph import entities do not have exact file-link coverage".to_owned(),
        ));
    }
    imports.sort_by(canonical_import_order);
    Ok(ValidatedCatalogArtifactRowsV1 {
        symbol_count,
        files: files.into_values().collect(),
        imports,
    })
}

/// Derives the interactive catalog from the sealed generation's manifest rows
/// and streams the catalog artifact into `out`. Symbols are decoded and
/// serialized one at a time; only the much smaller file/import validation
/// indexes remain live across the write. This is the seal-time half of
/// catalog-at-seal: one linear pass over rows already in RAM, instead of
/// materializing the complete multi-index lookup catalog beside the manifest.
pub fn write_interactive_catalog_artifact(
    manifest: &GraphGenerationManifest,
    out: &mut dyn Write,
    cancellation: &dyn GraphCancellation,
) -> Result<(), CodeGraphProjectionError> {
    let rows = hotpath::measure_block!(
        "code_graph.catalog.seal_validate",
        validate_catalog_artifact_rows(manifest, cancellation)
    )?;
    check_cancelled(cancellation)?;
    let artifact = InteractiveCatalogArtifactViewV1 {
        format: INTERACTIVE_CATALOG_ARTIFACT_FORMAT_V1,
        graph_generation: manifest.generation.as_str(),
        symbols: CatalogSymbolRowsV1 {
            manifest,
            cancellation,
            count: rows.symbol_count,
        },
        files: CancellableRowsV1 {
            rows: &rows.files,
            cancellation,
        },
        imports: CancellableRowsV1 {
            rows: &rows.imports,
            cancellation,
        },
    };
    serde_json::to_writer(out, &artifact).map_err(|error| {
        if cancellation.is_cancelled() {
            CodeGraphProjectionError::Cancelled
        } else {
            CodeGraphProjectionError::Unavailable(format!(
                "failed to encode code graph interactive catalog artifact: {error}"
            ))
        }
    })
}

/// Decodes a digest-verified catalog artifact back into the in-memory
/// catalog, revalidating structural invariants row by row.
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
