//! Transport-neutral Remote Brain application boundary.

pub mod auth;
pub mod capture;
pub mod composition;
pub mod protocol;
pub mod query;
pub mod replay;
pub mod replay_contract;
pub mod replay_node;
pub mod status;

#[cfg(test)]
mod query_tests;
