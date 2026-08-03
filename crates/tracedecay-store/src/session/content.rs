//! Immutable, authorization-bound session content contracts.
//!
//! Content identity detects corruption and deduplicates storage. It is never a
//! read capability: callers must present the immutable occurrence or summary
//! reference that authorized the content when it was written.

use std::str;

use thiserror::Error;
use tracedecay_domain::{
    CanonicalObservationIdV1, ContentDigest, DurableObservationV1, MessageOccurrenceIdV1,
    ProjectionOutputOrdinalV1, RetrievalAnchorId, SanitizationReceiptId, SessionSummaryIdV1,
    canonical_json_bytes,
};

/// Maximum number of Unicode scalar values admitted to one derived FTS value.
pub const MAX_SESSION_CONTENT_FTS_CHARS: usize = 64 * 1_024;
/// Maximum number of content objects returned by one anti-join GC page.
pub const MAX_SESSION_CONTENT_GC_PAGE_SIZE: usize = 1_000;
const MAX_SESSION_CONTENT_FILE_LOCATOR_BYTES: usize = 4_096;

/// The one canonical sanitized value carried by an immutable content object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SessionContentKindV1 {
    ObservationJson,
    MessageText,
    SummaryText,
}

impl SessionContentKindV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObservationJson => "observation_json",
            Self::MessageText => "message_text",
            Self::SummaryText => "summary_text",
        }
    }
}

/// A sanitized source value before it is encoded into an immutable object.
///
/// Observation content deliberately holds the existing durable observation
/// type, so its payload/receipt validation and remote wire representation stay
/// authoritative rather than gaining a second body-reference format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionContentValueV1 {
    ObservationJson(DurableObservationV1),
    MessageText(String),
    SummaryText(String),
}

impl SessionContentValueV1 {
    pub const fn kind(&self) -> SessionContentKindV1 {
        match self {
            Self::ObservationJson(_) => SessionContentKindV1::ObservationJson,
            Self::MessageText(_) => SessionContentKindV1::MessageText,
            Self::SummaryText(_) => SessionContentKindV1::SummaryText,
        }
    }

    /// Encodes the exact canonical bytes that content identity covers.
    pub fn canonical_bytes(&self) -> SessionContentResult<Vec<u8>> {
        match self {
            Self::ObservationJson(observation) => canonical_json_bytes(observation)
                .map_err(|_| SessionContentErrorV1::CanonicalJsonEncoding),
            Self::MessageText(text) | Self::SummaryText(text) => Ok(text.as_bytes().to_vec()),
        }
    }
}

/// Opaque locator supplied only after the payload durability authority has
/// completed its own write. This contract never opens or writes a file.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionContentFileLocatorV1(String);

impl SessionContentFileLocatorV1 {
    pub fn new(value: impl Into<String>) -> SessionContentResult<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(SessionContentErrorV1::FileLocatorEmpty);
        }
        if value.len() > MAX_SESSION_CONTENT_FILE_LOCATOR_BYTES {
            return Err(SessionContentErrorV1::FileLocatorTooLong {
                max: MAX_SESSION_CONTENT_FILE_LOCATOR_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(SessionContentErrorV1::FileLocatorContainsControl);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exactly one persistence representation for the canonical value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionContentStorageV1 {
    InlineBytes(Vec<u8>),
    DurableFile(SessionContentFileLocatorV1),
}

impl SessionContentStorageV1 {
    pub fn inline_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::InlineBytes(bytes) => Some(bytes),
            Self::DurableFile(_) => None,
        }
    }

    pub fn file_locator(&self) -> Option<&SessionContentFileLocatorV1> {
        match self {
            Self::InlineBytes(_) => None,
            Self::DurableFile(locator) => Some(locator),
        }
    }
}

/// Stable database key for a content object. The digest is not an authority.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionContentKeyV1 {
    digest: ContentDigest,
}

impl SessionContentKeyV1 {
    pub fn new(digest: ContentDigest) -> Self {
        Self { digest }
    }

