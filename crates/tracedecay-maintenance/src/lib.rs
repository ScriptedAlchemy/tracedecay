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
// Pedantic docs: Results already carry typed errors; this crate is not a
// rustdoc surface.
#![allow(clippy::missing_errors_doc)]
// Pedantic `#[must_use]` on every bool/query helper across the kernel.
#![allow(clippy::must_use_candidate)]
// Extracted retention/orphan/backup passes are still long, ordered journeys.
#![allow(clippy::too_many_lines)]
// Existing if-let / match style in those journeys.
#![allow(clippy::manual_let_else)]
#![allow(clippy::single_match_else)]
#![allow(clippy::match_same_arms)]
// Helpers sit next to the pass they serve.
#![allow(clippy::items_after_statements)]
// Owned error values and explicit port arguments at crate boundaries.
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::too_many_arguments)]
// Telemetry record methods keep `&self` so the registry stays the owner.
#![allow(clippy::unused_self)]
// Store-file suffix checks and sqlite page/clock conversions.
#![allow(clippy::case_sensitive_file_extension_comparisons)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
// Uniform fallible surfaces that currently always return Ok.
#![allow(clippy::unnecessary_wraps)]

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
