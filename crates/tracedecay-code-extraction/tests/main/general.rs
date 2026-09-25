use tracedecay_code_extraction::{LanguageExtractor, LanguageRegistry, RustExtractor};
use tracedecay_domain::*;

#[test]
fn test_extract_derive_macros() {
    let source = r#"
#[derive(Debug, Clone, Serialize)]
pub struct Config { pub name: String }
"#;
    let result = RustExtractor
        .extract_artifact("src/config.rs", source)
        .result;
    let derives: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::DerivesMacro)
        .collect();
    let names: Vec<&str> = derives.iter().map(|r| r.reference_name.as_str()).collect();
    assert_eq!(names, ["Clone", "Debug", "Serialize"]);
}

#[test]
fn test_qualified_names() {
    let source = r#"
mod server {
    pub fn handle_request() {}
}
"#;
    let result = RustExtractor.extract_artifact("src/lib.rs", source).result;
    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(fns.len(), 1);
    assert_eq!(fns[0].qualified_name, "src/lib.rs::server::handle_request");
}

#[test]
fn test_language_registry_dispatches_by_extension() {
    let registry = LanguageRegistry::new();
    let language = |path: &str| registry.extractor_for_file(path).map(|e| e.language_name());
    assert_eq!(language("Main.scala"), Some("Scala"));
    assert_eq!(language("src/com/example/App.scala"), Some("Scala"));
    assert_eq!(language("script.sc"), Some("Scala"));
    assert_eq!(language("style.css"), None);
    assert_eq!(language("README.unknown"), None);
}
