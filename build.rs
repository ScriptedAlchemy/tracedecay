use std::hash::{Hash, Hasher};
use std::{collections::hash_map::DefaultHasher, fs, path::Path};

const DASHBOARD_ASSET_FILES: &[&str] = &[
    "dashboard/shell/dist/shell.js",
    "dashboard/shell/dist/shell.css",
    "dashboard/holographic/dist/index.js",
    "dashboard/holographic/dist/style.css",
    "dashboard/lcm/dist/index.js",
    "dashboard/lcm/dist/style.css",
    "dashboard/graph/dist/index.js",
    "dashboard/graph/dist/style.css",
];

fn emit_dashboard_asset_inputs() -> String {
    let mut hasher = DefaultHasher::new();
    for relative in DASHBOARD_ASSET_FILES {
        println!("cargo::rerun-if-changed={relative}");
        relative.hash(&mut hasher);
        let bytes = fs::read(relative).unwrap_or_default();
        bytes.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn main() {
    let out_path = Path::new("src/resources/logo.ansi");
    let logo_bytes = include_bytes!("src/resources/logo.png");
    let ansi = logo_art::image_to_ansi(logo_bytes, 90);
    fs::write(out_path, ansi).unwrap();
    println!("cargo::rerun-if-changed=src/resources/logo.png");
    println!(
        "cargo::rustc-env=TOKENSAVE_DASHBOARD_ASSET_STAMP={}",
        emit_dashboard_asset_inputs()
    );

    // Vendored WGSL grammar — compiled only when lang-wgsl is enabled.
    // Using vendored sources avoids pulling in tree-sitter-wgsl 0.0.6 which was
    // built against the incompatible tree-sitter 0.20 API.
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