    pub fn digest(&self) -> &ContentDigest {
        &self.digest
    }
}

/// Immutable physical content metadata. The inline/file choice is exclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContentObjectV1 {
    key: SessionContentKeyV1,
    storage: SessionContentStorageV1,
    byte_count: u64,
    char_count: u64,
}

impl SessionContentObjectV1 {
    pub fn inline(value: SessionContentValueV1) -> SessionContentResult<Self> {
        let bytes = value.canonical_bytes()?;
        let byte_count = count_bytes(&bytes)?;
        let char_count = count_chars(&bytes)?;
        Self::from_stored_parts(
            SessionContentKeyV1::new(ContentDigest::of_bytes(&bytes)),
            SessionContentStorageV1::InlineBytes(bytes),
            byte_count,
            char_count,
        )
    }

    /// Builds metadata for content already persisted through the payload
    /// durability authority. No filesystem work occurs here.
    pub fn durable_file(
        value: SessionContentValueV1,
        locator: SessionContentFileLocatorV1,
    ) -> SessionContentResult<Self> {
        let bytes = value.canonical_bytes()?;
        Self::from_stored_parts(
            SessionContentKeyV1::new(ContentDigest::of_bytes(&bytes)),
            SessionContentStorageV1::DurableFile(locator),
            count_bytes(&bytes)?,
            count_chars(&bytes)?,
        )
    }

    /// Reconstructs stored metadata. Inline bytes are rechecked against their
    /// digest, counts, UTF-8 representation, and JSON canonicality; an
    /// external file is deliberately not opened by this contract layer.
    pub fn from_stored_parts(
        key: SessionContentKeyV1,
        storage: SessionContentStorageV1,
        byte_count: u64,
        char_count: u64,
    ) -> SessionContentResult<Self> {
        validate_sqlite_count(byte_count)?;
        validate_sqlite_count(char_count)?;
        if let SessionContentStorageV1::InlineBytes(bytes) = &storage {
            if count_bytes(bytes)? != byte_count {
                return Err(SessionContentErrorV1::ByteCountMismatch);
            }
            if count_chars(bytes)? != char_count {
                return Err(SessionContentErrorV1::CharCountMismatch);
            }
            if ContentDigest::of_bytes(bytes) != *key.digest() {
                return Err(SessionContentErrorV1::DigestMismatch);
            }
        }
        Ok(Self {
            key,
            storage,
            byte_count,
            char_count,
        })
    }

    pub fn key(&self) -> &SessionContentKeyV1 {
        &self.key
    }

    pub fn digest(&self) -> &ContentDigest {
        self.key.digest()
    }

    pub fn storage(&self) -> &SessionContentStorageV1 {
        &self.storage
    }

    pub fn inline_bytes(&self) -> Option<&[u8]> {
        self.storage.inline_bytes()
    }

    pub fn file_locator(&self) -> Option<&SessionContentFileLocatorV1> {
        self.storage.file_locator()
    }

    pub const fn byte_count(&self) -> u64 {
        self.byte_count
    }

    pub const fn char_count(&self) -> u64 {
        self.char_count
    }
}

/// Occurrence-owned authorization. Construction derives rather than accepts
/// the occurrence identity, preventing a projection from inventing one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContentOccurrenceOwnerV1 {
    occurrence_id: MessageOccurrenceIdV1,
    receipt_id: SanitizationReceiptId,
}

impl SessionContentOccurrenceOwnerV1 {
    pub fn derive(
        observation_id: &CanonicalObservationIdV1,
        output_ordinal: ProjectionOutputOrdinalV1,
        receipt_id: SanitizationReceiptId,
    ) -> Self {
        Self {
            occurrence_id: MessageOccurrenceIdV1::derive(observation_id, output_ordinal),
            receipt_id,
        }
    }

    pub fn occurrence_id(&self) -> &MessageOccurrenceIdV1 {
        &self.occurrence_id
    }

    pub fn receipt_id(&self) -> &SanitizationReceiptId {
        &self.receipt_id
    }
}

