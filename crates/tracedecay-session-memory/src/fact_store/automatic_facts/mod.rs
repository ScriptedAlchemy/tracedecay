//! Terminal automatic fact receipt persistence and projection.

mod lifecycle;
mod records;
#[cfg(test)]
mod tests;

pub(super) use self::lifecycle::{
    project_memory_existing_automatic_fact_receipt_tx,
    project_memory_lookup_automatic_fact_operation_tx,
    project_memory_record_automatic_fact_operation_tx,
    project_memory_record_automatic_fact_receipt_tx,
};
pub(super) use self::records::{
    get_project_memory_automatic_fact_receipt_tx, list_project_memory_automatic_fact_receipts_tx,
    project_memory_automatic_fact_receipt_record_tx, project_memory_automatic_fact_request_value,
    project_memory_automatic_fact_state_label,
};
