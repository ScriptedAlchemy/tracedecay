//! Ports this crate exposes so the root crate can inject the subsystems that
//! stay above it.
//!
//! Everything the moved `agents`/`automation` trees needed from the root crate
//! and that the one-shot crate split did **not** move downward is expressed
//! here as an injected capability instead of a dependency edge. Each port has
//! a matching root-wiring row in `SEAMS.md`.

pub mod dashboard_assets;
