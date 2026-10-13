/// The task family a catalog tool serves, used to group tools for discovery.
///
/// Listings and analytics read it, but it is not part of the MCP wire
/// catalog, so regrouping never changes what a host sees.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ToolCategory {
    CodeSearch,
    CodeNavigation,
    CodeAnalysis,
    GitReview,
    Edit,
    Memory,
    Sessions,
    Work,
    ProjectRuntime,
    Operations,
}

impl ToolCategory {
    pub const fn label(self) -> &'static str {
        match self {
            Self::CodeSearch => "code search",
            Self::CodeNavigation => "code navigation",
            Self::CodeAnalysis => "code analysis & tests",
            Self::GitReview => "git & review",
            Self::Edit => "edit",
            Self::Memory => "memory",
            Self::Sessions => "sessions",
            Self::Work => "work & workflow",
            Self::ProjectRuntime => "project & runtime",
            Self::Operations => "configuration & automation",
        }
    }

    /// Classifies a catalog tool by its canonical (`tracedecay_`-prefixed) or
    /// short name. Names outside the catalog return `None`.
    pub fn of(tool_name: &str) -> Option<Self> {
        let name = tool_name.strip_prefix("tracedecay_").unwrap_or(tool_name);
        let category = match name {
            "search"
            | "grep"
            | "context"
            | "retrieve"
            | "signature_search"
            | "code_symbol_search"
            | "code_phrase_search"
            | "code_exact_occurrence"
            | "code_facets"
            | "ast_grep_search"
            | "config"
            | "todos"
            | "multi_root_execute" => Self::CodeSearch,
            "node"
            | "callers"
            | "callees"
            | "call_chain"
            | "impact"
            | "affected"
            | "implementations"
            | "type_hierarchy"
            | "derives"
            | "file_dependents"
            | "find_exact_symbol"
            | "by_qualified_name"
            | "qualified_name"
            | "signature"
            | "source_body"
            | "source_lines"
            | "source_outline"
            | "files"
            | "code_declaration"
            | "code_references"
            | "code_type_definition"
            | "code_timeline"
            | "similar"
            | "rename_preview"
            | "port_status"
            | "port_order" => Self::CodeNavigation,
            "dead_code"
            | "module_api"
            | "circular"
            | "hotspots"
            | "unmounted_files"
            | "rank"
            | "largest"
            | "coupling"
            | "inheritance_depth"
            | "distribution"
            | "recursion"
            | "complexity"
            | "doc_coverage"
            | "god_class"
            | "unsafe_patterns"
            | "diagnostics"
            | "diagnose"
            | "constructors"
            | "field_sites"
            | "gini"
            | "dependency_depth"
            | "dsm"
            | "redundancy"
            | "health"
            | "health_delta"
            | "test_map"
            | "test_risk"
            | "test_results"
            | "affected_tests"
            | "run_affected_tests"
            | "feedback_advisory_cycle"
            | "feedback_diagnostics"
            | "feedback_expand"
            | "feedback_get"
            | "feedback_impact"
            | "feedback_list"
            | "feedback_proximity" => Self::CodeAnalysis,
            "diff_context"
            | "changelog"
            | "commit_context"
            | "pr_context"
            | "branch_search"
            | "branch_diff"
            | "branch_list"
            | "git_status"
            | "git_diff"
            | "git_history"
            | "git_hunks"
            | "git_blame"
            | "git_preview"
            | "git_apply"
            | "stack_snapshot"
            | "github_stack_signal_expand"
            | "preflight_native_integration"
            | "approve_native_integration"
            | "apply_native_integration"
            | "cancel_native_integration"
            | "native_integration_status"
            | "worktree_inventory"
            | "worktree_cleanup_inspect"
            | "worktree_cleanup_confirm"
            | "worktree_cleanup_reconcile"
            | "worktree_cleanup_remove" => Self::GitReview,
            "str_replace"
            | "multi_str_replace"
            | "insert_at"
            | "insert_at_symbol"
            | "replace_symbol"
            | "ast_grep_rewrite"
            | "move_symbol"
            | "rename_symbol"
            | "source_edit_reconcile"
            | "source_edit_rollback" => Self::Edit,
            "memory_status"
            | "fact_feedback"
            | "fact_store_add"
            | "fact_store_search"
            | "fact_store_probe"
            | "fact_store_related"
            | "fact_store_reason"
            | "fact_store_contradict"
            | "fact_store_curate"
            | "fact_store_get"
            | "fact_store_list"
            | "fact_store_update"
            | "fact_store_remove"
            | "fact_store_supersede" => Self::Memory,
            "message_search"
            | "sessions_for"
            | "session_lookup"
            | "session_refresh_begin"
            | "session_refresh_cancel"
            | "session_refresh_status"
            | "workflows"
            | "lcm_status"
            | "lcm_doctor"
            | "lcm_load_session"
            | "lcm_grep"
            | "lcm_describe"
            | "lcm_expand"
            | "lcm_expand_query"
            | "context_scout_budget"
            | "context_scout_cancel"
            | "context_scout_capability"
            | "context_scout_claim"
            | "context_scout_delivery"
            | "context_scout_explain"
            | "context_scout_feedback"
            | "context_scout_pause"
            | "context_scout_recent"
            | "context_scout_resume"
            | "context_scout_status" => Self::Sessions,
            "work_accept_proposal"
            | "work_adjudicate_duplicate"
            | "work_adjudicate_leak"
            | "work_admit_execution"
            | "work_admit_placement"
            | "work_attempt_status"
            | "work_cancel_attempt"
            | "work_compare_proposal"
            | "work_create"
            | "work_execution_history"
            | "work_experience"
            | "work_generate_proposal"
            | "work_hydrate_artifacts"
            | "work_list_attempts"
            | "work_mutate_graph"
            | "work_pause_run"
            | "work_placement_preflight"
            | "work_placement_status"
            | "work_prepare_duplicate_adjudication"
            | "work_prepare_graph_mutation"
            | "work_release_placement"
            | "work_resume_attempts"
            | "work_resume_run"
            | "work_retrieve_evidence"
            | "work_retry_attempt"
            | "work_review_proposal"
            | "work_run_control"
            | "work_start_attempt"
            | "work_synthesize"
            | "work_topology"
            | "work_topology_metrics"
            | "work_views"
            | "workflow_activate_definition"
            | "workflow_cancel_run"
            | "workflow_definition_history"
            | "workflow_diff_definition"
            | "workflow_get_definition"
            | "workflow_get_run"
            | "workflow_handoff_issue"
            | "workflow_handoff_redeem"
            | "workflow_list_definitions"
            | "workflow_pause_run"
            | "workflow_register_definition"
            | "workflow_reject_definition"
            | "workflow_resume_run"
            | "workflow_retire_definition"
            | "workflow_start_run"
            | "workflow_validate_definition" => Self::Work,
            "status"
            | "active_project"
            | "storage_status"
            | "remote_status"
            | "health_read"
            | "runtime"
            | "observatory_read"
            | "project_list"
            | "project_search"
            | "project_context"
            | "multi_root_scope_set_read"
            | "multi_root_scope_set_compare_and_swap" => Self::ProjectRuntime,
            "configuration_audit"
            | "configuration_batch"
            | "configuration_get"
            | "configuration_list"
            | "configuration_observed_state"
            | "configuration_protected_apply"
            | "configuration_protected_preview"
            | "configuration_rollback_apply"
            | "configuration_rollback_preview"
            | "configuration_set"
            | "configuration_unset"
            | "skill_list"
            | "skill_view"
            | "hermes_skill_bridge"
            | "automation_run_list"
            | "automation_run_view"
            | "automation_run_artifact_view"
            | "dashboard"
            | "analytics" => Self::Operations,
            _ => return None,
        };
        Some(category)
    }
}
