use std::collections::BTreeSet;
use std::fmt::Debug;

use tracedecay_code_index::capabilities::expected_seal_digest;
use tracedecay_code_index::generations::{GenerationPlanner, RebuildTriggerV1};
use tracedecay_code_index::intake::INTAKE_DIGEST_SEPARATOR;
use tracedecay_code_index::languages::StaticLanguageRegistry;
use tracedecay_domain::{
    ChunkerRevision, CodeGenerationManifestV1, ContentDigest, FileOccurrenceId, LanguageId,
    ManifestDigest, PrivacyDomainId, RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1,
    SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1, UtcMicros,
    ValidatedCodeSnapshotV1, canonical_sha256,
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
fn explicit_runtime_invalidations_force_typed_full_rebuilds() {
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
        RebuildTriggerV1::SanitizerRevision,
        RebuildTriggerV1::ChunkerRevision,
        RebuildTriggerV1::PrivacyKeyEpoch,
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
            RebuildTriggerV1::SanitizerRevision,
            RebuildTriggerV1::ChunkerRevision,
            RebuildTriggerV1::PrivacyKeyEpoch,
        ]
    );
    assert_eq!(plan.carried_forward, 0);
    assert_eq!(plan.reextract, 2);
    assert_eq!(plan.deleted, 0);
    assert!(plan.files.iter().all(|file| matches!(
        file.action,
        tracedecay_code_index::generations::FileExtractionActionV1::ReExtract { .. }
    )));
}

#[test]
fn invalidation_digest_fences_sibling_generation_identity_and_publication() {
    let planner = planner();
    let prior_snapshot = snapshot(vec![file("file.a", "src/a.rs", 'a')]);
    let prior = planner
        .plan_generation(&validated(prior_snapshot.clone()), None, UtcMicros(3_000))
        .expect("prior generation");
    let current = validated(snapshot(vec![file("file.a2", "src/a.rs", 'a')]));
    let no_invalidations = BTreeSet::new();
    let declared_invalidations = BTreeSet::from([
        RebuildTriggerV1::SanitizerRevision,
        RebuildTriggerV1::ChunkerRevision,
        RebuildTriggerV1::PrivacyKeyEpoch,
    ]);

    let incremental = planner
        .plan_increment(&prior, &prior_snapshot, &current, &BTreeSet::new())
        .expect("increment plan");
    let rebuilt = planner
        .plan_increment_with_invalidation(
            &prior,
            &prior_snapshot,
            &current,
            &BTreeSet::new(),
            &declared_invalidations,
        )
        .expect("declared rebuild plan");
    let ordinary_generation = planner
        .plan_generation_with_invalidation(
            &current,
            Some(&prior),
            &no_invalidations,
            UtcMicros(4_000),
        )
        .expect("ordinary generation");
    let rebuilt_generation = planner
        .plan_generation_with_invalidation(
            &current,
            Some(&prior),
            &declared_invalidations,
            UtcMicros(4_000),
        )
        .expect("rebuilt generation");
    let replay = planner
        .plan_generation_with_invalidation(
            &current,
            Some(&prior),
            &declared_invalidations,
            UtcMicros(4_000),
        )
        .expect("deterministic replay");

    assert_ne!(incremental.invalidation_digest, rebuilt.invalidation_digest);
    assert_eq!(
        incremental.invalidation_digest,
        ordinary_generation.invalidation_digest
    );
    assert_eq!(
        rebuilt.invalidation_digest,
        rebuilt_generation.invalidation_digest
    );
    assert_ne!(
        ordinary_generation.invalidation_digest,
        rebuilt_generation.invalidation_digest
    );
    assert_ne!(
        ordinary_generation.generation_id,
        rebuilt_generation.generation_id
    );
    assert_ne!(
        ordinary_generation.seal.expected_digest,
        rebuilt_generation.seal.expected_digest
    );
    assert_eq!(rebuilt_generation, replay);
}

#[test]
fn resealing_cannot_hide_a_generation_fingerprint_mismatch() {
    let planner = planner();
    let snapshot = validated(snapshot(vec![file("file.a", "src/a.rs", 'a')]));
    let generation = planner
        .plan_generation(&snapshot, None, UtcMicros(3_000))
        .expect("generation");
    let mut tampered = generation.clone();
    let mut identity = tampered.generation_id.as_str().to_owned();
    let replacement = if identity.ends_with('0') { '1' } else { '0' };
    identity.pop();
    identity.push(replacement);
    tampered.generation_id = id(identity.as_str());
    tampered.seal.expected_digest =
        expected_seal_digest(&tampered).expect("tampered manifest can be resealed");

    assert!(tampered.validate().is_err());
    assert!(
        planner
            .plan_generation(&snapshot, Some(&tampered), UtcMicros(4_000))
            .is_err()
    );
}

#[test]
fn legacy_v1_manifest_deserialization_migrates_and_remains_a_valid_parent() {
    let planner = planner();
    let snapshot = validated(snapshot(vec![file("file.a", "src/a.rs", 'a')]));
    let current = planner
        .plan_generation(&snapshot, None, UtcMicros(3_000))
        .expect("current manifest");
    let mut legacy_identity = current
        .generation_id
        .as_str()
        .split('.')
        .take(4)
        .collect::<Vec<_>>()
        .join(".");
    assert_eq!(legacy_identity.matches('.').count(), 3);

    let mut wire = serde_json::to_value(&current).expect("manifest wire");
    let object = wire.as_object_mut().expect("manifest object");
    object.insert(
        "generation_id".to_owned(),
        serde_json::Value::String(std::mem::take(&mut legacy_identity)),
    );
    object.remove("invalidation_digest");
    object
        .get_mut("seal")
        .and_then(serde_json::Value::as_object_mut)
        .expect("seal object")
        .insert(
            "expected_digest".to_owned(),
            serde_json::Value::String(format!("sha256:{}", "0".repeat(64))),
        );

    let mut migrated: CodeGenerationManifestV1 =
        serde_json::from_value(wire).expect("legacy wire migrates");
    migrated.seal.expected_digest = expected_seal_digest(&migrated).expect("legacy seal digest");
    let mut legacy_wire = serde_json::to_value(&migrated).expect("migrated wire");
    legacy_wire
        .as_object_mut()
        .expect("manifest object")
        .remove("invalidation_digest");
    let legacy_parent: CodeGenerationManifestV1 =
        serde_json::from_value(legacy_wire).expect("legacy fixture deserializes");

    legacy_parent.validate().expect("legacy parent validates");
    assert_eq!(
        legacy_parent.invalidation_digest,
        migrated.invalidation_digest
    );
    assert_ne!(
        legacy_parent.invalidation_digest,
        id::<ManifestDigest>(&format!("sha256:{}", "0".repeat(64)))
    );
    let child = planner
        .plan_generation(&snapshot, Some(&legacy_parent), UtcMicros(4_000))
        .expect("legacy parent accepted");
    assert_eq!(
        child.parent_generation.as_ref(),
        Some(&legacy_parent.generation_id)
    );
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
