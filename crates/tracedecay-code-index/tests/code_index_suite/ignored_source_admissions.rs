use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_index::{
    chunks::content_digest,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexGenerationScopeV1, CodeIndexIgnoredSourceAdmissionV1, CodeIndexProductionOwnerV1,
        CodeIndexPublishedGenerationV1,
    },
};
use tracedecay_domain::{
    CommitId, FileOccurrenceId, LanguageId, RepositoryDirtyStateV1, SanitizedCodeFileV1,
    SensitivityLevelV1, SnapshotFileDispositionV1,
};

use crate::{
    production_orchestration::{
        ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
    },
    support::{PartitionedSealV1, id, reseal_manifest},
};

const PRIMARY_IGNORED_PATH: &str = "node_modules/alpha/index.ts";
const SECONDARY_IGNORED_PATH: &str = "node_modules/zeta/index.ts";
const PRIMARY_IGNORED_SOURCE: &str = "export interface Alpha { value: string }\n";
const SECONDARY_IGNORED_SOURCE: &str = "export const zeta = 2;\n";

fn admission(logical_path: &str) -> CodeIndexIgnoredSourceAdmissionV1 {
    CodeIndexIgnoredSourceAdmissionV1 {
        logical_path: logical_path.to_owned(),
    }
}

fn add_present_typescript_file(
    request: &mut CodeIndexBuildRequestV1,
    occurrence: &str,
    logical_path: &str,
    source: &str,
) {
    let bytes = source.as_bytes().to_vec();
    let file_occurrence_id = id::<FileOccurrenceId>(occurrence);
    request.snapshot.files.push(SanitizedCodeFileV1 {
        file_occurrence_id: file_occurrence_id.clone(),
        logical_path: logical_path.to_owned(),
        language: Some(id::<LanguageId>("typescript")),
        content_digest: content_digest(&bytes),
        disposition: SnapshotFileDispositionV1::Present,
    });
    request.captured_files.push(CodeIndexCapturedFileV1 {
        file_occurrence_id,
        sanitized_bytes: Arc::from(bytes),
        sensitivity_level: SensitivityLevelV1::Public,
    });
    request.changed_files.insert(logical_path.to_owned());
}

fn request_with_ignored_sources(
    ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
) -> CodeIndexBuildRequestV1 {
    let mut request = request_with_source(
        "file.ignored-roster.root",
        1_700_000,
        "commit.ignored-roster",
        "tree.ignored-roster",
        "pub fn root() -> u32 { 1 }\n",
    );
    add_present_typescript_file(
        &mut request,
        "file.ignored-roster.alpha",
        PRIMARY_IGNORED_PATH,
        PRIMARY_IGNORED_SOURCE,
    );
    add_present_typescript_file(
        &mut request,
        "file.ignored-roster.zeta",
        SECONDARY_IGNORED_PATH,
        SECONDARY_IGNORED_SOURCE,
    );
    request.snapshot.files.sort_by(|left, right| {
        (&left.logical_path, &left.file_occurrence_id)
            .cmp(&(&right.logical_path, &right.file_occurrence_id))
    });
    let complete_content = request
        .captured_files
        .iter()
        .flat_map(|file| file.sanitized_bytes.iter().copied())
        .collect::<Vec<_>>();
    request.snapshot.content_identity = content_digest(&complete_content);
    request.snapshot.source_revision = None;
    request.ignored_source_admissions = ignored_source_admissions;
    request
        .snapshot
        .validate()
        .expect("ignored-source fixture snapshot is canonical");
    request
}

