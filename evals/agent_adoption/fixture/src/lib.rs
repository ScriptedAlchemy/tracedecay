//! Tiny self-contained "orders" domain used as the agent-adoption eval fixture.
//!
//! The crate is intentionally small and boringly named so that grounded eval
//! answers (symbol names, call edges, duplicated logic) are unambiguous.

pub mod discount;
pub mod inventory;
pub mod orders;
pub mod pricing;
