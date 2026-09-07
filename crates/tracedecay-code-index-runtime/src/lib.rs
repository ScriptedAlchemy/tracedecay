//! Code-index scheduler, git watch, git transactions, and semantic evaluation.
//!
//! This crate owns the daemon code-index runtime that used to live under
//! `tracedecay::daemon`. The composition root constructs the scheduler and
//! implements [`code_graph_seat::CodeGraphSeatRuntimePortV1`]; this crate must
//! not depend on `tracedecay`.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::similar_names)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::trivially_copy_pass_by_ref)]
#![allow(clippy::unused_self)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::struct_field_names)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::option_option)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::ref_option)]
#![allow(clippy::zero_sized_map_values)]
#![allow(clippy::used_underscore_binding)]
#![allow(clippy::manual_async_fn)]
#![allow(clippy::unused_async)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::if_not_else)]
#![allow(clippy::fn_params_excessive_bools)]
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::missing_fields_in_debug)]
#![allow(clippy::single_match_else)]
#![allow(clippy::large_futures)]
#![allow(unreachable_pub)]

pub mod code_graph_seat;
pub mod code_index_branch_diff;
pub mod code_index_executor;
pub mod code_index_scheduler;
pub mod code_index_task_support;
pub(crate) mod config;
pub mod git_index_transactions;
pub mod git_transactions;
#[cfg(unix)]
pub mod git_watch;
pub(crate) mod logging;
pub mod mcp_admission;
pub(crate) mod ports;
pub mod semantic_activation_reconciler;
pub(crate) mod semantic_code;
pub mod semantic_evaluation;
pub mod semantic_evaluation_shutdown;

/// Historical `crate::code_index` / `crate::query` paths from the root move.
pub(crate) use tracedecay_code_index as code_index;
pub(crate) use tracedecay_query as query;
pub(crate) use tracedecay_search_eval as search_eval;

/// Same abort authority re-exported by `tracedecay-daemon-service`.
pub use tracedecay_runtime_core::DAEMON_TASK_ABORT_DEADLINE;

pub use code_graph_seat::{
    CodeGraphReplayBindingV1, CodeGraphSeatLeaseV1, CodeGraphSeatRuntimePortV1,
};
pub use code_index_scheduler::CodeIndexSchedulerRegistryV1;
pub use code_index_scheduler::identity::resolved_scope_for_project;
pub use ports::{
    AdmissionParkLeaseV1, ApplicationCatalogProviderV1, ApplicationCatalogSnapshotErrorV1,
    CONNECTION_ADMISSION, GitWatchMaintenanceWakeV1, GitWatchSyncConfigV1,
    PreparedQueryActivationViewV1, park_admission,
};
pub use semantic_evaluation_shutdown::{
    SemanticEvaluationShutdownJoinV1, SemanticEvaluationShutdownReceiptV1,
    collect_semantic_evaluation_shutdown,
};

/// Installs the registered global/session schema into the kernel's fail-closed
/// port for this crate's test process.
#[cfg(test)]
pub(crate) fn register_test_schema_installer() {
    tracedecay_global_db::register_test_schema_installer();
}