/// Projection-owned authorization for the canonical observation envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContentProjectionOwnerV1 {
    observation_id: CanonicalObservationIdV1,
    receipt_id: SanitizationReceiptId,
}

impl SessionContentProjectionOwnerV1 {
    pub fn new(
        observation_id: CanonicalObservationIdV1,
        receipt_id: SanitizationReceiptId,
    ) -> Self {
        Self {
            observation_id,
            receipt_id,
        }
    }

    pub fn observation_id(&self) -> &CanonicalObservationIdV1 {
        &self.observation_id
    }

    pub fn receipt_id(&self) -> &SanitizationReceiptId {
        &self.receipt_id
    }
}

/// Summary-owned authorization, bound to the summary's retrieval anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContentSummaryOwnerV1 {
    summary_id: SessionSummaryIdV1,
    anchor_id: RetrievalAnchorId,
}

impl SessionContentSummaryOwnerV1 {
    pub fn new(summary_id: SessionSummaryIdV1, anchor_id: RetrievalAnchorId) -> Self {
        Self {
            summary_id,
            anchor_id,
        }
    }

    pub fn summary_id(&self) -> &SessionSummaryIdV1 {
        &self.summary_id
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }
}

/// The only valid content read authorities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionContentOwnerV1 {
    Projection(SessionContentProjectionOwnerV1),
    Occurrence(SessionContentOccurrenceOwnerV1),
    Summary(SessionContentSummaryOwnerV1),
}

impl SessionContentOwnerV1 {
    const fn kind_name(&self) -> &'static str {
        match self {
            Self::Projection(_) => "projection",
            Self::Occurrence(_) => "occurrence",
            Self::Summary(_) => "summary",
        }
    }
}

/// Immutable link from an authorized owner to one deduplicated content object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContentReferenceV1 {
    key: SessionContentKeyV1,
    content_kind: SessionContentKindV1,
    owner: SessionContentOwnerV1,
}

impl SessionContentReferenceV1 {
    pub fn new(
        object: &SessionContentObjectV1,
        content_kind: SessionContentKindV1,
        owner: SessionContentOwnerV1,
    ) -> SessionContentResult<Self> {
        let valid_owner = matches!(
            (content_kind, &owner),
            (
                SessionContentKindV1::ObservationJson,
                SessionContentOwnerV1::Projection(_)
            ) | (
                SessionContentKindV1::MessageText,
                SessionContentOwnerV1::Occurrence(_)
            ) | (
                SessionContentKindV1::SummaryText,
                SessionContentOwnerV1::Summary(_)
            )
        );
        if !valid_owner {
            return Err(SessionContentErrorV1::OwnerKindMismatch {
                content_kind,
                owner_kind: owner.kind_name(),
            });
        }
        Ok(Self {
            key: object.key().clone(),
            content_kind,
            owner,
        })
    }

    pub fn key(&self) -> &SessionContentKeyV1 {
        &self.key
    }

    pub fn owner(&self) -> &SessionContentOwnerV1 {
        &self.owner
    }

    pub const fn content_kind(&self) -> SessionContentKindV1 {
        self.content_kind
    }
}

/// Read request that carries an authorization edge instead of a content digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContentReadRequestV1 {
    reference: SessionContentReferenceV1,
}

impl SessionContentReadRequestV1 {
    pub fn new(reference: SessionContentReferenceV1) -> Self {
        Self { reference }
    }

    pub fn reference(&self) -> &SessionContentReferenceV1 {
        &self.reference
    }
}

/// Bounded, derived search text for the contentless FTS table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContentSearchTextV1(String);

