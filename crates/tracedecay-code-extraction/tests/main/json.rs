use tracedecay_code_extraction::{JsonExtractor, LanguageExtractor};
use tracedecay_domain::NodeKind;

fn fixture(relative: &str) -> String {
    match relative {
        "packages/shared/package.json" => {
            include_str!("../../fixtures/typescript-monorepo/packages/shared/package.json")
                .to_owned()
        }
        "apps/web/tsconfig.json" => {
            include_str!("../../fixtures/typescript-monorepo/apps/web/tsconfig.json").to_owned()
        }
        _ => panic!("unknown fixture: {relative}"),
    }
}

/// The manifest pairs the TypeScript resolver reads must survive as `Const`
/// symbols whose signature is the whole pair, comments included, so the seal
/// can parse `name`, `exports`, and `compilerOptions` from the file set.
#[test]
fn manifest_pairs_are_const_symbols_with_whole_pair_signatures() {
    let source = fixture("packages/shared/package.json");
    let artifact = JsonExtractor.extract_artifact("packages/shared/package.json", &source);
    assert!(
        artifact.result.errors.is_empty(),
        "{:?}",
        artifact.result.errors
    );
    let pairs = artifact
        .result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Const)
        .map(|node| (node.name.as_str(), node.signature.as_deref().unwrap_or("")))
        .collect::<Vec<_>>();
    assert_eq!(
        pairs.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["name", "version", "main", "types", "exports"]
    );
    assert_eq!(pairs[0].1, "\"name\": \"@fixture/shared\"");
    assert!(
        pairs[4].1.contains("\"./math\": \"./dist/math.js\""),
        "exports keeps its nested object: {}",
        pairs[4].1
    );
    assert_eq!(
        artifact
            .result
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Const && node.name == "exports")
            .map(|node| node.qualified_name.as_str()),
        Some("packages/shared/package.json::exports")
    );
}

#[test]
fn jsonc_comments_do_not_break_pair_extraction() {
    let source = fixture("apps/web/tsconfig.json");
    let artifact = JsonExtractor.extract_artifact("apps/web/tsconfig.json", &source);
    let names = artifact
        .result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Const)
        .map(|node| node.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["extends", "compilerOptions"]);
}

#[test]
fn array_documents_and_oversized_pairs_stay_bounded() {
    let artifact = JsonExtractor.extract_artifact("data.json", "[1, 2, 3]\n");
    assert_eq!(artifact.result.nodes.len(), 1, "only the file node");
    assert_eq!(artifact.result.nodes[0].kind, NodeKind::File);

    let huge = format!(
        "{{\n  \"packages\": {{\n{}\n  }}\n}}\n",
        (0..2_000)
            .map(|index| format!("    \"node_modules/pkg-{index}\": {{ \"version\": \"1.0.0\" }}"))
            .collect::<Vec<_>>()
            .join(",\n")
    );
    let artifact = JsonExtractor.extract_artifact("package-lock.json", &huge);
    let packages = artifact
        .result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Const && node.name == "packages")
        .expect("pair symbol");
    assert_eq!(packages.signature.as_deref(), Some("\"packages\": {"));
}
