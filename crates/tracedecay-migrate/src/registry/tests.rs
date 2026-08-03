//! Atomicity coverage for registry garbage collection.
//!
//! A project is registered in two generations — the `code_projects` registry
//! and the older `projects` accounting table — and collection has to remove
//! both or neither. A half-applied sweep either strips a live project's
//! registry row while its accounting row keeps it addressable, or leaves an
//! orphan behind for the next sweep to trip over. The transaction is the only
//! thing holding those two deletes together, so it is what these pin.

use std::path::Path;

use crate::root_seam::global_db::tests::harness::RegisteredGlobalDbHarness;
use crate::root_seam::global_db::{GraphScopeUpsert, RegisteredGlobalDb};
use tracedecay_runtime_core::db::engine::params;

use super::{
    delete_registry_gc_candidates_in_transaction, graph_scope_location_drift_is_repairable,
};

const PROJECT_ID: &str = "proj_gc";
const TOKENS: u64 = 11;

#[test]
fn graph_scope_location_drift_requires_the_same_immutable_owner() {
    let expected = GraphScopeUpsert {
        graph_scope_id: "store:project:profile_sharded:branch:feature".to_string(),
        project_id: "project".to_string(),
        store_id: "store:project:profile_sharded".to_string(),
        branch_name: "feature".to_string(),
        db_relpath: "projects/project/branches/current.db".to_string(),
        parent_scope_id: Some("parent-current".to_string()),
        last_synced_at: Some(2),
        writable: true,
    };
    assert!(graph_scope_location_drift_is_repairable(
        r#"["project","store:project:profile_sharded","feature","projects/project/branches/old.db","parent-old"]"#,
        &expected,
    ));
    assert!(!graph_scope_location_drift_is_repairable(
        r#"["other-project","store:project:profile_sharded","feature","projects/project/branches/old.db","parent-old"]"#,
        &expected,
    ));
}

/// Registers a project in both generations. The `code_projects` row is written
/// directly because `upsert_code_project` refuses an ephemeral root, and the
/// row's provenance is irrelevant to what collection must do with it.
async fn register_both_generations(db: &RegisteredGlobalDb, project: &Path) {
    db.upsert(project, TOKENS).await;
    let transaction = db
        .begin_write_transaction()
        .await
        .expect("begin registry fixture transaction");
    transaction
        .execute(
            "INSERT INTO code_projects
             (project_id, canonical_root, display_root, created_at, last_seen_at)
             VALUES (?1, ?2, ?2, 1, 1)",
            params![PROJECT_ID, project.to_string_lossy().as_ref()],
        )
        .await
        .expect("register code project");
    transaction.commit().await.expect("commit registry fixture");
}

async fn code_project_registered(db: &RegisteredGlobalDb) -> bool {
    let snapshot = db.read_snapshot().await.expect("open registry snapshot");
    let mut rows = snapshot
        .query(
            "SELECT 1 FROM code_projects WHERE project_id = ?1",
            params![PROJECT_ID],
        )
        .await
        .expect("query code project registration");
    rows.next()
        .await
        .expect("read code project registration")
        .is_some()
}

/// The ordinary sweep: one transaction removes the project from both
/// generations and reports one deletion from each.
#[tokio::test]
async fn registry_gc_deletes_both_registry_generations_in_one_commit() {
    let harness = RegisteredGlobalDbHarness::open("registry-gc-commit").await;
    let project = harness.registered.db_path().parent().unwrap().join("gone");
    register_both_generations(&harness.registered, &project).await;

    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin registry cleanup transaction");
    let deleted = delete_registry_gc_candidates_in_transaction(
        &transaction,
        &[PROJECT_ID.to_string()],
        std::slice::from_ref(&project),
    )
    .await
    .expect("delete registry cleanup plan");
    transaction.commit().await.expect("commit registry cleanup");

    assert_eq!(deleted, (1, 1));
    assert!(!code_project_registered(&harness.registered).await);
    assert_eq!(
        harness.registered.get_project_tokens(&project).await,
        Some(0)
    );
}

