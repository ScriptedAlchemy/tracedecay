//! Renders the typed public operation descriptors into `OUT_DIR`.
//!
//! The descriptors are a pure projection of the canonical application
//! registry, so generating them here keeps the projection and its authority
//! from drifting instead of policing a checked-in copy after the fact.
//! `src/codegen.rs` is the single generator; `src/bin/generate.rs` reuses it
//! for the checked-in TypeScript SDK sources.

// The TypeScript renderers in the shared generator have no build-time caller.
#[allow(dead_code)]
#[path = "src/codegen.rs"]
mod codegen;

fn main() {
    println!("cargo::rerun-if-changed=src/codegen.rs");
    let destination = std::path::Path::new(&std::env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("operations.rs");
    let rendered = codegen::render_rust_operations_source().expect("render operation descriptors");
    std::fs::write(&destination, rendered).expect("write generated operation descriptors");
}
