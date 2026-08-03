//! Shared, root-independent pieces of the TraceDecay dashboard API.
//!
//! Route composition, dashboard state, asset embedding, and storage
//! authorities remain owned by the root binary crate. This crate contains
//! only reusable request/response and SQL→JSON helpers; the root dashboard
//! module re-exports them through its compatibility façade.

pub mod util;
