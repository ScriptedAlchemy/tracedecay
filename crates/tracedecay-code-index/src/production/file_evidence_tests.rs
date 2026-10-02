use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use tracedecay_domain::EdgeAuthorityV1;

use super::changed_resolution_tests::{FIXTURE_ROOT, fixture_files, language_for, publish};
use super::worker_tests::{
    WorkerProjectionSink, WorkerPublicationStore, partitioned_restore, worker_config,
};
use super::*;
use crate::lineage::LineageKindV1;

type OwnerV1 = CodeIndexProductionOwnerV1<WorkerPublicationStore, WorkerProjectionSink>;

/// One seal of a generation over its parent's manifest: the manifest, every
/// stored segment, and the bytes this seal published itself.
struct SealV1 {
    manifest: Vec<u8>,
    segments: BTreeMap<String, Vec<u8>>,
    published_files: BTreeSet<String>,
    generation_evidence_bytes: usize,
}

fn seal(generation: &CodeIndexPublishedGenerationV1, parent: Option<&SealV1>) -> SealV1 {
    let mut segments = parent
        .map(|parent| parent.segments.clone())
        .unwrap_or_default();
    let mut published_files = BTreeSet::new();
    let mut generation_evidence_bytes = 0;
    let mut evidence_pack = Vec::new();
    let manifest = generation
        .encode_partitioned_sealed_with_parent(
            parent.map(|parent| parent.manifest.as_slice()),
            |publication| {
                match publication {
                    SealedGenerationSegmentPublicationV1::File { digest, bytes }
                    | SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes } => {
                        published_files.insert(digest.as_str().to_owned());
                        segments.insert(digest.as_str().to_owned(), bytes.to_vec());
                    }
                    SealedGenerationSegmentPublicationV1::CodeGraphPage {
                        page_digest,
                        bytes,
                        ..
                    } => {
                        segments.insert(page_digest.as_str().to_owned(), bytes.to_vec());
                    }
                    SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                        bytes, ..
                    } => {
                        generation_evidence_bytes += bytes.len();
                        evidence_pack.extend_from_slice(bytes);
                    }
                    SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                        segment_digest,
                        ..
                    } => {
                        segments.insert(
                            segment_digest.as_str().to_owned(),
                            std::mem::take(&mut evidence_pack),
                        );
                    }
                }
                Ok(())
            },
        )
        .expect("generation seals");
    SealV1 {
        manifest,
        segments,
        published_files,
        generation_evidence_bytes,
    }
}

/// The logical path of each file evidence segment `seal` describes, by digest.
fn file_evidence_paths(seal: &SealV1) -> BTreeMap<String, String> {
    let envelope = serde_json::from_slice::<serde_json::Value>(&seal.manifest).expect("manifest");
    let generation = &envelope["generation"];
    let files = generation["snapshot"]["files"]
        .as_array()
        .expect("snapshot files");
    generation["file_evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|evidence| {
            let key = usize::try_from(evidence["file_key"].as_u64().expect("file key"))
                .expect("file key");
            (
                evidence["segment_digest"]
                    .as_str()
                    .expect("evidence digest")
                    .to_owned(),
                files[key]["logical_path"]
                    .as_str()
                    .expect("logical path")
                    .to_owned(),
            )
        })
        .collect()
}

fn assert_restores_as_built(
    built: &CodeIndexPublishedGenerationV1,
    sealed: &SealV1,
    context: &str,
) {
    let restored = partitioned_restore(&sealed.manifest, &sealed.segments);
    assert_eq!(restored.edges, built.edges, "{context}: edges");
    assert_eq!(
        restored.unresolved_calls, built.unresolved_calls,
        "{context}: call limitations"
    );
    assert_eq!(restored.lineage, built.lineage, "{context}: lineage");
}

