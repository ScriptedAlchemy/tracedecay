//! A small refresh publishes its code graph as a delta over the generation
//! it replaces, and serves exactly what a cold build of the same tree serves.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_code_index::graph_projection::{
    CodeGraphProjectionStore, CodeGraphSemanticEdgeV1, CodeGraphSymbolSummaryV1,
};
use tracedecay_code_index_retention::code_index_generations::DurablePublicationPointerV1;
use tracedecay_code_index_runtime::CodeGraphReplayBindingV1;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1, scoped_code_index_store_root,
};
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::{CodeGenerationId, ProjectId, SymbolOccurrenceId};
use tracedecay_graph_db::{
    GraphGenerationRowSpill, GraphGenerationRows, GraphProjectorRevision, NeverCancelled,
    VerifiedGraphSnapshot,
};

use super::super::DaemonSessionRuntimeRegistryV1;

const MODULES: usize = 120;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture command failed: {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A module that defines a type, a value, and a function calling two other
/// modules' values through crate paths, so every module has cross-file
/// callers and callees.
fn module_source(index: usize, value: &str) -> String {
    let previous = (index + MODULES - 1) % MODULES;
    let far = (index + 7) % MODULES;
    format!(
        "pub struct Item{index:03} {{ pub value: usize }}\n\
         impl Item{index:03} {{\n    pub fn get(&self) -> usize {{ self.value }}\n}}\n\
         pub fn value_{index:03}() -> usize {{ {value} }}\n\
         pub fn call_{index:03}() -> usize {{\n    \
         crate::m{previous:03}::value_{previous:03}() + crate::m{far:03}::value_{far:03}()\n}}\n"
    )
}

fn write_corpus(project_root: &Path) {
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    let mut lib = String::new();
    for index in 0..MODULES {
        lib.push_str(&format!("pub mod m{index:03};\n"));
        std::fs::write(
            project_root.join(format!("src/m{index:03}.rs")),
            module_source(index, &index.to_string()),
        )
        .expect("module source");
    }
    std::fs::write(project_root.join("src/lib.rs"), lib).expect("crate root");
}

/// Three files change: one body edit that moves every symbol occurrence of
/// its file, one new function called in its own file, and one deleted
/// function whose two cross-file callers stay behind unchanged.
fn edit_three_files(project_root: &Path) {
    std::fs::write(project_root.join("src/m003.rs"), module_source(3, "3 + 1")).expect("edit m003");
    std::fs::write(
        project_root.join("src/m010.rs"),
        module_source(10, "added_010()")
            + "pub fn added_010() -> usize { crate::m050::value_050() }\n",
    )
    .expect("edit m010");
    let without_value = module_source(20, "20").replace("pub fn value_020() -> usize { 20 }\n", "");
    std::fs::write(project_root.join("src/m020.rs"), without_value).expect("edit m020");
}

fn sealed_binding(scoped_store: &Path) -> (CodeGenerationId, CodeGraphReplayBindingV1) {
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(scoped_store.join("active-code-generation-v1.json"))
            .expect("active generation pointer"),
    )
    .expect("decode active generation pointer");
    (
        CodeGenerationId::new(pointer.generation_id).expect("generation id"),
        CodeGraphReplayBindingV1 {
            generations_root: scoped_store.join("code-generations-v1"),
            sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
                pointer.state_digest,
            )
            .expect("sealed state digest"),
        },
    )
}

/// Every sealed receipt under `root`, with its directory.
fn sealed_receipts(root: &Path) -> Vec<(PathBuf, serde_json::Value)> {
    let mut receipts = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.map(|entry| entry.expect("directory entry")) {
            let path = entry.path();
            if entry.file_type().expect("entry type").is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "sealed.json") {
                let receipt = serde_json::from_slice(&std::fs::read(&path).expect("receipt"))
                    .expect("receipt json");
                receipts.push((directory.clone(), receipt));
            }
        }
    }
    receipts
}

fn receipt_for(root: &Path, generation: &str) -> (PathBuf, serde_json::Value) {
    sealed_receipts(root)
        .into_iter()
        .find(|(_, receipt)| receipt["generation"] == generation)
        .unwrap_or_else(|| panic!("no sealed receipt for {generation}"))
}

