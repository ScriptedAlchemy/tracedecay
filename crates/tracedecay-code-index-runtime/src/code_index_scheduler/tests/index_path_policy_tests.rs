//! `index.exclude.v1` / `index.include.v1` as a restarted owner applies them:
//! each open below is a daemon start under the policy its configuration
//! resolves, over the same sealed store.

use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;
use tracedecay_domain::{IndexPathPolicyV1, SnapshotFileDispositionV1};

use super::{
    GitFixture, SharedCodeIndexBytePoolV1, published, test_project_id,
    wait_for_live_complete_generation,
};
use crate::code_index_scheduler::{
    CodeIndexDemandAdmissionV1, CodeIndexHintPolicyV1, CodeIndexReconcileOutcomeV1,
    CodeIndexSchedulerRegistryV1, CodeIndexWorktreeSchedulerV1,
};
use crate::config::registry_default_index_path_policy;

const SOURCES: &[(&str, &str)] = &[
    ("src/lib.rs", "pub fn kept() {}\n"),
    ("generated-fixtures/gen.rs", "pub fn generated_only() {}\n"),
    ("vendor/kept/lib.rs", "pub fn vendored_kept() {}\n"),
];

/// The shipped defaults plus `extra_exclude`, with `include` as given.
fn policy(extra_exclude: &[&str], include: &[&str]) -> IndexPathPolicyV1 {
    let defaults = registry_default_index_path_policy();
    IndexPathPolicyV1::new(
        defaults
            .exclude_patterns()
            .iter()
            .cloned()
            .chain(extra_exclude.iter().map(|pattern| (*pattern).to_owned()))
            .collect(),
        include
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .collect(),
    )
    .expect("valid fixture patterns")
}

