//! Sparse publication must preserve the cold graph and exact chunk delta,
//! including callers whose source did not change when a target was re-keyed.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tracedecay_code_index::{
    chunks::content_digest,
    incremental::plan_chunk_increment,
    production::{
        CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionOwnerV1,
        CodeIndexPublishedGenerationV1, MemorySealedPublicationStoreV1,
    },
};
use tracedecay_domain::{
    FileOccurrenceId, LanguageId, ManifestDigest, SanitizedCodeFileV1, SensitivityLevelV1,
    SnapshotFileDispositionV1, SymbolOccurrenceId,
};

use crate::{
    production_orchestration::{ActiveControl, config, request_with_source},
    support::{cold_generation, id},
};

fn tree_request(util: &str, sealed_at: i64, phase: &str) -> CodeIndexBuildRequestV1 {
    let root = concat!(
        "pub mod main;\npub mod util;\npub mod other;\npub mod sibling;\n",
        "pub mod caller0;\npub mod caller1;\npub mod caller2;\npub mod caller3;\n",
    );
    let mut request = request_with_source(
        "file.sparse.seed",
        sealed_at,
        &format!("commit.sparse.{phase}"),
        &format!("tree.sparse.{phase}"),
        root,
    );
    let mut sources = vec![
        ("src/lib.rs".to_owned(), root.to_owned()),
        (
            "src/main.rs".to_owned(),
            "pub fn run() -> u32 { crate::util::helper() + crate::util::extra() }\n".to_owned(),
        ),
        (
            "src/sibling.rs".to_owned(),
            "pub fn second_caller() -> u32 { crate::util::helper() }\n".to_owned(),
        ),
        ("src/util.rs".to_owned(), util.to_owned()),
        (
            "src/other.rs".to_owned(),
            "pub fn new() -> u32 { 7 }\n".to_owned(),
        ),
    ];
    sources.extend((0..4).map(|n| {
        (
            format!("src/caller{n}.rs"),
            format!("pub fn caller{n}() {{ let _ = crate::other::new(); }}\n"),
        )
    }));
    sources.sort();
    request.snapshot.files.clear();
    request.captured_files.clear();
    request.changed_files.clear();
    let mut identity = Sha256::new();
    for (path, source) in sources {
        let digest = Sha256::digest([path.as_bytes(), b"\0", source.as_bytes()].concat());
        let occurrence = id::<FileOccurrenceId>(&format!(
            "file.sparse.{}",
            digest
                .iter()
                .take(12)
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        identity.update(digest);
        request.snapshot.files.push(SanitizedCodeFileV1 {
            file_occurrence_id: occurrence.clone(),
            logical_path: path.clone(),
            language: Some(id::<LanguageId>("rust")),
            content_digest: content_digest(source.as_bytes()),
            disposition: SnapshotFileDispositionV1::Present,
        });
        request.captured_files.push(CodeIndexCapturedFileV1 {
            file_occurrence_id: occurrence,
            sanitized_bytes: Arc::from(source.as_bytes()),
            sensitivity_level: SensitivityLevelV1::Public,
        });
        request.changed_files.insert(path);
    }
    request.snapshot.content_identity = content_digest(&identity.finalize());
    request
}

fn symbol(generation: &CodeIndexPublishedGenerationV1, name: &str) -> SymbolOccurrenceId {
    let matches = generation
        .symbols()
        .symbols
        .iter()
        .filter(|symbol| symbol.simple_name == name)
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "fixture must have exactly one {name}");
    matches[0].clone()
}

#[test]
fn comment_successor_skips_unrelated_evidence_and_publishes_the_cold_chunk_delta() {
    let util = (0..8)
        .map(|n| {
            format!("pub struct Kind{n};\nimpl Kind{n} {{ pub fn new() -> Self {{ Kind{n} }} }}\n")
        })
        .collect::<String>()
        + "pub fn helper() -> u32 { 1 }\npub fn extra() -> u32 { 2 }\n";
    let store = MemorySealedPublicationStoreV1::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store.clone()).expect("owner");
    let before_request = tree_request(&util, 1_100_000, "before");
    let parent = owner
        .build_and_publish(before_request.clone(), &ActiveControl)
        .expect("parent publishes");
    assert!(parent.cold_reason().is_some());
    let envelope: serde_json::Value =
        serde_json::from_slice(parent.manifest_bytes()).expect("sealed manifest envelope");
    let evidence_digest = |path: &str| {
        let key = before_request
            .snapshot
            .files
            .iter()
            .position(|file| file.logical_path == path)
            .expect("fixture path");
        let key = u64::try_from(key).expect("file key fits u64");
        let descriptor = envelope["generation"]["file_evidence"]
            .as_array()
            .expect("evidence descriptors")
            .iter()
            .find(|descriptor| descriptor["file_key"].as_u64() == Some(key))
            .expect("fixture must seal real cross-file evidence");
        serde_json::from_value::<ManifestDigest>(descriptor["segment_digest"].clone())
            .expect("evidence digest")
    };
    let unrelated = (0..4)
        .map(|n| evidence_digest(&format!("src/caller{n}.rs")))
        .map(|digest| {
            let reads = store.segment_reads(&digest);
            (digest, reads)
        })
        .collect::<Vec<_>>();
    let dependent = evidence_digest("src/main.rs");
    let dependent_reads = store.segment_reads(&dependent);
    let edited = format!("{util}// comment only\n");
    let cold_request = tree_request(&edited, 1_200_000, "after");
    let mut sparse_request = cold_request.clone();
    let edited_occurrence = sparse_request
        .snapshot
        .files
        .iter()
        .find(|file| file.logical_path == "src/util.rs")
        .expect("edited file")
        .file_occurrence_id
        .clone();
    sparse_request
        .captured_files
        .retain(|file| file.file_occurrence_id == edited_occurrence);
    assert_eq!(sparse_request.captured_files.len(), 1);
    sparse_request.changed_files = ["src/util.rs".to_owned()].into_iter().collect();
    let sparse = owner
        .build_and_publish(sparse_request, &ActiveControl)
        .expect("sparse successor publishes without recapturing callers");
    assert_eq!(sparse.cold_reason(), None);

    // Observe reads before restoring either result, not on a second build.
    for (digest, reads) in unrelated {
        assert_eq!(store.segment_reads(&digest), reads, "unrelated evidence");
    }
    assert!(store.segment_reads(&dependent) > dependent_reads);

    let mut cold_owner =
        CodeIndexProductionOwnerV1::new(config(), MemorySealedPublicationStoreV1::default())
            .expect("cold owner");
    let cold = cold_owner
        .build_and_publish(cold_request, &ActiveControl)
        .expect("cold successor publishes");
    assert!(cold.cold_reason().is_some());
    let before = cold_generation(&parent);
    let after = cold_generation(&sparse);
    let expected = cold_generation(&cold);
    let expected_delta = plan_chunk_increment(Some(before.chunks()), expected.chunks())
        .expect("cold chunks define the complete expected delta");
    assert!(!expected_delta.added_or_changed.is_empty());
    assert!(expected_delta.reused_count > 0);
    // A cold seal starts at repository sequence one, while a real sparse
    // successor advances its parent's generation sequence. Their canonical
    // partitions must agree, but their seal/generation identities must not.
    let changes = &sparse.projection().request().changes;
    changes
        .validate()
        .expect("published chunk delta is canonical");
    assert_eq!(
        changes.from_generation.as_ref(),
        Some(&parent.manifest().generation_id)
    );
    assert_eq!(changes.to_generation, sparse.manifest().generation_id);
    assert_ne!(changes.to_generation, expected_delta.to_generation);
    assert_eq!(changes.added_or_changed, expected_delta.added_or_changed);
    assert_eq!(changes.deleted, expected_delta.deleted);
    assert_eq!(changes.reused_count, expected_delta.reused_count);
    assert_eq!(changes.reused_digest, expected_delta.reused_digest);
    assert_eq!(after.edges(), expected.edges());

    let old_helper = symbol(&before, "helper");
    let helper = symbol(&after, "helper");
    let old_other = symbol(&before, "extra");
    let other = symbol(&after, "extra");
    let run = symbol(&after, "run");
    let sibling = symbol(&after, "second_caller");
    assert_ne!(old_helper, helper, "comment edit re-keys its target");
    assert_ne!(old_other, other);
    assert_eq!(run, symbol(&before, "run"));
    assert_eq!(sibling, symbol(&before, "second_caller"));
    for (from, to) in [(&run, &helper), (&run, &other), (&sibling, &helper)] {
        assert_eq!(
            after
                .edges()
                .iter()
                .filter(|edge| &edge.from_occurrence == from && &edge.to_occurrence == to)
                .count(),
            1,
            "each carried caller must point to the new occurrence exactly once"
        );
    }
    assert!(after.edges().iter().all(|edge| {
        edge.from_occurrence != old_helper
            && edge.to_occurrence != old_helper
            && edge.from_occurrence != old_other
            && edge.to_occurrence != old_other
    }));
    assert_eq!(
        after
            .edges()
            .iter()
            .filter(|edge| edge.to_occurrence == helper)
            .count(),
        2,
        "target fan-in survives repointing"
    );
    assert_eq!(
        after
            .edges()
            .iter()
            .filter(|edge| edge.from_occurrence == run)
            .count(),
        2,
        "untouched caller fan-out survives repointing"
    );
}
