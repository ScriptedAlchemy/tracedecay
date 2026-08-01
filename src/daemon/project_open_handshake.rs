//! Opening a project for one handshake, and how open failures are reported.
//!
//! Classifies the failure modes that first-touch bootstrap may repair -
//! never-enrolled identity, a missing index, a read-only store - apart from
//! genuine conflicts, and renders the client-visible refusal.
//!
//! Relocated verbatim from `daemon.rs` as a pure structural split; no logic
//! or signatures changed. `use super::*` re-exposes every name the parent
//! `daemon` module had in scope so the moved code resolves unchanged.

use super::*;

#[cfg(all(test, unix))]
pub(super) async fn open_project_for_handshake(
    project_path: &Path,
    handshake: &DaemonHandshake,
    store_administration: &StoreAdministration,
) -> Result<crate::tracedecay::TraceDecay> {
    let (cg, _) = open_project_for_handshake_with_health_mode(
        project_path,
        handshake,
        store_administration,
        false,
    )
    .await?;
    Ok(cg)
}

pub(super) async fn open_project_for_handshake_with_health_mode(
    project_path: &Path,
    handshake: &DaemonHandshake,
    store_administration: &StoreAdministration,
    defer_post_open_health: bool,
) -> Result<(crate::tracedecay::TraceDecay, Option<crate::db::Database>)> {
    let open_options = handshake.open_options();
    let registry_database = store_administration.registered_profile_database().await?;
    let (store_layout, first_touch) =
        match crate::tracedecay::TraceDecay::resolve_registered_configuration_layout(
            project_path,
            &open_options,
            registry_database.as_ref(),
            true,
        )
        .await
        {
            Ok(layout) => (layout, false),
            // A brand-new project has no enrollment marker, registry match, or
            // legacy shard, so identity resolution fails closed. When the client
            // explicitly asked to initialize (first-touch `tracedecay init`),
            // mint a fresh path-derived identity and let the missing-index
            // fallback below bootstrap it. Existing-but-unresolvable stores
            // raise their own identity-cutover errors instead of this one and
            // still fail closed.
            Err(err) if handshake.allow_init && is_unregistered_identity_error(&err) => (
                crate::tracedecay::TraceDecay::resolve_first_touch_configuration_layout(
                    project_path,
                    &open_options,
                    registry_database.as_ref(),
                    true,
                )
                .await?,
                true,
            ),
            Err(err) if is_unregistered_identity_error(&err) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "no TraceDecay index found at '{}'; run 'tracedecay init' first",
                        project_path.display()
                    ),
                });
            }
            Err(err) => return Err(err),
        };
    let project_id =
        store_layout
            .identity
            .project_id
            .as_deref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "registered project open requires an authoritative project identity"
                    .to_owned(),
            })?;
    // First-touch enrollment: the daemon's registered session runtime resolves
    // a project's store through its on-disk enrollment marker, which a
    // never-seen project does not yet have. Persist it now — under the same
    // minted identity the layout carries — so the session store can mount
    // before init bootstraps the graph. This is the honest first enrollment
    // step, not a bypass: it only runs on the explicit allow_init first-touch
    // path, and a subsequent open resolves this same marker deterministically.
    if first_touch {
        let enrollment_root = crate::worktree::repository_identity_root(project_path)
            .unwrap_or_else(|| project_path.to_path_buf());
        crate::storage::write_enrollment_marker(
            &enrollment_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.to_owned(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )?;
    }
    let configuration_database = store_administration
        .registered_project_session_database(project_path, &store_layout)
        .await?;
    let runtime_registry = store_administration.registered_runtime_registry().await?;
    let open_result = if defer_post_open_health {
        crate::tracedecay::TraceDecay::open_with_registered_configuration_deferred_post_open_health(
            project_path,
            open_options.clone(),
            store_layout.clone(),
            Arc::clone(&configuration_database),
            Arc::clone(&registry_database),
            Arc::clone(&runtime_registry),
        )
        .await
    } else {
        crate::tracedecay::TraceDecay::open_with_registered_configuration(
            project_path,
            open_options.clone(),
            store_layout.clone(),
            Arc::clone(&configuration_database),
            Arc::clone(&registry_database),
            Arc::clone(&runtime_registry),
        )
        .await
    };
    match open_result {
        Ok(cg) => {
            let deferred_post_open_health = defer_post_open_health.then(|| cg.db().clone());
            Ok((cg, deferred_post_open_health))
        }
        Err(open_err) if defer_post_open_health && is_readonly_database_error(&open_err) => {
            match crate::tracedecay::TraceDecay::open_read_only_with_registered_configuration(
                project_path,
                open_options,
                store_layout,
                configuration_database,
                registry_database,
                runtime_registry,
            )
            .await
            {
                Ok(cg) => {
                    cg.ensure_schema_current().await?;
                    Ok((cg, None))
                }
                Err(_) => Err(open_err),
            }
        }
        Err(open_err) if handshake.allow_init && is_missing_index_error(&open_err) => {
            // First-touch (or not-yet-indexed) bootstrap: create and index the
            // store under the daemon's authority. Surface the bootstrap error
            // itself on failure — the original "no index found" open error is a
            // misleading symptom that hides the real reason init could not
            // complete.
            crate::tracedecay::TraceDecay::init_and_index_with_registered_configuration(
                project_path,
                open_options,
                store_layout,
                configuration_database,
                registry_database,
                runtime_registry,
            )
            .await
            .map(|cg| (cg, None))
        }
        Err(open_err) => Err(open_err),
    }
}

