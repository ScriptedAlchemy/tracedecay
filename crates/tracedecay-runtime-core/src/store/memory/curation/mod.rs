//! Canonical tag curation and fact merges.

mod apply;
mod relations;
mod review;

#[cfg(test)]
mod destructive_tests;
#[cfg(test)]
mod merge_tests;
#[cfg(test)]
mod tests;

pub(super) use self::apply::{
    apply_project_memory_fact_curation_tx, merge_project_memory_facts_tx,
};
use self::relations::{
    available_curation_fact_tx, curated_correction_batch, link_facts_tx, normalize_tags_tx,
};
