mod bench;
mod branch;
mod daemon;
mod gain;
mod index;
mod profile_storage;
mod scope;
mod settings;
mod storage;

pub(crate) use bench::handle_bench;
pub(crate) use branch::handle_branch_action;
pub(crate) use daemon::{
    daemon_tool_json, daemon_tool_json_until, recover_truncated_mcp_result,
    reject_truncation_envelope, retained_tool_payload,
};
pub use gain::handle_gain;
pub(crate) use index::{handle_init, handle_no_command, handle_sync};
pub(crate) use profile_storage::handle_profile_storage_action;
pub(crate) use scope::resolve_project_scope;
pub(crate) use settings::{
    canonical_upload_enabled, current_configuration_revision, current_project_setting,
    handle_gitignore, handle_upload_counter, mutate_project_configuration,
    project_configuration_set, report_configuration_receipt,
};
pub(crate) use storage::{
    ProfileOfflineAuthority, handle_list, handle_wipe, join_outcome_and_restore,
    take_profile_offline,
};
