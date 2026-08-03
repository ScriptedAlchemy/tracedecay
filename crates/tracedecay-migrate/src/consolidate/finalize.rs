use super::*;
use crate::registry_adapter::{
    GraphScopeUpsert, RegistryDatabase, RegistryRuntime, StoreArtifactUpsert, StoreInstanceUpsert,
};

pub(super) async fn verify_destination(
    resolved: &ResolvedPlan,
    session_offsets: &sqlite::SessionMergeOffsets,
) -> Result<()> {
    let destination = &resolved.report.destination_data_root;
    let graph = destination.join(crate::config::DB_FILENAME);
    let sessions = destination.join(storage::SESSIONS_DB_FILENAME);
    let meta = branch_meta::load_branch_meta(destination)
        .ok_or_else(|| config_error("consolidated branch metadata is invalid"))?;
    let expected_branches = resolved
        .target_meta
        .branches
        .len()
        .saturating_add(resolved.source_meta.branches.len());
    if meta.branches.len() != expected_branches {
        return Err(config_error(format!(
            "destination has {} branch graph(s), expected {expected_branches}",
            meta.branches.len()
        )));
    }
    let destination_graphs = graph_db_paths_for_root(destination, &meta)?;
    let mut destination_identities = sqlite::GraphLogicalIdentities::default();
    for path in &destination_graphs {
        let snapshot = crate::sqlite_read_snapshot::open_in(path, resolved.scratch_root.path())
            .await
            .map_err(io_error)?;
        sqlite::quick_check_connection(snapshot.connection(), path).await?;
        if path == &graph {
            sqlite::extend_graph_identities(snapshot.connection(), &mut destination_identities)
                .await?;
        }
        snapshot.validate_source().map_err(io_error)?;
    }
    if !resolved
        .evidence
        .source_graph
        .identities
        .facts_union_matches(
            &resolved.evidence.target_graph.identities,
            &destination_identities,
        )
    {
        return Err(config_error(
            "destination fact logical union differs from frozen inputs",
        ));
    }
    if !resolved
        .evidence
        .source_graph
        .identities
        .feedback_union_matches(
            &resolved.evidence.target_graph.identities,
            &destination_identities,
        )
    {
        return Err(config_error(
            "destination feedback logical union differs from frozen inputs",
        ));
    }
    let destination_snapshots = crate::sqlite_read_snapshot::SnapshotSet::capture_in(
        std::slice::from_ref(&sessions),
        resolved.scratch_root.path(),
    )
    .await
    .map_err(io_error)?;
    sqlite::quick_check_in(&destination_snapshots, &sessions).await?;
    let input_root = destination.join(INPUT_DIR);
    let source_input = input_root.join("source-sessions.db");
    let target_input = input_root.join("target-sessions.db");
    let input_snapshots = crate::sqlite_read_snapshot::SnapshotSet::capture_in(
        &[source_input.clone(), target_input.clone()],
        resolved.scratch_root.path(),
    )
    .await
    .map_err(io_error)?;
    sqlite::verify_session_union_sql(
        &input_snapshots,
        &source_input,
        &target_input,
        &destination_snapshots,
        destination,
        session_offsets,
        &resolved.report.source.project_id,
    )
    .await?;
    Ok(())
}

pub(super) async fn register_destination<R: RegistryRuntime>(
    resolved: &ResolvedPlan,
    registry: &R,
) -> Result<()> {
    let global_path = resolved
        .report
        .destination_data_root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| config_error("destination shard has no profile root"))?
        .join("global.db");
    let db = registry
        .open_at(&global_path)
        .await
        .ok_or_else(|| config_error("could not open global registry for consolidation"))?;
    let project = db
        .upsert_code_project(
            &resolved.report.destination_project_id,
            &resolved.report.project_root,
            Some(&resolved.report.git_common_dir),
            git_remote_url(&resolved.report.project_root).as_deref(),
            Some(&resolved.target_meta.default_branch),
        )
        .await
        .ok_or_else(|| config_error("could not register consolidated project"))?;
    if !db
        .upsert_project_alias(&resolved.report.project_root, &project.project_id)
        .await
    {
        return Err(config_error(
            "could not register consolidated project alias",
        ));
    }
    let store_id = format!("store:{}:profile_sharded", project.project_id);
    let store_relpath = format!("projects/{}", project.project_id);
    let now = crate::tracedecay::current_timestamp();
    if !db
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: store_id.clone(),
            project_id: project.project_id.clone(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: store_relpath.clone(),
            manifest_relpath: Some(format!(
                "{store_relpath}/{}",
                storage::STORE_MANIFEST_FILENAME
            )),
            last_verified_at: Some(now),
            last_write_at: Some(now),
        })
        .await
    {
        return Err(config_error("could not register consolidated store"));
    }

    let meta = branch_meta::load_branch_meta(&resolved.report.destination_data_root)
        .ok_or_else(|| config_error("consolidated branch metadata disappeared"))?;
    for (branch_name, entry) in &meta.branches {
        if !db
            .upsert_graph_scope(GraphScopeUpsert {
                graph_scope_id: format!("{store_id}:branch:{branch_name}"),
                project_id: project.project_id.clone(),
                store_id: store_id.clone(),
                branch_name: branch_name.clone(),
                db_relpath: format!("{store_relpath}/{}", entry.db_file),
                parent_scope_id: entry
                    .parent
                    .as_deref()
                    .map(|parent| format!("{store_id}:branch:{parent}")),
                last_synced_at: entry.last_synced_at.parse().ok(),
                writable: true,
            })
            .await
        {
            return Err(config_error("could not register consolidated graph scope"));
        }
    }
    for (kind, relative) in [
        ("graph_db", crate::config::DB_FILENAME),
        ("sessions_db", storage::SESSIONS_DB_FILENAME),
        ("branch_meta", storage::BRANCH_META_FILENAME),
        ("store_manifest", storage::STORE_MANIFEST_FILENAME),
    ] {
        let path = resolved.report.destination_data_root.join(relative);
        if !db
            .upsert_store_artifact(StoreArtifactUpsert {
                store_id: store_id.clone(),
                artifact_kind: kind.to_string(),
                relpath: format!("{store_relpath}/{relative}"),
                size_bytes: fs::metadata(path)
                    .ok()
                    .and_then(|meta| i64::try_from(meta.len()).ok()),
                schema_version: (kind == "store_manifest")
                    .then(|| storage::STORE_MANIFEST_SCHEMA_VERSION.to_string()),
                updated_at: Some(now),
            })
            .await
        {
            return Err(config_error("could not register consolidated artifact"));
        }
    }
    db.checkpoint().await;
    Ok(())
}

pub(super) fn cut_over_markers(resolved: &ResolvedPlan) -> Result<()> {
    storage::write_enrollment_marker(
        &resolved.report.project_root,
        &EnrollmentMarker {
            project_id: resolved.report.destination_project_id.clone(),
            storage_mode: StorageMode::ProfileSharded,
        },
    )?;
    storage::write_repository_identity_marker(
        &resolved.report.project_root,
        &resolved.report.destination_project_id,
    )?;
    Ok(())
}
