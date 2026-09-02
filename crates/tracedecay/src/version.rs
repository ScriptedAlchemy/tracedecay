//! What version this binary reports, and which value is safe to compare.
//!
//! [`PACKAGE_VERSION`] is the released `SemVer` that Release Please owns. It is the
//! only value with defined precedence, so upgrade checks, plugin staleness
//! stamps, and anything ordering releases must keep using it.
//!
//! [`build_version()`] is what the binary reports about *itself*: the released
//! version plus `SemVer` build metadata naming the exact commit it was
//! compiled from (`0.0.66+<full sha>`, or `…+<full sha>.dirty` when the
//! worktree had uncommitted changes). `SemVer` requires build metadata to be
//! ignored when determining precedence, so a locally built checkout binary is
//! traceable to an exact tree without touching the `version` field in
//! `Cargo.toml`.
//!
//! Provenance comes from the registered product runtime
//! ([`crate::product_runtime`]): the shipping binary crate resolves its own
//! source identity at build time and registers it at process start. A build
//! with no provenance no longer exists — the generating binary fails to build
//! without it — so an unregistered process reads the typed
//! [`ProductRuntimeError::MissingProvider`] state instead of a fabricated
//! bare version.

use crate::product_runtime::ProductRuntimeError;

/// The released version Release Please owns. Compare precedence against this, not
/// against [`build_version()`].
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The self-identifying version of this binary: [`PACKAGE_VERSION`] plus build
/// metadata naming the commit it came from, precomposed by the registered
/// product runtime.
pub fn build_version() -> Result<&'static str, ProductRuntimeError> {
    Ok(crate::product_runtime::product_runtime()?.build_version())
}

#[cfg(test)]
mod tests {
    use super::{PACKAGE_VERSION, build_version};
    use crate::product_runtime::register_fixture_product_runtime;

    /// Build metadata never changes precedence, so whatever this process
    /// reports still begins with the version Release Please assigned, and it
    /// is byte-identical to the registered runtime's composition.
    #[test]
    fn a_registered_process_reports_the_runtime_composed_build_version() {
        let runtime = register_fixture_product_runtime();
        let version = build_version().expect("fixture product runtime registered");
        assert!(
            version.starts_with(PACKAGE_VERSION),
            "{version} must begin with the released version {PACKAGE_VERSION}"
        );
        assert_eq!(
            version,
            format!("{PACKAGE_VERSION}+{}", runtime.source().full_sha)
        );
    }
}
