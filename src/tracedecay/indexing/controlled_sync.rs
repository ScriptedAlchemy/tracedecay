use std::collections::HashSet;

use crate::db::DatabaseWriteTransaction;
use crate::errors::{Result, TraceDecayError};
use crate::resolution::ReferenceResolver;
use crate::types::{Edge, FileRecord};

use super::super::{TraceDecay, current_timestamp};
use super::{
    ExtractTuple, UNRESOLVED_REFS_PERSISTED_KEY, UNRESOLVED_REFS_PERSISTED_VALUE,
    extract_files_isolated, is_safe_lazy_dependency_path, normalize_rel_path,
    reindexed_symbol_scope, simple_ref_name,
};

pub(in crate::tracedecay) async fn lazy_index_ignored_dependency_files<F, C>(
    graph: &TraceDecay,
    file_paths: &[String],
    ensure_active: F,
    begin_commit: C,
) -> Result<Vec<String>>
where
    F: Fn() -> Result<()> + Send + Sync,
    C: Fn() -> Result<()> + Send + Sync,
{
    ensure_active()?;
    let live_branch = graph.branch_memo();
    graph.ensure_branch_writable_with("lazy index ignored dependency files", &live_branch)?;

    let mut accepted = Vec::new();
    let mut seen = HashSet::new();
    for path in file_paths {
        ensure_active()?;
        let normalized = normalize_rel_path(path.trim());
        if !is_safe_lazy_dependency_path(&normalized) || !seen.insert(normalized.clone()) {
            continue;
        }
        let abs_path = graph.project_root.join(&normalized);
        let Ok(metadata) = std::fs::metadata(&abs_path) else {
            continue;
        };
        if !metadata.is_file()
            || metadata.len() > graph.config.max_file_size
            || graph.registry.extractor_for_file(&normalized).is_none()
        {
            continue;
        }
        accepted.push(normalized);
        if accepted.len() >= 20 {
            break;
        }
    }
    ensure_active()?;

    let (extractions, _skipped) =
        extract_files_isolated(&graph.project_root, &graph.registry, accepted.clone());
    ensure_active()?;
    if extractions.is_empty() {
        return Ok(Vec::new());
    }
    let extracted_paths: HashSet<&str> = extractions
        .iter()
        .map(|(path, _, _, _, _)| path.as_str())
        .collect();
    let indexed_paths: Vec<String> = accepted
        .into_iter()
        .filter(|path| extracted_paths.contains(path.as_str()))
        .collect();

    let sync_lease = graph.begin_active_sync()?;
    ensure_active()?;
    let transaction = graph
        .db
        .begin_write_transaction("lazy index ignored dependency files")
        .await?;
    let started_at = std::time::Instant::now();
    if let Err(error) = prepare_lazy_index_transaction(
        graph,
        &transaction,
        &extractions,
        &indexed_paths,
        started_at,
        &ensure_active,
    )
    .await
    {
        return rollback_with_error(transaction, error).await;
    }

    if let Err(error) = sync_lease.commit() {
        return rollback_with_error(transaction, error).await;
    }
    if let Err(error) = begin_commit() {
        return rollback_with_error(transaction, error).await;
    }
    // The WAL commit is the operation's only durable publication. Checkpointing
    // is storage maintenance owned by the daemon/shutdown path, not a fallible
    // post-publication step in this request.
    transaction.commit().await?;
    Ok(indexed_paths)
}

