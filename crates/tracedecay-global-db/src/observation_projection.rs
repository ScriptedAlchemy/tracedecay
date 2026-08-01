mod apply;
mod migration;
#[cfg(test)]
mod migration_tests;
mod rebuild;
mod schema;
mod state;
mod transition;

pub(super) use apply::{derive_projection, derive_projection_with_alias, verify_workflow_effects};
pub use migration::{
    advance_projection_version_migration_until_cancelled_with_engine,
    prepare_projection_version_migration_with_engine,
};
pub use rebuild::{project_observation_with_engine, rebuild_projection_with_engine};
pub(super) use schema::{
    converge_v4_projection_anchor_bindings, ensure_observation_projection_performance_indexes,
    ensure_observation_projection_schema,
};
pub(super) use state::{converge_session_project_paths, verify_output_authority};
