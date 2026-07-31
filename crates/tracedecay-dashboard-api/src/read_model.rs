//! Root composition façade over the API-owned dashboard presentation contract.
//!
//! [`tracedecay_api::read_model`] owns the normative [`DashboardEnvelopeV1<T>`]
//! shape and every truthfulness invariant behind it; this module only re-exports
//! that contract under the path dashboard routes already use and resolves the
//! exact scope from live composition state, which the adapter crate cannot see.

pub use tracedecay_api::read_model::*;

/// Resolve the exact scope from the live dashboard state.
#[must_use]
pub fn scope_from_state(state: &super::DashboardState) -> DashboardScopeV1 {
    DashboardScopeV1 {
        project_id: state.project_id.clone(),
        storage_mode: state.storage_mode.clone(),
        store_root: state.store_root.display().to_string(),
    }
}
