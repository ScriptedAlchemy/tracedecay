//! Agent host integrations (`agents`) and self-improvement automation
//! (`automation`) for TraceDecay.
//!
//! These two subsystems are mutually recursive — `agents` reaches into
//! `automation` for skill/memory installation targets and `automation` reaches
//! back into `agents` for host discovery and bundle composition — so they are
//! extracted from the root crate as a single unit. Inside this crate the
//! former `crate::agents::*` / `crate::automation::*` paths keep resolving
//! unchanged, which is why the module names are preserved verbatim.
//!
//! The root crate re-exports both modules from `src/agents.rs` and
//! `src/automation.rs` so every previously public path
//! (`tracedecay::agents::…`, `tracedecay::automation::…`) still resolves.
//!
//! Remaining root couplings that this crate satisfies through injected ports
//! rather than a dependency edge are cataloged in `SEAMS.md`.

/// Installs the registered global/session schema into the kernel's fail-closed
/// port for this crate's test process.
///
/// `Database::publish_test_runtime` materialises a profile-scoped sidecar shard
/// that the kernel initialises through
/// `tracedecay_runtime_core::ports::registered_schema`. That port fails closed
/// until the real schema — owned by `tracedecay-global-db` — is registered.
/// Production wires it from the daemon composition root; this crate's test
/// target reuses the identical installer through its `test-helpers`
/// dev-dependency. Idempotent: the port keeps the first registration, so every
/// fixture entry point can call it unconditionally.
///
/// Fixtures built on `tracedecay_global_db::tests::harness` register the
/// installer themselves; only fixtures that reach `publish_test_runtime`
/// directly need this call.
#[cfg(test)]
pub(crate) fn register_test_schema_installer() {
    tracedecay_global_db::register_test_schema_installer();
}

pub mod agents;
pub mod analytics;
pub mod automation;
pub mod ports;
pub mod product_version;
pub mod tool_name;

pub use product_version::PRODUCT_VERSION;
pub(crate) use tracedecay_usecases as application;
pub(crate) use tracedecay_usecases::request_identity;

// Kernel shims. `tracedecay-runtime-core` owns the substrate these two
// subsystems were extracted alongside; aliasing the kernel modules into this
// crate's root keeps every historical `crate::<module>::…` path in the moved
// code resolving verbatim, exactly as the root crate's `src/<module>.rs` shims
// do on the other side of the split.
pub(crate) use tracedecay_runtime_core::{
    config, db, errors, lifecycle_lease, memory, privacy, runtime_identity, serde_util, storage,
    store, worktree,
};

/// Kernel-owned slice of the former root `tracedecay` façade module.
///
/// Only `current_timestamp` moved down into the kernel; the `TraceDecay`
/// orchestrator itself stays in the root crate and reaches this crate through
/// [`ports::ProjectRuntime`].
pub(crate) mod tracedecay {
    pub(crate) use crate::ports::project_runtime::TraceDecay;
    pub(crate) use tracedecay_runtime_core::tracedecay::current_timestamp;
}
