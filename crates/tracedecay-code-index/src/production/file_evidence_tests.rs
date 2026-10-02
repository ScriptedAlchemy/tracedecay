//! Sealed cross-file evidence restores what its build derived, and an edit
//! over a sealed parent reads and writes only what it changes.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_domain::EdgeAuthorityV1;

use super::sparse_differential_tests::{FIXTURE_ROOT, fixture_files, language_for, publish};
use super::worker_tests::{WorkerProjectionSink, WorkerPublicationStore, worker_config};
use super::*;
use crate::lineage::LineageKindV1;

/// A sealed store that counts the segment reads builds over it make.
#[derive(Clone, Default)]
struct CountingStoreV1 {
    inner: WorkerPublicationStore,
    reads: Arc<AtomicUsize>,
}

impl CodeIndexAtomicPublicationPort for CountingStoreV1 {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexSealedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        let Some(active) = self.inner.load_active(scope)? else {
            return Ok(None);
        };
        let read = self.inner.segment_reader();
        let reads = Arc::clone(&self.reads);
        Ok(Some(CodeIndexSealedGenerationV1::new(
            Arc::from(active.manifest_bytes()),
            Arc::new(move |request, buffer: &mut Vec<u8>| {
                reads.fetch_add(1, Ordering::Relaxed);
                read(request, buffer)
            }),
        )))
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1> {
        self.inner
            .publish_atomically(scope, expected_active_generation, generation)
    }
}

type OwnerV1 = CodeIndexProductionOwnerV1<WorkerPublicationStore, WorkerProjectionSink>;

/// The file evidence segments a sparse successor wrote, by the logical
/// path each names.
fn written_evidence(published: &CodeIndexPublishedBuildV1) -> Vec<(String, usize)> {
    let CodeIndexSealedPublicationV1::Sparse(sparse) = published.publication() else {
        panic!("the edit seals over its parent");
    };
    let generation =
        super::partitioned_codec::parse_partitioned_manifest(published.manifest_bytes())
            .expect("sealed manifest");
    let written = sparse
        .written_segments()
        .filter(|(kind, _, _)| *kind == "file_evidence")
        .map(|(_, digest, bytes)| (digest.clone(), bytes))
        .collect::<std::collections::BTreeMap<_, _>>();
    generation
        .file_evidence
        .iter()
        .filter_map(|descriptor| {
            let bytes = written.get(&descriptor.segment_digest)?;
            Some((
                generation.snapshot.files[descriptor.file_key as usize]
                    .logical_path
                    .clone(),
                *bytes,
            ))
        })
        .collect()
}

/// Every language fixture's sealed cross-file evidence restores to exactly
/// what its cold build derived, and an edit over it restores identity
/// lineage for every carried file and moved lineage for the edited one.
#[test]
fn sealed_file_evidence_restores_the_built_edges_calls_and_lineage() {
    for language in ["rust", "typescript", "python", "go", "java", "ruby"] {
        let tree = fixture_files(&Path::new(FIXTURE_ROOT).join(language));
        let store = WorkerPublicationStore::default();
        let mut owner = OwnerV1::new(worker_config(), store.clone(), WorkerProjectionSink)
            .expect("production owner");
        let cold = publish(&mut owner, &tree, None, false, 1_000_000);
        let built = cold.decoded().expect("a cold seal holds its generation");
        assert!(
            built
                .edges
                .iter()
                .any(|edge| edge.authority == EdgeAuthorityV1::NameResolved),
            "{language}: the fixture resolves cross-file edges"
        );
        let scope = CodeIndexGenerationScopeV1::for_snapshot(built.snapshot());
        let restored = store
            .decode_active(&scope)
            .expect("restores")
            .expect("active");
        assert_eq!(restored.edges, built.edges, "{language} cold: edges");
        assert_eq!(
            restored.unresolved_calls, built.unresolved_calls,
            "{language} cold: calls"
        );
        assert!(
            restored.lineage.is_empty(),
            "{language} cold: a cold build has no lineage"
        );

        let with_symbols = built
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
        assert_eq!(
            edited.cold_reason(),
            None,
            "{language}: the edit seals over its parent"
        );
        let restored = store
            .decode_active(&scope)
            .expect("restores")
            .expect("active");
        let continuations = restored
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
        assert_eq!(
            restored.lineage.len(),
            restored.symbols.symbols.len(),
            "{language}: every symbol has lineage"
        );
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

/// An edit to one file of a corpus whose every file calls into one hub
/// reads and writes a bounded number of segments: it decodes neither the
/// parent nor the hub's callers, and it reseals only the edited file's
/// evidence, on the first edit after a cold build and on every later one.
#[test]
fn an_edit_reads_and_writes_only_what_it_changes() {
    const LEAVES: usize = 300;
    let store = CountingStoreV1::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), store.clone(), WorkerProjectionSink)
            .expect("production owner");
    let base_tree = hub_tree(LEAVES, "1");
    let edited_leaf = base_tree
        .iter()
        .position(|(path, _)| path == "src/leaf_000.rs")
        .expect("the edited leaf");
    let base = publish(&mut owner, &base_tree, None, false, 2_000_000);
    assert_eq!(
        base.decoded()
            .expect("cold seal")
            .edges
            .iter()
            .filter(|edge| edge.authority == EdgeAuthorityV1::NameResolved)
            .count(),
        LEAVES,
        "every leaf binds its call into the hub"
    );
    for (sealed_at, offset) in [(2_100_000, "1000"), (2_200_000, "2000")] {
        store.reads.store(0, Ordering::Relaxed);
        let edited = publish(
            &mut owner,
            &hub_tree(LEAVES, offset),
            Some(edited_leaf),
            true,
            sealed_at,
        );
        let reads = store.reads.load(Ordering::Relaxed);
        assert!(
            reads < 24,
            "an edit to one of {LEAVES} files read {reads} parent segments"
        );
        let written = written_evidence(&edited);
        assert_eq!(
            written
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["src/leaf_000.rs"]
        );
        assert!(written.iter().map(|(_, bytes)| bytes).sum::<usize>() < 4096);
    }
    let scope = CodeIndexGenerationScopeV1::for_snapshot(base.snapshot());
    let restored = store
        .inner
        .decode_active(&scope)
        .expect("restores")
        .expect("active");
    assert_eq!(
        restored
            .edges
            .iter()
            .filter(|edge| edge.authority == EdgeAuthorityV1::NameResolved)
            .count(),
        LEAVES
    );
}
