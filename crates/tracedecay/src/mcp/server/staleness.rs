//! Index-freshness admission for edit-shaped MCP tools.

pub(crate) fn needs_lazy_sync_before_dispatch(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "tracedecay_ast_grep_rewrite"
            | "tracedecay_insert_at"
            | "tracedecay_insert_at_symbol"
            | "tracedecay_move_symbol"
            | "tracedecay_multi_str_replace"
            | "tracedecay_replace_symbol"
            | "tracedecay_str_replace"
    )
}
