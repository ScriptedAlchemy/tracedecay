use std::path::Path;

fn main() {
    compile_rust_grammar();
    if std::env::var("CARGO_FEATURE_LANG_WGSL").is_ok() {
        compile_wgsl_grammar();
    }
}

fn compile_wgsl_grammar() {
    let wgsl_dir = Path::new("vendor/tree-sitter-wgsl/src");
    cc::Build::new()
        .include(wgsl_dir)
        .file(wgsl_dir.join("parser.c"))
        .file(wgsl_dir.join("scanner.c"))
        .warnings(false)
        .compile("tree_sitter_wgsl");
    println!("cargo::rerun-if-changed=vendor/tree-sitter-wgsl/src/parser.c");
    println!("cargo::rerun-if-changed=vendor/tree-sitter-wgsl/src/scanner.c");
}

fn compile_rust_grammar() {
    let rust_dir = Path::new("vendor/tree-sitter-rust/src");
    let mut build = cc::Build::new();
    build
        .std("c11")
        .include(rust_dir)
        // The grammar tier dependency also links tree-sitter-rust. Give the
        // package-owned patched grammar private symbols so both can coexist.
        .define("tree_sitter_rust", "tracedecay_tree_sitter_rust")
        .define(
            "tree_sitter_rust_external_scanner_create",
            "tracedecay_tree_sitter_rust_external_scanner_create",
        )
        .define(
            "tree_sitter_rust_external_scanner_destroy",
            "tracedecay_tree_sitter_rust_external_scanner_destroy",
        )
        .define(
            "tree_sitter_rust_external_scanner_scan",
            "tracedecay_tree_sitter_rust_external_scanner_scan",
        )
        .define(
            "tree_sitter_rust_external_scanner_serialize",
            "tracedecay_tree_sitter_rust_external_scanner_serialize",
        )
        .define(
            "tree_sitter_rust_external_scanner_deserialize",
            "tracedecay_tree_sitter_rust_external_scanner_deserialize",
        )
        .file(rust_dir.join("parser.c"))
        .file(rust_dir.join("scanner.c"))
        .warnings(false)
        .compile("tracedecay_tree_sitter_rust");
    println!("cargo::rerun-if-changed=vendor/tree-sitter-rust/src/parser.c");
    println!("cargo::rerun-if-changed=vendor/tree-sitter-rust/src/scanner.c");
}
