//! Automation runner, ledger, scheduler, and managed-skill runtime.
//!
//! Contracts stay in the wave-1 leaf `tracedecay-automation`. This crate sits
//! at the agent-hosts layer and may depend on usecases, global-db, and
//! sessions. It must not depend on `tracedecay-agent-hosts`: host install
//! helpers that automation used to call on `agents` arrive through
//! [`automation::host_io`] (registered by agent-hosts / the composition root).
//!
//! Historical `crate::automation::*`, `crate::errors`, `crate::ports`, and
//! `crate::agents::*` paths from the agent-hosts extraction keep resolving
//! so the moved modules stay a mechanical move.

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

/// Installs the registered global/session schema into the kernel's fail-closed
/// port for this crate's test process.
#[cfg(test)]
pub(crate) fn register_test_schema_installer() {
    tracedecay_global_db::register_test_schema_installer();
}

pub mod automation;
pub mod ports;

pub(crate) use tracedecay_application::request_identity;
pub(crate) use tracedecay_domain::errors;
pub(crate) use tracedecay_runtime_core::{
    config, db, memory, privacy, runtime_identity, storage, store, worktree,
};
pub(crate) use tracedecay_session_memory as application;

/// Kernel-owned timestamp plus the project-runtime port historically reached
/// as `crate::tracedecay`.
pub(crate) mod tracedecay {
    pub(crate) use crate::ports::project_runtime::TraceDecay;
    pub(crate) use tracedecay_runtime_core::tracedecay::current_timestamp;
}

/// Host-install surface historically reached as `crate::agents`.
///
/// The functions that used to live on `tracedecay-agent-hosts::agents` and
/// that automation still calls are either implemented here (pure helpers) or
/// registered through [`automation::host_io`].
#[allow(unused_imports)]
pub(crate) mod agents {
    pub use crate::automation::host_io::{
        ManagedSkillExportReport, export_managed_skills_to_agent_hosts,
        export_managed_skills_to_agents, home_dir, safe_remove_host_file, safe_write_json_file,
        safe_write_text_file, uses_default_user_profile, with_host_config_write_intents,
    };

    pub(crate) mod plugin_bundle {
        pub use crate::automation::host_io::{PluginFile, codex_agent_files};
    }

    pub(crate) mod prompt_rules {
        pub const SKILL_INDEX_START: &str = crate::automation::host_io::SKILL_INDEX_START;
    }
}
