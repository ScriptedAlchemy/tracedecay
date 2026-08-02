//! Which version this crate stamps into everything a host can see.
//!
//! `env!("CARGO_PKG_VERSION")` resolves per compiled crate, so inside
//! `tracedecay-agent-hosts` it is this library crate's own `0.1.0` — not the
//! version of the `tracedecay` product a user installed. Every plugin
//! manifest, cache path, staleness warning, and provenance header this crate
//! writes is compared by hosts (and by the root crate's tests) against the
//! product version, so the sub-crate version there is simply wrong.
//!
//! [`PRODUCT_VERSION`] is that product version. `build.rs` reads the root
//! package's `version` out of the workspace-root `Cargo.toml` — the single
//! authoring point release-plz already owns — and bakes it into
//! `TRACEDECAY_PRODUCT_VERSION`, exactly the way it bakes `TRACEDECAY_GIT_SHA`
//! for the same reason. There is no literal version in this crate's source.

/// Reads the root package's version out of the workspace-root manifest.
///
/// `build.rs` compiles this same module through a `#[path]` declaration, so
/// the parser that bakes [`PRODUCT_VERSION`] is the parser the tests below
/// verify it against rather than a second copy that can drift.
pub mod root_manifest;

/// The TraceDecay product version: the `version` of the root `tracedecay`
/// package, baked in by `build.rs`.
///
/// Use this — never `env!("CARGO_PKG_VERSION")` — for anything a host, a
/// deployed plugin manifest, or a user-visible path will compare against an
/// installed `tracedecay` binary.
pub const PRODUCT_VERSION: &str = env!("TRACEDECAY_PRODUCT_VERSION");

#[cfg(test)]
mod tests {
    use super::{PRODUCT_VERSION, root_manifest};

    /// The repository root, two directories above this crate — the same hop
    /// `build.rs` makes to find the manifest it stamps from.
    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The drift guard. A release bump of the root package that failed to
    /// reach this binary would otherwise only show up as hosts silently
    /// comparing plugin manifests against the wrong version.
    #[test]
    fn the_baked_version_is_the_root_packages_version() {
        let authored = root_manifest::resolve(&repo_root())
            .expect("the workspace-root manifest must declare the root package version");
        assert_eq!(
            PRODUCT_VERSION,
            authored,
            "the baked product version drifted from {}",
            root_manifest::manifest_path(&repo_root()).display()
        );
    }
}
