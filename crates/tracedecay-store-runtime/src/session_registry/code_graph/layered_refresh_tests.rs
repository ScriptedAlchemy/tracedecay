//! A small refresh publishes its code graph as a delta over the generation
//! it replaces, and serves exactly what a cold build of the same tree serves.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracedecay_code_index::graph_projection::{
    CodeGraphLayeredReportV1, CodeGraphProjectionStore, CodeGraphSemanticEdgeV1,
    CodeGraphSymbolSummaryV1,
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
fn module_source(modules: usize, index: usize, value: &str) -> String {
    let previous = (index + modules - 1) % modules;
    let far = (index + 7) % modules;
    format!(
        "pub struct Item{index:03} {{ pub value: usize }}\n\
         impl Item{index:03} {{\n    pub fn get(&self) -> usize {{ self.value }}\n}}\n\
         pub fn value_{index:03}() -> usize {{ {value} }}\n\
         pub fn call_{index:03}() -> usize {{\n    \
         crate::m{previous:03}::value_{previous:03}() + crate::m{far:03}::value_{far:03}()\n}}\n"
    )
}

fn write_corpus(project_root: &Path, modules: usize) {
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    let mut lib = String::new();
    for index in 0..modules {
        lib.push_str(&format!("pub mod m{index:03};\n"));
        std::fs::write(
            project_root.join(format!("src/m{index:03}.rs")),
            module_source(modules, index, &index.to_string()),
        )
        .expect("module source");
    }
    std::fs::write(project_root.join("src/lib.rs"), lib).expect("crate root");
}

/// Three files change: one body edit that moves every symbol occurrence of
/// its file, one new function called in its own file, and one deleted
/// function whose two cross-file callers stay behind unchanged.
fn edit_three_files(project_root: &Path, modules: usize) {
    std::fs::write(
        project_root.join("src/m003.rs"),
        module_source(modules, 3, "3 + 1"),
    )
    .expect("edit m003");
    std::fs::write(
        project_root.join("src/m010.rs"),
        module_source(modules, 10, "added_010()")
            + "pub fn added_010() -> usize { crate::m050::value_050() }\n",
    )
    .expect("edit m010");
    let without_value =
        module_source(modules, 20, "20").replace("pub fn value_020() -> usize { 20 }\n", "");
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

/// The fixture repository, its code-index store, and the daemon profile the
/// refreshes publish through.
struct RefreshFixture {
    root: PathBuf,
    project_root: PathBuf,
    canonical_project: PathBuf,
    scoped_store: PathBuf,
    project_id: ProjectId,
}

impl RefreshFixture {
    /// A committed corpus of `modules` modules under `root`.
    fn create(root: &Path, modules: usize) -> Self {
        let project_root = root.join("project");
        write_corpus(&project_root, modules);
        git(&project_root, &["init", "-q", "-b", "main"]);
        git(&project_root, &["config", "user.name", "TraceDecay Test"]);
        git(
            &project_root,
            &["config", "user.email", "tracedecay@example.invalid"],
        );
        let project_id = ProjectId::new("project.layered-refresh").expect("project id");
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            &project_root,
            project_id.as_str(),
        )
        .expect("project enrollment");
        let canonical_project = project_root.canonicalize().expect("canonical project root");
        let fixture = Self {
            scoped_store: scoped_code_index_store_root(
                &root.join("code-index-store"),
                &canonical_project,
            ),
            root: root.to_path_buf(),
            project_root,
            canonical_project,
            project_id,
        };
        fixture.commit("layered refresh corpus");
        fixture
    }

    fn commit(&self, message: &str) {
        git(&self.project_root, &["add", "-A"]);
        git(&self.project_root, &["commit", "-qm", message]);
    }

    /// Seals the worktree's next code generation; returns its runtime source,
    /// generation, parent, and replay binding.
    fn seal(
        &self,
    ) -> (
        RuntimeSource,
        CodeGenerationId,
        Option<CodeGenerationId>,
        CodeGraphReplayBindingV1,
    ) {
        let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
            self.project_id.clone(),
            &self.canonical_project,
            self.scoped_store.clone(),
            Arc::new(SharedCodeIndexBytePoolV1::default()),
        )
        .expect("open worktree scheduler");
        scheduler.reconcile_now().expect("seal a generation");
        let latest = scheduler.latest_complete().expect("complete generation");
        let generation = latest.generation();
        let source = RuntimeSource {
            project: self.project_id.clone(),
            repository: generation.snapshot().repository.clone(),
            worktree: scheduler.identity().worktree_id().clone(),
            reference: generation.snapshot().reference.clone(),
        };
        let parent = generation.manifest().parent_generation.clone();
        drop(latest);
        drop(scheduler);
        let (sealed, binding) = sealed_binding(&self.scoped_store);
        (source, sealed, parent, binding)
    }

    async fn open_profile(
        &self,
        name: &str,
        epoch: u64,
    ) -> (
        tracedecay_runtime_core::db::DaemonDatabaseScope,
        DaemonSessionRuntimeRegistryV1,
        Arc<tracedecay_runtime_core::db::Database>,
    ) {
        let profile = self.root.join(name);
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(&profile, epoch, name)
            .expect("daemon database scope");
        let registry = DaemonSessionRuntimeRegistryV1::open(
            profile_identity::load_or_create(&profile).expect("profile identity"),
        )
        .await
        .expect("session runtime registry");
        let database = registry
            .project_memory(self.project_id.clone(), [self.canonical_project.clone()])
            .await
            .expect("project graph database");
        (scope, registry, database)
    }
}

