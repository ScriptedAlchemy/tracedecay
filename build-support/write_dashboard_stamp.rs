//! Standalone dashboard source-stamp writer.
//!
//! CI's dashboard artifact job builds `dashboard/app-dist` with npm directly
//! (no cargo), so `build.rs` never runs there to write
//! `dashboard/app-dist/.source-stamp`. This binary compiles with plain
//! `rustc --edition 2024` and prints the exact stamp `build.rs` computes, by
//! reusing the same `dashboard_cache` module, so the uploaded artifact ships a
//! truthful stamp for the sources it was built from.
//!
//! Usage: `write_dashboard_stamp [repository-root]` (defaults to `.`), stamp
//! is printed to stdout with no trailing newline, matching what `build.rs`
//! writes to the stamp file.

#![allow(dead_code)]

#[path = "dashboard_cache.rs"]
mod dashboard_cache;

use std::path::Path;

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".to_owned());
    print!("{}", dashboard_cache::source_stamp(Path::new(&root)));
}
