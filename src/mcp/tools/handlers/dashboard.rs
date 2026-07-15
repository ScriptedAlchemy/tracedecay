//! Handler for the `tracedecay_dashboard` MCP tool.
//!
//! Starts (or stops) the project dashboard HTTP server as a managed background
//! tokio task inside the running MCP server process. Idempotent: returns the
//! existing URL if already running for this process. Supports optional `stop`
//! action to shut down a previously-started instance.

use serde_json::{Value, json};
use std::sync::Arc;

use crate::errors::{Result, TraceDecayError};
use crate::global_db::GlobalDb;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;
use super::super::render;

use crate::dashboard::{
    AutomationSchedulerReconciler, DEFAULT_PORT, DashboardAutomationWriter, bind_dashboard,
    build_state_with_automation_reconciler, router,
};

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

fn validate_mcp_dashboard_host(host: &str) -> Result<&str> {
    let host = host.trim();
    if host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "::1") {
        return Ok(host);
    }

    Err(TraceDecayError::Config {
        message: format!(
            "tracedecay_dashboard host is loopback-only; use 127.0.0.1, localhost, or ::1 (got {host:?})"
        ),
    })
}

fn dashboard_tool_result(cg: &TraceDecay, args: &Value, payload: &Value) -> ToolResult {
    let text = render::finalize(Some(cg.project_root()), args, payload, || {
        render::generic_md(payload)
    });
    ToolResult::new(
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
        vec![],
    )
}

/// Handles `tracedecay_dashboard` tool calls.
pub(super) async fn handle_dashboard(
    cg: &TraceDecay,
    args: Value,
    retained_project_session_db: Option<&Arc<GlobalDb>>,
    automation_scheduler_reconciler: Option<AutomationSchedulerReconciler>,
    automation_writer: DashboardAutomationWriter,
) -> Result<ToolResult> {
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
            Ok(dashboard_tool_result(cg, &args, &payload))
        }
        "start" | "" => {
            let host = args
                .get("host")
                .and_then(|v| v.as_str())
                .map(validate_mcp_dashboard_host)
                .transpose()?
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
                return Ok(dashboard_tool_result(
                    cg,
                    &args,
                    &json!({
                        "status": "already_running",
                        "url": handle.url
                    }),
                ));
            }

            // Shared construction with the CLI path: resolved LCM/session store
            // selection included. No catch-up ingest spawn here — the host
            // MCP server already swept hookless transcripts at startup.
            let state = build_state_with_automation_reconciler(
                cg,
                retained_project_session_db.cloned(),
                automation_scheduler_reconciler,
                automation_writer,
            )
            .await;

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

            Ok(dashboard_tool_result(
                cg,
                &args,
                &json!({
                    "status": "started",
                    "url": url,
                    "host": host,
                    "port": addr.port()
                }),
            ))
        }
        other => Err(TraceDecayError::Config {
            message: format!(
                "unknown action for tracedecay_dashboard: {other} (use 'start' or 'stop')"
            ),
        }),
    }
}