/// `layered`, published by `runtime`, holds exactly the rows a cold build
/// of its code generation holds: the same digest over rows spilled cold in
/// its own namespace, and, published cold in a fresh profile, the same
/// served rows and the same answers to every graph read.
async fn assert_matches_cold_build(
    fixture: &RefreshFixture,
    source: &RuntimeSource,
    runtime: &super::RetainedCodeGraphRuntimeV1,
    layered: VerifiedGraphSnapshot,
    binding: CodeGraphReplayBindingV1,
    profile: &str,
    epoch: u64,
) {
    let generation = runtime.generation_id.clone();
    // Namespaces are per profile and bind the digest, so the digest check
    // runs in the layered generation's own namespace, and the cross-profile
    // check compares every served row.
    let cold_in_namespace = GraphGenerationRows::from(
        super::super::code_graph_manifest::spill_sealed_generation_graph_from_roots(
            &runtime.generations_root,
            &runtime.replay_root,
            &runtime.sealed_state_digest,
            &generation,
            layered.projection().clone(),
            &GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .expect("projector revision"),
            GraphGenerationRowSpill::create(
                fixture.root.join(format!("{profile}-rows")),
                layered.projection().clone(),
            )
            .expect("cold row spill"),
            &|| Ok(()),
        )
        .expect("cold rows"),
    );
    assert_eq!(
        cold_in_namespace
            .expected_recovered_digest(&|| Ok(()))
            .expect("cold digest"),
        layered.verified_head().recovered_digest,
        "the layered generation holds exactly the cold build's rows"
    );

    let (_scope, registry, database) = fixture.open_profile(profile, epoch).await;
    let cold_runtime = source
        .retain(&registry, &database, &generation, binding)
        .await
        .expect("retain the cold graph runtime");
    let cold = cold_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("publish the generation cold");
    let (_, cold_receipt) = receipt_for(&fixture.root.join(profile), cold.generation().as_str());
    assert_eq!(cold_receipt["form"], "compact");
    assert_eq!(cold.generation(), layered.generation());
    assert_eq!(scan_rows(&layered), scan_rows(&cold));

    let from_layered = answers(layered, &generation);
    let from_cold = answers(cold, &generation);
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
    drop(cold_runtime);
}

/// The report of the delta `runtime`'s publication builds its rows as, or
/// `None` when it builds them cold. Taken before the publication, which
/// retires the replay of the parent the delta layers over.
fn layered_report(runtime: &super::RetainedCodeGraphRuntimeV1) -> Option<CodeGraphLayeredReportV1> {
    let projection = tracedecay_code_index::graph_projection::code_graph_projection_identity(
        runtime.authority.namespace().clone(),
    )
    .expect("code graph projection");
    let authority_lease: Arc<dyn super::RetainedGraphStoreLeaseV1> = runtime.authority.clone();
    let registration = || super::GraphDbRegistration {
        authority_lease: Arc::clone(&authority_lease),
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(super::AtomicGraphCancellationV1::new(Arc::clone(
            &runtime.lifecycle_cancelled,
        ))),
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(300),
    };
    runtime
        .build_graph_rows(
            &projection,
            &GraphProjectorRevision::try_from(
                tracedecay_code_index::graph_projection::CODE_GRAPH_PROJECTOR_REVISION.to_owned(),
            )
            .expect("projector revision"),
            &registration,
            &|| Ok(()),
        )
        .expect("rebuild the generation's rows")
        .1
}

/// The file identity of a sealed container, which a hard link shares.
#[cfg(unix)]
fn file_identity(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).expect("sealed container metadata");
    (metadata.dev(), metadata.ino())
}