/// Every language fixture's sealed cross-file evidence restores to exactly
/// what its build derived, on a cold build and on an edit over it, whose
/// lineage carries implicit, moved, and explicit rows.
#[test]
fn sealed_file_evidence_restores_the_built_edges_calls_and_lineage() {
    for language in ["rust", "typescript", "python", "go", "java", "ruby"] {
        let tree = fixture_files(&Path::new(FIXTURE_ROOT).join(language));
        let mut owner = OwnerV1::new(
            worker_config(),
            WorkerPublicationStore::default(),
            WorkerProjectionSink,
        )
        .expect("production owner");
        let cold = publish(&mut owner, &tree, None, false, 1_000_000);
        assert!(
            cold.edges
                .iter()
                .any(|edge| edge.authority == EdgeAuthorityV1::NameResolved),
            "{language}: the fixture resolves cross-file edges"
        );
        let cold_seal = seal(&cold, None);
        assert_restores_as_built(&cold, &cold_seal, &format!("{language} cold"));

        let with_symbols = cold
            .files
            .iter()
            .filter(|file| !file.artifacts.symbols.is_empty())
            .map(|file| file.authority.logical_path.as_str())
            .collect::<BTreeSet<_>>();
        let index = tree
            .iter()
            .position(|(path, _)| {
                !matches!(language_for(path), "toml" | "json")
                    && !path.ends_with("go.mod")
                    && with_symbols.contains(path.as_str())
            })
            .expect("a source file with symbols");
        let mut edited_tree = tree.clone();
        edited_tree[index].1 = format!("\n\n{}", tree[index].1);
        let edited = publish(&mut owner, &edited_tree, Some(index), true, 1_100_000);
        let continuations = edited
            .lineage
            .iter()
            .map(|candidate| {
                (
                    candidate.kind,
                    candidate.prior_occurrence == candidate.current_occurrence,
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            continuations,
            BTreeSet::from([
                (LineageKindV1::Unchanged, false),
                (LineageKindV1::Unchanged, true)
            ]),
            "{language}: the edit moves the edited file's symbols and keeps every other"
        );
        let edited_seal = seal(&edited, Some(&cold_seal));
        assert_restores_as_built(&edited, &edited_seal, &format!("{language} edit"));
    }
}

/// A crate of `leaves` files that each call into `src/hub.rs`.
fn hub_tree(leaves: usize, edit: &str) -> Vec<(String, String)> {
    let mut lib = String::from("pub mod hub;\n");
    let mut tree = vec![(
        "Cargo.toml".to_owned(),
        "[package]\nname = \"hubcrate\"\nversion = \"0.1.0\"\n".to_owned(),
    )];
    for leaf in 0..leaves {
        lib.push_str(&format!("pub mod leaf_{leaf:03};\n"));
        let offset = if leaf == 0 { edit } else { "1" };
        tree.push((
            format!("src/leaf_{leaf:03}.rs"),
            format!("pub fn leaf_{leaf:03}() -> u32 {{\n    crate::hub::hub() + {offset}\n}}\n"),
        ));
    }
    tree.push((
        "src/hub.rs".to_owned(),
        "pub fn hub() -> u32 {\n    7\n}\n".to_owned(),
    ));
    tree.push(("src/lib.rs".to_owned(), lib));
    tree.sort();
    tree
}

/// An edit publishes the evidence of the files it changes. Every other
/// file's cross-file edges and lineage keep their sealed bytes, so the bytes
/// an edit writes for evidence do not grow with the corpus's cross-file
/// edges.
#[test]
fn an_edit_republishes_only_the_evidence_of_the_file_it_changes() {
    const LEAVES: usize = 300;
    let mut owner = OwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
    .expect("production owner");
    let base_tree = hub_tree(LEAVES, "1");
    let edited_leaf = base_tree
        .iter()
        .position(|(path, _)| path == "src/leaf_000.rs")
        .expect("the edited leaf");
    let base = publish(&mut owner, &base_tree, None, false, 2_000_000);
    assert_eq!(
        base.edges
            .iter()
            .filter(|edge| edge.authority == EdgeAuthorityV1::NameResolved)
            .count(),
        LEAVES,
        "every leaf binds its call into the hub"
    );
    let base_seal = seal(&base, None);
    // The first edit after a cold build seals every file's lineage once.
    let first = publish(
        &mut owner,
        &hub_tree(LEAVES, "1000"),
        Some(edited_leaf),
        true,
        2_100_000,
    );
    let first_seal = seal(&first, Some(&base_seal));
    let second = publish(
        &mut owner,
        &hub_tree(LEAVES, "2000"),
        Some(edited_leaf),
        true,
        2_200_000,
    );
    let second_seal = seal(&second, Some(&first_seal));
    assert_restores_as_built(&second, &second_seal, "second edit");

    let evidence = file_evidence_paths(&second_seal);
    let evidence_bytes = second_seal.generation_evidence_bytes
        + evidence
            .keys()
            .filter(|digest| second_seal.published_files.contains(*digest))
            .map(|digest| second_seal.segments[digest].len())
            .sum::<usize>();
    assert!(
        evidence_bytes < 4096,
        "an edit to one of {LEAVES} files wrote {evidence_bytes} evidence bytes ({} in the generation stream)",
        second_seal.generation_evidence_bytes
    );
    let published = evidence
        .iter()
        .filter(|(digest, _)| second_seal.published_files.contains(*digest))
        .map(|(_, path)| path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(published, ["src/leaf_000.rs"]);
    assert_eq!(
        evidence.len(),
        file_evidence_paths(&first_seal).len(),
        "every file keeps its evidence segment"
    );
}