impl SessionContentSearchTextV1 {
    pub fn new(value: impl Into<String>) -> SessionContentResult<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(SessionContentErrorV1::FtsTextEmpty);
        }
        let count = value.chars().count();
        if count > MAX_SESSION_CONTENT_FTS_CHARS {
            return Err(SessionContentErrorV1::FtsTextTooLong {
                count,
                max: MAX_SESSION_CONTENT_FTS_CHARS,
            });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Bounded anti-join page request for physical content reclamation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionContentGcRequestV1 {
    limit: usize,
}

impl SessionContentGcRequestV1 {
    pub fn new(limit: usize) -> SessionContentResult<Self> {
        if !(1..=MAX_SESSION_CONTENT_GC_PAGE_SIZE).contains(&limit) {
            return Err(SessionContentErrorV1::InvalidGcPageLimit {
                limit,
                max: MAX_SESSION_CONTENT_GC_PAGE_SIZE,
            });
        }
        Ok(Self { limit })
    }

    pub const fn limit(self) -> usize {
        self.limit
    }
}

/// Content-contract failures that do not expose payload data.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SessionContentErrorV1 {
    #[error("canonical observation JSON could not be encoded")]
    CanonicalJsonEncoding,
    #[error("session content bytes are not valid UTF-8")]
    ContentNotUtf8,
    #[error("session content byte count does not match inline bytes")]
    ByteCountMismatch,
    #[error("session content character count does not match inline bytes")]
    CharCountMismatch,
    #[error("session content digest does not match inline bytes")]
    DigestMismatch,
    #[error("session content count exceeds SQLite's signed integer range")]
    CountExceedsSqliteRange,
    #[error("session content file locator is empty")]
    FileLocatorEmpty,
    #[error("session content file locator exceeds {max} bytes")]
    FileLocatorTooLong { max: usize },
    #[error("session content file locator contains a control character")]
    FileLocatorContainsControl,
    #[error("session content kind {content_kind:?} cannot use {owner_kind} authorization")]
    OwnerKindMismatch {
        content_kind: SessionContentKindV1,
        owner_kind: &'static str,
    },
    #[error("session content FTS text is empty")]
    FtsTextEmpty,
    #[error("session content FTS text has {count} characters; maximum is {max}")]
    FtsTextTooLong { count: usize, max: usize },
    #[error("session content GC page limit {limit} must be between 1 and {max}")]
    InvalidGcPageLimit { limit: usize, max: usize },
}

pub type SessionContentResult<T> = Result<T, SessionContentErrorV1>;

fn count_bytes(bytes: &[u8]) -> SessionContentResult<u64> {
    u64::try_from(bytes.len()).map_err(|_| SessionContentErrorV1::CountExceedsSqliteRange)
}

fn count_chars(bytes: &[u8]) -> SessionContentResult<u64> {
    let text = str::from_utf8(bytes).map_err(|_| SessionContentErrorV1::ContentNotUtf8)?;
    u64::try_from(text.chars().count()).map_err(|_| SessionContentErrorV1::CountExceedsSqliteRange)
}

