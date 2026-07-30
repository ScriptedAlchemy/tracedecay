//! Transport-neutral Remote Brain application boundary.

pub mod auth;
pub mod capture;
pub mod composition;
pub mod protocol;
pub mod query;
pub mod recovery;
pub mod replay;
pub mod status;

#[cfg(test)]
mod query_tests;