async fn prepare_lazy_index_transaction<F>(
    graph: &TraceDecay,
    transaction: &DatabaseWriteTransaction<'_>,
    extractions: &[ExtractTuple],
    indexed_paths: &[String],
    started_at: std::time::Instant,
    ensure_active: &F,
) -> Result<()>
where
    F: Fn() -> Result<()> + Send + Sync,
{
    ensure_active()?;
    let mut queued_edges: Vec<&Edge> = Vec::new();
    for (file_path, result, hash, size, mtime) in extractions {
        graph
            .db
            .delete_nodes_by_file_unguarded(transaction, file_path)
            .await?;
        ensure_active()?;
        graph
            .db
            .insert_nodes_unguarded(transaction, &result.nodes)
            .await?;
        ensure_active()?;
        queued_edges.extend(&result.edges);
        if !result.unresolved_refs.is_empty() {
            graph
                .db
                .insert_unresolved_refs_unguarded(transaction, &result.unresolved_refs)
                .await?;
            ensure_active()?;
        }
        graph
            .db
            .upsert_file_unguarded(
                transaction,
                &FileRecord {
                    path: file_path.clone(),
                    content_hash: hash.clone(),
                    size: *size,
                    modified_at: *mtime,
                    indexed_at: current_timestamp(),
                    node_count: result.nodes.len() as u32,
                },
            )
            .await?;
        ensure_active()?;
    }
    if !queued_edges.is_empty() {
        let edges: Vec<Edge> = queued_edges.into_iter().cloned().collect();
        graph.db.insert_edges_unguarded(transaction, &edges).await?;
        ensure_active()?;
    }

    let unresolved_refs_complete = graph
        .db
        .get_metadata_unguarded(transaction, UNRESOLVED_REFS_PERSISTED_KEY)
        .await?
        .as_deref()
        == Some(UNRESOLVED_REFS_PERSISTED_VALUE);
    if !unresolved_refs_complete {
        return Err(TraceDecayError::project_route(
            "dependency_graph_rebuild_required",
            true,
            "lazy dependency indexing requires the unresolved-reference graph rebuild",
        ));
    }

    let (short_names, qualified_names) = reindexed_symbol_scope(extractions);
    let path_set: HashSet<&str> = indexed_paths.iter().map(String::as_str).collect();
    let unresolved = graph.db.get_unresolved_refs_unguarded(transaction).await?;
    let scoped = unresolved
        .into_iter()
        .filter(|reference| {
            path_set.contains(reference.file_path.as_str())
                || short_names.contains(simple_ref_name(&reference.reference_name))
                || qualified_names.contains(reference.reference_name.as_str())
        })
        .collect::<Vec<_>>();
    if !scoped.is_empty() {
        let all_nodes = graph.db.get_all_nodes_unguarded(transaction).await?;
        let resolver = ReferenceResolver::from_nodes(&all_nodes);
        let resolution = resolver.resolve_all(&scoped);
        let edges = resolver.create_edges(&resolution.resolved);
        if !edges.is_empty() {
            graph.db.insert_edges_unguarded(transaction, &edges).await?;
        }
    }
    ensure_active()?;

    graph
        .db
        .set_metadata_unguarded(
            transaction,
            "last_sync_at",
            &current_timestamp().to_string(),
        )
        .await?;
    ensure_active()?;
    graph
        .db
        .set_metadata_unguarded(
            transaction,
            "last_sync_duration_ms",
            &started_at.elapsed().as_millis().to_string(),
        )
        .await?;
    ensure_active()
}

