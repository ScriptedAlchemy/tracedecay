use crate::automation::config_error;
use crate::errors::Result;
use crate::mcp::tools::SessionAuthorities;
use serde_json::{Value, json};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::hermes::user_review;
use super::ingest::ingest_transcript_with_cancellation;
use super::required_str;

/// Admit a Codex terminal receipt by retaining its follow-up work in
/// the daemon. The hook only receives this acknowledgement; transcript ingest
/// and review are cancellable daemon-owned work for that exact session.
pub(super) fn retain_codex_stop(
    args: &Value,
    profile_root: &Path,
    session_runtime_registry: &Arc<
        crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1,
    >,
    session_authorities: SessionAuthorities<'_>,
) -> Result<Value> {
    let session_id = required_str(args, "session_id")?.to_owned();
    let user_sessions = session_authorities
        .user
        .cloned()
        .ok_or_else(|| config_error("daemon user session database is unavailable"))?;
    let profile_identity = session_authorities
        .profile_identity
        .cloned()
        .ok_or_else(|| config_error("daemon profile identity is unavailable"))?;
    let profile_registered = session_authorities
        .profile_registered
        .cloned()
        .ok_or_else(|| config_error("daemon profile admission authority is unavailable"))?;
    let profile_root = profile_root.to_path_buf();
    let weak_registry = Arc::downgrade(session_runtime_registry);
    let task_session_id = session_id.clone();
    let accepted = session_runtime_registry.retain_hook_task(
        "codex",
        &session_id,
        move |cancellation| async move {
            if cancellation.is_cancelled() {
                return;
            }
            let Some(session_runtime_registry) = weak_registry.upgrade() else {
                return;
            };
            let Ok(global_db) = session_runtime_registry.profile_database().await else {
                return;
            };
            let ingest_args = json!({
                "action": "ingest_transcript",
                "provider": "codex",
                "user_scope": true,
                "session_id": task_session_id,
            });
            let authorities = SessionAuthorities::new(None, Some(&user_sessions))
                .with_profile_identity(Some(&profile_identity))
                .with_registered_databases(None, Some(&profile_registered));
            let ingested = ingest_transcript_with_cancellation(
                None,
                &ingest_args,
                Some(&profile_root),
                Some(global_db.as_ref()),
                authorities,
                &cancellation,
            )
            .await
            .ok()
            .and_then(|result| result.get("messages_upserted").and_then(Value::as_u64))
            .is_some_and(|count| count > 0);
            if ingested && !cancellation.is_cancelled() {
                if let Some(session_id) = ingest_args.get("session_id").cloned() {
                    let _ = await_terminal_operation(
                        &cancellation,
                        user_review(
                            &json!({
                                "action": "user_review",
                                "provider": "codex",
                                "session_id": session_id,
                            }),
                            &profile_root,
                            &session_runtime_registry,
                        ),
                    )
                    .await;
                }
            }
        },
    );
    if !accepted {
        return Err(config_error(
            "daemon retained terminal-hook task is unavailable",
        ));
    }
    Ok(json!({
        "action": "codex_stop",
        "status": "accepted",
        "session_id": session_id,
    }))
}

pub(super) async fn await_terminal_operation<T>(
    cancellation: &crate::application::observation::ObservationCancellation,
    operation: impl Future<Output = T>,
) -> Option<T> {
    tokio::pin!(operation);
    loop {
        if cancellation.is_cancelled() {
            return None;
        }
        tokio::select! {
            output = &mut operation => return Some(output),
            () = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }
}
