//! Verified file-inventory admission for `tracedecay://files`.

use serde_json::Value;
use tracedecay_contracts::graph_tool::GraphToolResultV1;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::retrieval::{
    CallableCodeOperationKind, FilesLayoutV1, FilesSurfaceRequestV1, ServedCodeGraphGenerationV1,
    callable_code_operation,
};
use tracedecay_contracts::{CancellationSignal, Deadline};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_graph_query::VerifiedGraphQueryRequest;
use tracedecay_mcp::handlers::graph::{freshness_lines, graph_read_freshness};
use tracedecay_mcp::handlers::info::{compute_files, render_files_md};
use tracedecay_mcp::tools::dispatch_ceiling::TOOL_DISPATCH_CEILING;

use super::{JsonRpcResponse, McpServer};

const FILES_RESOURCE_URI: &str = "tracedecay://files";
const FILES_RESOURCE_MIME: &str = "text/plain";

impl McpServer {
    /// Admits the verified generation's file inventory for the active project.
    ///
    /// The resource uses the same source-metadata graph operation, request
    /// identity, deadline, and cancellation the `tracedecay_files` tool
    /// carries. A missing port, cancelled or expired admission, or an
    /// unready generation stays a typed unavailable body. The listing is
    /// never backfilled from a raw or stale store.
    pub(crate) async fn read_resource_files(&self, id: Value) -> JsonRpcResponse {
        let text = match self.admit_verified_file_inventory().await {
            Ok(text) => text,
            Err(text) => text,
        };
        Self::resource_contents(id, FILES_RESOURCE_URI, FILES_RESOURCE_MIME, &text)
    }

    async fn admit_verified_file_inventory(&self) -> Result<String, String> {
        let Some(port) = self.verified_graph_query_port.as_deref() else {
            return Err(files_resource_unavailable("authority_unavailable"));
        };
        let request_id = mint_global_request_id(GlobalRequestSurface::McpFallback)
            .map_err(|_| files_resource_unavailable("authority_unavailable"))?;
        let cancellation = CancellationSignal::active("cancellation.mcp.resources.files")
            .map_err(|_| files_resource_unavailable("authority_unavailable"))?;
        let deadline = resource_files_deadline()?;
        if deadline.is_elapsed_at(tracedecay_contracts::now_micros()) {
            return Err(files_resource_unavailable("timed_out"));
        }
        let operation = callable_code_operation(CallableCodeOperationKind::SourceMetadata)
            .map_err(|_| files_resource_unavailable("authority_unavailable"))?;
        let graph = port
            .open(VerifiedGraphQueryRequest::new(
                &operation,
                request_id,
                deadline,
                &cancellation,
            ))
            .await
            .map_err(|error| files_resource_from_error(&error))?;
        if cancellation.is_cancelled() {
            return Err(files_resource_unavailable("cancelled"));
        }

        let cg = self.reopen_if_branch_drifted().await;
        let freshness_payload = if let Some(reader) =
            self.dashboard_code_index_freshness_reader.as_ref()
        {
            match reader(cg.project_root().to_path_buf()).await {
                Ok(worktree) => Some(
                    tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1::from_scheduler_read(
                        worktree,
                    ),
                ),
                Err(failure) => Some(
                    tracedecay_contracts::code_index_freshness::CodeIndexFreshnessPayloadV1::from_read_failure(
                        failure,
                    ),
                ),
            }
        } else {
            None
        };
        let worktree_omitted_sources = freshness_payload.as_ref().and_then(|payload| {
            payload
                .worktrees
                .iter()
                .next()
                .filter(|worktree| {
                    worktree.latest_generation_id.as_deref() == Some(graph.generation().as_str())
                })
                .and_then(|worktree| worktree.omitted_sources.clone())
        });
        let completion = compute_files(
            &graph,
            FilesSurfaceRequestV1 {
                path: None,
                pattern: None,
                layout: Some(FilesLayoutV1::Grouped),
            },
            self.scope_prefix(),
            worktree_omitted_sources,
        )
        .await
        .map_err(|error| files_resource_from_error(&error))?;
        let GraphToolResultV1::Files(result) = completion.result else {
            return Err(files_resource_unavailable("authority_unavailable"));
        };
        let freshness = graph_read_freshness(
            &ServedCodeGraphGenerationV1 {
                generation: graph.generation().as_str().to_owned(),
                freshness: graph.freshness(),
                worktree: None,
            },
            freshness_payload.as_ref(),
        );
        let mut text = freshness_lines(&freshness);
        text.push_str(&render_files_md(&result));
        Ok(text)
    }
}

fn resource_files_deadline() -> Result<Deadline, String> {
    let horizon_micros = i64::try_from(TOOL_DISPATCH_CEILING.as_micros())
        .map_err(|_| files_resource_unavailable("timed_out"))?;
    Deadline::new(UtcMicros(
        tracedecay_contracts::now_micros()
            .0
            .saturating_add(horizon_micros),
    ))
    .map_err(|_| files_resource_unavailable("timed_out"))
}

fn files_resource_unavailable(reason: &str) -> String {
    format!("status: unavailable\nreason: {reason}")
}

fn files_resource_from_error(error: &TraceDecayError) -> String {
    files_resource_unavailable(files_resource_reason(error))
}

fn files_resource_reason(error: &TraceDecayError) -> &'static str {
    match error.project_route_context() {
        Some((reason_code, _, _)) => files_resource_reason_code(reason_code),
        None => "authority_unavailable",
    }
}

fn files_resource_reason_code(reason_code: &str) -> &'static str {
    match reason_code {
        "verified-code-graph-read-unavailable" => "authority_unavailable",
        "store_open_cancelled" | "cancelled" => "cancelled",
        "timed_out" | "deadline_exceeded" => "timed_out",
        "generation_unavailable" => "generation_unavailable",
        "generation_unverified" => "generation_unverified",
        "graph_warming" => "graph_warming",
        _ if reason_code.contains("cancel") => "cancelled",
        _ if reason_code.contains("deadline") || reason_code.contains("timeout") => "timed_out",
        _ if reason_code.contains("warming") => "graph_warming",
        _ if reason_code.contains("unverified") => "generation_unverified",
        _ if reason_code.contains("generation") => "generation_unavailable",
        _ => "authority_unavailable",
    }
}

#[cfg(test)]
mod tests {
    use super::{files_resource_reason, files_resource_unavailable};
    use tracedecay_domain::errors::TraceDecayError;

    #[test]
    fn missing_graph_authority_stays_typed_unavailable() {
        let error = TraceDecayError::project_route(
            "verified-code-graph-read-unavailable",
            false,
            "the exact project verified graph query is not mounted",
        );
        assert_eq!(files_resource_reason(&error), "authority_unavailable");
        assert_eq!(
            files_resource_unavailable("authority_unavailable"),
            "status: unavailable\nreason: authority_unavailable"
        );
    }

    #[test]
    fn converging_and_cancelled_admissions_keep_their_typed_reasons() {
        assert_eq!(
            files_resource_reason(&TraceDecayError::project_route(
                "generation_unavailable",
                true,
                "the verified generation is still converging",
            )),
            "generation_unavailable"
        );
        assert_eq!(
            files_resource_reason(&TraceDecayError::project_route(
                "graph_warming",
                true,
                "the admitted generation is reopening",
            )),
            "graph_warming"
        );
        assert_eq!(
            files_resource_reason(&TraceDecayError::project_route(
                "store_open_cancelled",
                true,
                "the caller cancelled before admission finished",
            )),
            "cancelled"
        );
    }
}
