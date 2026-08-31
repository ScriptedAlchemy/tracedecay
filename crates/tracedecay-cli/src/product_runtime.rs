//! The product runtime this binary generates: the commit it was compiled
//! from, the self-identifying version it reports, and the dashboard bundle it
//! embeds.
//!
//! `build.rs` resolves source provenance (verified git worktree, release env,
//! or `cargo package` VCS journal — a build with none fails) and validates the
//! dashboard bundle into `$OUT_DIR/product_runtime_generated.rs`; this module
//! is that file's only mount point and assembles the provider `main` registers
//! into the composition library at process start.

mod generated {
    include!(concat!(env!("OUT_DIR"), "/product_runtime_generated.rs"));
}

/// `"{CARGO_PKG_VERSION}+{full_sha}"` plus `".dirty"` when the source tree
/// carried uncommitted changes: the version every CLI surface reports.
pub(crate) use generated::PRODUCT_BUILD_VERSION;
/// Full 40-hex commit sha this binary was built from; stamped into generated
/// host plugins as their `generator_commit` provenance.
pub(crate) use generated::PRODUCT_FULL_SHA;

pub(crate) fn provider() -> tracedecay::ProductRuntimeProvider {
    tracedecay::ProductRuntimeProvider {
        release_version: env!("CARGO_PKG_VERSION"),
        source: tracedecay::ProductSourceProvenance {
            full_sha: generated::PRODUCT_FULL_SHA,
            dirty: generated::PRODUCT_SOURCE_DIRTY,
        },
        dashboard: generated::STATIC_DASHBOARD_ASSETS,
    }
}