fn validate_sqlite_count(value: u64) -> SessionContentResult<()> {
    if value > i64::MAX as u64 {
        return Err(SessionContentErrorV1::CountExceedsSqliteRange);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tracedecay_domain::{
        CanonicalObservationIdV1, ContentDigest, MessageOccurrenceIdV1, ProjectionOutputOrdinalV1,
        SanitizationReceiptId,
    };

    #[test]
    fn inline_content_derives_the_canonical_digest_and_counts() {
        let object =
            SessionContentObjectV1::inline(SessionContentValueV1::MessageText("héllo".to_owned()))
                .unwrap();

        assert_eq!(
            object.digest(),
            &ContentDigest::of_bytes("héllo".as_bytes())
        );
        assert_eq!(object.byte_count(), 6);
        assert_eq!(object.char_count(), 5);
        assert_eq!(object.inline_bytes(), Some("héllo".as_bytes()));
        assert!(object.file_locator().is_none());
    }

    #[test]
    fn identical_bytes_share_one_content_key_across_semantic_kinds() {
        let message = SessionContentObjectV1::inline(SessionContentValueV1::MessageText(
            "{\"answer\":42}".to_owned(),
        ))
        .unwrap();
        let summary = SessionContentObjectV1::inline(SessionContentValueV1::SummaryText(
            "{\"answer\":42}".to_owned(),
        ))
        .unwrap();

        assert_eq!(message.key(), summary.key());
    }

    #[test]
    fn stored_inline_content_refuses_digest_and_count_mismatches() {
        let key = SessionContentKeyV1::new(ContentDigest::of_bytes(b"expected"));

        let error = SessionContentObjectV1::from_stored_parts(
            key,
            SessionContentStorageV1::InlineBytes(b"actual".to_vec()),
            6,
            6,
        )
        .unwrap_err();

        assert!(matches!(error, SessionContentErrorV1::DigestMismatch));
    }

    #[test]
    fn occurrence_reference_derives_the_canonical_occurrence_id() {
        let observation_id =
            CanonicalObservationIdV1::new(format!("sha256:{}", "a".repeat(64))).unwrap();
        let output_ordinal = ProjectionOutputOrdinalV1::new(7);
        let receipt_id = SanitizationReceiptId::new("receipt.content.fixture").unwrap();
        let owner =
            SessionContentOccurrenceOwnerV1::derive(&observation_id, output_ordinal, receipt_id);

        assert_eq!(
            owner.occurrence_id(),
            &MessageOccurrenceIdV1::derive(&observation_id, output_ordinal)
        );

        let object = SessionContentObjectV1::inline(SessionContentValueV1::MessageText(
            "sanitized".to_owned(),
        ))
        .unwrap();
        let reference = SessionContentReferenceV1::new(
            &object,
            SessionContentKindV1::MessageText,
            SessionContentOwnerV1::Occurrence(owner),
        )
        .unwrap();

        assert_eq!(reference.key(), object.key());
        assert!(matches!(
            reference.owner(),
            SessionContentOwnerV1::Occurrence(_)
        ));
    }

    #[test]
    fn summary_content_cannot_use_an_occurrence_authorization() {
        let object = SessionContentObjectV1::inline(SessionContentValueV1::SummaryText(
            "sanitized summary".to_owned(),
        ))
        .unwrap();
        let observation_id =
            CanonicalObservationIdV1::new(format!("sha256:{}", "b".repeat(64))).unwrap();
        let owner = SessionContentOccurrenceOwnerV1::derive(
            &observation_id,
            ProjectionOutputOrdinalV1::new(0),
            SanitizationReceiptId::new("receipt.content.summary-mismatch").unwrap(),
        );

        let error = SessionContentReferenceV1::new(
            &object,
            SessionContentKindV1::SummaryText,
            SessionContentOwnerV1::Occurrence(owner),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            SessionContentErrorV1::OwnerKindMismatch { .. }
        ));
    }

    #[test]
    fn observation_json_requires_projection_authorization() {
        let object =
            SessionContentObjectV1::inline(SessionContentValueV1::MessageText("{}".to_owned()))
                .unwrap();
        let observation_id =
            CanonicalObservationIdV1::new(format!("sha256:{}", "c".repeat(64))).unwrap();
        let receipt_id = SanitizationReceiptId::new("receipt.content.projection").unwrap();
        let occurrence = SessionContentOccurrenceOwnerV1::derive(
            &observation_id,
            ProjectionOutputOrdinalV1::new(0),
            receipt_id.clone(),
        );
        assert!(
            SessionContentReferenceV1::new(
                &object,
                SessionContentKindV1::ObservationJson,
                SessionContentOwnerV1::Occurrence(occurrence),
            )
            .is_err()
        );

        let projection = SessionContentProjectionOwnerV1::new(observation_id, receipt_id);
        assert!(
            SessionContentReferenceV1::new(
                &object,
                SessionContentKindV1::ObservationJson,
                SessionContentOwnerV1::Projection(projection),
            )
            .is_ok()
        );
    }

    #[test]
    fn fts_text_has_a_strict_character_bound() {
        let text = "x".repeat(MAX_SESSION_CONTENT_FTS_CHARS + 1);
        let error = SessionContentSearchTextV1::new(text).unwrap_err();

        assert!(matches!(
            error,
            SessionContentErrorV1::FtsTextTooLong { .. }
        ));
    }
}
