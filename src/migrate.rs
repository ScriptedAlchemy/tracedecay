//! Root-owned seam onto the residual `tracedecay-migrate` modules.
//!
//! Only the final-V2 schema stamp and the profile backup remain; both are
//! re-exported here so their `crate::migrate::*` and `tracedecay::migrate::*`
//! caller paths keep resolving until each finds its final owner.

pub use tracedecay_migrate::{final_v2, profile_backup};