/// The atomicity claim. The `code_projects` delete runs first and succeeds; the
/// `projects` delete then aborts. Rolling back has to take the first delete
/// with it, or the sweep leaves a project addressable through its accounting
/// row with no registry row behind it.
#[tokio::test]
async fn registry_gc_rolls_back_both_generations_when_one_delete_fails() {
    let harness = RegisteredGlobalDbHarness::open("registry-gc-rollback").await;
    let project = harness.registered.db_path().parent().unwrap().join("gone");
    register_both_generations(&harness.registered, &project).await;

    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin registry cleanup transaction");
    transaction
        .execute(
            "CREATE TRIGGER reject_registry_gc
             BEFORE DELETE ON projects
             BEGIN SELECT RAISE(ABORT, 'forced registry cleanup failure'); END",
            (),
        )
        .await
        .expect("install failure trigger");

    let result = delete_registry_gc_candidates_in_transaction(
        &transaction,
        &[PROJECT_ID.to_string()],
        std::slice::from_ref(&project),
    )
    .await;
    assert!(
        result.is_err(),
        "the aborted storage delete must fail the sweep"
    );
    transaction
        .rollback()
        .await
        .expect("roll back the failed registry cleanup");

    assert!(
        code_project_registered(&harness.registered).await,
        "a failed sweep must not leave the code registry generation deleted"
    );
    assert_eq!(
        harness.registered.get_project_tokens(&project).await,
        Some(TOKENS)
    );
}

/// Collection and re-registration race on every daemon that sweeps while an
/// agent is active. The sweep's write transaction must serialize the refresh
/// rather than interleave with it, so the refresh lands whole afterwards
/// instead of resurrecting one generation and not the other.
#[tokio::test]
async fn registry_gc_transaction_serializes_a_concurrent_project_refresh() {
    let harness = RegisteredGlobalDbHarness::open("registry-gc-serialize").await;
    let project = harness.registered.db_path().parent().unwrap().join("gone");
    register_both_generations(&harness.registered, &project).await;

    // The refresh races the sweep through the *same* registered runtime, which
    // is how a daemon actually reaches this database: one mount, one serialized
    // writer lane. A second independent mount would not serialize — connection
    // policy pins `busy_timeout = 0` precisely so SQLite never waits behind the
    // runtime's own queue — and would fail the write outright instead.
    let concurrent_db = std::sync::Arc::clone(&harness.registered);

    let transaction = harness
        .registered
        .begin_write_transaction()
        .await
        .expect("begin registry cleanup transaction");

    let concurrent_project = project.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let refresh = tokio::spawn(async move {
        let _ = started_tx.send(());
        concurrent_db.upsert(&concurrent_project, 22).await;
        concurrent_db.get_project_tokens(&concurrent_project).await
    });
    started_rx.await.unwrap();
    tokio::task::yield_now().await;
    assert!(
        !refresh.is_finished(),
        "the refresh must block on the sweep's write transaction"
    );

    let deleted = delete_registry_gc_candidates_in_transaction(
        &transaction,
        &[PROJECT_ID.to_string()],
        std::slice::from_ref(&project),
    )
    .await
    .expect("delete registry cleanup plan");
    transaction.commit().await.expect("commit registry cleanup");
    assert_eq!(deleted, (1, 1));

    let refreshed = tokio::time::timeout(std::time::Duration::from_secs(5), refresh)
        .await
        .expect("the concurrent refresh must resume once the sweep commits")
        .expect("the concurrent refresh task must complete");
    assert_eq!(
        refreshed,
        Some(22),
        "the refresh lands whole after the sweep rather than interleaving with it"
    );
}

/// Registry liveness must resolve the whole identity — aliases, the shared git
/// common directory, and registered store instances — before an unreviewed
/// pass retires a row. Deleting `code_projects` cascades those rows away, so a
/// roots-only check silently destroys a live project's registration.
mod liveness {
    use std::path::{Path, PathBuf};

    use crate::registry::{
        RootLivenessV1, StaleRootScope, code_project_root_exists, probe_root,
        project_context_liveness, stale_project_contexts,
    };
    use crate::root_seam::global_db::{
        CodeProjectRecord, ProjectAliasRecord, ProjectRegistryContext, ProjectStoreContext,
        StoreInstanceRecord,
    };

    const GONE: &str = "/definitely/not/here/retired-checkout";

