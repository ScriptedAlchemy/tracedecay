//! Shared canonical application-result presentation contracts.

pub mod json;
pub mod markdown;
pub mod tool_result;
pub mod view;

/// One rendered answer without the per-call values that differ on every call
/// for the same answer, in either output format. Every other byte is kept.
pub fn without_per_call_fields(text: &str) -> String {
    json::without_per_call_fields(text).unwrap_or_else(|| markdown::without_per_call_lines(text))
}

#[cfg(test)]
mod tests {
    use crate::tools::response_trailers::account_tool_result;
    use serde_json::{Value, json};
    use tracedecay_contracts::retrieval::{CodeGraphReadFreshnessV1, ServedCodeGraphGenerationV1};
    use tracedecay_contracts::{
        ApplicationEnvelope, ApplicationOutcome, RequestId, ResolvedScope, ResultContractRef,
    };
    use tracedecay_daemon_protocol::RequestedOutputFormat;
    use tracedecay_domain::{ProjectId, RepositoryId, UtcMicros, WorktreeId};
    use tracedecay_tool_catalog::{BindingId, SchemaId};

    use super::tool_result::render_application_result;
    use super::without_per_call_fields;

    /// A source-outline evidence envelope as the daemon serves it, with every
    /// per-call value derived from `call` and the outline from `symbol`.
    fn envelope(call: u64, symbol: &str) -> Value {
        let at = 1_792_079_618_000_000 + call * 1_000;
        json!({
            "contract": {"schema_id": "schema.application.primitive.source-outline.result", "schema_revision": 1},
            "request_id": format!("request.mcp.{call:032x}"),
            "scope": {
                "project_id": "proj_eb257f12ca4b64ff",
                "reference": "refs/heads/main",
                "repository_id": "repository.daemon.source",
                "scope_digest": format!("sha256:{}", "4".repeat(64)),
                "worktree_id": "worktree.daemon.source",
            },
            "cost": {
                "wall_micros": call * 31,
                "point_reads": {"graph_sealed": call, "graph_staging": 0},
                "adjacency_queries": 1,
                "adjacency_rows": 2,
                "bytes_hydrated": 3,
                "catalog_symbols": 4,
            },
            "outcome": {"outcome": "evidence", "value": {
                "authority": {
                    "authorized_scope_digest": format!("sha256:{}", "4".repeat(64)),
                    "disclosure": "evidence",
                    "grant_digest": format!("sha256:{}", "2".repeat(64)),
                    "grant_id": "grant.daemon.callable-code.2",
                    "grant_revision": 1,
                    "policy": {
                        "decision_id": "route.callable-code.binding.tracedecay-daemon.project-open",
                        "digest": format!("sha256:{}", "f".repeat(64)),
                        "evaluator_revision": "project-source-access.v1",
                        "revision": 1,
                    },
                    "revalidated_at": at,
                },
                "contributions": [],
                "coverage": {
                    "completeness": "complete",
                    "domains": [{"completeness": "complete", "domain": "source"}],
                    "eligible": 1,
                    "requested_domains": ["source"],
                    "returned": 1,
                    "visited": 1,
                },
                "evidence_authorities": [],
                "execution": {
                    "budget": {"bytes_consumed": 0, "elapsed_micros": call * 7, "units_consumed": 0},
                    "cancellation": null,
                    "effective_deadline": {"expires_at": at + 10_000_000},
                    "ended_at": at + 500,
                    "started_at": at,
                    "termination": "completed",
                },
                "omissions": [],
                "page": {
                    "cursor": null,
                    "expires_at": null,
                    "returned": 1,
                    "sort_contract_id": "sort.application.primitive.v1",
                    "sort_revision": 1,
                    "total": 1,
                },
                "payload": {"file": "src/lib.rs", "symbols": [{"name": symbol, "line": 1}]},
                "scores": [],
                "temporal": {
                    "freshness": "current",
                    "requested_at": at,
                    "requested_mode": {"kind": "current"},
                    "resolved_at": at,
                    "source_generation": null,
                    "watermark_digest": null,
                },
            }},
        })
    }

