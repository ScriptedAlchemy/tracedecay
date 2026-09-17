//! Writes the checked-in TypeScript SDK sources from the canonical registry.
//!
//! Rust descriptors are generated into `OUT_DIR` by the SDK crate's
//! `build.rs`, so only the published npm package's sources are written into
//! the checkout here. Both entry points share `src/codegen.rs`.

// The Rust renderer in the shared generator has no caller on this path.
#[allow(dead_code)]
#[path = "../codegen.rs"]
mod codegen;

use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: generate <repository-root>")?;
    codegen::write_typescript_sdk(&root)
}