/// `generation`'s receipt in `profile`, asserted layered over `base`, whose
/// container file identity was `base_container`: the layer's base is those
/// bytes by hard link, and the delta it encodes is under a tenth of the
/// generation's rows.
fn assert_layered_over(
    profile: &Path,
    generation: &str,
    base: &str,
    #[cfg(unix)] base_container: (u64, u64),
) {
    let (directory, receipt) = receipt_for(profile, generation);
    assert_eq!(receipt["form"], "layered");
    assert_eq!(receipt["base"]["generation"], base);
    #[cfg(unix)]
    assert_eq!(
        file_identity(&directory.join("base.grafeo")),
        base_container,
        "the refresh references its base's container instead of re-encoding it"
    );
    let rows = receipt["entities"].as_u64().unwrap() + receipt["relations"].as_u64().unwrap();
    let delta = rows_in(&receipt["row_sum"]) + rows_in(&receipt["base"]["hidden_row_sum"])
        - rows_in(&receipt["base"]["row_sum"]);
    assert!(
        delta * 10 < rows,
        "a small refresh encoded {delta} of its generation's {rows} rows"
    );
}

/// Fails on a build that re-encodes the whole graph for a small refresh:
/// each refresh's artifact must be layered over the last cold container by
/// hard link and encode a small fraction of the rows a cold build encodes,
/// including a second refresh whose parent is itself layered. Fails as well
/// if a layered generation records a digest, serves a row, or answers a read
/// differently from a cold build of the same tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_refreshes_seal_deltas_that_serve_like_their_cold_builds() {
    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let fixture = RefreshFixture::create(&root, MODULES);
    let project_root = fixture.project_root.clone();
    let (source, base_generation, _, base_binding) = fixture.seal();
    let (_scope, registry, database) = fixture.open_profile("profile", 45).await;
    let base_runtime = source
        .retain(&registry, &database, &base_generation, base_binding.clone())
        .await
        .expect("retain the base graph runtime");
    let base = base_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("publish the base graph cold");
    let profile = root.join("profile");
    let (base_directory, base_receipt) = receipt_for(&profile, base.generation().as_str());
    assert_eq!(base_receipt["form"], "compact");
    #[cfg(unix)]
    let base_container = file_identity(&base_directory.join("generation.grafeo"));
    #[cfg(not(unix))]
    let _ = base_directory;

    // A daemon restart between the base and the refresh: the base serves
    // straight from its sealed artifact, never installed in the new graph
    // registry, and must still be the refresh's base.
    drop(base);
    drop(base_runtime);
    drop((registry, database, _scope));
    let (_scope, registry, database) = fixture.open_profile("profile", 45).await;
    let base_runtime = source
        .retain(&registry, &database, &base_generation, base_binding.clone())
        .await
        .expect("retain the base graph runtime after a restart");
    let base = base_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("recover the base graph after a restart");

    // A three-file edit.
    edit_three_files(&project_root, MODULES);
    fixture.commit("edit three files");
    let (source, child_generation, child_parent, child_binding) = fixture.seal();
    assert_eq!(child_parent.as_ref(), Some(&base_generation));
    let child_runtime = source
        .retain(
            &registry,
            &database,
            &child_generation,
            child_binding.clone(),
        )
        .await
        .expect("retain the child graph runtime");
    // Work stays local to the output pages affected by the edit. Sealing has
    // already resolved those pages, so graph projection resolves nothing.
    let child_report = layered_report(&child_runtime).expect("the child layers over its base");
    assert_eq!(child_report.resolved_references, 0);
    assert!(
        child_report.reextracted_files < child_report.reused_files,
        "the refresh materialized more changed pages than it reused: {child_report:?}"
    );
    assert!(
        child_report.removed_files < child_report.reused_files,
        "the refresh hid more changed pages than it reused: {child_report:?}"
    );
    assert!(
        child_report.delta_rows.0 > 0 && child_report.delta_rows.1 > 0,
        "the edit must produce a nonempty graph delta: {child_report:?}"
    );
    let child = child_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("publish the refresh");
    assert_layered_over(
        &profile,
        child.generation().as_str(),
        base.generation().as_str(),
        #[cfg(unix)]
        base_container,
    );
    assert_matches_cold_build(
        &fixture,
        &source,
        &child_runtime,
        child,
        child_binding,
        "profile-cold",
        46,
    )
    .await;

    // A file added and a file deleted, over the layered child: the delta is
    // still taken against the cold base. The deleted file's callers stay.
    std::fs::remove_file(project_root.join("src/m030.rs")).expect("delete m030");
    std::fs::write(
        project_root.join("src/extra.rs"),
        "pub fn extra_value() -> usize { crate::m031::value_031() + crate::m003::value_003() }\n",
    )
    .expect("add extra");
    fixture.commit("add and delete a file");
    let (source, grandchild_generation, grandchild_parent, grandchild_binding) = fixture.seal();
    assert_eq!(grandchild_parent.as_ref(), Some(&child_generation));
    let grandchild_runtime = source
        .retain(
            &registry,
            &database,
            &grandchild_generation,
            grandchild_binding.clone(),
        )
        .await
        .expect("retain the grandchild graph runtime");
    // Still against the cold base, the cumulative edit remains page-local and
    // consumes the resolution outputs sealed with the grandchild.
    let grandchild_report =
        layered_report(&grandchild_runtime).expect("the grandchild layers over its base");
    assert_eq!(grandchild_report.resolved_references, 0);
    assert!(
        grandchild_report.reextracted_files < grandchild_report.reused_files,
        "the cumulative refresh materialized more changed pages than it reused: \
         {grandchild_report:?}"
    );
    assert!(
        grandchild_report.removed_files < grandchild_report.reused_files,
        "the cumulative refresh hid more changed pages than it reused: {grandchild_report:?}"
    );
    assert!(
        grandchild_report.delta_rows.0 > 0 && grandchild_report.delta_rows.1 > 0,
        "the cumulative edit must produce a nonempty graph delta: {grandchild_report:?}"
    );
    let grandchild = grandchild_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("publish the second refresh");
    assert_layered_over(
        &profile,
        grandchild.generation().as_str(),
        base.generation().as_str(),
        #[cfg(unix)]
        base_container,
    );
    assert_matches_cold_build(
        &fixture,
        &source,
        &grandchild_runtime,
        grandchild,
        grandchild_binding,
        "profile-cold-second",
        47,
    )
    .await;
    drop((base_runtime, child_runtime, grandchild_runtime));
}

