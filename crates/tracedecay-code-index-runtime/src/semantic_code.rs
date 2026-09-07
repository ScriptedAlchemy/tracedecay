//! Process-wide semantic lifecycle owner, without the root doctor projection.

use std::sync::Arc;

pub(crate) use tracedecay_semantic::*;

pub(crate) fn default_lifecycle_root() -> Option<std::path::PathBuf> {
    tracedecay_runtime_core::config::user_data_dir()
        .map(|root| tracedecay_semantic::default_lifecycle_root_in(&root))
}

pub(crate) fn shared_lifecycle_owner() -> Option<Arc<SemanticModelLifecycleOwnerV1>> {
    tracedecay_semantic::shared_lifecycle_owner(&default_lifecycle_root()?)
}