fn publish(request: CodeIndexBuildRequestV1) -> Arc<CodeIndexPublishedGenerationV1> {
    let mut owner = CodeIndexProductionOwnerV1::new(
        config(),
        SharedPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .expect("production owner");
    owner
        .build_and_publish(request, &ActiveControl)
        .expect("ignored-source generation publishes")
}

fn assert_rejected_before_publication(request: CodeIndexBuildRequestV1) {
    let scope = CodeIndexGenerationScopeV1::for_snapshot(&request.snapshot);
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");

    owner
        .build_and_publish(request, &ActiveControl)
        .expect_err("invalid ignored-source admission must be rejected");
    assert!(
        store
            .load_active(&scope)
            .expect("publication store remains readable")
            .is_none(),
        "an invalid ignored-source admission reached atomic publication"
    );
}

#[test]
fn sorted_unique_ignored_source_roster_round_trips_with_present_snapshot_membership() {
    let generation = publish(request_with_ignored_sources(vec![
        admission(PRIMARY_IGNORED_PATH),
        admission(SECONDARY_IGNORED_PATH),
    ]));

    assert_eq!(
        generation
            .ignored_source_admissions()
            .iter()
            .map(|entry| entry.logical_path.as_str())
            .collect::<Vec<_>>(),
        vec![PRIMARY_IGNORED_PATH, SECONDARY_IGNORED_PATH]
    );
    assert_eq!(
        generation.repository_parse_identity().dirty,
        RepositoryDirtyStateV1::Dirty
    );
    for entry in generation.ignored_source_admissions() {
        assert!(generation.snapshot().files.iter().any(|file| {
            file.logical_path.as_str() == entry.logical_path.as_str()
                && file.disposition == SnapshotFileDispositionV1::Present
        }));
    }

    let sealed = PartitionedSealV1::of(&generation);
    let restored = sealed.restored();
    assert_eq!(
        restored
            .ignored_source_admissions()
            .iter()
            .map(|entry| entry.logical_path.as_str())
            .collect::<Vec<_>>(),
        vec![PRIMARY_IGNORED_PATH, SECONDARY_IGNORED_PATH]
    );
    assert_eq!(
        restored.repository_parse_identity().dirty,
        RepositoryDirtyStateV1::Dirty
    );
    assert_eq!(PartitionedSealV1::of(&restored).manifest, sealed.manifest);
}

#[test]
fn ignored_source_roster_requires_dirty_repository_evidence() {
    for dirty in [
        RepositoryDirtyStateV1::Clean,
        RepositoryDirtyStateV1::Conflicted,
    ] {
        let mut request = request_with_ignored_sources(vec![admission(PRIMARY_IGNORED_PATH)]);
        request.repository_parse_identity.dirty = dirty;
        assert_rejected_before_publication(request);
    }
}

#[test]
fn ignored_source_roster_requires_snapshot_without_source_revision() {
    let mut request = request_with_ignored_sources(vec![admission(PRIMARY_IGNORED_PATH)]);
    request.snapshot.source_revision = Some(id::<CommitId>("commit.must-not-pin"));

    assert_rejected_before_publication(request);
}

#[test]
fn ignored_source_roster_rejects_duplicate_paths_before_publication() {
    let request = request_with_ignored_sources(vec![
        admission(PRIMARY_IGNORED_PATH),
        admission(PRIMARY_IGNORED_PATH),
    ]);

    assert_rejected_before_publication(request);
}

#[test]
fn ignored_source_roster_rejects_unsorted_paths_before_publication() {
    let request = request_with_ignored_sources(vec![
        admission(SECONDARY_IGNORED_PATH),
        admission(PRIMARY_IGNORED_PATH),
    ]);

    assert_rejected_before_publication(request);
}

#[test]
fn ignored_source_roster_rejects_noncanonical_paths_before_publication() {
    let request =
        request_with_ignored_sources(vec![admission("node_modules/alpha/../alpha/index.ts")]);

    assert_rejected_before_publication(request);
}

#[test]
fn ignored_source_roster_rejects_paths_missing_from_the_snapshot() {
    let mut request = request_with_ignored_sources(vec![admission(SECONDARY_IGNORED_PATH)]);
    let missing_occurrence = request
        .snapshot
        .files
        .iter()
        .find(|file| file.logical_path == SECONDARY_IGNORED_PATH)
        .map(|file| file.file_occurrence_id.clone())
        .expect("secondary ignored source exists in the fixture");
    request
        .snapshot
        .files
        .retain(|file| file.file_occurrence_id != missing_occurrence);
    request
        .captured_files
        .retain(|file| file.file_occurrence_id != missing_occurrence);
    request.changed_files.remove(SECONDARY_IGNORED_PATH);

    assert_rejected_before_publication(request);
}

#[test]
fn ignored_source_roster_rejects_snapshot_membership_that_is_not_present() {
    let mut request = request_with_ignored_sources(vec![admission(SECONDARY_IGNORED_PATH)]);
    let admitted_occurrence = request
        .snapshot
        .files
        .iter_mut()
        .find(|file| file.logical_path == SECONDARY_IGNORED_PATH)
        .map(|file| {
            file.disposition = SnapshotFileDispositionV1::Ignored;
            file.file_occurrence_id.clone()
        })
        .expect("secondary ignored source exists in the fixture");
    request
        .captured_files
        .retain(|file| file.file_occurrence_id != admitted_occurrence);

    assert_rejected_before_publication(request);
}

#[test]
fn sealed_ignored_source_roster_rejects_semantic_tampering_after_outer_reseal() {
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let seal = PartitionedSealV1::of(&generation);
    let mut envelope = seal.envelope();
    let roster = envelope["generation"]["ignored_source_admissions"]
        .as_array_mut()
        .expect("sealed generation carries the required ignored-source roster");
    assert_eq!(roster.len(), 1);
    roster[0]["logical_path"] = Value::String(SECONDARY_IGNORED_PATH.to_owned());

    seal.restore(&reseal_manifest(envelope))
        .expect_err("a self-consistent outer digest cannot forge roster state");
}

#[test]
fn sealed_json_requires_repository_parse_identity_without_a_default() {
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let seal = PartitionedSealV1::of(&generation);
    let mut envelope = seal.envelope();
    assert!(
        envelope["generation"]
            .as_object_mut()
            .expect("sealed generation object")
            .remove("repository_parse_identity")
            .is_some()
    );

    let error = seal
        .restore(&reseal_manifest(envelope))
        .expect_err("sealed generations may not default missing repository parse identity");
    assert!(
        error.to_string().contains("repository_parse_identity"),
        "missing repository parse identity reached the wrong rejection: {error}"
    );
}

#[test]
fn sealed_nonempty_roster_rejects_non_dirty_repository_identity_after_outer_reseal() {
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let seal = PartitionedSealV1::of(&generation);
    let envelope = seal.envelope();
    assert_eq!(
        envelope["generation"]["repository_parse_identity"]["dirty"],
        Value::String("dirty".to_owned())
    );

    for forged_dirty_state in ["clean", "conflicted"] {
        let mut forged = envelope.clone();
        forged["generation"]["repository_parse_identity"]["dirty"] =
            Value::String(forged_dirty_state.to_owned());
        seal.restore(&reseal_manifest(forged))
            .expect_err("a nonempty ignored-source roster requires durable Dirty evidence");
    }
}

#[test]
fn sealed_nonempty_roster_rejects_forged_snapshot_source_revision_after_outer_reseal() {
    let mut pinned_request = request_with_ignored_sources(Vec::new());
    pinned_request.snapshot.source_revision = Some(id::<CommitId>("commit.pinned-fixture"));
    let pinned = publish(pinned_request);
    let pinned_envelope = PartitionedSealV1::of(&pinned).envelope();
    let valid_source_revision =
        pinned_envelope["generation"]["snapshot"]["source_revision"].clone();
    assert!(!valid_source_revision.is_null());

    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let seal = PartitionedSealV1::of(&generation);
    let mut envelope = seal.envelope();
    envelope["generation"]["snapshot"]["source_revision"] = valid_source_revision;

    seal.restore(&reseal_manifest(envelope))
        .expect_err("a nonempty ignored-source roster cannot restore with a pinned snapshot");
}

#[test]
fn sealed_json_requires_ignored_source_roster_without_a_default() {
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let seal = PartitionedSealV1::of(&generation);
    let mut envelope = seal.envelope();
    assert!(
        envelope["generation"]
            .as_object_mut()
            .expect("sealed generation object")
            .remove("ignored_source_admissions")
            .is_some()
    );

    let error = seal
        .restore(&reseal_manifest(envelope))
        .expect_err("sealed generations may not default a missing ignored-source roster");
    assert!(
        error.to_string().contains("ignored_source_admissions"),
        "missing roster reached the wrong rejection: {error}"
    );
}

#[test]
fn sealed_json_requires_ignored_source_admissions_digest_without_a_default() {
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let seal = PartitionedSealV1::of(&generation);
    let mut envelope = seal.envelope();
    assert!(
        envelope["generation"]
            .as_object_mut()
            .expect("sealed generation object")
            .remove("ignored_source_admissions_digest")
            .is_some()
    );

    let error = seal
        .restore(&reseal_manifest(envelope))
        .expect_err("sealed generations may not default a missing ignored-source roster digest");
    assert!(
        error
            .to_string()
            .contains("ignored_source_admissions_digest"),
        "missing roster digest reached the wrong rejection: {error}"
    );
}

#[test]
fn sealed_json_rejects_legacy_ignored_sources_alias_after_outer_reseal() {
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let seal = PartitionedSealV1::of(&generation);
    let mut envelope = seal.envelope();
    let generation = envelope["generation"]
        .as_object_mut()
        .expect("sealed generation object");
    let roster = generation
        .remove("ignored_source_admissions")
        .expect("sealed generation carries the required ignored-source roster");
    assert!(
        generation
            .insert("ignored_sources".to_owned(), roster)
            .is_none()
    );

    seal.restore(&reseal_manifest(envelope))
        .expect_err("legacy ignored_sources alias must not restore");
}

#[test]
fn sealed_state_digest_changes_when_ignored_source_roster_changes() {
    let one = PartitionedSealV1::of(&publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )])))
    .envelope();
    let two = PartitionedSealV1::of(&publish(request_with_ignored_sources(vec![
        admission(PRIMARY_IGNORED_PATH),
        admission(SECONDARY_IGNORED_PATH),
    ])))
    .envelope();

    assert_eq!(one["generation"]["snapshot"], two["generation"]["snapshot"]);
    assert_ne!(one["state_digest"], two["state_digest"]);
}
