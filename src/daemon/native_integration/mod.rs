//! Daemon composition of the Plan 36 native-integration authority.
//!
//! `registry` retains one composed transaction coordinator per exact
//! project/repository identity; `store` bridges the synchronous
//! `tracedecay-store` contract onto the async registered session database
//! through one bounded actor per database.

mod registry;
mod store;

pub(crate) use registry::{
    DaemonNativeIntegrationOwner, DaemonNativeIntegrationServiceRegistry,
    DaemonProjectNativeIntegrationService,
};
pub(crate) use store::SharedDaemonNativeIntegrationStore;
