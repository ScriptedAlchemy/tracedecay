use std::path::Path;

fn main() {
    if std::env::var("CARGO_FEATURE_LANG_WGSL").is_ok() {
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
}