fn start(
    fixture: &GitFixture,
    store: &Path,
    path_policy: IndexPathPolicyV1,
) -> CodeIndexWorktreeSchedulerV1 {
    CodeIndexWorktreeSchedulerV1::open_with_policy(
        test_project_id(),
        fixture.path(),
        store.to_path_buf(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
        CodeIndexHintPolicyV1::default(),
        path_policy,
    )
    .expect("open code-index owner")
}

fn indexed_paths(scheduler: &CodeIndexWorktreeSchedulerV1) -> Vec<String> {
    scheduler
        .latest_complete()
        .expect("published generation")
        .generation()
        .snapshot()
        .files
        .iter()
        .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
        .map(|file| file.logical_path.clone())
        .collect()
}

#[test]
fn a_restart_under_a_changed_path_policy_republishes_exactly_the_matching_files() {
    let fixture = GitFixture::new(SOURCES);
    let store = TempDir::new().expect("store root");

    let mut first = start(&fixture, store.path(), policy(&[], &[]));
    let first_generation = published(first.reconcile_now().expect("first index")).generation_id;
    assert_eq!(
        indexed_paths(&first),
        ["generated-fixtures/gen.rs", "src/lib.rs"]
    );
    drop(first);

    let mut unchanged = start(&fixture, store.path(), policy(&[], &[]));
    assert!(
        matches!(
            unchanged.reconcile_now().expect("unchanged restart"),
            CodeIndexReconcileOutcomeV1::Noop(_)
        ),
        "an unchanged policy must not republish"
    );
    assert_eq!(
        unchanged
            .latest_complete()
            .expect("retained generation")
            .generation()
            .manifest()
            .generation_id,
        first_generation
    );
    drop(unchanged);

    let mut excluded = start(
        &fixture,
        store.path(),
        policy(&["generated-fixtures/**"], &[]),
    );
    published(excluded.reconcile_now().expect("exclude restart"));
    assert_eq!(indexed_paths(&excluded), ["src/lib.rs"]);
    drop(excluded);

    let mut included = start(
        &fixture,
        store.path(),
        policy(&["generated-fixtures/**"], &["vendor/kept/**"]),
    );
    published(included.reconcile_now().expect("include restart"));
    assert_eq!(
        indexed_paths(&included),
        ["src/lib.rs", "vendor/kept/lib.rs"]
    );
    drop(included);

    let mut restored = start(&fixture, store.path(), policy(&[], &[]));
    published(restored.reconcile_now().expect("restored restart"));
    assert_eq!(
        indexed_paths(&restored),
        ["generated-fixtures/gen.rs", "src/lib.rs"]
    );
}

#[test]
fn a_sealed_roster_is_reusable_only_under_the_policy_it_was_captured_with() {
    let fixture = GitFixture::new(SOURCES);
    let store = TempDir::new().expect("store root");
    let mut first = start(&fixture, store.path(), policy(&[], &[]));
    published(first.reconcile_now().expect("first index"));
    let generation = first
        .latest_complete()
        .expect("published generation")
        .generation()
        .clone();
    drop(first);

    let same = start(&fixture, store.path(), policy(&[], &[]));
    let excluding = start(
        &fixture,
        store.path(),
        policy(&["generated-fixtures/**"], &[]),
    );
    let including = start(&fixture, store.path(), policy(&[], &["vendor/kept/**"]));
    assert!(same.roster_follows_path_policy(&generation));
    assert!(!excluding.roster_follows_path_policy(&generation));
    assert!(!including.roster_follows_path_policy(&generation));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hook_hints_for_excluded_paths_queue_no_work() {
    let fixture = GitFixture::new(SOURCES);
    let store = TempDir::new().expect("store root");
    let registry = CodeIndexSchedulerRegistryV1::new(1);
    registry
        .mount_worktree(
            test_project_id(),
            fixture.path(),
            store.path().to_path_buf(),
        )
        .await
        .expect("mount worktree");
    wait_for_live_complete_generation(&registry, fixture.path()).await;

    assert_eq!(
        registry
            .notify_hook_paths(fixture.path(), &["vendor/kept/lib.rs".to_owned()])
            .await,
        CodeIndexDemandAdmissionV1::NotApplicable,
        "a hint for a path the default policy excludes must not wake the owner"
    );
    assert_eq!(
        registry
            .notify_hook_paths(
                fixture.path(),
                &["vendor/kept/lib.rs".to_owned(), "src/lib.rs".to_owned()],
            )
            .await,
        CodeIndexDemandAdmissionV1::Queued,
        "an ordinary source path in the same batch is still delivered"
    );
    registry.shutdown().await;
}

#[test]
fn submodule_policy_binds_live_graph_and_historical_child_pins_across_restart() {
    use super::{git_stdout, run_git_in};
    use crate::code_index_scheduler::{
        branch_generations::BranchGenerationReadControlV1, git_tree_capture::ExactGitTreeSourceV1,
    };
    use tracedecay_code_index::grep_search::{GrepSearchQuery, search_tree_with_cancel};
    use tracedecay_domain::{CommitId, RefId, RelationEdgeKindV1, RepositoryDirtyStateV1, TreeId};

    let parent = GitFixture::new(&[(
        "dev/launch.ts",
        "import { feature } from '../app/lib';\nexport function launch() { return feature(); }\n",
    )]);
    let child = GitFixture::new(&[
        (
            "lib.ts",
            "export function feature() { return 'pinned_symbol'; }\n",
        ),
        ("gone.ts", "export function gone() {}\n"),
        ("media.bin", "local binary fixture"),
        (".gitignore", "*.private.ts\n"),
    ]);
    run_git_in(
        parent.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            child.path().to_str().unwrap(),
            "app",
        ],
    );
    run_git_in(parent.path(), &["commit", "-qm", "bind application"]);
    run_git_in(parent.path(), &["branch", "pinned"]);
    let pinned = ExactGitTreeSourceV1 {
        reference: RefId::new("refs/heads/pinned").unwrap(),
        revision: CommitId::new(git_stdout(parent.path(), &["rev-parse", "HEAD"])).unwrap(),
        tree: TreeId::new(git_stdout(parent.path(), &["rev-parse", "HEAD^{tree}"])).unwrap(),
    };
    let store = TempDir::new().unwrap();
    let enabled = policy(&[], &[]).with_submodules(true);
    let grep = |needle: &str, policy: &IndexPathPolicyV1| {
        search_tree_with_cancel(
            parent.path(),
            &GrepSearchQuery {
                pattern: needle.to_owned(),
                fixed_strings: true,
                case_sensitive: true,
                path_glob: Some("app/**".into()),
                scope_prefix: None,
                context_lines: 0,
                max_results: 8,
            },
            policy,
            || false,
        )
        .unwrap()
        .hits
        .into_iter()
        .map(|hit| hit.file.to_string())
        .collect::<Vec<_>>()
    };
    let child_path = std::path::Path::new("app/lib.ts");
    let direct_reads = |enabled| {
        tracedecay_code_index::source_walk::submodules::LiveSourceBoundaries::open(
            parent.path(),
            enabled,
        )
        .unwrap()
    };
    assert!(direct_reads(false).resolve(child_path).unwrap().is_none());
    let active_reads = direct_reads(true);
    assert!(active_reads.resolve(child_path).unwrap().is_some());
    let mut off = start(&parent, store.path(), policy(&[], &[]));
    off.reconcile_now().unwrap();
    assert_eq!(indexed_paths(&off), ["dev/launch.ts"]);
    assert!(grep("pinned_symbol", &policy(&[], &[])).is_empty());
    drop(off);

    let mut on = start(&parent, store.path(), enabled.clone());
    on.reconcile_now().unwrap();
    assert_eq!(
        indexed_paths(&on),
        ["app/gone.ts", "app/lib.ts", "dev/launch.ts"]
    );
    assert_eq!(grep("pinned_symbol", &enabled), ["app/lib.ts"]);
    let assert_edge =
        |generation: &tracedecay_code_index::production::CodeIndexPublishedGenerationV1| {
            let symbol = |name: &str| {
                generation
                    .symbols()
                    .symbols
                    .iter()
                    .find(|item| item.simple_name == name)
                    .unwrap()
                    .occurrence
                    .clone()
            };
            assert!(
                generation
                    .edges()
                    .iter()
                    .any(|edge| edge.kind == RelationEdgeKindV1::Calls
                        && edge.from_occurrence == symbol("launch")
                        && edge.to_occurrence == symbol("feature"))
            );
        };
    assert_edge(on.latest_complete().unwrap().generation());
    let checkout = parent.path().join("app");
    std::fs::write(
        checkout.join("ignored.private.ts"),
        "const private_marker = 1;",
    )
    .unwrap();
    assert!(grep("private_marker", &enabled).is_empty());
    std::fs::write(
        checkout.join("lib.ts"),
        "export function feature() { return 'successor_symbol'; }\n",
    )
    .unwrap();
    std::fs::remove_file(checkout.join("gone.ts")).unwrap();
    run_git_in(&checkout, &["add", "-A"]);
    run_git_in(
        &checkout,
        &["commit", "-qm", "move child HEAD independently"],
    );
    std::fs::write(
        checkout.join("pending.ts"),
        "export function pending() {}\n",
    )
    .unwrap();
    assert!(on.ensure_fresh_for_query().unwrap().is_some());
    assert_eq!(
        indexed_paths(&on),
        ["app/lib.ts", "app/pending.ts", "dev/launch.ts"]
    );
    assert_eq!(
        on.latest_complete()
            .unwrap()
            .generation()
            .repository_parse_identity()
            .dirty,
        RepositoryDirtyStateV1::Dirty
    );
    assert_eq!(grep("successor_symbol", &enabled), ["app/lib.ts"]);
    run_git_in(parent.path(), &["submodule", "deinit", "-f", "app"]);
    assert!(!checkout.join(".git").exists());
    assert!(active_reads.resolve(child_path).unwrap().is_none());
    let historical = on
        .publish_exact_git_tree_generation(
            &pinned,
            &BranchGenerationReadControlV1 {
                deadline: None,
                cancellation: None,
            },
        )
        .unwrap();
    let historical = historical.generation();
    assert_edge(historical);
    assert!(
        !historical
            .snapshot()
            .files
            .iter()
            .any(|file| file.logical_path == "app/media.bin")
    );
    assert!(
        historical
            .snapshot()
            .files
            .iter()
            .any(|file| file.logical_path == "app/gone.ts")
    );
    assert!(
        !historical
            .snapshot()
            .files
            .iter()
            .any(|file| file.logical_path == "app/pending.ts")
    );
    let pinned_file = historical
        .snapshot()
        .files
        .iter()
        .find(|file| file.logical_path == "app/lib.ts")
        .unwrap();
    assert_eq!(
        pinned_file.content_digest,
        tracedecay_code_index::chunks::content_digest(
            b"export function feature() { return 'pinned_symbol'; }\n"
        )
    );
    drop(on);
    let mut off = start(&parent, store.path(), policy(&[], &[]));
    off.reconcile_now().unwrap();
    assert_eq!(indexed_paths(&off), ["dev/launch.ts"]);
}

#[test]
fn parent_filtered_source_does_not_require_submodule_registration_when_disabled() {
    use super::run_git_in;
    use tracedecay_code_index::chunks::content_digest;
    use tracedecay_domain::LanguageId;

    let source = "pub fn parent() {}\n";
    let parent = GitFixture::new(&[
        ("lib.rs", source),
        (".gitmodules", "[submodule \"child\"]\npath = child\n"),
    ]);
    run_git_in(parent.path(), &["config", "core.autocrlf", "true"]);
    std::fs::write(parent.path().join("lib.rs"), source.replace('\n', "\r\n")).unwrap();
    std::fs::write(parent.path().join(".gitmodules"), "[unfinished").unwrap();
    let boundaries = tracedecay_code_index::source_walk::submodules::LiveSourceBoundaries::open(
        parent.path(),
        false,
    )
    .unwrap();
    let sealed = super::super::sealed_source_bytes(
        &boundaries,
        "lib.rs",
        &LanguageId::new("rust").unwrap(),
        &content_digest(source.as_bytes()),
    )
    .unwrap();
    assert_eq!(sealed.as_deref(), Some(source.as_bytes()));
}

#[test]
fn retained_graph_checks_require_a_successful_submodule_probe_each_time() {
    let registration = "[submodule \"child\"]\npath = child\n";
    let parent = GitFixture::new(&[
        ("lib.rs", "pub fn first() {}\n"),
        (".gitmodules", registration),
    ]);
    let store = TempDir::new().unwrap();
    // Keep the sealed attribution worktree-relative across the configuration edit.
    std::fs::write(parent.path().join("lib.rs"), "pub fn latest() {}\n").unwrap();
    let mut scheduler = start(
        &parent,
        store.path(),
        policy(&[], &[]).with_submodules(true),
    );
    published(scheduler.reconcile_now().unwrap());
    let metadata = scheduler
        .publication
        .load_active_manifest()
        .unwrap()
        .unwrap()
        .metadata;
    // A partially staged edit leaves the worktree and its source bytes readable,
    // while the index registration still cannot establish submodule identity.
    let registration_path = parent.path().join(".gitmodules");
    std::fs::write(&registration_path, "[unfinished").unwrap();
    super::run_git_in(parent.path(), &["add", ".gitmodules"]);
    std::fs::write(&registration_path, registration).unwrap();
    for _ in 0..2 {
        assert!(
            !scheduler
                .freshness_fence()
                .git_metadata(parent.path())
                .can_prove_freshness()
        );
        let serving_witness = scheduler.currency_witness_for_sealed_snapshot(
            &metadata.manifest().generation_id,
            &metadata.snapshot().content_identity,
        );
        let retained = scheduler
            .reconcile_retained_text_generation_with(&metadata, false)
            .unwrap();
        assert_eq!(
            (serving_witness.is_some(), retained.is_some()),
            (false, false),
            "unchanged source bytes cannot replace a failed submodule probe"
        );
    }
    super::run_git_in(parent.path(), &["add", ".gitmodules"]);
    assert!(matches!(
        scheduler
            .reconcile_retained_text_generation_with(&metadata, false)
            .unwrap(),
        Some(CodeIndexReconcileOutcomeV1::Noop(_))
    ));
    assert!(
        scheduler
            .currency_witness_for_sealed_snapshot(
                &metadata.manifest().generation_id,
                &metadata.snapshot().content_identity,
            )
            .is_some()
    );
}
