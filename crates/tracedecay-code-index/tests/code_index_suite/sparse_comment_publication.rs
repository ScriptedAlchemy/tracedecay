//! Sparse publication must preserve the cold graph and exact chunk delta,
//! including callers whose source did not change when a target was re-keyed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use tracedecay_code_index::{
    chunks::content_digest,
    incremental::plan_chunk_increment,
    production::{
        CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionErrorV1,
        CodeIndexProductionOwnerV1, CodeIndexPublishedGenerationV1, MemorySealedPublicationStoreV1,
    },
};
use tracedecay_domain::{
    FileOccurrenceId, LanguageId, ManifestDigest, RelationEdgeKindV1, SanitizedCodeFileV1,
    SensitivityLevelV1, SnapshotFileDispositionV1, SymbolOccurrenceId,
};
use tracedecay_runtime_core::logging::set_tracing_capture;

use crate::{
    production_orchestration::{ActiveControl, config, request_with_source},
    support::{PartitionedSealV1, cold_generation, id, reseal_manifest},
};

// Count actual graph row emission, not publication writes: a redundant seal
// can regenerate the same digest and be deduplicated before reaching the store.
struct CountGraphEmissions(Arc<AtomicU64>);

impl tracing::Subscriber for CountGraphEmissions {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.name() == "code_index.seal.collect.emit"
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

fn with_graph_emission_count<T>(build: impl FnOnce() -> T) -> (T, u64) {
    let emissions = Arc::new(AtomicU64::new(0));
    let _capture = set_tracing_capture(CountGraphEmissions(Arc::clone(&emissions)));
    let result = build();
    (result, emissions.load(Ordering::Relaxed))
}

fn tree_request(util: &str, sealed_at: i64, phase: &str) -> CodeIndexBuildRequestV1 {
    let root = concat!(
        "pub mod consumer;\npub mod util;\npub mod other;\npub mod sibling;\n",
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
            "src/consumer.rs".to_owned(),
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
        .filter(|symbol| symbol.qualified_name == name)
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "fixture must have exactly one {name}");
    matches[0].clone()
}

fn assert_unrelated_calls(generation: &CodeIndexPublishedGenerationV1) {
    let target = symbol(generation, "src/other.rs::new");
    for n in 0..4 {
        let caller = symbol(generation, &format!("src/caller{n}.rs::caller{n}"));
        assert_eq!(
            generation
                .edges()
                .iter()
                .filter(|edge| {
                    edge.from_occurrence == caller
                        && edge.to_occurrence == target
                        && edge.kind == RelationEdgeKindV1::Calls
                })
                .count(),
            1,
            "caller{n} must bind the unrelated file, not just seal a descriptor"
        );
    }
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
    let (parent, cold_emissions) = with_graph_emission_count(|| {
        owner.build_and_publish(before_request.clone(), &ActiveControl)
    });
    let parent = parent.expect("parent publishes");
    assert!(parent.cold_reason().is_some());
    assert_eq!(
        cold_emissions,
        before_request.snapshot.files.len() as u64,
        "the cold control must observe actual graph work for every fixture file"
    );
    // Descriptor presence alone does not prove a resolved call. Establish
    // the four actual unrelated edges before taking the read baselines.
    let before = cold_generation(&parent);
    assert_unrelated_calls(&before);
    let envelope: serde_json::Value =
        serde_json::from_slice(parent.manifest_bytes()).expect("sealed manifest envelope");
    let segment_digest = |kind: &str, path: &str| {
        let key = before_request
            .snapshot
            .files
            .iter()
            .position(|file| file.logical_path == path)
            .expect("fixture path");
        let key = u64::try_from(key).expect("file key fits u64");
        let descriptor = envelope["generation"][kind]
            .as_array()
            .expect("segment descriptors")
            .iter()
            .find(|descriptor| descriptor["file_key"].as_u64() == Some(key))
            .unwrap_or_else(|| panic!("{path} must have a sealed {kind} descriptor"));
        serde_json::from_value::<ManifestDigest>(descriptor["segment_digest"].clone())
            .expect("segment digest")
    };
    let unrelated = (0..4)
        .map(|n| segment_digest("file_evidence", &format!("src/caller{n}.rs")))
        .map(|digest| {
            let reads = store.segment_reads(&digest);
            (digest, reads)
        })
        .collect::<Vec<_>>();
    let unrelated_files = (0..4)
        .map(|n| format!("src/caller{n}.rs"))
        .chain(["src/other.rs".to_owned()])
        .map(|path| {
            let digest = segment_digest("file_segments", &path);
            let reads = store.segment_reads(&digest);
            (path, digest, reads)
        })
        .collect::<Vec<_>>();
    let dependent = segment_digest("file_evidence", "src/consumer.rs");
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
    let (sparse, sparse_emissions) =
        with_graph_emission_count(|| owner.build_and_publish(sparse_request, &ActiveControl));
    let sparse = sparse.expect("sparse successor publishes without recapturing callers");
    assert_eq!(sparse.cold_reason(), None);

    // Observe reads before restoring either result, not on a second build.
    for (digest, reads) in unrelated {
        assert_eq!(store.segment_reads(&digest), reads, "unrelated evidence");
    }
    for (path, digest, reads) in unrelated_files {
        assert_eq!(store.segment_reads(&digest), reads, "unrelated file {path}");
    }
    assert!(store.segment_reads(&dependent) > dependent_reads);

    // Check the same measured publication before either result is restored.
    // Equal descriptors alone would miss needless resealing of identical bytes.
    let successor_envelope: serde_json::Value =
        serde_json::from_slice(sparse.manifest_bytes()).expect("successor manifest");
    let parent_pages = envelope["generation"]["code_graph_pages"]
        .as_array()
        .expect("parent graph pages");
    let successor_pages = successor_envelope["generation"]["code_graph_pages"]
        .as_array()
        .expect("successor graph pages");
    assert_eq!(parent_pages.len(), before_request.snapshot.files.len());
    assert_eq!(successor_pages.len(), parent_pages.len());
    let mut changed_graph_paths = Vec::new();
    for parent_page in parent_pages {
        let key = parent_page["file_key"].as_u64().expect("graph file key");
        let file_index = usize::try_from(key).expect("file key fits usize");
        let file = &before_request.snapshot.files[file_index];
        let path = file.logical_path.as_str();
        let successor_page = successor_pages
            .iter()
            .find(|page| page["file_key"].as_u64() == Some(key))
            .expect("every parent file retains a graph page");
        if successor_page["page_digest"] != parent_page["page_digest"] {
            changed_graph_paths.push(path);
        } else {
            assert_eq!(successor_page, parent_page, "carried graph page {path}");
        }
    }
    changed_graph_paths.sort_unstable();
    assert_eq!(
        changed_graph_paths,
        vec!["src/consumer.rs", "src/sibling.rs", "src/util.rs"],
        "only the edited file and its two real dependent owners change graph pages"
    );
    assert_eq!(
        sparse_emissions,
        changed_graph_paths.len() as u64,
        "unchanged graph pages must not be emitted again before digest deduplication"
    );

    let mut cold_owner =
        CodeIndexProductionOwnerV1::new(config(), MemorySealedPublicationStoreV1::default())
            .expect("cold owner");
    let cold = cold_owner
        .build_and_publish(cold_request, &ActiveControl)
        .expect("cold successor publishes");
    assert!(cold.cold_reason().is_some());
    let after = cold_generation(&sparse);
    let expected = cold_generation(&cold);
    assert_unrelated_calls(&after);
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
    let parent_replay = &parent
        .manifest()
        .source_commitments
        .as_ref()
        .expect("parent sealed its source")
        .full_replay_digest;
    let expected_reuse = tracedecay_domain::code_reused_partition_arc_share_digest(
        parent_replay,
        &parent.manifest().generation_id,
        &sparse.manifest().generation_id,
        expected_delta.reused_count,
    )
    .expect("sparse reuse binds the parent to the cold delta's unchanged chunks");
    assert_eq!(changes.reused_digest, expected_reuse);
    let corpus = |generation: &CodeIndexPublishedGenerationV1| {
        generation
            .chunks()
            .chunks()
            .iter()
            .map(|chunk| (chunk.id.clone(), chunk.content_digest.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(corpus(&after), corpus(&expected));
    assert_eq!(after.edges(), expected.edges());

    let old_helper = symbol(&before, "src/util.rs::helper");
    let helper = symbol(&after, "src/util.rs::helper");
    let old_other = symbol(&before, "src/util.rs::extra");
    let other = symbol(&after, "src/util.rs::extra");
    let run = symbol(&after, "src/consumer.rs::run");
    let sibling = symbol(&after, "src/sibling.rs::second_caller");
    assert_ne!(old_helper, helper, "comment edit re-keys its target");
    assert_ne!(old_other, other);
    assert_eq!(run, symbol(&before, "src/consumer.rs::run"));
    assert_eq!(sibling, symbol(&before, "src/sibling.rs::second_caller"));
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

#[test]
fn sealed_evidence_refuses_a_resealed_false_negative_target_filter() {
    let util = "pub fn helper() -> u32 { 1 }\npub fn extra() -> u32 { 2 }\n";
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), MemorySealedPublicationStoreV1::default())
            .expect("owner");
    let published = owner
        .build_and_publish(tree_request(util, 1_100_000, "filter"), &ActiveControl)
        .expect("generation publishes");
    let generation = cold_generation(&published);
    assert_unrelated_calls(&generation);
    let seal = PartitionedSealV1::of(&generation);
    let mut envelope = seal.envelope();
    let descriptor = envelope["generation"]["file_evidence"]
        .as_array_mut()
        .expect("evidence descriptors")
        .iter_mut()
        .find(|descriptor| {
            descriptor["target_filter"]
                .as_array()
                .expect("target filter words")
                .iter()
                .any(|word| word.as_u64().expect("target filter word") != 0)
        })
        .expect("real cross-file evidence must have a nonempty target filter");
    descriptor["target_filter"] = serde_json::json!([0_u64, 0_u64, 0_u64, 0_u64]);
    // Recompute the outer digest so this reaches semantic descriptor checking,
    // not an unrelated manifest authentication failure. Segment bytes stay intact.
    let tampered = reseal_manifest(envelope);
    CodeIndexPublishedGenerationV1::partitioned_text_metadata(&tampered)
        .expect("resealed metadata is structurally valid");
    let error = seal
        .restore(&tampered)
        .expect_err("a false-negative target filter must not restore");
    assert!(
        matches!(
            &error,
            CodeIndexProductionErrorV1::Contract(message)
                if message == "sealed file evidence does not match its descriptor"
        ),
        "unexpected evidence refusal: {error}"
    );
    assert_eq!(seal.restored().edges(), generation.edges());
}
