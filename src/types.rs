//! Root shim for the kernel `types` module.
//!
//! The implementation moved to `tracedecay_runtime_core::types` in the one-shot
//! crate split. This glob keeps every historical `crate::types::…` path resolving
//! from the root crate.

pub use tracedecay_runtime_core::types::*;

/// The source-edit result types are owned by `tracedecay-application`. The
/// kernel deliberately does not re-export them (that edge would point back up
/// out of the kernel), so the root shim unions both halves to keep every
/// historical `crate::types::{EditResult, …}` path resolving.
pub use tracedecay_application::source_edit::{
    AstGrepResult, EditResult, InsertResult, MoveHint, MoveResult, MultiEditResult,
    RenameFileEditV1, RenameResult, RenameSymbolBindingV1, RenameTextOnlyMatchV1,
};