    fn project(canonical_root: &str) -> CodeProjectRecord {
        CodeProjectRecord {
            project_id: "proj_live".to_string(),
            canonical_root: canonical_root.to_string(),
            display_root: canonical_root.to_string(),
            git_common_dir: None,
            git_remote_url: None,
            default_branch: None,
            created_at: 0,
            last_seen_at: 0,
        }
    }

    fn context(project: CodeProjectRecord) -> ProjectRegistryContext {
        ProjectRegistryContext {
            project,
            aliases: Vec::new(),
            stores: Vec::new(),
        }
    }

    fn store_context() -> ProjectStoreContext {
        ProjectStoreContext {
            store: StoreInstanceRecord {
                store_id: "store_live".to_string(),
                project_id: "proj_live".to_string(),
                store_kind: "graph".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath: "stores/live".to_string(),
                manifest_relpath: None,
                created_at: 0,
                last_verified_at: None,
                last_write_at: None,
            },
            graph_scopes: Vec::new(),
            artifacts: Vec::new(),
        }
    }

    fn live_dir() -> PathBuf {
        std::env::current_dir().unwrap()
    }

    #[test]
    fn a_live_alias_keeps_the_identity_live() {
        let mut ctx = context(project(GONE));
        ctx.aliases.push(ProjectAliasRecord {
            alias_path: live_dir().to_string_lossy().into_owned(),
            project_id: "proj_live".to_string(),
            last_seen_at: 0,
        });

        assert_eq!(project_context_liveness(&ctx), RootLivenessV1::Live);
        assert!(
            stale_project_contexts(
                std::slice::from_ref(&ctx),
                &[],
                StaleRootScope::AllRootsMissing
            )
            .is_empty(),
            "a project with a live alias must never be a GC candidate"
        );
    }

    #[test]
    fn a_registered_store_instance_keeps_the_identity_live() {
        let mut ctx = context(project(GONE));
        ctx.stores.push(store_context());

        assert_eq!(project_context_liveness(&ctx), RootLivenessV1::Live);
        assert!(
            stale_project_contexts(
                std::slice::from_ref(&ctx),
                &[],
                StaleRootScope::AllRootsMissing
            )
            .is_empty(),
            "deleting this row would cascade its live store instance away"
        );
    }

    #[test]
    fn a_live_git_common_dir_keeps_a_linked_worktree_identity_live() {
        let mut record = project(GONE);
        record.git_common_dir = Some(live_dir().to_string_lossy().into_owned());
        let ctx = context(record);

        assert_eq!(project_context_liveness(&ctx), RootLivenessV1::Live);
        assert!(
            stale_project_contexts(
                std::slice::from_ref(&ctx),
                &[],
                StaleRootScope::AllRootsMissing
            )
            .is_empty()
        );
    }

    #[test]
    fn every_root_proven_absent_is_still_a_candidate() {
        let ctx = context(project(GONE));

        assert_eq!(project_context_liveness(&ctx), RootLivenessV1::Absent);
        assert_eq!(
            stale_project_contexts(
                std::slice::from_ref(&ctx),
                &[],
                StaleRootScope::AllRootsMissing
            )
            .len(),
            1,
            "proven absence is the one condition that permits retirement"
        );
    }

    #[test]
    fn an_unverifiable_root_is_never_treated_as_absent() {
        assert_eq!(probe_root(&live_dir()), RootLivenessV1::Live);
        assert_eq!(probe_root(Path::new(GONE)), RootLivenessV1::Absent);
        assert!(!RootLivenessV1::Unverifiable.permits_retirement());
        assert!(!RootLivenessV1::Live.permits_retirement());
        assert!(RootLivenessV1::Absent.permits_retirement());

        // Merging keeps the strongest evidence: unverifiable never decays into
        // absence just because a sibling root was proven gone.
        assert_eq!(
            RootLivenessV1::Unverifiable.merge(RootLivenessV1::Absent),
            RootLivenessV1::Unverifiable
        );
        assert_eq!(
            RootLivenessV1::Unverifiable.merge(RootLivenessV1::Live),
            RootLivenessV1::Live
        );

        let mut record = project(GONE);
        record.git_common_dir = Some(GONE.to_string());
        assert!(
            !code_project_root_exists(&record),
            "a row whose every root is proven absent still reports absent"
        );
    }
}
