//! Sanitized intake port (Plan 25, "Sanitized intake"): accept only
//! receipt-bound sanitized snapshots carrying repository, checkout, worktree,
//! ref, source revision, sanitizer revision, and content identity; reject
//! missing, stale, mixed-snapshot, or unsanitized input before parsing.
//!
//! Filesystem watching, repository reads, snapshot coalescing, and redaction
//! belong to capture, not this boundary (Plan 25, "Does not own").

use std::{collections::BTreeMap, ops::Deref};

use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    ContentDigest, DomainError, FileOccurrenceId, IntakeRejectionV1, ProjectId, RefId,
    RepositoryId, SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1, UtcMicros,
    ValidatedCodeFileV1, ValidatedCodeSnapshotV1, WorktreeId, canonical_sha256,
};

use super::languages::LanguageRegistry;

/// The intake validation contract (Plan 25 phase 2). This is the only legal
/// entry into the indexer: architecture tests construct the indexer through
/// `CodeIndexIntake` and the projection sink only.
pub trait CodeIndexIntake {
    /// Validate one sanitized snapshot, rejecting missing, stale,
    /// mixed-snapshot, or unsanitized input before any parsing occurs.
    fn validate(
        &self,
        snapshot: SanitizedCodeSnapshotV1,
    ) -> Result<ValidatedCodeSnapshotV1, IntakeRejectionV1>;

    /// Mint an opaque capability for one admitted sanitized snapshot.
    fn admit(
        &self,
        snapshot: SanitizedCodeSnapshotV1,
    ) -> Result<SanitizedSnapshotCapabilityV1, IntakeRejectionV1>;

    /// Bind one raw extraction input to an admitted snapshot capability.
    fn bind_file(
        &self,
        capability: &SanitizedSnapshotCapabilityV1,
        project_id: &ProjectId,
        file: ValidatedCodeFileV1,
    ) -> Result<ReceiptBoundCodeFileV1, IntakeRejectionV1>;
}

/// Domain separator for the canonical intake digest (Plan 25: digests are
/// domain-separated so cross-contract collisions are impossible).
pub const INTAKE_DIGEST_SEPARATOR: &str = "tracedecay.code-index-intake.v1";

/// Content digest over byte-exact sanitized source.
pub fn content_digest(bytes: &[u8]) -> ContentDigest {
    ContentDigest::of_bytes(bytes)
}

/// Opaque proof that [`CodeIndexIntake::admit`] accepted one exact sanitized
/// snapshot. It is intentionally neither deserializable nor publicly
/// constructible: only the intake boundary can mint source authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanitizedSnapshotCapabilityV1 {
    snapshot: ValidatedCodeSnapshotV1,
    files_by_occurrence: BTreeMap<FileOccurrenceId, usize>,
}

impl SanitizedSnapshotCapabilityV1 {
    fn new(snapshot: ValidatedCodeSnapshotV1) -> Self {
        let files_by_occurrence = snapshot
            .snapshot
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.file_occurrence_id.clone(), index))
            .collect();
        Self {
            snapshot,
            files_by_occurrence,
        }
    }

    /// The immutable validated snapshot bound to this capability.
    pub fn snapshot(&self) -> &ValidatedCodeSnapshotV1 {
        &self.snapshot
    }
}

/// Opaque extraction input whose bytes, file digest, snapshot digest, and
/// sanitization receipts were bound by [`CodeIndexIntake::bind_file`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptBoundCodeFileAuthorityV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: Option<WorktreeId>,
    pub reference: Option<RefId>,
    pub logical_path: String,
    pub content_digest: ContentDigest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptBoundCodeFileV1 {
    file: ValidatedCodeFileV1,
    authority: ReceiptBoundCodeFileAuthorityV1,
}

impl ReceiptBoundCodeFileV1 {
    /// The byte-exact sanitized source authorized for extraction and chunking.
    pub fn sanitized_bytes(&self) -> &[u8] {
        &self.file.sanitized_bytes
    }

    pub fn authority(&self) -> &ReceiptBoundCodeFileAuthorityV1 {
        &self.authority
    }

    pub(crate) fn validated_file(&self) -> &ValidatedCodeFileV1 {
        &self.file
    }
}

