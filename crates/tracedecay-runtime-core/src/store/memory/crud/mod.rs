//! Canonical fact CRUD, commit, feedback, and automatic fact application.
//!
//! Re-exports below preserve every `crud::*` path used outside this module.

pub(super) const DEFAULT_TRUST: f64 = 0.5;

mod add;
mod commands;
mod commit;
mod feedback;
mod lineage;
mod project;
mod queries;

#[cfg(test)]
mod payload_access_tests;
#[cfg(test)]
mod tests;

pub(super) use self::commands::{
    add_project_memory_fact_tx, load_mutable_project_memory_fact_tx, remove_project_memory_fact_tx,
    update_project_memory_fact_tx,
};
use self::commands::{
    commit_receipt_json, project_memory_commit_receipt_from_operation_tx,
    project_memory_feedback_action_label, project_memory_feedback_delta,
    project_memory_update_feedback_projection_tx,
};
use self::commit::{anchor_matches, commit_fact_tx};
use self::feedback::CommitAttempt;
pub(super) use self::feedback::{
    apply_project_memory_automatic_fact_tx, inspect_project_memory_fact_controlled_tx,
    project_memory_fact_feedback_history_tx, record_project_memory_fact_feedback_tx,
};
use self::lineage::{
    Projection, ensure_event_references, ensure_fact_identity, event_exists, event_matches,
    insert_event, load_current_projection, payload_is_purged_projection,
    publish_current_projection, receipt_outcome,
};
use self::project::active_fact_count_tx;
pub(super) use self::project::{
    commit_batch_tx, find_project_memory_fact_by_content_digest_controlled_tx,
    find_project_memory_fact_by_content_digest_tx, get_project_memory_fact_controlled_tx,
    initial_batch, list_project_memory_facts_controlled_tx, payload_metadata,
    project_memory_fact_history_controlled_tx, sanitize_payload, verified_payload,
};
pub(super) use self::queries::{
    fact_response_metadata_tx, get_retrieval_anchor_tx, load_current_fact_tx,
    query_current_facts_tx, query_fact_as_of_response_tx, query_fact_as_of_tx,
    query_fact_current_response_tx, query_fact_current_tx, query_fact_lineage_controlled_tx,
    query_fact_lineage_response_tx, query_fact_lineage_tx,
};