async fn rollback_with_error<T>(
    transaction: DatabaseWriteTransaction<'_>,
    primary: TraceDecayError,
) -> Result<T> {
    match transaction.rollback().await {
        Ok(()) => Err(primary),
        Err(rollback) => Err(TraceDecayError::Database {
            operation: "rollback lazy dependency indexing".to_owned(),
            message: format!("{primary}; rollback also failed: {rollback}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tracedecay_application::CancellationSignal;
    use tracedecay_domain::UtcMicros;
    use tracedecay_usecases::tracedecay::{GraphRequestControl, GraphRuntimePort};

    use super::super::TraceDecay;
    use crate::errors::TraceDecayError;
    use crate::tracedecay::TraceDecayOpenOptions;

    async fn node_modules_graph(
        dependency_source: &[u8],
    ) -> (TraceDecay, tempfile::TempDir, String) {
        let isolation = tempfile::tempdir().expect("isolated profile");
        let project = isolation.path().join("project");
        let profile = isolation.path().join("profile");
        let dependency = "node_modules/branch-pkg/index.rs".to_owned();
        fs::create_dir_all(project.join("src")).expect("source directory");
        fs::create_dir_all(project.join("node_modules/branch-pkg")).expect("dependency directory");
        fs::write(project.join("src/lib.rs"), "pub fn project_entry() {}\n").expect("source file");
        fs::write(project.join(".gitignore"), "node_modules/\n").expect("ignore dependency tree");
        let graph = TraceDecay::init_with_options(
            &project,
            TraceDecayOpenOptions {
                profile_root: Some(profile.clone()),
                global_db_path: Some(profile.join("global.db")),
            },
        )
        .await
        .expect("graph");
        graph.index_all().await.expect("initial index");
        fs::write(project.join(&dependency), dependency_source).expect("dependency source");
        assert!(
            graph
                .db()
                .get_file(&dependency)
                .await
                .expect("dependency file state")
                .is_none(),
            "normal indexing must leave node_modules ignored"
        );
        (graph, isolation, dependency)
    }

    async fn install_metadata_failure(graph: &TraceDecay) {
        let transaction = graph
            .db()
            .begin_write_transaction("install lazy-index metadata failure")
            .await
            .expect("failure fixture transaction");
        transaction
            .execute_batch(
                "CREATE TRIGGER fail_lazy_index_metadata
                 BEFORE INSERT ON metadata
                 WHEN NEW.key = 'last_sync_at'
                 BEGIN
                   SELECT RAISE(FAIL, 'injected lazy-index metadata failure');
                 END;",
            )
            .await
            .expect("failure fixture trigger");
        transaction.commit().await.expect("failure fixture commit");
    }

    async fn install_deferred_commit_failure(graph: &TraceDecay) {
        let transaction = graph
            .db()
            .begin_write_transaction("install lazy-index commit failure")
            .await
            .expect("failure fixture transaction");
        transaction
            .execute_batch(
                "CREATE TABLE lazy_index_commit_parent (
                   id INTEGER PRIMARY KEY
                 );
                 CREATE TABLE lazy_index_commit_child (
                   parent_id INTEGER,
                   FOREIGN KEY(parent_id) REFERENCES lazy_index_commit_parent(id)
                     DEFERRABLE INITIALLY DEFERRED
                 );
                 CREATE TRIGGER fail_lazy_index_commit
                 AFTER INSERT ON metadata
                 WHEN NEW.key = 'last_sync_at'
                 BEGIN
                   INSERT INTO lazy_index_commit_child(parent_id) VALUES (1);
                 END;",
            )
            .await
            .expect("failure fixture schema");
        transaction.commit().await.expect("failure fixture commit");
    }

    #[tokio::test]
    async fn lazy_index_reports_only_successfully_extracted_node_modules_files() {
        let (graph, _isolation, dependency) = node_modules_graph(b"\xff\xfe\xfd").await;
        let cancellation =
            CancellationSignal::active("cancel.lazy-index-skip").expect("cancellation");

        let indexed = <TraceDecay as GraphRuntimePort>::lazy_index_ignored_dependency_files(
            &graph,
            std::slice::from_ref(&dependency),
            GraphRequestControl {
                deadline: None,
                cancellation: Some(&cancellation),
            },
        )
        .await
        .expect("skipped extraction is a settled empty result");

        assert!(indexed.is_empty());
        assert!(!cancellation.commit_started());
        assert!(
            graph
                .db()
                .get_file(&dependency)
                .await
                .expect("dependency file state")
                .is_none()
        );
    }

    #[tokio::test]
    async fn cancellation_at_commit_boundary_rolls_back_every_lazy_index_write() {
        let (graph, _isolation, dependency) =
            node_modules_graph(b"pub struct BranchOnly { pub value: String }\n").await;
        let cancellation =
            CancellationSignal::active("cancel.lazy-index-boundary").expect("cancellation");
        let commit_cancellation = cancellation.clone();
        let metadata_before = graph
            .db()
            .get_metadata("last_sync_at")
            .await
            .expect("initial sync metadata");

        let error = super::lazy_index_ignored_dependency_files(
            &graph,
            std::slice::from_ref(&dependency),
            || Ok(()),
            || {
                assert!(commit_cancellation.cancel(UtcMicros(41)));
                if commit_cancellation.try_begin_commit() {
                    Ok(())
                } else {
                    Err(TraceDecayError::project_route(
                        "dependency_hint_cancelled",
                        true,
                        "dependency hint operation was cancelled",
                    ))
                }
            },
        )
        .await
        .expect_err("cancellation must win before the durable commit");

        assert_eq!(
            error.project_route_context().map(|context| context.0),
            Some("dependency_hint_cancelled")
        );
        assert!(!cancellation.commit_started());
        assert!(
            graph
                .db()
                .get_file(&dependency)
                .await
                .expect("dependency file state")
                .is_none()
        );
        assert_eq!(
            graph
                .db()
                .get_metadata("last_sync_at")
                .await
                .expect("settled sync metadata"),
            metadata_before
        );
    }

    #[tokio::test]
    async fn lazy_index_follow_up_failure_rolls_back_before_commit_claim() {
        let (graph, _isolation, dependency) =
            node_modules_graph(b"pub struct BranchOnly { pub value: String }\n").await;
        install_metadata_failure(&graph).await;
        let cancellation =
            CancellationSignal::active("cancel.lazy-index-follow-up").expect("cancellation");

        let error = <TraceDecay as GraphRuntimePort>::lazy_index_ignored_dependency_files(
            &graph,
            std::slice::from_ref(&dependency),
            GraphRequestControl {
                deadline: None,
                cancellation: Some(&cancellation),
            },
        )
        .await
        .expect_err("metadata failure must fail the whole lazy index");

        assert!(
            error
                .to_string()
                .contains("injected lazy-index metadata failure")
        );
        assert!(!cancellation.commit_started());
        assert!(
            graph
                .db()
                .get_file(&dependency)
                .await
                .expect("dependency file state")
                .is_none()
        );
    }

    #[tokio::test]
    async fn lazy_index_commit_failure_has_no_durable_graph_result() {
        let (graph, _isolation, dependency) =
            node_modules_graph(b"pub struct BranchOnly { pub value: String }\n").await;
        install_deferred_commit_failure(&graph).await;
        let cancellation =
            CancellationSignal::active("cancel.lazy-index-commit-failure").expect("cancellation");

        let error = <TraceDecay as GraphRuntimePort>::lazy_index_ignored_dependency_files(
            &graph,
            std::slice::from_ref(&dependency),
            GraphRequestControl {
                deadline: None,
                cancellation: Some(&cancellation),
            },
        )
        .await
        .expect_err("deferred constraint must fail SQLite commit");

        assert!(error.to_string().contains("commit"));
        assert!(cancellation.commit_started());
        assert!(
            graph
                .db()
                .get_file(&dependency)
                .await
                .expect("dependency file state")
                .is_none()
        );
    }

    #[tokio::test]
    async fn committed_lazy_index_returns_terminal_durable_paths() {
        let (graph, _isolation, dependency) =
            node_modules_graph(b"pub struct BranchOnly { pub value: String }\n").await;
        let cancellation =
            CancellationSignal::active("cancel.lazy-index-committed").expect("cancellation");

        let indexed = <TraceDecay as GraphRuntimePort>::lazy_index_ignored_dependency_files(
            &graph,
            std::slice::from_ref(&dependency),
            GraphRequestControl {
                deadline: None,
                cancellation: Some(&cancellation),
            },
        )
        .await
        .expect("lazy dependency index");

        assert_eq!(indexed, [dependency.clone()]);
        assert!(cancellation.commit_started());
        assert!(!cancellation.cancel(UtcMicros(41)));
        assert!(
            graph
                .db()
                .get_file(&dependency)
                .await
                .expect("dependency file state")
                .is_some()
        );
        assert!(
            graph
                .get_nodes_by_name("BranchOnly")
                .await
                .expect("indexed symbol")
                .iter()
                .any(|node| node.file_path == dependency)
        );
    }
}