/// The layered report of a three-file refresh over a cold base of `modules`
/// modules, taken before it publishes.
async fn three_file_refresh_report(modules: usize, epoch: u64) -> CodeGraphLayeredReportV1 {
    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let fixture = RefreshFixture::create(&root, modules);
    let (source, base_generation, _, base_binding) = fixture.seal();
    let (_scope, registry, database) = fixture.open_profile("profile", epoch).await;
    let base_runtime = source
        .retain(&registry, &database, &base_generation, base_binding)
        .await
        .expect("retain the base graph runtime");
    let _base = base_runtime
        .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
        .expect("publish the base graph cold");
    edit_three_files(&fixture.project_root, modules);
    fixture.commit("edit three files");
    let (source, child_generation, _, child_binding) = fixture.seal();
    let child_runtime = source
        .retain(&registry, &database, &child_generation, child_binding)
        .await
        .expect("retain the child graph runtime");
    let report = layered_report(&child_runtime).expect("the refresh layers over its base");
    drop((base_runtime, child_runtime));
    report
}

/// The same edit materializes the same output-page delta while unrelated
/// corpus pages grow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_small_refresh_materializes_the_same_output_pages_at_both_base_sizes() {
    let small = three_file_refresh_report(MODULES, 48).await;
    let large = three_file_refresh_report(MODULES * 2, 49).await;
    assert_eq!(small.resolved_references, 0);
    assert_eq!(large.resolved_references, 0);
    assert_eq!(small.reextracted_files, large.reextracted_files);
    assert_eq!(small.removed_files, large.removed_files);
    assert_eq!(small.delta_rows, large.delta_rows);
}

const PEAK_PROBE_MODULES: &str = "TRACEDECAY_LAYERED_PEAK_PROBE_MODULES";
const PEAK_PROBE_LINE: &str = "layered-refresh-peak-kib ";

/// Kibibytes one `/proc/self/status` field holds.
#[cfg(target_os = "linux")]
fn status_kib(field: &str) -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("process status");
    status
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|rest| rest.trim_start_matches(':').trim().strip_suffix(" kB"))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("process status has no {field}"))
}

/// Runs `work` and returns how far the process's resident set peaked above
/// where it stood when `work` began, in KiB. Freed allocator pages are
/// returned first, so the peak is new memory `work` needed.
#[cfg(target_os = "linux")]
fn peak_kib_during<T>(work: impl FnOnce() -> T) -> (T, u64) {
    let _ = tracedecay_runtime_core::resident_memory::release_process_allocator_memory_v1();
    std::fs::write("/proc/self/clear_refs", "5").expect("reset the peak resident set");
    let start = status_kib("VmRSS");
    let value = work();
    (value, status_kib("VmHWM").saturating_sub(start))
}

