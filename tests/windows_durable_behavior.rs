//! Durable temporal, LCM, storage, and contract behavior used by Windows CI.
//!
//! This target consolidates the retained behavior through the same product
//! helpers as the ordinary suites. CI selects this binary as a whole instead
//! of maintaining module-prefix filters or expected test counts.

pub use tracedecay::{db, errors, global_db};

#[path = "common/mod.rs"]
mod common;
#[path = "storage_suite/support.rs"]
mod support;

#[path = "session_suite/lcm_payload.rs"]
mod lcm_payload;
#[path = "session_suite/lcm_query/mod.rs"]
mod lcm_query;
#[path = "session_suite/lcm_schema/mod.rs"]
mod lcm_schema;
#[path = "session_suite/lcm_summary_lineage_review.rs"]
mod lcm_summary_lineage_review;
#[path = "session_suite/temporal_application.rs"]
mod temporal_kernel_behavior;
#[path = "session_suite/temporal_projection/mod.rs"]
mod temporal_projection;

#[path = "storage_suite/corruption_test.rs"]
mod corruption_test;
#[path = "storage_suite/db_query_test.rs"]
mod db_query_test;
#[path = "storage_suite/fact_merge_hydration_test.rs"]
mod fact_merge_hydration;
#[path = "storage_suite/migration_manifest_test.rs"]
mod migration_manifest;

#[path = "../crates/tracedecay-domain/tests/session_contract.rs"]
mod domain_session_contract;
