//! Handler for the `tokensave_dashboard` MCP tool.
//!
//! Starts (or stops) the project dashboard HTTP server as a managed background
//! tokio task inside the running MCP server process. Idempotent: returns the
//! existing URL if already running for this process. Supports optional `stop`
//! action to shut down a previously-started instance.

use serde_json::{json, Value};

use crate::errors::{Result, TokenSaveError};
use crate::tokensave::TokenSave;

use super::super::ToolResult;
use super::truncate_response;

use crate::dashboard::{bind_dashboard, router, DashboardState, DEFAULT_PORT};

/// Internal handle for a managed dashboard instance.
struct RunningDashboard {
    url: String,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

/// Global manager for at most one dashboard per MCP server process.
/// Uses `OnceLock` + inner `Mutex` so it can be initialized on first use from async.
static DASHBOARD_MANAGER: std::sync::OnceLock<tokio::sync::Mutex<Option<RunningDashboard>>> =
    std::sync::OnceLock::new();

fn get_manager() -> &'static tokio::sync::Mutex<Option<RunningDashboard>> {
    DASHBOARD_MANAGER.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// Handles `tokensave_dashboard` tool calls.
pub(super) async fn handle_dashboard(cg: &TokenSave, args: Value) -> Result<ToolResult> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("start");

    match action {
        "stop" => {
            let manager = get_manager();
            let mut guard = manager.lock().await;
            let payload = if let Some(handle) = guard.take() {
                let _ = handle.shutdown.send(());
                json!({ "status": "stopped", "previous_url": handle.url })
            } else {
                json!({ "status": "not_running" })
            };
            let formatted = serde_json::to_string_pretty(&payload).unwrap_or_default();
            Ok(ToolResult {
                value: json!({
                    "content": [{ "type": "text", "text": truncate_response(&formatted) }]
                }),
                touched_files: vec![],
            })
        }
        "start" | "" => {
            let host = args
                .get("host")
                .and_then(|v| v.as_str())
                .unwrap_or("127.0.0.1")
                .to_string();
            let port = args
                .get("port")
                .and_then(serde_json::Value::as_u64)
                .and_then(|p| u16::try_from(p).ok())
                .unwrap_or(DEFAULT_PORT);

            let manager = get_manager();
            let mut guard = manager.lock().await;

            if let Some(handle) = guard.as_ref() {
                // already running — idempotent return
                let formatted = serde_json::to_string_pretty(&json!({
                    "status": "already_running",
                    "url": handle.url
                }))
                .unwrap_or_default();
                return Ok(ToolResult {
                    value: json!({
                        "content": [{ "type": "text", "text": truncate_response(&formatted) }]
                    }),
                    touched_files: vec![],
                });
            }

            // Build dashboard state (re-uses the same construction as CLI path)
            let global = crate::global_db::GlobalDb::open().await;
            let state = DashboardState {
                mem_conn: cg.dashboard_connection(),
                mem_db_path: cg.dashboard_db_path().display().to_string(),
                lcm_conn: global
                    .as_ref()
                    .map(crate::global_db::GlobalDb::dashboard_connection),
                lcm_db_path: crate::global_db::global_db_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                project_root: cg.project_root().to_path_buf(),
                curate_preview: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
            };

            let app = router(state);
            let (listener, addr) = bind_dashboard(&host, port).await?;
            let url = format!("http://{addr}/");

            let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

            tokio::spawn(async move {
                // Use with_graceful_shutdown so `stop` can cleanly terminate serve.
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await;
            });

            *guard = Some(RunningDashboard {
                url: url.clone(),
                shutdown: shutdown_tx,
            });

            let formatted = serde_json::to_string_pretty(&json!({
                "status": "started",
                "url": url,
                "host": host,
                "port": addr.port()
            }))
            .unwrap_or_default();

            Ok(ToolResult {
                value: json!({
                    "content": [{ "type": "text", "text": truncate_response(&formatted) }]
                }),
                touched_files: vec![],
            })
        }
        other => Err(TokenSaveError::Config {
            message: format!(
                "unknown action for tokensave_dashboard: {other} (use 'start' or 'stop')"
            ),
        }),
    }
}
