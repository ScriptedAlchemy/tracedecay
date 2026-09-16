use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_code_index::{
    chunks::{
        ChunkingFailureV1, CodeFileIndexArtifactsV1, CodeIndexImportEvidenceV1, content_digest,
    },
    production::{
        CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionErrorV1,
        CodeIndexProductionOwnerV1, CodeIndexPublishedGenerationV1,
        SEALED_GENERATION_FORMAT_REVISION_V1, sealed_generation_payload_digest,
    },
};
use tracedecay_domain::{
    EdgeAuthorityV1, FileOccurrenceId, LanguageId, RelationEdgeKindV1, SanitizationReceiptId,
    SanitizedCodeFileV1, SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan,
    SymbolOccurrenceId, canonical_sha256,
};

use crate::{
    production_orchestration::{
        ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
    },
    support::id,
};

const FIRST_SOURCE: &str = concat!(
    "import type { Foo, Bar as Baz } from \"../models\";\n",
    "export function local() { return Foo; }\n",
);
const SECOND_SOURCE: &str = concat!(
    "import { run as execute } from \"runner\";\n",
    "execute();\n",
);

fn import_request() -> CodeIndexBuildRequestV1 {
    let mut request = request_with_source(
        "file.import.a",
        1_500_000,
        "commit.imports.1",
        "tree.imports.1",
        FIRST_SOURCE,
    );
    let first = &mut request.snapshot.files[0];
    first.logical_path = "src/a.ts".to_owned();
    first.language = Some(id::<LanguageId>("typescript"));

    let second_bytes = SECOND_SOURCE.as_bytes().to_vec();
    let second_occurrence = id::<FileOccurrenceId>("file.import.b");
    request.snapshot.files.push(SanitizedCodeFileV1 {
        file_occurrence_id: second_occurrence.clone(),
        logical_path: "src/b.ts".to_owned(),
        language: Some(id::<LanguageId>("typescript")),
        content_digest: content_digest(&second_bytes),
        disposition: SnapshotFileDispositionV1::Present,
    });
    request.captured_files.push(CodeIndexCapturedFileV1 {
        file_occurrence_id: second_occurrence,
        sanitized_bytes: Arc::from(second_bytes),
        sensitivity_level: SensitivityLevelV1::Public,
    });
    request.snapshot.content_identity = content_digest(
        [FIRST_SOURCE.as_bytes(), SECOND_SOURCE.as_bytes()]
            .concat()
            .as_slice(),
    );
    request.changed_files.clear();
    request.changed_files.insert("src/a.ts".to_owned());
    request.changed_files.insert("src/b.ts".to_owned());
    request
        .snapshot
        .validate()
        .expect("two-file import snapshot is canonical");
    request
}

