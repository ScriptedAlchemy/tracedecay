use std::collections::BTreeSet;
use std::fmt::Debug;

use tracedecay::code_index::capabilities::expected_seal_digest;
use tracedecay::code_index::generations::{GenerationPlanner, RebuildTriggerV1};
use tracedecay::code_index::intake::INTAKE_DIGEST_SEPARATOR;
use tracedecay::code_index::languages::StaticLanguageRegistry;
use tracedecay_domain::{
    ChunkerRevision, ContentDigest, FileOccurrenceId, LanguageId, PrivacyDomainId, RepositoryId,
    SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision,
    SnapshotFileDispositionV1, UtcMicros, ValidatedCodeSnapshotV1, canonical_sha256,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn content_digest(byte: char) -> ContentDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn file(occurrence: &str, path: &str, digest_byte: char) -> SanitizedCodeFileV1 {
    SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>(occurrence),
        logical_path: path.to_owned(),
        language: Some(id::<LanguageId>("rust")),
        content_digest: content_digest(digest_byte),
        disposition: SnapshotFileDispositionV1::Present,
    }
}

fn snapshot(files: Vec<SanitizedCodeFileV1>) -> SanitizedCodeSnapshotV1 {
    SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.incremental"),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.incremental")],
        content_identity: content_digest('f'),
        captured_at: UtcMicros(1_000),
        files,
    }
}

fn validated(snapshot: SanitizedCodeSnapshotV1) -> ValidatedCodeSnapshotV1 {
    let intake_digest =
        canonical_sha256(&(INTAKE_DIGEST_SEPARATOR, &snapshot)).expect("snapshot digest");
    ValidatedCodeSnapshotV1 {
        snapshot,
        intake_digest,
        validated_at: UtcMicros(2_000),
    }
}

fn planner() -> GenerationPlanner<StaticLanguageRegistry> {
    GenerationPlanner::new(
        id::<RepositoryId>("repository.incremental"),
        StaticLanguageRegistry::new(),
        id::<ChunkerRevision>("chunker.v1"),
        id::<PrivacyDomainId>("privacy.incremental"),
        3,
    )
}

#[test]
fn immutable_generation_seals_are_deterministic_and_parent_bound() {
    let planner = planner();
    let snapshot = validated(snapshot(vec![file("file.a", "src/lib.rs", 'a')]));

    let first = planner
        .plan_generation(&snapshot, None, UtcMicros(3_000))
        .expect("first generation");
    let replay = planner
        .plan_generation(&snapshot, None, UtcMicros(3_000))
        .expect("deterministic replay");
    let child = planner
        .plan_generation(&snapshot, Some(&first), UtcMicros(4_000))
        .expect("child generation");

    assert_eq!(first, replay);
    assert_eq!(
        first.seal.expected_digest,
        expected_seal_digest(&first).expect("seal recomputes")
    );
    assert_eq!(child.parent_generation, Some(first.generation_id.clone()));
    assert_ne!(child.seal.expected_digest, first.seal.expected_digest);
    assert!(first.generation_id < child.generation_id);
}

#[test]
fn explicit_identity_and_corruption_invalidations_force_typed_full_rebuilds() {
    let planner = planner();
    let prior_snapshot = snapshot(vec![
        file("file.a", "src/a.rs", 'a'),
        file("file.b", "src/b.rs", 'b'),
    ]);
    let prior = planner
        .plan_generation(&validated(prior_snapshot.clone()), None, UtcMicros(3_000))
        .expect("prior generation");
    let current = validated(snapshot(vec![
        file("file.a2", "src/a.rs", 'a'),
        file("file.b2", "src/b.rs", 'b'),
    ]));
    let invalidations = BTreeSet::from([
        RebuildTriggerV1::CanonicalSchema,
        RebuildTriggerV1::IdentityInputs,
        RebuildTriggerV1::QuarantinedCorruption,
    ]);

    let plan = planner
        .plan_increment_with_invalidation(
            &prior,
            &prior_snapshot,
            &current,
            &BTreeSet::new(),
            &invalidations,
        )
        .expect("full rebuild plan");

    assert!(plan.is_full_rebuild());
    assert_eq!(
        plan.rebuild_triggers,
        vec![
            RebuildTriggerV1::CanonicalSchema,
            RebuildTriggerV1::IdentityInputs,
            RebuildTriggerV1::QuarantinedCorruption,
        ]
    );
    assert_eq!(plan.carried_forward, 0);
    assert_eq!(plan.reextract, 2);
    assert_eq!(plan.deleted, 0);
    assert!(plan.files.iter().all(|file| matches!(
        file.action,
        tracedecay::code_index::generations::FileExtractionActionV1::ReExtract { .. }
    )));
}

#[test]
fn file_increment_planning_reports_deletion_without_reparsing_unchanged_files() {
    let planner = planner();
    let prior_snapshot = snapshot(vec![
        file("file.a", "src/a.rs", 'a'),
        file("file.b", "src/b.rs", 'b'),
    ]);
    let prior = planner
        .plan_generation(&validated(prior_snapshot.clone()), None, UtcMicros(3_000))
        .expect("prior generation");
    let current = validated(snapshot(vec![file("file.a2", "src/a.rs", 'a')]));

    let plan = planner
        .plan_increment(&prior, &prior_snapshot, &current, &BTreeSet::new())
        .expect("increment plan");

    assert!(!plan.is_full_rebuild());
    assert_eq!(plan.carried_forward, 1);
    assert_eq!(plan.reextract, 0);
    assert_eq!(plan.deleted, 1);
    assert_eq!(plan.prior_generation, prior.generation_id);
}
