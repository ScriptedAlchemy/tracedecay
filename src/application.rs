//! Compatibility shim for the product use cases that now live in
//! `tracedecay-usecases`.
//!
//! The whole former `src/application/` tree moved to
//! `crates/tracedecay-usecases/src/` during the one-shot crate split. The
//! former `src/application/mod.rs` became that crate's `lib.rs`, so every
//! previously reachable `crate::application::…` path is re-exported here and
//! root modules, binaries, and integration tests keep compiling against the old
//! path while the aftermath campaign rewrites call sites.
//!
//! Note the split of responsibilities: this shim keeps the **root** crate's
//! paths alive. Sibling crates that reference "root application" seams in their
//! own `SEAMS.md` get direct `tracedecay-usecases` dependencies later — a shim
//! in the root binary crate cannot serve them.
//!
//! Unresolved root couplings that the move could not carry across the boundary
//! are catalogued in `crates/tracedecay-usecases/SEAMS.md`.

pub use tracedecay_usecases::*;

pub(crate) mod hint_outcomes;

// The use-case crate owns transport-independent admission behavior. The root
// composition crate augments that surface with the registered daemon-backed
// fixture required by root and integration tests.
#[path = "application/host_admission.rs"]
pub mod host_admission;
