use std::path::Path;

const ROOT_DEPENDENCIES: [&str; 6] = [
    concat!("crate", "::application"),
    concat!("crate", "::code_index"),
    concat!("crate", "::config"),
    concat!("crate", "::query"),
    concat!("crate", "::search_eval"),
    concat!("crate", "::semantic_code"),
];

#[test]
fn semantic_implementation_depends_on_root_only_through_adapter() {
    let manifest_dir = option_env!("CARGO_MANIFEST_DIR").unwrap_or(".");
    let root = Path::new(manifest_dir);
    let semantic_dir = root.join("src/semantic_code");
    let mut implementation_files = vec![root.join("src/semantic_code.rs")];
    implementation_files.extend(
        std::fs::read_dir(&semantic_dir)
            .expect("read semantic module directory")
            .map(|entry| entry.expect("read semantic module entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
            .filter(|path| {
                !matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("ownership_test.rs" | "root_adapter.rs")
                )
            }),
    );
    implementation_files.sort();

    let mut violations = Vec::new();
    for path in implementation_files {
        let relative = path.strip_prefix(root).expect("semantic source under root");
        let source = std::fs::read_to_string(&path).expect("read semantic source");
        for dependency in ROOT_DEPENDENCIES {
            if source.contains(dependency) {
                violations.push(format!("{}: {dependency}", relative.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "semantic implementation bypasses root_adapter.rs:\n{}",
        violations.join("\n")
    );
}