impl Deref for ReceiptBoundCodeFileV1 {
    type Target = ValidatedCodeFileV1;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

/// Registry-backed sanitized intake validator.
///
/// Admission checks, in order:
///
/// 1. `MissingReceipt` — no sanitization receipts bind the snapshot.
/// 2. `IncompatibleSanitizerRevision` — the snapshot's sanitizer revision is
///    not the revision this indexer was built for.
/// 3. `StaleSnapshot` — the snapshot is older than the configured maximum
///    age relative to the validator's reference time (only when a staleness
///    bound is configured).
/// 4. `MixedSnapshot` — the snapshot fails single-snapshot canonical
///    structure: duplicate or unordered receipts, duplicate file occurrence
///    identities or logical paths, or non-canonical file ordering (the
///    domain contract's snapshot validation owns these rules).
/// 5. `UnsanitizedInput` — the snapshot fails sanitized-file canonical
///    structure (empty or non-canonical logical path, a `Present` file with
///    no declared language), or a `Present` file declares a language the
///    language registry does not know — capture must mark such files
///    `UnsupportedLanguage`, `Ignored`, `Binary`, or `Generated` instead of
///    presenting them as sanitized source.
///
/// The validator is pure: `reference_time` is supplied at construction so
/// validation is deterministic for a given validator instance.
pub struct SanitizedCodeIntake<R: LanguageRegistry> {
    registry: R,
    expected_sanitizer_revision: SanitizerRevision,
    reference_time: UtcMicros,
    max_snapshot_age_micros: Option<i64>,
}

impl<R: LanguageRegistry> SanitizedCodeIntake<R> {
    /// Create an intake validator with no staleness bound.
    pub fn new(
        registry: R,
        expected_sanitizer_revision: SanitizerRevision,
        reference_time: UtcMicros,
    ) -> Self {
        Self {
            registry,
            expected_sanitizer_revision,
            reference_time,
            max_snapshot_age_micros: None,
        }
    }

    /// Pin a staleness bound: snapshots captured more than
    /// `max_age_micros` before the reference time are rejected as stale.
    #[must_use]
    pub fn with_max_snapshot_age_micros(mut self, max_age_micros: i64) -> Self {
        self.max_snapshot_age_micros = Some(max_age_micros);
        self
    }

    /// The language registry backing admission decisions.
    pub fn registry(&self) -> &R {
        &self.registry
    }

    fn validate_snapshot(
        &self,
        snapshot: SanitizedCodeSnapshotV1,
    ) -> Result<ValidatedCodeSnapshotV1, IntakeRejectionV1> {
        if snapshot.sanitization_receipts.is_empty() {
            return Err(IntakeRejectionV1::MissingReceipt);
        }
        if snapshot.sanitizer_revision != self.expected_sanitizer_revision {
            return Err(IntakeRejectionV1::IncompatibleSanitizerRevision);
        }
        if let Some(max_age) = self.max_snapshot_age_micros {
            let age = self.reference_time.0.saturating_sub(snapshot.captured_at.0);
            if age > max_age {
                return Err(IntakeRejectionV1::StaleSnapshot);
            }
        }

        // Single-snapshot canonical structure and per-file sanitized form
        // are owned by the domain contract; map its errors to typed
        // rejections.
        snapshot.validate().map_err(|error| rejection_for(&error))?;

        // Registry-backed admission: every presented source file must
        // declare a language this build can extract.
        let admitted = snapshot.files.iter().all(|file| {
            file.disposition != SnapshotFileDispositionV1::Present
                || file
                    .language
                    .as_ref()
                    .is_some_and(|language| self.registry.descriptor(language).is_some())
        });
        if !admitted {
            return Err(IntakeRejectionV1::UnsanitizedInput);
        }

        let intake_digest = canonical_sha256(&(INTAKE_DIGEST_SEPARATOR, &snapshot))
            .expect("sanitized snapshots serialize canonically");
        Ok(ValidatedCodeSnapshotV1 {
            snapshot,
            intake_digest,
            validated_at: self.reference_time,
        })
    }
}

/// Map a domain snapshot-validation error to the typed intake rejection.
fn rejection_for(error: &DomainError) -> IntakeRejectionV1 {
    match error {
        // Duplicate or non-canonically-ordered receipts, occurrences, or
        // paths mean the input mixes snapshots.
        DomainError::DuplicateId { .. } => IntakeRejectionV1::MixedSnapshot,
        DomainError::NonCanonical { field }
            if *field == "snapshot sanitization receipt order"
                || *field == "snapshot file order" =>
        {
            IntakeRejectionV1::MixedSnapshot
        }
        DomainError::Empty { field } if *field == "snapshot sanitization receipts" => {
            IntakeRejectionV1::MissingReceipt
        }
        // Everything else (logical-path form, present-without-language,
        // non-canonical identities) is unsanitized input.
        _ => IntakeRejectionV1::UnsanitizedInput,
    }
}

impl<R: LanguageRegistry> CodeIndexIntake for SanitizedCodeIntake<R> {
    fn validate(
        &self,
        snapshot: SanitizedCodeSnapshotV1,
    ) -> Result<ValidatedCodeSnapshotV1, IntakeRejectionV1> {
        self.validate_snapshot(snapshot)
    }