/// The row count a recorded row sum carries in its last eight bytes.
fn rows_in(sum: &serde_json::Value) -> u64 {
    let hex = sum.as_str().expect("row sum hex");
    u64::from_str_radix(&hex[hex.len() - 16..], 16).expect("row count")
}

/// The worktree source every generation of the fixture publishes from.
struct RuntimeSource {
    project: ProjectId,
    repository: tracedecay_domain::RepositoryId,
    worktree: tracedecay_domain::WorktreeId,
    reference: Option<tracedecay_domain::RefId>,
}

impl RuntimeSource {
    async fn retain(
        &self,
        registry: &DaemonSessionRuntimeRegistryV1,
        database: &Arc<tracedecay_runtime_core::db::Database>,
        generation: &CodeGenerationId,
        binding: CodeGraphReplayBindingV1,
    ) -> super::super::Result<super::RetainedCodeGraphRuntimeV1> {
        registry
            .retain_code_graph_runtime(
                self.project.clone(),
                self.repository.clone(),
                self.worktree.clone(),
                self.reference.clone(),
                generation.clone(),
                Arc::clone(database),
                binding,
            )
            .await
    }
}

/// Every row a snapshot serves, by identity, as the graph API decodes it.
fn scan_rows(
    snapshot: &VerifiedGraphSnapshot,
) -> (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
) {
    let mut entities = std::collections::BTreeMap::new();
    let mut relations = std::collections::BTreeMap::new();
    let (mut after_entity, mut after_relation) = (None, None);
    let (mut entities_done, mut relations_done) = (false, false);
    while !entities_done || !relations_done {
        let page = snapshot
            .read_projection(tracedecay_graph_db::GraphProjectionReadRequest {
                namespace: snapshot.projection().namespace.clone(),
                projection: snapshot.projection().projection.clone(),
                after_entity: after_entity.clone(),
                after_relation: after_relation.clone(),
                max_entities: if entities_done { 0 } else { 1000 },
                max_relations: if relations_done { 0 } else { 1000 },
                cancellation: Arc::new(NeverCancelled),
            })
            .unwrap();
        if !entities_done {
            for entity in page.entities {
                entities.insert(entity.identity.as_str().to_owned(), format!("{entity:?}"));
            }
            after_entity = page.next_entity;
            entities_done = after_entity.is_none();
        }
        if !relations_done {
            for relation in page.relations {
                relations.insert(
                    relation.identity.as_str().to_owned(),
                    format!("{relation:?}"),
                );
            }
            after_relation = page.next_relation;
            relations_done = after_relation.is_none();
        }
    }
    (entities, relations)
}

struct Answers {
    symbols: Vec<CodeGraphSymbolSummaryV1>,
    callers: Vec<Vec<CodeGraphSemanticEdgeV1>>,
    callees: Vec<Vec<CodeGraphSemanticEdgeV1>>,
    exact: Vec<Vec<CodeGraphSymbolSummaryV1>>,
    file_dependents: Vec<(String, BTreeSet<String>)>,
}

/// What the graph tools read: every symbol, each one's callers and callees,
/// exact qualified-name lookup, and the file dependency map.
fn answers(snapshot: VerifiedGraphSnapshot, generation: &CodeGenerationId) -> Answers {
    let cancellation =
        || -> Arc<dyn tracedecay_graph_db::GraphCancellation> { Arc::new(NeverCancelled) };
    let store = CodeGraphProjectionStore::from_verified_snapshot(snapshot, generation.clone())
        .expect("projection store");
    store
        .mark_interactive_catalog_warming()
        .expect("mark warming");
    store.warm_serving_engine().expect("warm serving engine");
    store
        .warm_interactive_catalog_with_cancellation(cancellation())
        .expect("warm catalog");
    let reader = store
        .interactive_reader_with_cancellation(generation, cancellation())
        .expect("interactive reader");
    let page = reader
        .symbols_page(None, 100_000, cancellation())
        .expect("every symbol");
    assert!(!page.has_more);
    let seeds = page
        .symbols
        .iter()
        .map(|symbol| symbol.occurrence.clone())
        .collect::<Vec<SymbolOccurrenceId>>();
    let exact = page
        .symbols
        .iter()
        .filter_map(|symbol| symbol.metadata.as_ref())
        .map(|metadata| {
            reader
                .resolve_qualified_name(&metadata.qualified_name, None, 8, cancellation())
                .expect("exact lookup")
        })
        .collect();
    let mut file_dependents = reader
        .file_dependencies(cancellation())
        .expect("file dependencies")
        .adjacency
        .iter()
        .map(|(file, dependencies)| (file.clone(), dependencies.iter().cloned().collect()))
        .collect::<Vec<_>>();
    file_dependents.sort();
    Answers {
        callers: reader
            .callers(&seeds, &[], 1_000_000, cancellation())
            .expect("callers"),
        callees: reader
            .callees(&seeds, &[], 1_000_000, cancellation())
            .expect("callees"),
        symbols: page.symbols,
        exact,
        file_dependents,
    }
}

