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

/// Operator-log line for a maintenance kernel. Callers supply structured fields.
pub fn log_maintenance_event(event: &str, fields: &[(&str, String)]) {
    if event == "retention_degraded" {
        tracing::warn!(target: "tracedecay_maintenance", event, ?fields, "maintenance event");
    } else {
        tracing::info!(target: "tracedecay_maintenance", event, ?fields, "maintenance event");
    }
}

#[cfg(test)]
mod logging_tests {
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn degraded_retention_is_visible_at_warn_while_success_remains_informational() {
        for level in [tracing::Level::WARN, tracing::Level::INFO] {
            let buffer = Arc::new(Mutex::new(Vec::new()));
            let writer = Capture(Arc::clone(&buffer));
            let subscriber = tracing_subscriber::fmt()
                .without_time()
                .with_max_level(level)
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                super::log_maintenance_event(
                    "retention_degraded",
                    &[
                        ("pass", "code_generations".to_owned()),
                        (
                            "failure",
                            "registered_enrollment_inventory_unavailable".to_owned(),
                        ),
                    ],
                );
                super::log_maintenance_event(
                    "retention_compaction",
                    &[("freed_pages", "12".to_owned())],
                );
            });
            let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
            assert!(output.contains("WARN"));
            assert!(output.contains("retention_degraded"));
            assert!(output.contains("code_generations"));
            assert!(output.contains("registered_enrollment_inventory_unavailable"));
            assert_eq!(
                output.contains("retention_compaction"),
                level == tracing::Level::INFO
            );
        }
    }
}