/// One corpus size's measurement, run alone in its own process by
/// [`a_layered_refresh_peaks_below_a_cold_build_at_both_base_sizes`] so no
/// other test's memory lands in it: after a restart, a three-file refresh
/// published as a delta over its cold base, then the same generation
/// published cold in a fresh profile.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "a measurement process that the peak test runs, one corpus size at a time"]
async fn layered_refresh_peak_probe() {
    let modules: usize = std::env::var(PEAK_PROBE_MODULES)
        .expect("the probe runs under the peak test")
        .parse()
        .expect("module count");
    let temporary = tempfile::tempdir().expect("temporary fixture parent");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let fixture = RefreshFixture::create(&root, modules);
    let (source, base_generation, _, base_binding) = fixture.seal();
    {
        let (_scope, registry, database) = fixture.open_profile("profile", 45).await;
        source
            .retain(&registry, &database, &base_generation, base_binding.clone())
            .await
            .expect("retain the base graph runtime")
            .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
            .expect("publish the base graph cold");
    }
    edit_three_files(&fixture.project_root, modules);
    fixture.commit("edit three files");
    let (source, child_generation, _, child_binding) = fixture.seal();

    let (_scope, registry, database) = fixture.open_profile("profile", 45).await;
    let child_runtime = source
        .retain(
            &registry,
            &database,
            &child_generation,
            child_binding.clone(),
        )
        .await
        .expect("retain the child graph runtime");
    let (child, layered_kib) = peak_kib_during(|| {
        child_runtime
            .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
            .expect("publish the refresh")
    });
    let (_, receipt) = receipt_for(&root.join("profile"), child.generation().as_str());
    assert_eq!(receipt["form"], "layered");
    drop((child, child_runtime, registry, database, _scope));

    let (_scope, registry, database) = fixture.open_profile("profile-cold", 46).await;
    let cold_runtime = source
        .retain(&registry, &database, &child_generation, child_binding)
        .await
        .expect("retain the cold graph runtime");
    let (cold, cold_kib) = peak_kib_during(|| {
        cold_runtime
            .publish_verified_snapshot(Arc::new(AtomicBool::new(false)))
            .expect("publish the generation cold")
    });
    let (_, receipt) = receipt_for(&root.join("profile-cold"), cold.generation().as_str());
    assert_eq!(receipt["form"], "compact");
    println!("{PEAK_PROBE_LINE}{layered_kib} {cold_kib}");
}

/// `(layered, cold)` peak KiB of one corpus size, measured in a process of
/// its own.
#[cfg(target_os = "linux")]
fn measure_peaks(modules: usize) -> (u64, u64) {
    let output = Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--ignored",
            "--exact",
            "session_registry::code_graph::layered_refresh_tests::layered_refresh_peak_probe",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PEAK_PROBE_MODULES, modules.to_string())
        .output()
        .expect("run the peak probe");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "peak probe for {modules} modules failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let line = stdout
        .lines()
        .find_map(|line| line.split_once(PEAK_PROBE_LINE).map(|(_, peaks)| peaks))
        .unwrap_or_else(|| panic!("peak probe for {modules} modules printed no peaks: {stdout}"));
    let mut peaks = line
        .split_whitespace()
        .map(|value| value.parse::<u64>().expect("peak KiB"));
    let layered = peaks.next().expect("layered peak");
    let cold = peaks.next().expect("cold peak");
    println!("{modules} modules: layered refresh +{layered} KiB, cold build +{cold} KiB");
    (layered, cold)
}

/// Fails when a layered refresh loads its base graph: its peak then carries
/// the whole base engine, rising above the cold build of the same tree and
/// growing with the base like it. At two base sizes the refresh must peak
/// below the cold build, and grow less than the cold build does between
/// them; what still grows is the whole-corpus resolution both builds run.
#[cfg(target_os = "linux")]
#[test]
fn a_layered_refresh_peaks_below_a_cold_build_at_both_base_sizes() {
    let small = measure_peaks(MODULES);
    let large = measure_peaks(MODULES * 4);
    assert!(small.0 < small.1, "{MODULES} modules: {small:?}");
    assert!(large.0 < large.1, "{} modules: {large:?}", MODULES * 4);
    assert!(
        large.0.saturating_sub(small.0) < large.1.saturating_sub(small.1),
        "the refresh grew with its base like a cold build: {small:?} -> {large:?}"
    );
}
