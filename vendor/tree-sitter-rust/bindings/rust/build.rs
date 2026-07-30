use std::path::Path;

fn main() {
    let src_dir =
        Path::new("../../crates/tracedecay-code-extraction/vendor/tree-sitter-rust/src");
    let parser_path = src_dir.join("parser.c");
    let scanner_path = src_dir.join("scanner.c");

    let mut build = cc::Build::new();
    build
        .std("c11")
        .include(src_dir)
        .file(&parser_path)
        .file(&scanner_path);

    #[cfg(target_env = "msvc")]
    build.flag("-utf-8");

    build.compile("tree-sitter-rust");
    println!("cargo:rerun-if-changed={}", parser_path.display());
    println!("cargo:rerun-if-changed={}", scanner_path.display());
}
