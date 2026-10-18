use std::path::Path;

use tracedecay_domain::{BrainId, UserProfileId};

/// Durable profile identity read by route and hook composition.
pub trait ProfileIdentityReadPort: Send + Sync {
    fn profile_root(&self) -> &Path;
    fn brain_id(&self) -> &BrainId;
    fn profile_id(&self) -> &UserProfileId;
}
