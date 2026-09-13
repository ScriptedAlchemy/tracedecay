#![cfg(feature = "lang-metal")]

//! Metal rides on the C++ grammar the medium bundle ships, so a build with
//! only `lang-metal` enabled must extract real Metal Shading Language through
//! both the cold path and the retained-tree path.

use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::MetalExtractor;
use tracedecay_code_extraction::incremental::{
    ParseDocumentIdentity, ParseLimits, ParseReuse, RetainedParseDocument,
};
use tracedecay_code_extraction::parsed_extraction::ParsedExtractionDisposition;
use tracedecay_domain::*;

const SHADER: &str = r#"#include <metal_stdlib>
using namespace metal;

struct VertexIn {
    float3 position [[attribute(0)]];
    float2 uv [[attribute(1)]];
};

struct VertexOut {
    float4 position [[position]];
    float2 uv;
};

namespace lighting {
    float lambert(float3 normal, float3 light) {
        return max(dot(normal, light), 0.0f);
    }
}

class Material {
public:
    float roughness;
    float shade(float3 normal, float3 light) const {
        return lighting::lambert(normal, light) * (1.0f - roughness);
    }
};
"#;

fn extract() -> ExtractionResult {
    let result = MetalExtractor.extract("shader.metal", SHADER);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
}

fn names(result: &ExtractionResult, kind: NodeKind) -> Vec<&str> {
    result
        .nodes
        .iter()
        .filter(|node| node.kind == kind)
        .map(|node| node.name.as_str())
        .collect()
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned()).unwrap_or_else(|error| panic!("{value}: {error}"))
}

#[test]
fn metal_dispatches_on_its_extension() {
    let registry = tracedecay_code_extraction::LanguageRegistry::new();
    let extractor = registry
        .extractor_for_file("shaders/lit.metal")
        .expect("Metal extension registered");
    assert_eq!(extractor.language_name(), "Metal");
    assert_eq!(extractor.retained_grammar_key("shaders/lit.metal"), "cpp");
}

#[test]
fn metal_extracts_cpp_structure_through_the_medium_bundle_grammar() {
    let result = extract();

    let files = names(&result, NodeKind::File);
    assert_eq!(files, ["shader.metal"]);

    let structs = names(&result, NodeKind::Struct);
    assert!(structs.contains(&"VertexIn"), "structs: {structs:?}");
    assert!(structs.contains(&"VertexOut"), "structs: {structs:?}");

    let fields = names(&result, NodeKind::Field);
    assert!(fields.contains(&"position"), "fields: {fields:?}");
    assert!(fields.contains(&"uv"), "fields: {fields:?}");
    assert!(fields.contains(&"roughness"), "fields: {fields:?}");

    assert_eq!(names(&result, NodeKind::Namespace), ["lighting"]);
    assert_eq!(names(&result, NodeKind::Function), ["lambert"]);
    assert_eq!(names(&result, NodeKind::Class), ["Material"]);
    assert_eq!(names(&result, NodeKind::Method), ["shade"]);

    let shade = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Method && node.name == "shade")
        .expect("shade method");
    assert_eq!(shade.qualified_name, "shader.metal::Material::shade");
    assert!(
        result.unresolved_refs.iter().any(|reference| {
            reference.from_node_id == shade.id
                && reference.reference_kind == EdgeKind::Calls
                && reference.reference_name.ends_with("lambert")
        }),
        "shade must call lambert: {:?}",
        result.unresolved_refs
    );
}

/// The retained-tree path resolves Metal to the `cpp` grammar key and yields
/// the same canonical rows as the cold extraction.
#[test]
fn metal_retained_tree_extraction_matches_cold_extraction() {
    let (document, report) = RetainedParseDocument::open(
        ParseDocumentIdentity::Repository {
            project_id: id::<ProjectId>("project.metal"),
            repository_id: id::<RepositoryId>("repository.metal"),
            worktree_id: Some(id::<WorktreeId>("worktree.metal")),
            reference: Some(id::<RefId>("refs/heads/main")),
            commit: Some(id::<CommitId>("commit-a")),
            tree: Some(id::<TreeId>("tree-a")),
            dirty: RepositoryDirtyStateV1::Clean,
            logical_path: "shader.metal".to_owned(),
        },
        "metal",
        SHADER,
        ParseLimits::default(),
    )
    .expect("metal parses through the cpp grammar");
    assert_eq!(report.reuse, ParseReuse::Initial);

    let retained = document
        .extract_canonical(&MetalExtractor, &report, None)
        .expect("retained extraction");
    assert_eq!(
        retained.disposition,
        ParsedExtractionDisposition::FullDocument
    );

    let mut cold = extract();
    let mut retained = retained.result;
    for result in [&mut cold, &mut retained] {
        result.duration_ms = 0;
        for node in &mut result.nodes {
            node.updated_at = 0;
        }
    }
    assert_eq!(
        serde_json::to_value(&cold).expect("serialize cold rows"),
        serde_json::to_value(&retained).expect("serialize retained rows")
    );
}