/// Whether `err` is the specific fail-closed error raised when identity
/// resolution finds no enrollment marker, registry match, or legacy shard for a
/// project — i.e. a genuinely never-enrolled project. Conflicting or ambiguous
/// *existing* stores raise distinct identity-cutover errors and are excluded, so
/// first-touch bootstrap never masks a real conflict.
fn is_unregistered_identity_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains(
                "registered configuration layout requires an enrolled or registry-resolved project identity"
            )
    )
}

pub(super) fn is_missing_index_error(err: &TraceDecayError) -> bool {
    matches!(
        err,
        TraceDecayError::Config { message }
            if message.contains("no TraceDecay index found")
                || message.contains("no TraceDecay database found")
                || message.contains("parent DB not found")
                || (message.contains("parent branch '") && message.contains("' has no DB"))
    )
}

fn is_readonly_database_error(err: &TraceDecayError) -> bool {
    if !err.is_database_error() {
        return false;
    }
    match err {
        TraceDecayError::Database { message, .. } => {
            message.to_ascii_lowercase().contains("readonly database")
        }
        #[allow(deprecated)]
        TraceDecayError::DatabaseOperation { source, .. } => source
            .to_string()
            .to_ascii_lowercase()
            .contains("readonly database"),
        _ => false,
    }
}

pub(super) async fn write_project_open_error(
    transport: &mut impl McpTransport,
    request_line: &str,
    error: &TraceDecayError,
) -> Result<()> {
    let id = serde_json::from_str::<JsonRpcRequest>(request_line)
        .ok()
        .and_then(|request| request.id)
        .unwrap_or(serde_json::Value::Null);
    let response = project_open_error_response(id, error);
    write_json_rpc_response(transport, &response).await
}

pub(super) fn project_open_error_response(
    id: serde_json::Value,
    error: &TraceDecayError,
) -> JsonRpcResponse {
    match error {
        TraceDecayError::Config { message }
            if message.contains(PROJECT_OPEN_FAILURE_RETRY_HINT) =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_route_open_backoff",
                    "retryable": true,
                    "retry_after_ms": PROJECT_OPEN_FAILURE_RETRY_BACKOFF.as_millis() as u64,
                })),
            )
        }
        TraceDecayError::Config { message }
            if message.starts_with("daemon project open task capacity reached") =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_open_task_capacity_reached",
                    "retryable": true,
                    "capacity": MAX_TRACKED_PROJECT_OPEN_TASKS,
                })),
            )
        }
        TraceDecayError::Config { message }
            if message.starts_with("daemon project server capacity reached") =>
        {
            JsonRpcResponse::error_with_data(
                id,
                ErrorCode::InternalError,
                message.clone(),
                Some(json!({
                    "kind": "project_server_capacity_reached",
                    "retryable": true,
                    "capacity": MAX_CACHED_PROJECT_SERVERS,
                })),
            )
        }
        _ => JsonRpcResponse::error(id, ErrorCode::InternalError, error.to_string()),
    }
}