    fn admit(
        &self,
        snapshot: SanitizedCodeSnapshotV1,
    ) -> Result<SanitizedSnapshotCapabilityV1, IntakeRejectionV1> {
        self.validate_snapshot(snapshot)
            .map(SanitizedSnapshotCapabilityV1::new)
    }

    fn bind_file(
        &self,
        capability: &SanitizedSnapshotCapabilityV1,
        project_id: &ProjectId,
        file: ValidatedCodeFileV1,
    ) -> Result<ReceiptBoundCodeFileV1, IntakeRejectionV1> {
        if project_id.validate().is_err() {
            return Err(IntakeRejectionV1::UnsanitizedInput);
        }
        if file.snapshot_digest != capability.snapshot.intake_digest
            || file.file.disposition != SnapshotFileDispositionV1::Present
            || content_digest(&file.sanitized_bytes) != file.file.content_digest
            || std::str::from_utf8(&file.sanitized_bytes).is_err()
        {
            return Err(IntakeRejectionV1::UnsanitizedInput);
        }
        let admitted_file = capability
            .files_by_occurrence
            .get(&file.file.file_occurrence_id)
            .and_then(|index| capability.snapshot.snapshot.files.get(*index));
        if admitted_file != Some(&file.file) {
            return Err(IntakeRejectionV1::UnsanitizedInput);
        }
        let snapshot = &capability.snapshot.snapshot;
        let authority = ReceiptBoundCodeFileAuthorityV1 {
            project_id: project_id.clone(),
            repository_id: snapshot.repository.clone(),
            worktree_id: snapshot.worktree.clone(),
            reference: snapshot.reference.clone(),
            logical_path: file.file.logical_path.clone(),
            content_digest: file.file.content_digest.clone(),
        };
        Ok(ReceiptBoundCodeFileV1 { file, authority })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        CodeGenerationId, CommitId, ContentDigest, FileOccurrenceId, LanguageId, RefId,
        RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, WorktreeId,
    };

    use crate::languages::StaticLanguageRegistry;

    fn digest(byte: char) -> ContentDigest {
        ContentDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("valid digest")
    }

    fn receipt(name: &str) -> SanitizationReceiptId {
        SanitizationReceiptId::new(format!("receipt.{name}")).expect("valid receipt id")
    }

    fn lang(name: &str) -> LanguageId {
        LanguageId::new(name).expect("valid language id")
    }

    fn present_file(occurrence: &str, path: &str, language: &str) -> SanitizedCodeFileV1 {
        SanitizedCodeFileV1 {
            file_occurrence_id: FileOccurrenceId::new(format!("file.{occurrence}"))
                .expect("valid occurrence id"),
            logical_path: path.to_owned(),
            language: Some(lang(language)),
            content_digest: digest('a'),
            disposition: SnapshotFileDispositionV1::Present,
        }
    }