    fn rendered(call: u64, symbol: &str, format: RequestedOutputFormat) -> String {
        let envelope = serde_json::from_value(envelope(call, symbol)).expect("evidence envelope");
        let result = render_application_result(
            None,
            "source_outline",
            &BindingId::new("binding.mcp.source_outline.v1").unwrap(),
            &Ok(envelope),
            format,
        )
        .expect("rendered");
        result.value["content"][0]["text"]
            .as_str()
            .expect("text")
            .to_owned()
    }

    #[test]
    fn both_formats_drop_exactly_the_per_call_values() {
        for format in [RequestedOutputFormat::Json, RequestedOutputFormat::Markdown] {
            let first = rendered(1, "reserve_stock", format);
            let repeat = rendered(2, "reserve_stock", format);
            let changed = rendered(3, "release_stock", format);
            assert_ne!(first, repeat, "{format:?} answers carry per-call values");
            assert_eq!(
                without_per_call_fields(&first),
                without_per_call_fields(&repeat),
                "{format:?}: {first}"
            );
            assert_ne!(
                without_per_call_fields(&first),
                without_per_call_fields(&changed),
                "{format:?} answers with another result stay distinct"
            );
        }
    }

    fn texts(result: &crate::ToolResult) -> Vec<String> {
        result.value["content"]
            .as_array()
            .expect("content blocks")
            .iter()
            .map(|block| block["text"].as_str().expect("text block").to_owned())
            .collect()
    }

    /// The shared renderer takes the stale trailer and the touched files from
    /// the envelope alone, so any typed tool gains them in either format.
    #[test]
    fn envelope_freshness_and_files_render_the_trailer_and_footer_in_both_formats() {
        let root = tempfile::tempdir().expect("project");
        std::fs::create_dir_all(root.path().join("src")).expect("src");
        std::fs::write(root.path().join("src/lib.rs"), "a".repeat(800)).expect("source");
        let envelope = ApplicationEnvelope {
            contract: ResultContractRef::new(SchemaId::new("schema.test.result").unwrap(), 1)
                .unwrap(),
            request_id: RequestId::new("request.mcp.render.trailers").unwrap(),
            scope: ResolvedScope::new(
                ProjectId::new("project.mcp.render").unwrap(),
                RepositoryId::new("repository.mcp.render").unwrap(),
                WorktreeId::new("worktree.mcp.render").unwrap(),
                None,
            )
            .unwrap(),
            outcome: ApplicationOutcome::Result(json!({"items": ["src/lib.rs::answer"]})),
            touched_files: vec!["src/lib.rs".to_owned()],
            code_graph: Some(ServedCodeGraphGenerationV1 {
                generation: "generation.render.stale.1".to_owned(),
                worktree: None,
                freshness: CodeGraphReadFreshnessV1::LastCompleteStale {
                    sealed_at: UtcMicros(10),
                    rebuild_in_flight: true,
                },
            }),
            analytics: None,
            cost: None,
        };
        let binding = BindingId::new("binding.mcp.code-callers.v1").unwrap();
        for format in [RequestedOutputFormat::Markdown, RequestedOutputFormat::Json] {
            let mut rendered = render_application_result(
                Some(root.path()),
                "code_callers",
                &binding,
                &Ok(envelope.clone()),
                format,
            )
            .expect("rendered");
            assert_eq!(rendered.touched_files, vec!["src/lib.rs".to_owned()]);
            account_tool_result(Some(root.path()), &mut rendered);
            let blocks = texts(&rendered);
            assert_eq!(blocks.len(), 3, "{format:?}: {blocks:?}");
            assert!(
                blocks[1].starts_with(
                    "\ncode_graph_freshness: stale, serving the last complete generation \
                     generation.render.stale.1 (sealed "
                ),
                "{format:?}: {blocks:?}"
            );
            let after = (blocks[0].len() + blocks[1].len()) / 4;
            assert_eq!(
                blocks[2],
                format!("\ntracedecay_metrics: before=200 after={after} unit=bytes/4"),
                "{format:?}"
            );
        }
    }
}
