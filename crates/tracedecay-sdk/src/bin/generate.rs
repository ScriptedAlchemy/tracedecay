//! Writes the checked-in Rust operation descriptors and TypeScript SDK sources
//! from the canonical registry through the shared `src/codegen.rs`.

#[path = "../codegen.rs"]
mod codegen;

use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: generate <repository-root>")?;
    codegen::write_sdk_sources(&root)
}
