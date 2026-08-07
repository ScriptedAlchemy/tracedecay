//! Root-owned seam onto the residual `tracedecay-migrate` module.
//!
//! Only the profile backup remains; it is re-exported here so its
//! `crate::migrate::profile_backup` and `tracedecay::migrate::profile_backup`
//! caller paths keep resolving until it finds its final owner.

pub use tracedecay_migrate::profile_backup;