/// Fails on a build that re-encodes the whole graph for a small refresh:
/// the refresh's artifact must be layered over its predecessor's container
/// by hard link and encode a small fraction of the rows a cold build
/// encodes. Fails as well if the layered generation records a digest or
/// answers any read differently from a cold build of the same tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_three_file_refresh_seals_a_delta_that_serves_like_its_cold_build() {
    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let project_root = root.join("project");
    write_corpus(&project_root);
    git(&project_root, &["init", "-q", "-b", "main"]);
    git(&project_root, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project_root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-qm", "layered refresh corpus"]);
    let project_id = ProjectId::new("project.layered-refresh").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let canonical_project = project_root.canonicalize().expect("canonical project root");
    let scoped_store =
        scoped_code_index_store_root(&root.join("code-index-store"), &canonical_project);
    let seal_next = || {
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            project_id.clone(),
            &canonical_project,
            scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open worktree scheduler");
        scheduler.reconcile_now().expect("seal a generation");
        let latest = scheduler.latest_complete().expect("complete generation");
        let generation = latest.generation();
        (
            generation.snapshot().repository.clone(),
            generation.snapshot().reference.clone(),
            scheduler.identity().worktree_id().clone(),
            generation.manifest().parent_generation.clone(),
        )
    };
    let (repository, reference, worktree, _) = seal_next();
    let (parent_generation, parent_binding) = sealed_binding(&scoped_store);

    let profile_root = root.join("profile");
    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        45,
        "layered refresh",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(
        profile_identity::load_or_create(&profile_root).expect("profile identity"),
    )
    .await
    .expect("session runtime registry");
    let project_database = registry
        .project_memory(project_id.clone(), [canonical_project.clone()])
        .await
        .expect("project graph database");
    let source = RuntimeSource {
        project: project_id.clone(),
        repository,
        worktree,
        reference,
    };
    let parent_runtime = source
        .retain(
            &registry,
            &project_database,
            &parent_generation,
            parent_binding,
        )
        .await
        .expect("retain the parent graph runtime");
    let parent = parent_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("publish the parent graph cold");

    edit_three_files(&project_root);
    git(&project_root, &["add", "."]);
    git(&project_root, &["commit", "-qm", "edit three files"]);
    let (_, _, _, child_parent) = seal_next();
    assert_eq!(child_parent.as_ref(), Some(&parent_generation));
    let (child_generation, child_binding) = sealed_binding(&scoped_store);
    let child_runtime = source
        .retain(
            &registry,
            &project_database,
            &child_generation,
            child_binding.clone(),
        )
        .await
        .expect("retain the child graph runtime");
    let layered = child_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("publish the refresh");

    // The refresh sealed a delta whose base is the parent's own container.
    let (parent_directory, parent_receipt) =
        receipt_for(&root.join("profile"), parent.generation().as_str());
    let (child_directory, child_receipt) =
        receipt_for(&root.join("profile"), layered.generation().as_str());
    assert_eq!(parent_receipt["form"], "compact");
    assert_eq!(child_receipt["form"], "layered");
    assert_eq!(
        child_receipt["base"]["generation"],
        parent.generation().as_str()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let parent_container = std::fs::metadata(parent_directory.join("generation.grafeo"))
            .expect("parent container");
        let base_link = std::fs::metadata(child_directory.join("base.grafeo")).expect("base link");
        assert_eq!(
            (base_link.dev(), base_link.ino()),
            (parent_container.dev(), parent_container.ino()),
            "the refresh references the parent's container instead of re-encoding it"
        );
    }
    let cold_rows =
        child_receipt["entities"].as_u64().unwrap() + child_receipt["relations"].as_u64().unwrap();
    let delta_rows = rows_in(&child_receipt["row_sum"])
        + rows_in(&child_receipt["base"]["hidden_row_sum"])
        - rows_in(&child_receipt["base"]["row_sum"]);
    assert!(
        delta_rows * 10 < cold_rows,
        "a three-file refresh encoded {delta_rows} of its generation's {cold_rows} rows"
    );

    // The same child generation published cold in an isolated profile.
    let cold_profile = root.join("profile-cold");
    let _cold_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &cold_profile,
        46,
        "layered refresh cold reference",
    )
    .expect("cold daemon database scope");
    let cold_registry = DaemonSessionRuntimeRegistryV1::open(
        profile_identity::load_or_create(&cold_profile).expect("cold profile identity"),
    )
    .await
    .expect("cold session runtime registry");
    let cold_database = cold_registry
        .project_memory(project_id.clone(), [canonical_project.clone()])
        .await
        .expect("cold project graph database");
    let cold_runtime = source
        .retain(
            &cold_registry,
            &cold_database,
            &child_generation,
            child_binding,
        )
        .await
        .expect("retain the cold graph runtime");
    let cold = cold_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("publish the child graph cold");
    let (_, cold_receipt) = receipt_for(&cold_profile, cold.generation().as_str());
    assert_eq!(cold_receipt["form"], "compact");
    assert_eq!(cold.generation(), layered.generation());
    // Namespaces are per profile and bind the digest, so the digest check
    // runs in the layered generation's own namespace, and the cross-profile
    // check compares every served row.
    let cold_in_namespace = GraphGenerationRows::from(
        super::super::code_graph_manifest::spill_sealed_generation_graph_from_roots(
            &child_runtime.generations_root,
            &child_runtime.replay_root,
            &child_runtime.sealed_state_digest,
            &child_generation,
            layered.projection().clone(),
            &GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .expect("projector revision"),
            GraphGenerationRowSpill::create(root.join("cold-rows"), layered.projection().clone())
                .expect("cold row spill"),
            &|| Ok(()),
        )
        .expect("cold rows of the child"),
    );
    assert_eq!(
        cold_in_namespace
            .expected_recovered_digest(&|| Ok(()))
            .expect("cold digest"),
        layered.verified_head().recovered_digest,
        "the layered generation holds exactly the cold build's rows"
    );
    assert_eq!(
        cold_in_namespace.row_counts(),
        (
            child_receipt["entities"].as_u64().unwrap() as usize,
            child_receipt["relations"].as_u64().unwrap() as usize
        )
    );
    assert_eq!(scan_rows(&layered), scan_rows(&cold));
    assert_eq!(
        (
            cold_receipt["entities"].clone(),
            cold_receipt["relations"].clone()
        ),
        (
            child_receipt["entities"].clone(),
            child_receipt["relations"].clone()
        )
    );

    let graph_generation = child_generation.clone();
    let from_layered = answers(layered, &graph_generation);
    let from_cold = answers(cold, &graph_generation);
    assert!(
        from_cold.symbols.len() > MODULES * 3,
        "the cold graph serves the corpus: {} symbols",
        from_cold.symbols.len()
    );
    assert!(
        from_cold
            .callers
            .iter()
            .filter(|edges| !edges.is_empty())
            .count()
            > MODULES,
        "cross-file callers resolve in the cold graph"
    );
    assert_eq!(from_layered.symbols, from_cold.symbols);
    assert_eq!(from_layered.callers, from_cold.callers);
    assert_eq!(from_layered.callees, from_cold.callees);
    assert_eq!(from_layered.exact, from_cold.exact);
    assert_eq!(from_layered.file_dependents, from_cold.file_dependents);
    drop((parent_runtime, child_runtime, cold_runtime));
}