    fn snapshot(mut files: Vec<SanitizedCodeFileV1>) -> SanitizedCodeSnapshotV1 {
        files.sort_by(|left, right| {
            (&left.logical_path, &left.file_occurrence_id)
                .cmp(&(&right.logical_path, &right.file_occurrence_id))
        });
        SanitizedCodeSnapshotV1 {
            repository: RepositoryId::new("repo.fixture").expect("valid repo id"),
            worktree: Some(WorktreeId::new("worktree.fixture").expect("valid worktree id")),
            reference: Some(RefId::new("refs/heads/main").expect("valid ref id")),
            source_revision: Some(CommitId::new("commit.fixture").expect("valid commit id")),
            sanitizer_revision: SanitizerRevision::new("sanitizer.v1").expect("valid revision"),
            sanitization_receipts: vec![receipt("one")],
            content_identity: digest('b'),
            captured_at: UtcMicros(1_000_000),
            files,
        }
    }

    fn intake() -> SanitizedCodeIntake<StaticLanguageRegistry> {
        SanitizedCodeIntake::new(
            StaticLanguageRegistry::new(),
            SanitizerRevision::new("sanitizer.v1").expect("valid revision"),
            UtcMicros(2_000_000),
        )
    }

    #[test]
    fn accepts_a_receipt_bound_sanitized_snapshot() {
        let snapshot = snapshot(vec![
            present_file("one", "src/lib.py", "python"),
            present_file("two", "src/main.rs", "rust"),
            SanitizedCodeFileV1 {
                language: None,
                disposition: SnapshotFileDispositionV1::Binary,
                ..present_file("three", "assets/logo.bin", "rust")
            },
        ]);
        let validated = intake().validate(snapshot).expect("snapshot admitted");
        assert_eq!(validated.validated_at, UtcMicros(2_000_000));
        // Deterministic digest for identical input.
        let again = intake()
            .validate(validated.snapshot.clone())
            .expect("snapshot admitted");
        assert_eq!(validated.intake_digest, again.intake_digest);
    }

    #[test]
    fn rejects_missing_receipts() {
        let mut snapshot = snapshot(vec![present_file("one", "src/main.rs", "rust")]);
        snapshot.sanitization_receipts.clear();
        assert_eq!(
            intake().validate(snapshot),
            Err(IntakeRejectionV1::MissingReceipt)
        );
    }

    #[test]
    fn rejects_incompatible_sanitizer_revision() {
        let mut snapshot = snapshot(vec![present_file("one", "src/main.rs", "rust")]);
        snapshot.sanitizer_revision =
            SanitizerRevision::new("sanitizer.v0").expect("valid revision");
        assert_eq!(
            intake().validate(snapshot),
            Err(IntakeRejectionV1::IncompatibleSanitizerRevision)
        );
    }

    #[test]
    fn rejects_stale_snapshots_only_when_bounded() {
        let bounded = intake().with_max_snapshot_age_micros(500_000);
        let stale = snapshot(vec![present_file("one", "src/main.rs", "rust")]);
        assert_eq!(
            bounded.validate(stale.clone()),
            Err(IntakeRejectionV1::StaleSnapshot)
        );
        // The same snapshot passes without a staleness bound, and a fresh
        // snapshot passes the bound.
        intake().validate(stale).expect("unbounded admits");
        let mut fresh = snapshot(vec![present_file("one", "src/main.rs", "rust")]);
        fresh.captured_at = UtcMicros(1_800_000);
        bounded.validate(fresh).expect("fresh snapshot admitted");
    }

    #[test]
    fn rejects_mixed_snapshots() {
        // Duplicate file occurrence identity.
        let duplicate_occurrence = snapshot(vec![
            present_file("one", "src/a.rs", "rust"),
            present_file("one", "src/b.rs", "rust"),
        ]);
        assert_eq!(
            intake().validate(duplicate_occurrence),
            Err(IntakeRejectionV1::MixedSnapshot)
        );
        // Duplicate logical path.
        let duplicate_path = snapshot(vec![
            present_file("one", "src/a.rs", "rust"),
            present_file("two", "src/a.rs", "rust"),
        ]);
        assert_eq!(
            intake().validate(duplicate_path),
            Err(IntakeRejectionV1::MixedSnapshot)
        );
        // Duplicate (and therefore non-canonically ordered) receipts.
        let mut duplicate_receipt = snapshot(vec![present_file("one", "src/a.rs", "rust")]);
        duplicate_receipt.sanitization_receipts.push(receipt("one"));
        assert_eq!(
            intake().validate(duplicate_receipt),
            Err(IntakeRejectionV1::MixedSnapshot)
        );
        // Non-canonically ordered receipts.
        let mut unordered_receipts = snapshot(vec![present_file("one", "src/a.rs", "rust")]);
        unordered_receipts.sanitization_receipts = vec![receipt("two"), receipt("one")];
        assert_eq!(
            intake().validate(unordered_receipts),
            Err(IntakeRejectionV1::MixedSnapshot)
        );
    }

