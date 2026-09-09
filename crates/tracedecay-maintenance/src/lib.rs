//! Offline retention, complete-profile backups, and storage reports.
//!
//! This crate owns maintenance that consumes explicit global-db and
//! runtime-core storage ports. It never names daemon types. The code-index
//! generation retention kernel lives in `tracedecay-code-index-retention`;
//! storage reports consume it. Profile-registry open/composition that still
//! requires `DaemonSessionRuntimeRegistryV1` stays in the root crate.

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

pub mod clock;
pub mod compaction_receipt;
pub mod generation;
pub mod lease;
pub mod loop_run;
pub mod profile_backup;
pub mod retention;
pub mod store_maintenance;
pub mod telemetry;
pub mod tick;

#[cfg(test)]
mod logging_tests {
    use tracedecay_runtime_core::logging::format_daemon_log_line;

    #[test]
    fn retention_degraded_formats_the_daemon_operator_line() {
        let fields = [
            ("pass", "code_generations".to_owned()),
            (
                "failure",
                "registered_enrollment_inventory_unavailable".to_owned(),
            ),
        ];
        assert_eq!(
            format_daemon_log_line("retention_degraded", &fields),
            "[tracedecay] event=retention_degraded pass=code_generations failure=registered_enrollment_inventory_unavailable"
        );
    }

    #[test]
    fn retention_compaction_formats_freed_pages() {
        let fields = [("freed_pages", "12".to_owned())];
        assert_eq!(
            format_daemon_log_line("retention_compaction", &fields),
            "[tracedecay] event=retention_compaction freed_pages=12"
        );
    }
}
