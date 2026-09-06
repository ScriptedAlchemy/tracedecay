//! Sealed-generation restore contracts on a real multi-file corpus: decode
//! determinism across indexing widths, corrupt-payload rejection, and the
//! sealed format-revision gate.

use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::parallelism::{
    clear_forced_indexing_workers_for_test, force_indexing_workers_for_test,
};
use tracedecay_code_index::production::{
    CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionOwnerV1,
    CodeIndexPublishedGenerationV1, MINIMUM_SEALED_GENERATION_FORMAT_REVISION,
    UninterruptibleCodeIndexControlV1, sealed_generation_payload_digest,
};
use tracedecay_domain::{
    FileOccurrenceId, LanguageId, SanitizedCodeFileV1, SensitivityLevelV1,
    SnapshotFileDispositionV1,
};

use crate::production_orchestration::{
    ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
};
use crate::support::id;

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

fn sealed_multi_file_generation() -> Vec<u8> {
    let mut request = request_with_source(
        "file.sealed-restore.root",
        1_800_000,
        "commit.sealed-restore",
        "tree.sealed-restore",
        "pub fn root() -> u32 { 41 }\n",
    );
    add_present_typescript_file(
        &mut request,
        "file.sealed-restore.alpha",
        "src/alpha.ts",
        "export const alpha = 1;\nexport function shared(): number { return alpha }\n",
    );
    add_present_typescript_file(
        &mut request,
        "file.sealed-restore.beta",
        "src/beta.ts",
        "export const beta = 2;\n",
    );
    add_present_typescript_file(
        &mut request,
        "file.sealed-restore.gamma",
        "src/gamma.ts",
        "export function gamma(): number { return 3 }\n",
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
    request
        .snapshot
        .validate()
        .expect("multi-file fixture snapshot is canonical");

    let mut owner = CodeIndexProductionOwnerV1::new(
        config(),
        SharedPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .expect("production owner");
    owner
        .build_and_publish(request, &ActiveControl)
        .expect("multi-file generation publishes")
        .encode_sealed()
        .expect("multi-file generation seals")
}

/// Clears the forced width even when the guarded decode panics, so a failing
/// assertion cannot leak a width-one pool into unrelated tests.
struct ForcedSerialWidth;

impl ForcedSerialWidth {
    fn install() -> Self {
        force_indexing_workers_for_test(1);
        Self
    }
}

impl Drop for ForcedSerialWidth {
    fn drop(&mut self) {
        clear_forced_indexing_workers_for_test();
    }
}

/// Restore fans per-file authority reconstruction across the indexing pool.
/// Width is sizing policy, never semantics: a width-one and a full-width
/// restore of the same sealed bytes must re-encode to the identical envelope.
#[test]
fn sealed_restore_reencodes_identically_at_serial_and_parallel_widths() {
    let sealed = sealed_multi_file_generation();

    let serial = {
        let _width = ForcedSerialWidth::install();
        CodeIndexPublishedGenerationV1::decode_sealed(&sealed).expect("width-one restore")
    };
    let parallel =
        CodeIndexPublishedGenerationV1::decode_sealed(&sealed).expect("full-width restore");

    assert_eq!(
        serial
            .encode_sealed()
            .expect("width-one restored generation seals"),
        sealed
    );
    assert_eq!(
        parallel
            .encode_sealed()
            .expect("full-width restored generation seals"),
        sealed
    );
}

/// Streaming seat from a seekable reader must keep the same envelope bytes
/// as the in-memory decode: the digest proof and per-file restore cannot
/// change generation identity.
#[test]
fn sealed_seek_reader_restore_reencodes_identically() {
    let sealed = sealed_multi_file_generation();
    let admitted = u64::try_from(sealed.len()).expect("sealed length fits u64");
    let restored = CodeIndexPublishedGenerationV1::decode_sealed_seek_reader(
        std::io::Cursor::new(sealed.as_slice()),
        admitted,
        None,
        &UninterruptibleCodeIndexControlV1,
    )
    .expect("seek restore")
    .expect("compatible revision");
    assert_eq!(
        restored
            .encode_sealed()
            .expect("seek-restored generation seals"),
        sealed
    );
}

/// `unresolved_references` was added to the per-file artifact after revision
/// six had already been persisted. Those earlier records mean exactly “no
/// retained cross-file reference candidates”; restoring them must preserve
/// that meaning so the scheduler can observe the old chunker revision and
/// build a current successor rather than retrying a decode failure forever.
#[test]
fn sealed_restore_defaults_absent_unresolved_references() {
    let sealed = sealed_multi_file_generation();
    let mut envelope: Value = serde_json::from_slice(&sealed).expect("sealed envelope JSON");
    let files = envelope["generation"]["files"]
        .as_array_mut()
        .expect("sealed generation files");
    for file in files {
        file["artifacts"]
            .as_object_mut()
            .expect("file artifacts")
            .remove("unresolved_references");
    }
    let state_digest = sealed_generation_payload_digest(6, &envelope["generation"])
        .expect("compatible generation digest");
    envelope["state_digest"] = Value::String(state_digest.as_str().to_owned());
    let historical = serde_json::to_vec(&envelope).expect("historical sealed generation");

    let restored = CodeIndexPublishedGenerationV1::decode_sealed(&historical)
        .expect("revision-six records without the additive field restore");
    let restored: Value = serde_json::from_slice(
        &restored
            .encode_sealed()
            .expect("restored generation reseals"),
    )
    .expect("restored envelope JSON");
    assert!(
        restored["generation"]["files"]
            .as_array()
            .expect("restored files")
            .iter()
            .all(|file| file["artifacts"]["unresolved_references"]
                .as_array()
                .is_some_and(Vec::is_empty)),
        "historical files must restore with an explicit empty unresolved-reference authority"
    );
}

#[test]
fn sealed_restore_rejects_one_corrupt_payload_byte() {
    let mut sealed = sealed_multi_file_generation();
    let position = sealed
        .windows(5)
        .position(|window| window == b"gamma")
        .expect("the sealed payload carries the fixture symbol");
    sealed[position] = b'q';

    let error = CodeIndexPublishedGenerationV1::decode_sealed(&sealed)
        .expect_err("a corrupt payload byte must be rejected");

    assert!(
        error.to_string().contains("state digest does not match"),
        "corrupt payload reached the wrong rejection: {error}"
    );
}

#[test]
fn sealed_restore_refuses_superseded_and_adjacent_revisions() {
    let sealed = sealed_multi_file_generation();
    let envelope: Value = serde_json::from_slice(&sealed).expect("sealed envelope JSON");

    // Below the minimum: refused with the typed rebuild error, so the daemon
    // rebuilds the generation instead of decoding a retired envelope shape.
    let mut superseded = envelope.clone();
    superseded["generation"]["format_revision"] =
        Value::from(MINIMUM_SEALED_GENERATION_FORMAT_REVISION - 1);
    let superseded = serde_json::to_vec(&superseded).expect("superseded sealed-generation JSON");
    for error in [
        CodeIndexPublishedGenerationV1::decode_sealed_if_compatible(&superseded).err(),
        CodeIndexPublishedGenerationV1::decode_sealed(&superseded).err(),
    ] {
        let error = error.expect("a superseded revision must be refused");
        assert!(
            error.to_string().contains("will be rebuilt from source"),
            "superseded revision reached the wrong rejection: {error}"
        );
    }

    // Above every revision this build knows: abstain, then refuse.
    let mut incompatible = envelope;
    incompatible["generation"]["format_revision"] = Value::from(8);
    let incompatible =
        serde_json::to_vec(&incompatible).expect("incompatible sealed-generation JSON");
    assert!(matches!(
        CodeIndexPublishedGenerationV1::decode_sealed_if_compatible(&incompatible),
        Ok(None)
    ));
    CodeIndexPublishedGenerationV1::decode_sealed(&incompatible)
        .expect_err("adjacent sealed-generation revisions are incompatible");
}
