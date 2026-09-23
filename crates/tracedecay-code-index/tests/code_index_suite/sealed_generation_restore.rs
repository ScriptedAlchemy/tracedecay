//! Sealed-generation restore contracts on a real multi-file corpus: decode
//! determinism across indexing widths, corrupt-manifest rejection, and the
//! sealed format-revision gate.

use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::parallelism::{
    clear_forced_indexing_workers_for_test, force_indexing_workers_for_test,
};
use tracedecay_code_index::production::{
    CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionErrorV1,
    CodeIndexProductionOwnerV1, SEALED_GENERATION_FORMAT_REVISION_V1,
};
use tracedecay_domain::{
    FileOccurrenceId, LanguageId, SanitizedCodeFileV1, SensitivityLevelV1,
    SnapshotFileDispositionV1,
};

use crate::production_orchestration::{
    ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
};
use crate::support::{PartitionedSealV1, id, reseal_manifest};

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

fn sealed_multi_file_generation() -> PartitionedSealV1 {
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
    PartitionedSealV1::of(
        &owner
            .build_and_publish(request, &ActiveControl)
            .expect("multi-file generation publishes"),
    )
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
/// restore of the same sealed bytes must re-encode to the identical manifest.
#[test]
fn sealed_restore_reencodes_identically_at_serial_and_parallel_widths() {
    let sealed = sealed_multi_file_generation();

    let serial = {
        let _width = ForcedSerialWidth::install();
        sealed.restored()
    };
    let parallel = sealed.restored();

    assert_eq!(PartitionedSealV1::of(&serial).manifest, sealed.manifest);
    assert_eq!(PartitionedSealV1::of(&parallel).manifest, sealed.manifest);
}

#[test]
fn sealed_restore_rejects_one_corrupt_manifest_byte() {
    let sealed = sealed_multi_file_generation();
    let mut manifest = sealed.manifest.clone();
    let position = manifest
        .windows(5)
        .position(|window| window == b"gamma")
        .expect("the sealed manifest carries the fixture path");
    manifest[position] = b'q';

    let error = sealed
        .restore(&manifest)
        .expect_err("a corrupt manifest byte must be rejected");

    assert!(
        error.to_string().contains("state digest does not match"),
        "corrupt manifest reached the wrong rejection: {error}"
    );
}

#[test]
fn sealed_restore_refuses_superseded_and_adjacent_revisions() {
    let sealed = sealed_multi_file_generation();
    let envelope = sealed.envelope();

    // Every retired revision, the monolithic envelope included, is refused
    // with the typed rebuild error, so the daemon rebuilds the generation
    // instead of decoding a retired shape.
    for retired in [9, SEALED_GENERATION_FORMAT_REVISION_V1 - 1] {
        let mut superseded = envelope.clone();
        superseded["generation"]["format_revision"] = Value::from(retired);
        let error = sealed
            .restore(&reseal_manifest(superseded))
            .expect_err("a superseded revision must be refused");
        assert!(
            matches!(
                error,
                CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
                    if revision == retired
            ),
            "superseded revision reached the wrong rejection: {error}"
        );
        assert!(error.to_string().contains("will be rebuilt from source"));
    }

    // Above every revision this build knows: refused as incompatible.
    let mut incompatible = envelope;
    incompatible["generation"]["format_revision"] =
        Value::from(SEALED_GENERATION_FORMAT_REVISION_V1 + 1);
    let error = sealed
        .restore(&reseal_manifest(incompatible))
        .expect_err("adjacent sealed-generation revisions are incompatible");
    assert!(
        error.to_string().contains("incompatible"),
        "adjacent revision reached the wrong rejection: {error}"
    );
}
