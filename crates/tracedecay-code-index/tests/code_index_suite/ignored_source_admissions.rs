use serde_json::Value;
use tracedecay_code_index::{
    chunks::content_digest,
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexGenerationScopeV1, CodeIndexIgnoredSourceAdmissionV1, CodeIndexProductionOwnerV1,
        CodeIndexPublishedGenerationV1, SEALED_GENERATION_FORMAT_REVISION_V1,
        sealed_generation_payload_digest,
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
    support::id,
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
        sanitized_bytes: bytes,
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

fn publish(request: CodeIndexBuildRequestV1) -> CodeIndexPublishedGenerationV1 {
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

fn sealed_envelope(generation: &CodeIndexPublishedGenerationV1) -> Value {
    serde_json::from_slice(
        &generation
            .encode_sealed()
            .expect("ignored-source generation seals"),
    )
    .expect("sealed ignored-source generation JSON")
}

fn reseal_outer_state(mut envelope: Value) -> Vec<u8> {
    let format_revision = u32::try_from(
        envelope["generation"]["format_revision"]
            .as_u64()
            .expect("forged generation format revision"),
    )
    .expect("format revision fits u32");
    let state_digest = sealed_generation_payload_digest(format_revision, &envelope["generation"])
        .expect("forged generation has a digest");
    envelope["state_digest"] = Value::String(state_digest.as_str().to_owned());
    serde_json::to_vec(&envelope).expect("forged sealed-generation JSON")
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

    let sealed = generation
        .encode_sealed()
        .expect("ignored-source generation seals");
    let restored = CodeIndexPublishedGenerationV1::decode_sealed(&sealed)
        .expect("ignored-source generation restores");
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
    assert_eq!(
        restored.encode_sealed().expect("restored generation seals"),
        sealed
    );
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
    let mut envelope = sealed_envelope(&generation);
    let roster = envelope["generation"]["ignored_source_admissions"]
        .as_array_mut()
        .expect("sealed generation carries the required ignored-source roster");
    assert_eq!(roster.len(), 1);
    roster[0]["logical_path"] = Value::String(SECONDARY_IGNORED_PATH.to_owned());

    CodeIndexPublishedGenerationV1::decode_sealed(&reseal_outer_state(envelope))
        .expect_err("a self-consistent outer digest cannot forge roster state");
}

#[test]
fn sealed_json_requires_repository_parse_identity_without_a_default() {
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let mut envelope = sealed_envelope(&generation);
    assert!(
        envelope["generation"]
            .as_object_mut()
            .expect("sealed generation object")
            .remove("repository_parse_identity")
            .is_some()
    );

    let error = CodeIndexPublishedGenerationV1::decode_sealed(&reseal_outer_state(envelope))
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
    let envelope = sealed_envelope(&generation);
    assert_eq!(
        envelope["generation"]["repository_parse_identity"]["dirty"],
        Value::String("dirty".to_owned())
    );

    for forged_dirty_state in ["clean", "conflicted"] {
        let mut forged = envelope.clone();
        forged["generation"]["repository_parse_identity"]["dirty"] =
            Value::String(forged_dirty_state.to_owned());
        CodeIndexPublishedGenerationV1::decode_sealed(&reseal_outer_state(forged))
            .expect_err("a nonempty ignored-source roster requires durable Dirty evidence");
    }
}

#[test]
fn sealed_nonempty_roster_rejects_forged_snapshot_source_revision_after_outer_reseal() {
    let mut pinned_request = request_with_ignored_sources(Vec::new());
    pinned_request.snapshot.source_revision = Some(id::<CommitId>("commit.pinned-fixture"));
    let pinned = publish(pinned_request);
    let pinned_envelope = sealed_envelope(&pinned);
    let valid_source_revision =
        pinned_envelope["generation"]["snapshot"]["source_revision"].clone();
    assert!(!valid_source_revision.is_null());

    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let mut envelope = sealed_envelope(&generation);
    envelope["generation"]["snapshot"]["source_revision"] = valid_source_revision;

    CodeIndexPublishedGenerationV1::decode_sealed(&reseal_outer_state(envelope))
        .expect_err("a nonempty ignored-source roster cannot restore with a pinned snapshot");
}

#[test]
fn sealed_json_requires_ignored_source_roster_without_a_default() {
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let mut envelope = sealed_envelope(&generation);
    assert!(
        envelope["generation"]
            .as_object_mut()
            .expect("sealed generation object")
            .remove("ignored_source_admissions")
            .is_some()
    );

    let error = CodeIndexPublishedGenerationV1::decode_sealed(&reseal_outer_state(envelope))
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
    let mut envelope = sealed_envelope(&generation);
    assert!(
        envelope["generation"]
            .as_object_mut()
            .expect("sealed generation object")
            .remove("ignored_source_admissions_digest")
            .is_some()
    );

    let error = CodeIndexPublishedGenerationV1::decode_sealed(&reseal_outer_state(envelope))
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
    let mut envelope = sealed_envelope(&generation);
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

    CodeIndexPublishedGenerationV1::decode_sealed(&reseal_outer_state(envelope))
        .expect_err("legacy ignored_sources alias must not restore");
}

#[test]
fn sealed_state_digest_changes_when_ignored_source_roster_changes() {
    let one = sealed_envelope(&publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )])));
    let two = sealed_envelope(&publish(request_with_ignored_sources(vec![
        admission(PRIMARY_IGNORED_PATH),
        admission(SECONDARY_IGNORED_PATH),
    ])));

    assert_eq!(one["generation"]["snapshot"], two["generation"]["snapshot"]);
    assert_ne!(one["state_digest"], two["state_digest"]);
}

#[test]
fn sealed_format_writes_revision_six_reads_five_and_rejects_adjacent_revisions() {
    assert_eq!(SEALED_GENERATION_FORMAT_REVISION_V1, 6);
    let generation = publish(request_with_ignored_sources(vec![admission(
        PRIMARY_IGNORED_PATH,
    )]));
    let sealed = generation
        .encode_sealed()
        .expect("revision-six generation seals");
    assert!(
        CodeIndexPublishedGenerationV1::sealed_format_is_compatible(&sealed)
            .expect("revision-six compatibility probe")
    );

    let mut legacy = sealed_envelope(&generation);
    legacy["generation"]["format_revision"] = Value::from(5);
    let legacy = reseal_outer_state(legacy);
    assert!(
        CodeIndexPublishedGenerationV1::sealed_format_is_compatible(&legacy)
            .expect("revision-five compatibility probe")
    );
    CodeIndexPublishedGenerationV1::decode_sealed(&legacy)
        .expect("revision-five generation remains readable");

    for incompatible_revision in [4, 7] {
        let mut incompatible = sealed_envelope(&generation);
        incompatible["generation"]["format_revision"] = Value::from(incompatible_revision);
        let incompatible =
            serde_json::to_vec(&incompatible).expect("incompatible sealed-generation JSON");
        assert!(
            !CodeIndexPublishedGenerationV1::sealed_format_is_compatible(&incompatible)
                .expect("incompatible revision compatibility probe")
        );
        CodeIndexPublishedGenerationV1::decode_sealed(&incompatible)
            .expect_err("adjacent sealed-generation revisions are incompatible");
    }
}
