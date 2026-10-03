//! Session / LCM / refresh family: transcript-derived reads plus the
//! refresh lifecycle (begin → status/cancel as effects over the
//! {scope,session,source,target} selector envelope).

use serde_json::{Value, json};

use crate::queries::{PrimeStep, QueryContext, ToolGroup, five};

use super::{eq, rq};

fn lookup_meta(order: &str, page_size: u32) -> Value {
    json!({
        "order": order,
        "page": {"page_size": page_size, "cursor": null},
        "projection": "summary",
        "temporal": {"kind": "current"},
    })
}

fn sid(ctx: &QueryContext) -> String {
    ctx.seeds
        .lcm_session
        .clone()
        .unwrap_or_else(|| "td-bench-missing".into())
}

/// `{scope,session,source,target}` refreshed per call; `None` means the seed
/// begin failed and the refresh groups run the not-found lane instead.
fn selectors(ctx: &QueryContext) -> Value {
    ctx.seeds
        .refresh_selectors
        .clone()
        .unwrap_or_else(|| json!({"missing": true}))
}

/// Selectors + a minted handle (status/cancel inputs).
fn refresh_args(ctx: &QueryContext) -> Value {
    let mut args = selectors(ctx);
    args["handle"] = json!(ctx
        .seeds
        .refresh_handle
        .clone()
        .unwrap_or_else(|| "td-bench-refresh-missing".into()));
    args
}

pub(crate) fn groups(ctx: &QueryContext, out: &mut Vec<ToolGroup>) {
    let session = sid(ctx);
    let msg_id = ctx
        .seeds
        .lcm_message_id
        .clone()
        .unwrap_or_else(|| "td-bench-message-missing".into());

    // Session/LCM reads need an ingested session — the seed writes one when
    // transcript ingest is mounted under this composition; otherwise the
    // whole family stays an honest seed-ledger skip.
    let has_session = ctx.seeds.lcm_session.is_some();
    if has_session {
    out.push(ToolGroup {
        tool: "tracedecay_message_search",
        queries: five(|i| {
            rq(
                "tracedecay_message_search",
                "message_search",
                json!({
                    "query": *["bench", "import", "fn", "error", "test"].iter().nth(i).unwrap_or(&""),
                    "session_id": session,
                    "provider": "codex",
                    "limit": 10,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_session_lookup",
        queries: five(|i| {
            rq(
                "tracedecay_session_lookup",
                "session_lookup",
                json!({
                    "session_id": session,
                    "meta": lookup_meta("temporal_descending", 5 + i as u32),
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_workflows",
        queries: five(|_i| {
            rq(
                "tracedecay_workflows",
                "workflows",
                json!({"session_id": session, "limit": 10}),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_lcm_describe",
        queries: five(|_i| {
            rq(
                "tracedecay_lcm_describe",
                "lcm_describe",
                json!({
                    "provider": "codex",
                    "session_id": session,
                    "target": {"kind": "canonical_occurrence", "message_id": msg_id},
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_lcm_expand_query",
        queries: five(|i| {
            rq(
                "tracedecay_lcm_expand_query",
                "lcm_expand_query",
                json!({
                    "provider": "codex",
                    "session_id": session,
                    "prompt": *["bench", "mount", "session", "store", "index"].iter().nth(i).unwrap_or(&""),
                    "max_results": 10,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_lcm_expand",
        queries: five(|_i| {
            rq(
                "tracedecay_lcm_expand",
                "lcm_expand",
                json!({
                    "provider": "codex",
                    "session_id": session,
                    "target": {"kind": "canonical_occurrence", "message_id": msg_id},
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_lcm_grep",
        queries: five(|i| {
            rq(
                "tracedecay_lcm_grep",
                "lcm_grep",
                json!({
                    "query": *["bench", "import", "fn", "error", "test"].iter().nth(i).unwrap_or(&""),
                    "session_id": session,
                    "provider": "codex",
                    "limit": 10,
                }),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_lcm_status",
        queries: five(|i| {
            rq(
                "tracedecay_lcm_status",
                "lcm_status",
                json!({
                    "session_id": session,
                    "provider": "codex",
                    "deep": i % 2 == 0,
                }),
            )
        }),
    });
    }
    out.push(ToolGroup {
        tool: "tracedecay_lcm_doctor",
        queries: five(|_i| {
            rq("tracedecay_lcm_doctor", "lcm_doctor", json!({}))
        }),
    });
    if let Some(branch) = ctx.seeds.branch.clone() {
        out.push(ToolGroup {
            tool: "tracedecay_sessions_for",
            queries: five(|_i| {
                rq(
                    "tracedecay_sessions_for",
                    "sessions_for",
                    json!({"git_ref": "branch", "value": branch, "limit": 10}),
                )
            }),
        });
    }

    // refresh: begin is the timed effect; status/cancel take the same
    // selector envelope plus the minted handle. Skips with the session
    // family when no refreshable session exists.
    if ctx.seeds.refresh_selectors.is_some() {
    out.push(ToolGroup {
        tool: "tracedecay_session_refresh_begin",
        queries: five(|_i| {
            eq(
                "tracedecay_session_refresh_begin",
                "refresh_begin",
                selectors(ctx),
                |_ctx, _iter| Vec::new(),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_session_refresh_status",
        queries: five(|_i| {
            rq(
                "tracedecay_session_refresh_status",
                "refresh_status",
                refresh_args(ctx),
            )
        }),
    });
    out.push(ToolGroup {
        tool: "tracedecay_session_refresh_cancel",
        queries: five(|_i| {
            let mut args = selectors(ctx);
            args["handle"] = json!("{{handle}}");
            eq(
                "tracedecay_session_refresh_cancel",
                "refresh_cancel",
                args,
                |ctx, _iter| vec![begin_prime(ctx)],
            )
        }),
    });
    }
}

fn begin_prime(ctx: &QueryContext) -> PrimeStep {
    let mut args = selectors(ctx);
    args["format"] = json!("json");
    PrimeStep {
    inject: Vec::new(),
        tool: "tracedecay_session_refresh_begin",
        args,
        capture: &[("dig:handle", "handle")],
    }
}
