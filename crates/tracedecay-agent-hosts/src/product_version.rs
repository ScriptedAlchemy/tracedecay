//! Which version this crate stamps into everything a host can see.
//!
//! `env!("CARGO_PKG_VERSION")` resolves per compiled crate, so inside
//! `tracedecay-agent-hosts` it is this library crate's own `0.1.0` — not the
//! version of the `tracedecay` product a user installed. Every plugin
//! manifest, cache path, staleness warning, and provenance header this crate
//! writes is compared by hosts (and by the root crate's tests) against the
//! product version, so the sub-crate version there is simply wrong.
//!
//! [`PRODUCT_VERSION`] is that product version. `build.rs` reads
//! `[workspace.package].version` out of the workspace-root `Cargo.toml` — the single
//! authoring point Release Please owns — and bakes it into
//! `TRACEDECAY_PRODUCT_VERSION`. There is no literal version in this crate's
//! source. (Commit provenance, by contrast, is never baked here: rendering
//! that stamps a generator commit takes it as a function argument from the
//! owning binary's registered product runtime.)

/// Reads the product version out of the workspace-root manifest.
///
/// `build.rs` compiles this same module through a `#[path]` declaration, so
/// the parser that bakes [`PRODUCT_VERSION`] is the parser the tests below
/// verify it against rather than a second copy that can drift.
pub mod root_manifest;

/// The TraceDecay product version from the workspace authority, baked in by
/// `build.rs`.
///
/// Use this — never `env!("CARGO_PKG_VERSION")` — for anything a host, a
/// deployed plugin manifest, or a user-visible path will compare against an
/// installed `tracedecay` binary.
pub const PRODUCT_VERSION: &str = env!("TRACEDECAY_PRODUCT_VERSION");