fn published_import_generation() -> Arc<CodeIndexPublishedGenerationV1> {
    let mut owner = CodeIndexProductionOwnerV1::new(
        config(),
        SharedPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .expect("production owner");
    owner
        .build_and_publish(import_request(), &ActiveControl)
        .expect("parser-backed import generation publishes")
}

fn published_rust_workspace(sources: &[(&str, &str, &str)]) -> Arc<CodeIndexPublishedGenerationV1> {
    let mut request = request_with_source(
        "file.rust-workspace.seed",
        1_500_000,
        "commit.rust-workspace.1",
        "tree.rust-workspace.1",
        "",
    );
    request.snapshot.files.clear();
    request.snapshot.sanitization_receipts.clear();
    request.captured_files.clear();
    request.changed_files.clear();
    let mut identity = Vec::new();
    for (ordinal, (occurrence, path, source)) in sources.iter().copied().enumerate() {
        let file_occurrence_id = id::<FileOccurrenceId>(occurrence);
        let bytes = source.as_bytes();
        request.snapshot.files.push(SanitizedCodeFileV1 {
            file_occurrence_id: file_occurrence_id.clone(),
            logical_path: path.to_owned(),
            language: Some(id::<LanguageId>("rust")),
            content_digest: content_digest(bytes),
            disposition: SnapshotFileDispositionV1::Present,
        });
        request
            .snapshot
            .sanitization_receipts
            .push(id::<SanitizationReceiptId>(&format!(
                "receipt.rust-workspace.{ordinal}"
            )));
        request.captured_files.push(CodeIndexCapturedFileV1 {
            file_occurrence_id,
            sanitized_bytes: Arc::from(bytes),
            sensitivity_level: SensitivityLevelV1::Public,
        });
        request.changed_files.insert(path.to_owned());
        identity.extend_from_slice(path.as_bytes());
        identity.push(0);
        identity.extend_from_slice(bytes);
    }
    request.snapshot.files.sort_by(|left, right| {
        (&left.logical_path, &left.file_occurrence_id)
            .cmp(&(&right.logical_path, &right.file_occurrence_id))
    });
    request.snapshot.content_identity = content_digest(&identity);
    request
        .snapshot
        .validate()
        .expect("Rust workspace snapshot is canonical");

    CodeIndexProductionOwnerV1::new(
        config(),
        SharedPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .expect("production owner")
    .build_and_publish(request, &ActiveControl)
    .expect("Rust workspace generation publishes")
}

fn symbol_occurrence(
    generation: &CodeIndexPublishedGenerationV1,
    qualified_name: &str,
) -> SymbolOccurrenceId {
    generation
        .symbols()
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == qualified_name)
        .unwrap_or_else(|| panic!("missing symbol {qualified_name}"))
        .occurrence
        .clone()
}

fn assert_resolved_edge(
    generation: &CodeIndexPublishedGenerationV1,
    from: &SymbolOccurrenceId,
    to: &SymbolOccurrenceId,
    kind: RelationEdgeKindV1,
) {
    assert!(
        generation.edges().iter().any(|edge| {
            edge.from_occurrence == *from
                && edge.to_occurrence == *to
                && edge.kind == kind
                && edge.authority == EdgeAuthorityV1::NameResolved
        }),
        "missing {kind:?} edge from {from} to {to}"
    );
}

#[test]
fn rust_child_glob_imports_parent_use_bindings_for_calls() {
    let generation = published_rust_workspace(&[
        (
            "file.glob.parent",
            "crates/service/src/parent.rs",
            "mod child;\nuse child::f;\nmod dispatch;\n",
        ),
        (
            "file.glob.target",
            "crates/service/src/parent/child.rs",
            "pub fn f() {}\n",
        ),
        (
            "file.glob.dispatch",
            "crates/service/src/parent/dispatch.rs",
            "use super::*;\npub fn g() { f(); f(); }\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/service/src/parent/dispatch.rs::g");
    let target = symbol_occurrence(&generation, "crates/service/src/parent/child.rs::f");
    assert!(
        generation.imports().iter().any(|binding| {
            binding.logical_path == "crates/service/src/parent/dispatch.rs"
                && binding.module_specifier == "super"
                && binding.is_glob
        }),
        "the parser must retain the child glob as structured import evidence: {:?}",
        generation.imports()
    );

    assert_resolved_edge(&generation, &caller, &target, RelationEdgeKindV1::Calls);
    assert_eq!(
        generation
            .edges()
            .iter()
            .filter(|edge| {
                edge.from_occurrence == caller
                    && edge.to_occurrence == target
                    && edge.kind == RelationEdgeKindV1::Calls
            })
            .count(),
        2,
        "resolution caching must preserve each call site's evidence edge"
    );
}

#[test]
fn rust_cross_crate_impl_binds_through_public_reexport_chain() {
    let generation = published_rust_workspace(&[
        (
            "file.reexport.a-lib",
            "crates/a/src/lib.rs",
            "mod api;\npub use api::T;\n",
        ),
        (
            "file.reexport.a-api",
            "crates/a/src/api.rs",
            "mod traits;\npub use traits::T;\n",
        ),
        (
            "file.reexport.a-trait",
            "crates/a/src/api/traits.rs",
            "pub trait T {}\n",
        ),
        (
            "file.reexport.b-lib",
            "crates/b/src/lib.rs",
            "use a::T;\npub struct S;\nimpl T for S {}\n",
        ),
    ]);
    let implementor = symbol_occurrence(&generation, "crates/b/src/lib.rs::S");
    let target = symbol_occurrence(&generation, "crates/a/src/api/traits.rs::T");

    assert_resolved_edge(
        &generation,
        &implementor,
        &target,
        RelationEdgeKindV1::Implements,
    );
}

#[test]
fn rust_constructor_and_typed_receiver_calls_bind_through_the_crate_path() {
    let generation = published_rust_workspace(&[
        (
            "file.builder.lib",
            "crates/widgets/src/lib.rs",
            "mod builder;\npub use crate::builder::{Builder, Widget};\n",
        ),
        (
            "file.builder.impl",
            "crates/widgets/src/builder.rs",
            "pub struct Widget;\npub struct Builder;\nimpl Builder {\n    pub fn new() -> Builder { Builder }\n    pub fn build(&self) -> Widget { Widget }\n}\n",
        ),
        (
            "file.builder.app",
            "crates/app/src/main.rs",
            "fn assemble() -> widgets::Widget {\n    let mut builder: widgets::Builder = widgets::Builder::new();\n    builder.build()\n}\nfn main() { assemble(); }\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let constructor = symbol_occurrence(&generation, "crates/widgets/src/builder.rs::Builder::new");
    let build = symbol_occurrence(&generation, "crates/widgets/src/builder.rs::Builder::build");

    assert_resolved_edge(
        &generation,
        &caller,
        &constructor,
        RelationEdgeKindV1::Calls,
    );
    assert_resolved_edge(&generation, &caller, &build, RelationEdgeKindV1::Calls);
}

#[test]
fn rust_factory_new_style_receivers_do_not_fabricate_calls_edges() {
    let generation = published_rust_workspace(&[
        (
            "file.factory.lib",
            "crates/widgets/src/lib.rs",
            "pub struct Factory;\npub struct Product;\nimpl Factory {\n    pub fn new() -> Product { Product }\n    pub fn run(&self) {}\n}\nimpl Product {\n    pub fn run(&self) {}\n}\n",
        ),
        (
            "file.factory.app",
            "crates/app/src/main.rs",
            "fn assemble() {\n    let p = widgets::Factory::new();\n    p.run();\n}\nfn main() { assemble(); }\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let factory_run = symbol_occurrence(&generation, "crates/widgets/src/lib.rs::Factory::run");
    let product_run = symbol_occurrence(&generation, "crates/widgets/src/lib.rs::Product::run");
    let factory_new = symbol_occurrence(&generation, "crates/widgets/src/lib.rs::Factory::new");

    assert_resolved_edge(
        &generation,
        &caller,
        &factory_new,
        RelationEdgeKindV1::Calls,
    );
    assert!(
        !generation.edges().iter().any(|edge| {
            edge.from_occurrence == caller
                && edge.to_occurrence == factory_run
                && edge.kind == RelationEdgeKindV1::Calls
        }),
        "Factory::new() must not invent a Factory::run caller edge"
    );
    assert!(
        !generation.edges().iter().any(|edge| {
            edge.from_occurrence == caller
                && edge.to_occurrence == product_run
                && edge.kind == RelationEdgeKindV1::Calls
        }),
        "without an explicit type, abstain from a Product::run caller edge"
    );
}

#[test]
fn rust_inherent_impl_methods_resolve_when_type_and_impl_are_in_different_files() {
    let generation = published_rust_workspace(&[
        (
            "file.split.lib",
            "crates/widgets/src/lib.rs",
            "mod methods;\npub struct Builder;\n",
        ),
        (
            "file.split.methods",
            "crates/widgets/src/methods.rs",
            "use super::Builder;\nimpl Builder {\n    pub fn build(&self) {}\n}\n",
        ),
        (
            "file.split.app",
            "crates/app/src/main.rs",
            "fn assemble(builder: &widgets::Builder) {\n    builder.build();\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let build = symbol_occurrence(&generation, "crates/widgets/src/methods.rs::Builder::build");

    assert_resolved_edge(&generation, &caller, &build, RelationEdgeKindV1::Calls);
}

#[test]
fn rust_inherent_method_does_not_bind_to_a_same_named_type_in_another_module() {
    let generation = published_rust_workspace(&[
        (
            "file.homonym.lib",
            "crates/widgets/src/lib.rs",
            "mod inner;\npub struct Builder;\nimpl Builder {\n    pub fn finish(&self) {}\n}\n",
        ),
        (
            "file.homonym.inner",
            "crates/widgets/src/inner.rs",
            "pub struct Builder;\nimpl Builder {\n    pub fn build(&self) {}\n}\n",
        ),
        (
            "file.homonym.app",
            "crates/app/src/main.rs",
            "fn assemble(builder: &widgets::Builder) {\n    builder.build();\n    builder.finish();\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let finish = symbol_occurrence(&generation, "crates/widgets/src/lib.rs::Builder::finish");
    let inner_build = symbol_occurrence(&generation, "crates/widgets/src/inner.rs::Builder::build");

    assert_resolved_edge(&generation, &caller, &finish, RelationEdgeKindV1::Calls);
    assert!(
        generation.edges().iter().all(|edge| {
            edge.from_occurrence != caller
                || edge.to_occurrence != inner_build
                || edge.kind != RelationEdgeKindV1::Calls
        }),
        "`inner::Builder::build` belongs to a different type than `widgets::Builder`"
    );
}

#[test]
fn rust_inherent_impl_resolves_when_type_is_reexported_from_a_submodule() {
    let generation = published_rust_workspace(&[
        (
            "file.reexported-type.lib",
            "crates/widgets/src/lib.rs",
            "mod builder;\nmod methods;\nmod finish;\npub use crate::builder::Builder;\n",
        ),
        (
            "file.reexported-type.builder",
            "crates/widgets/src/builder.rs",
            "pub struct Builder;\n",
        ),
        (
            "file.reexported-type.methods",
            "crates/widgets/src/methods.rs",
            "use crate::builder::Builder;\nimpl Builder {\n    pub fn build(&self) {}\n}\n",
        ),
        (
            "file.reexported-type.finish",
            "crates/widgets/src/finish.rs",
            "use crate::Builder;\nimpl Builder {\n    pub fn finish(&self) {}\n}\n",
        ),
        (
            "file.reexported-type.app",
            "crates/app/src/main.rs",
            "fn assemble(builder: &widgets::Builder) {\n    builder.build();\n    builder.finish();\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let build = symbol_occurrence(&generation, "crates/widgets/src/methods.rs::Builder::build");
    let finish = symbol_occurrence(&generation, "crates/widgets/src/finish.rs::Builder::finish");

    assert_resolved_edge(&generation, &caller, &build, RelationEdgeKindV1::Calls);
    assert_resolved_edge(&generation, &caller, &finish, RelationEdgeKindV1::Calls);
}

#[test]
fn rust_std_module_paths_do_not_bind_to_same_stem_project_files() {
    let generation = published_rust_workspace(&[
        (
            "file.std-stem.widgets-lib",
            "crates/widgets/src/lib.rs",
            "pub mod fs;\n",
        ),
        (
            "file.std-stem.widgets-fs",
            "crates/widgets/src/fs.rs",
            "pub fn read(path: &str) -> Vec<u8> { path.as_bytes().to_vec() }\n",
        ),
        (
            "file.std-stem.ignore-lib",
            "crates/ignore/src/lib.rs",
            "mod walk;\npub use walk::WalkBuilder;\n",
        ),
        (
            "file.std-stem.ignore-walk",
            "crates/ignore/src/walk.rs",
            "pub struct WalkBuilder;\nimpl WalkBuilder {\n    pub fn new() -> WalkBuilder { WalkBuilder }\n}\n",
        ),
        (
            "file.std-stem.app",
            "crates/app/src/main.rs",
            "use std::fs;\nfn load() {\n    fs::read(\"x\");\n}\nfn walk() {\n    ignore::WalkBuilder::new();\n}\nfn main() { load(); walk(); }\n",
        ),
    ]);
    let load = symbol_occurrence(&generation, "crates/app/src/main.rs::load");
    let project_read = symbol_occurrence(&generation, "crates/widgets/src/fs.rs::read");
    let walk = symbol_occurrence(&generation, "crates/app/src/main.rs::walk");
    let walk_builder_new =
        symbol_occurrence(&generation, "crates/ignore/src/walk.rs::WalkBuilder::new");

    assert!(
        generation.edges().iter().all(|edge| {
            edge.from_occurrence != load
                || edge.to_occurrence != project_read
                || edge.kind != RelationEdgeKindV1::Calls
        }),
        "`fs::read` through `use std::fs` must not bind to a project `fs.rs::read`"
    );
    assert_resolved_edge(
        &generation,
        &walk,
        &walk_builder_new,
        RelationEdgeKindV1::Calls,
    );
}

#[test]
fn rust_typed_parameter_method_call_binds_through_import_and_crate_reexport() {
    let generation = published_rust_workspace(&[
        (
            "file.hiargs.main",
            "crates/core/src/main.rs",
            "mod flags;\nuse crate::flags::HiArgs;\nfn search(args: &HiArgs) -> bool {\n    args.walk_builder();\n    true\n}\nfn main() {}\n",
        ),
        (
            "file.hiargs.flags",
            "crates/core/src/flags/mod.rs",
            "mod hiargs;\npub(crate) use crate::flags::hiargs::HiArgs;\n",
        ),
        (
            "file.hiargs.impl",
            "crates/core/src/flags/hiargs.rs",
            "pub(crate) struct HiArgs;\nimpl HiArgs {\n    pub(crate) fn walk_builder(&self) {}\n}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/core/src/main.rs::search");
    let target = symbol_occurrence(
        &generation,
        "crates/core/src/flags/hiargs.rs::HiArgs::walk_builder",
    );

    assert_resolved_edge(&generation, &caller, &target, RelationEdgeKindV1::Calls);
}

#[test]
fn rust_dotted_call_on_untyped_receiver_stays_unresolved() {
    let generation = published_rust_workspace(&[
        (
            "file.untyped.lib",
            "crates/widgets/src/lib.rs",
            "pub struct Builder;\nimpl Builder {\n    pub fn build(&self) {}\n}\n",
        ),
        (
            "file.untyped.app",
            "crates/app/src/main.rs",
            "fn assemble(builders: Vec<widgets::Builder>) {\n    for builder in builders {\n        builder.build();\n    }\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let build = symbol_occurrence(&generation, "crates/widgets/src/lib.rs::Builder::build");

    assert!(
        generation.edges().iter().all(|edge| {
            edge.from_occurrence != caller
                || edge.to_occurrence != build
                || edge.kind != RelationEdgeKindV1::Calls
        }),
        "a receiver bound by a `for` pattern has no stated type and must not bind"
    );
}

#[test]
fn rust_parent_glob_does_not_override_a_local_type_binding() {
    let generation = published_rust_workspace(&[
        (
            "file.shadow.parent",
            "crates/service/src/parent.rs",
            "mod target;\nuse target::T;\nmod dispatch;\n",
        ),
        (
            "file.shadow.target",
            "crates/service/src/parent/target.rs",
            "pub trait T {}\n",
        ),
        (
            "file.shadow.dispatch",
            "crates/service/src/parent/dispatch.rs",
            "use super::*;\ntrait T {}\npub struct S;\nimpl T for S {}\n",
        ),
    ]);
    let implementor = symbol_occurrence(&generation, "crates/service/src/parent/dispatch.rs::S");
    let glob_target = symbol_occurrence(&generation, "crates/service/src/parent/target.rs::T");

    assert!(
        generation.edges().iter().all(|edge| {
            edge.from_occurrence != implementor
                || edge.to_occurrence != glob_target
                || edge.kind != RelationEdgeKindV1::Implements
        }),
        "a local binding must suppress the parent-glob candidate"
    );
}

fn sealed_envelope(generation: &CodeIndexPublishedGenerationV1) -> Value {
    serde_json::from_slice(&generation.encode_sealed().expect("import generation seals"))
        .expect("sealed generation JSON")
}

fn file_artifact(envelope: &Value, index: usize) -> CodeFileIndexArtifactsV1 {
    serde_json::from_value(envelope["generation"]["files"][index]["artifacts"].clone())
        .expect("file artifact JSON")
}

fn import_rows_mut(envelope: &mut Value, file_index: usize) -> &mut Vec<Value> {
    envelope["generation"]["files"][file_index]["artifacts"]["imports"]
        .as_array_mut()
        .expect("sealed file imports")
}

fn assert_serialized_artifact_has_no_self_digest(envelope: &Value, file_index: usize) {
    let imports = serde_json::from_value::<Vec<CodeIndexImportEvidenceV1>>(
        envelope["generation"]["files"][file_index]["artifacts"]["imports"].clone(),
    )
    .expect("forged canonical import rows");
    let recomputed = canonical_sha256(&("attacker-controlled-import-rows", imports.as_slice()))
        .expect("forged canonical import-row digest");
    assert!(recomputed.as_str().starts_with("sha256:"));
    assert!(
        envelope["generation"]["files"][file_index]["artifacts"]
            .get("import_rows_digest")
            .is_none(),
        "sealed file artifacts must not contain a recomputable self-digest authority"
    );
}

fn reseal_import_envelope(mut envelope: Value) -> Vec<u8> {
    let format_revision = u32::try_from(
        envelope["generation"]["format_revision"]
            .as_u64()
            .expect("forged payload format revision"),
    )
    .expect("format revision fits u32");
    let state_digest = sealed_generation_payload_digest(format_revision, &envelope["generation"])
        .expect("forged payload state digest");
    envelope["state_digest"] = Value::String(state_digest.as_str().to_owned());
    serde_json::to_vec(&envelope).expect("forged sealed generation JSON")
}

fn resealed_import_payload_error(envelope: Value, mutation: &str) -> CodeIndexProductionErrorV1 {
    let bytes = reseal_import_envelope(envelope);

    match CodeIndexPublishedGenerationV1::decode_sealed(&bytes) {
        Ok(_) => panic!("{mutation} restored after the outer state digest was recomputed"),
        Err(error) => error,
    }
}

fn assert_resealed_import_payload_is_rejected(envelope: Value, mutation: &str) {
    let _ = resealed_import_payload_error(envelope, mutation);
}

fn assert_sealed_envelope_restores(envelope: &Value) {
    CodeIndexPublishedGenerationV1::decode_sealed(&reseal_import_envelope(envelope.clone()))
        .expect("baseline generation restores");
}

#[test]
fn file_import_artifacts_require_nondefault_canonical_rows() {
    let generation = published_import_generation();
    let envelope = sealed_envelope(&generation);
    let artifacts = file_artifact(&envelope, 0);

    assert_eq!(
        artifacts
            .imports
            .iter()
            .map(|row| (
                row.logical_path.as_str(),
                row.file_occurrence_id.as_str(),
                row.module_specifier.as_str(),
                row.imported_name.as_deref(),
                row.local_name.as_deref(),
                row.namespace,
                row.module_kind,
                row.span,
                row.start_line,
                row.start_column,
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "src/a.ts",
                "file.import.a",
                "../models",
                Some("Foo"),
                Some("Foo"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 14,
                    end_byte: 17,
                },
                0,
                14,
            ),
            (
                "src/a.ts",
                "file.import.a",
                "../models",
                Some("Bar"),
                Some("Baz"),
                ImportNamespaceV1::Type,
                ImportModuleKindV1::ProjectRelative,
                SourceSpan {
                    start_byte: 19,
                    end_byte: 29,
                },
                0,
                19,
            ),
        ]
    );
    artifacts.validate().expect("canonical import rows");

    let mut missing = envelope["generation"]["files"][0]["artifacts"].clone();
    assert!(
        missing
            .as_object_mut()
            .expect("file artifact object")
            .remove("imports")
            .is_some(),
        "the serialized artifact must carry its required imports field"
    );
    let error = serde_json::from_value::<CodeFileIndexArtifactsV1>(missing)
        .expect_err("imports must be a required field without a serde default");
    assert!(
        error.to_string().contains("missing field `imports`"),
        "unexpected missing-imports error: {error}"
    );

    let mut reordered = artifacts.clone();
    reordered.imports.swap(0, 1);
    let error = reordered
        .validate()
        .expect_err("source-order reversal must be rejected");
    assert!(matches!(error, ChunkingFailureV1::NonCanonicalIdentity(_)));

    let mut duplicated = artifacts;
    let duplicate = duplicated.imports[0].clone();
    duplicated.imports.insert(1, duplicate);
    let error = duplicated
        .validate()
        .expect_err("duplicate import rows must be rejected");
    assert!(matches!(error, ChunkingFailureV1::NonCanonicalIdentity(_)));
}

#[test]
fn file_import_artifacts_bind_file_consistent_path_and_nonempty_span_to_indexed_extent() {
    let generation = published_import_generation();
    let artifacts = file_artifact(&sealed_envelope(&generation), 0);
    let indexed_end = artifacts
        .chunks
        .chunks
        .iter()
        .map(|chunk| chunk.anchor.source_span.end_byte)
        .max()
        .expect("complete file has indexed chunks");

    assert!(artifacts.imports.iter().all(|row| {
        row.logical_path == "src/a.ts"
            && row.file_occurrence_id == artifacts.chunks.document.file_occurrence_id
            && !row.span.is_empty()
            && row.span.end_byte <= indexed_end
    }));
    assert_eq!(
        artifacts
            .imports
            .iter()
            .map(|row| {
                &FIRST_SOURCE[usize::try_from(row.span.start_byte).expect("span start")
                    ..usize::try_from(row.span.end_byte).expect("span end")]
            })
            .collect::<Vec<_>>(),
        vec!["Foo", "Bar as Baz"]
    );

    let mut wrong_file = artifacts.clone();
    for row in &mut wrong_file.imports {
        row.file_occurrence_id = id("file.foreign");
    }
    assert!(wrong_file.validate().is_err());

    let mut inconsistent_path = artifacts.clone();
    inconsistent_path.imports[1].logical_path = "src/foreign.ts".to_owned();
    assert!(inconsistent_path.validate().is_err());

    let mut empty_span = artifacts.clone();
    empty_span.imports[0].span.end_byte = empty_span.imports[0].span.start_byte;
    assert!(empty_span.validate().is_err());

    let mut out_of_bounds = artifacts;
    out_of_bounds.imports[0].span.end_byte = indexed_end + 1;
    assert!(out_of_bounds.validate().is_err());
}

#[test]
fn raw_use_and_imported_bindings_never_become_canonical_symbols() {
    let generation = published_import_generation();
    let symbols = &generation.symbols().symbols;

    assert!(
        symbols.iter().any(|symbol| symbol.simple_name == "local"),
        "the fixture must exercise canonical TypeScript symbol materialization"
    );
    assert!(symbols.iter().all(|symbol| {
        symbol.kind != "use"
            && !matches!(
                symbol.simple_name.as_str(),
                "../models" | "runner" | "Foo" | "Bar" | "Baz" | "run" | "execute"
            )
    }));
}

#[test]
fn sealed_revision_nine_import_generation_round_trips_to_identical_bytes() {
    assert_eq!(SEALED_GENERATION_FORMAT_REVISION_V1, 11);
    let first = published_import_generation();
    let first_sealed = first.encode_sealed().expect("first generation seals");
    let second_sealed = published_import_generation()
        .encode_sealed()
        .expect("identical generation seals");
    assert_eq!(first_sealed, second_sealed);

    let envelope: Value = serde_json::from_slice(&first_sealed).expect("sealed generation JSON");
    assert_eq!(envelope["generation"]["format_revision"], 9);
    let restored =
        CodeIndexPublishedGenerationV1::decode_sealed(&first_sealed).expect("rev9 restores");
    assert_eq!(restored.imports(), first.imports());
    assert_eq!(
        restored.encode_sealed().expect("restored generation seals"),
        first_sealed
    );
}

#[test]
fn sealed_import_generation_rejects_semantic_tampering_after_outer_digest_recompute() {
    let generation = published_import_generation();
    let mut envelope = sealed_envelope(&generation);
    assert_sealed_envelope_restores(&envelope);

    import_rows_mut(&mut envelope, 0)[0]["imported_name"] = Value::String("Forged".to_owned());
    file_artifact(&envelope, 0)
        .validate()
        .expect("binding-name tamper remains structurally canonical");
    let error = resealed_import_payload_error(envelope, "binding-name tamper");
    assert!(
        error
            .to_string()
            .contains("import evidence does not match parser-backed extraction rows"),
        "semantic tamper reached the wrong authority rejection: {error}"
    );
}

#[test]
fn sealed_import_generation_rejects_semantic_tampering_after_import_and_outer_digest_recompute() {
    let generation = published_import_generation();
    let mut envelope = sealed_envelope(&generation);
    assert_sealed_envelope_restores(&envelope);

    import_rows_mut(&mut envelope, 0)[0]["imported_name"] = Value::String("Forged".to_owned());
    assert_serialized_artifact_has_no_self_digest(&envelope, 0);
    file_artifact(&envelope, 0)
        .validate()
        .expect("self-consistent import digest remains structurally canonical");

    let error = resealed_import_payload_error(
        envelope,
        "binding-name tamper with recomputed import-row digest",
    );
    assert!(
        error
            .to_string()
            .contains("import evidence does not match parser-backed extraction rows"),
        "self-consistent import forgery reached the wrong authority rejection: {error}"
    );
}

#[test]
fn sealed_import_generation_rejects_reorder_and_duplicate_after_outer_digest_recompute() {
    let generation = published_import_generation();
    let envelope = sealed_envelope(&generation);
    assert_sealed_envelope_restores(&envelope);

    let mut reordered = envelope.clone();
    import_rows_mut(&mut reordered, 0).swap(0, 1);
    assert_resealed_import_payload_is_rejected(reordered, "row reorder");

    let mut duplicated = envelope;
    let duplicate = import_rows_mut(&mut duplicated, 0)[0].clone();
    import_rows_mut(&mut duplicated, 0).insert(1, duplicate);
    assert_resealed_import_payload_is_rejected(duplicated, "duplicate row");
}

#[test]
fn sealed_import_generation_rejects_wrong_file_path_and_span_after_outer_digest_recompute() {
    let generation = published_import_generation();
    let envelope = sealed_envelope(&generation);
    assert_sealed_envelope_restores(&envelope);

    let mut wrong_file = envelope.clone();
    for row in import_rows_mut(&mut wrong_file, 0) {
        row["file_occurrence_id"] = Value::String("file.foreign".to_owned());
    }
    assert_resealed_import_payload_is_rejected(wrong_file, "foreign file occurrence");

    let mut wrong_path = envelope.clone();
    for row in import_rows_mut(&mut wrong_path, 0) {
        row["logical_path"] = Value::String("src/foreign.ts".to_owned());
    }
    assert_resealed_import_payload_is_rejected(wrong_path, "foreign logical path");

    let mut empty_span = envelope.clone();
    let start = import_rows_mut(&mut empty_span, 0)[0]["span"]["start_byte"].clone();
    import_rows_mut(&mut empty_span, 0)[0]["span"]["end_byte"] = start;
    assert_resealed_import_payload_is_rejected(empty_span, "empty source span");

    let mut out_of_bounds = envelope;
    import_rows_mut(&mut out_of_bounds, 0)[0]["span"]["end_byte"] =
        Value::from(FIRST_SOURCE.len() as u64 + 1);
    assert_resealed_import_payload_is_rejected(out_of_bounds, "out-of-bounds source span");
}
