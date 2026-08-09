//! Compatibility fact CRUD, canonical commit path, mirror writes, feedback, and automatic fact application.
//!
//! Re-exports below preserve every `crud::*` path used outside this module.

pub(super) const DEFAULT_TRUST: f64 = 0.5;

const PROJECT_MEMORY_RETENTION_CLASS: &str = "compatibility-runtime-v1";

mod commands;
mod commit;
mod feedback;
mod lineage;
mod mirror;
mod queries;

pub(super) use self::commands::*;
use self::commit::*;
pub(super) use self::feedback::*;
pub(super) use self::lineage::*;
pub(super) use self::mirror::*;
pub(super) use self::queries::*;
