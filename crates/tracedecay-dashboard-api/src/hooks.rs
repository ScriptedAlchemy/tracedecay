//! Hook contracts plus the readiness projection installed by the composition root.

pub use tracedecay_hooks::*;
pub use tracedecay_usecases::analytics_bridge::{
    HookReadinessProjectionPort, aggregate_hook_completed_readiness,
    install_hook_readiness_projection,
};
