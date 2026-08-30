//! Consolidated runtime-acceptance integration suite.
//!
//! Former standalone `test-helpers` targets, compiled as modules of one binary
//! so a root-crate edit no longer relinks a dozen ~700 MiB test binaries.

#![recursion_limit = "256"]

#[path = "../common/mod.rs"]
mod common;

mod advisory_runtime_acceptance;
mod application_production_reachability;
#[allow(clippy::unwrap_used)]
mod cross_host_handoff_test;
mod daemon_runtime_acceptance;
mod grafeo_restart_acceptance;
mod host_event_fixture_test;
mod lifecycle_production_authority_test;
mod private_route_restart_acceptance;
mod runtime_surface_acceptance;
#[allow(clippy::option_env_unwrap)]
mod search_eval_cli_test;
#[cfg(unix)]
mod tool_client_transport;
mod windows_durable_behavior;

// Path-included by the former `windows_durable_behavior` crate root so
// `crate::common` / `crate::support` in those files keep resolving.
#[path = "../../../../crates/tracedecay-domain/tests/session_contract.rs"]
mod domain_session_contract;
#[path = "../storage_suite/fact_merge_hydration_test.rs"]
mod fact_merge_hydration;
#[path = "../session_suite/lcm_summary_lineage_review.rs"]
mod lcm_summary_lineage_review;
#[path = "../storage_suite/support.rs"]
mod support;
#[path = "../session_suite/temporal_projection/mod.rs"]
mod temporal_projection;
