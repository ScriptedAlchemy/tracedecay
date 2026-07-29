//! Architecture boundary guards for the TraceDecay workspace.
//!
//! Formerly the single-file `tests/architecture_boundaries.rs` target, now
//! split into per-area modules. Cargo auto-discovers
//! `tests/architecture_boundaries/main.rs` as a test target named
//! `architecture_boundaries`, so target-scoped filters keep working.
//!
//! - `manifest`: PR8 workspace/package/target snapshot and physical Cargo
//!   manifest classification contract.
//! - `module_scanner`: shared Rust module/include scanner and its resolver
//!   tests.
//! - `query_kernel`: `src/query` purity guards (dependency roots, macros,
//!   attributes, module graph) and the temporal kernel boundary test.
//! - `dependency_boundaries`: forbidden-path layering guards for
//!   application/domain/store/query/API code.
//! - `session_store_boundaries`: session/registered-database edge guards for
//!   the modules that completed the store-port inversion.

mod compile_isolation;
mod dependency_boundaries;
mod manifest;
mod module_scanner;
mod query_kernel;
mod session_store_boundaries;