    #[test]
    fn rejects_unsanitized_present_files_and_admits_explicit_dispositions() {
        // A present file without a declared language is unsanitized input.
        let mut no_language = present_file("one", "src/main.rs", "rust");
        no_language.language = None;
        assert_eq!(
            intake().validate(snapshot(vec![no_language])),
            Err(IntakeRejectionV1::UnsanitizedInput)
        );
        // A present file declaring an unregistered language must have been
        // marked unsupported by capture; presenting it is unsanitized input.
        let unknown_language = present_file("one", "src/main.rs", "not-a-language");
        assert_eq!(
            intake().validate(snapshot(vec![unknown_language])),
            Err(IntakeRejectionV1::UnsanitizedInput)
        );
        // A non-canonical logical path is unsanitized input.
        let mut bad_path = present_file("one", "src/main.rs", "rust");
        bad_path.logical_path = "/absolute/main.rs".to_owned();
        assert_eq!(
            intake().validate(snapshot(vec![bad_path])),
            Err(IntakeRejectionV1::UnsanitizedInput)
        );
        // Explicit capture dispositions for an unresolvable file admit.
        for disposition in [
            SnapshotFileDispositionV1::Deleted,
            SnapshotFileDispositionV1::Renamed,
            SnapshotFileDispositionV1::Ignored,
            SnapshotFileDispositionV1::Binary,
            SnapshotFileDispositionV1::Generated,
            SnapshotFileDispositionV1::UnsupportedLanguage,
        ] {
            let file = SanitizedCodeFileV1 {
                language: None,
                disposition,
                ..present_file("one", "src/main.unknownext", "rust")
            };
            intake()
                .validate(snapshot(vec![file]))
                .unwrap_or_else(|_| panic!("{disposition:?} admitted explicitly"));
        }
    }

    #[test]
    fn registry_backed_admission_uses_the_declared_language() {
        // A declared registered language admits even when the path carries
        // no extension; the descriptor registry is the admission authority.
        let declared = present_file("one", "src/no-extension", "rust");
        intake()
            .validate(snapshot(vec![declared]))
            .expect("declared language resolves");

        // Every compiled-in language admits.
        let registry = StaticLanguageRegistry::new();
        for descriptor in registry.descriptors() {
            let file = present_file(
                "one",
                &format!("src/probe.{}", descriptor.extensions[0]),
                descriptor.language.as_str(),
            );
            intake()
                .validate(snapshot(vec![file]))
                .unwrap_or_else(|_| panic!("{} admitted", descriptor.language));
        }
    }

    #[test]
    fn file_binding_consumes_the_admitted_capability_without_revalidating_snapshot() {
        let bytes = b"fn main() {}\n".to_vec();
        let mut descriptor = present_file("one", "src/main.rs", "rust");
        descriptor.content_digest = content_digest(&bytes);
        let capability = intake()
            .admit(snapshot(vec![descriptor.clone()]))
            .expect("snapshot capability");
        let late_binder = SanitizedCodeIntake::new(
            StaticLanguageRegistry::new(),
            SanitizerRevision::new("sanitizer.v1").expect("valid revision"),
            UtcMicros(10_000_000),
        )
        .with_max_snapshot_age_micros(1);

        late_binder
            .bind_file(
                &capability,
                &ProjectId::new("project.fixture").expect("valid project"),
                ValidatedCodeFileV1 {
                    generation_id: CodeGenerationId::new("generation.fixture")
                        .expect("valid generation"),
                    file: descriptor,
                    snapshot_digest: capability.snapshot().intake_digest.clone(),
                    sanitized_bytes: bytes,
                },
            )
            .expect("opaque capability remains authoritative after admission");
    }
}
