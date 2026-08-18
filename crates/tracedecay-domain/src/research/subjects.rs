use serde::{Deserialize, Serialize};

use super::error::DomainError;
use super::evidence::LogSafeText;
use super::id::{CatalogGenerationId, EntityId, LocatorDigest, ManifestDigest};

/// Canonical entity categories needed by the research slice.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EntityKind {
    Actor,
    Repository,
    Project,
    PullRequest,
    Check,
    Review,
    Release,
    Session,
    Thread,
    Turn,
    Agent,
    Message,
    MessageOccurrence,
    SessionSummary,
    EvidenceSpan,
    EvidenceBurst,
    Workflow,
    ResponseHandle,
    SourceRecord,
    WebSource,
    Document,
    Plan,
    Artifact,
    Other(LogSafeText),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct EntityRef {
    pub id: EntityId,
    pub kind: EntityKind,
}

impl EntityRef {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.id.validate()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshotRefV1 {
    pub generation: CatalogGenerationId,
    pub digest: ManifestDigest,
}

impl CatalogSnapshotRefV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.generation.validate()?;
        self.digest.validate()
    }
}

/// Source-local position without literal path or source text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourcePosition {
    ByteOffset { start: u64, end: u64 },
    RowId { row_id: i64 },
    Sequence { sequence: u64 },
    ObjectKey { digest: LocatorDigest },
}

impl SourcePosition {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::ByteOffset { start, end } if start > end => Err(DomainError::UnknownReference {
                field: "source position byte range",
            }),
            Self::ObjectKey { digest } => digest.validate(),
            Self::ByteOffset { .. } | Self::RowId { .. } | Self::Sequence { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_position_rejects_inverted_byte_range() {
        assert_eq!(
            SourcePosition::ByteOffset {
                start: 100,
                end: 10
            }
            .validate(),
            Err(DomainError::UnknownReference {
                field: "source position byte range",
            })
        );
        assert!(
            SourcePosition::ByteOffset {
                start: 10,
                end: 100
            }
            .validate()
            .is_ok()
        );
    }
}
