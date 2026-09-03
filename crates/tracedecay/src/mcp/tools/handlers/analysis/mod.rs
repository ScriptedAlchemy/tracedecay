//! Composition-root diagnostics support that depends on LSP and global state.

mod diagnostics;

pub(super) use diagnostics::handle_diagnostics;
