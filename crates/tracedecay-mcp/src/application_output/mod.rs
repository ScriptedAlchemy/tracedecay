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
    use serde_json::{Value, json};
    use tracedecay_daemon_protocol::RequestedOutputFormat;
    use tracedecay_tool_catalog::BindingId;

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
}
