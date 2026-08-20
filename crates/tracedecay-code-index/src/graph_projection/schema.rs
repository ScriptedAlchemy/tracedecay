//! Durable labels, properties, and identities for the code-graph projection.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::FileOccurrenceId;
use tracedecay_graph_db::{
    GraphEntity, GraphEntityId, GraphProperty, GraphPropertyName, GraphRelationId,
};

use super::CodeGraphProjectionError;
use crate::chunks::CodeIndexImportEvidenceV1;

pub(super) const SYMBOL_RECORD_PROPERTY: &str = "symbol-record";
pub(super) const FILE_RECORD_PROPERTY: &str = "file-record";
pub(super) const IMPORT_RECORD_PROPERTY: &str = "import-record";
pub(super) const SYMBOL_LABEL: &str = "CodeSymbol";
pub(super) const FILE_LABEL: &str = "CodeFile";
pub(super) const IMPORT_LABEL: &str = "CodeImport";
pub(super) const FILE_IMPORT_EDGE_KIND: &str = "CodeFileContainsImport";

pub(super) fn stable_identity(kind: &str, value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(value.as_bytes());
    format!("{kind}:{}", hex::encode(digest.finalize()))
}

pub(super) fn file_entity_id(
    file: &FileOccurrenceId,
) -> Result<GraphEntityId, CodeGraphProjectionError> {
    GraphEntityId::new(stable_identity("file", file.as_str())).map_err(Into::into)
}

pub(super) fn import_entity_id(
    import: &CodeIndexImportEvidenceV1,
) -> Result<GraphEntityId, CodeGraphProjectionError> {
    GraphEntityId::new(stable_identity("import", &hex::encode(serialize(import)?)))
        .map_err(Into::into)
}

pub(super) fn file_import_relation_id(
    import: &CodeIndexImportEvidenceV1,
) -> Result<GraphRelationId, CodeGraphProjectionError> {
    let import_id = import_entity_id(import)?;
    file_import_relation_id_with(import, &import_id)
}

/// Same relation identity with the import entity id already derived, so a
/// caller that just computed it does not serialize and hash the import again.
pub(super) fn file_import_relation_id_with(
    import: &CodeIndexImportEvidenceV1,
    import_id: &GraphEntityId,
) -> Result<GraphRelationId, CodeGraphProjectionError> {
    GraphRelationId::new(stable_identity(
        "file-import",
        &format!(
            "{}\0{}",
            import.file_occurrence_id.as_str(),
            import_id.as_str()
        ),
    ))
    .map_err(Into::into)
}

pub(super) fn serialize(value: &impl Serialize) -> Result<Vec<u8>, CodeGraphProjectionError> {
    serde_json::to_vec(value).map_err(|error| CodeGraphProjectionError::Contract(error.to_string()))
}

pub(super) fn deserialize_property<T>(
    entity: &GraphEntity,
    name: &str,
) -> Result<T, CodeGraphProjectionError>
where
    T: for<'de> Deserialize<'de>,
{
    let property = entity
        .properties
        .get(&GraphPropertyName::new(name)?)
        .ok_or_else(|| {
            CodeGraphProjectionError::Corrupt(format!("code graph entity is missing {name}"))
        })?;
    let GraphProperty::Bytes(bytes) = property else {
        return Err(CodeGraphProjectionError::Corrupt(format!(
            "code graph entity {name} has the wrong type"
        )));
    };
    serde_json::from_slice(bytes)
        .map_err(|error| CodeGraphProjectionError::Corrupt(error.to_string()))
}

pub(super) fn has_label(entity: &GraphEntity, label: &str) -> bool {
    entity
        .labels
        .iter()
        .any(|candidate| candidate.as_str() == label)
}
