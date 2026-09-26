use std::sync::Arc;

use serde_json::Value;
use tracedecay_code_extraction::{ImportModuleKindV1, ImportNamespaceV1};
use tracedecay_code_index::{
    chunks::{CodeIndexImportEvidenceV1, content_digest},
    production::{
        CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionErrorV1,
        CodeIndexProductionOwnerV1, CodeIndexPublishedGenerationV1,
        SEALED_GENERATION_FORMAT_REVISION_V1,
    },
};
use tracedecay_domain::{
    EdgeAuthorityV1, FileOccurrenceId, LanguageId, RelationEdgeKindV1, SanitizationReceiptId,
    SanitizedCodeFileV1, SensitivityLevelV1, SnapshotFileDispositionV1, SourceSpan,
    SymbolOccurrenceId,
};

use crate::{
    production_orchestration::{
        ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
    },
    support::{PartitionedSealV1, id},
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

/// tokio 1.53.1 `tokio/tests/macros_rename_test.rs`, verbatim.
const TOKIO_MACROS_RENAME_TEST: &str = r#"#![cfg(all(feature = "full", not(target_os = "wasi")))] // Wasi doesn't support threading

#[allow(unused_imports)]
use std as tokio;

use ::tokio as tokio1;

mod test {
    pub use ::tokio;
}

async fn compute() -> usize {
    let join = tokio1::spawn(async { 1 });
    join.await.unwrap()
}

#[tokio1::main(crate = "tokio1")]
async fn compute_main() -> usize {
    compute().await
}

#[test]
fn crate_rename_main() {
    assert_eq!(1, compute_main());
}

#[tokio1::test(crate = "tokio1")]
async fn crate_rename_test() {
    assert_eq!(1, compute().await);
}

#[test::tokio::test(crate = "test::tokio")]
async fn crate_path_test() {
    assert_eq!(1, compute().await);
}
"#;

#[test]
fn rust_extern_prelude_absolute_use_paths_index_like_their_relative_form() {
    let generation = published_rust_workspace(&[
        ("file.absolute.a", "crates/a/src/lib.rs", "pub trait T {}\n"),
        (
            "file.absolute.b",
            "crates/b/src/lib.rs",
            // The local `mod a` must not capture the extern-prelude path `::a`.
            "mod a {}\nuse ::a::T;\npub struct S;\nimpl T for S {}\n",
        ),
        (
            "file.absolute.tokio",
            "tokio/tests/macros_rename_test.rs",
            TOKIO_MACROS_RENAME_TEST,
        ),
    ]);

    let bindings = |path: &str| {
        generation
            .imports()
            .iter()
            .filter(|binding| binding.logical_path == path)
            .map(|binding| {
                (
                    binding.module_specifier.as_str(),
                    binding.imported_name.as_deref(),
                    binding.local_name.as_deref(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        bindings("crates/b/src/lib.rs"),
        [("a", Some("T"), Some("T"))]
    );
    // `use ::tokio as tokio1;` binds a crate root, exactly like `use std as tokio;`.
    assert_eq!(
        bindings("tokio/tests/macros_rename_test.rs"),
        Vec::<(&str, Option<&str>, Option<&str>)>::new()
    );

    assert_resolved_edge(
        &generation,
        &symbol_occurrence(&generation, "crates/b/src/lib.rs::S"),
        &symbol_occurrence(&generation, "crates/a/src/lib.rs::T"),
        RelationEdgeKindV1::Implements,
    );
    let compute_main = symbol_occurrence(
        &generation,
        "tokio/tests/macros_rename_test.rs::compute_main",
    );
    let compute = symbol_occurrence(&generation, "tokio/tests/macros_rename_test.rs::compute");
    assert!(
        generation
            .edges()
            .iter()
            .any(|edge| edge.from_occurrence == compute_main
                && edge.to_occurrence == compute
                && edge.kind == RelationEdgeKindV1::Calls),
        "the file behind `use ::tokio as tokio1;` must be indexed with its calls"
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
fn rust_generic_inherent_impl_matches_a_nominal_typed_receiver() {
    let generation = published_rust_workspace(&[
        (
            "file.generic.lib",
            "crates/widgets/src/lib.rs",
            "mod methods;\npub struct Builder<T>(pub T);\n",
        ),
        (
            "file.generic.methods",
            "crates/widgets/src/methods.rs",
            "use super::Builder;\nimpl<T> Builder<T> {\n    pub fn build(&self) {}\n}\n",
        ),
        (
            "file.generic.app",
            "crates/app/src/main.rs",
            "fn assemble(builder: &widgets::Builder<u8>) {\n    builder.build();\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let build = symbol_occurrence(
        &generation,
        "crates/widgets/src/methods.rs::Builder<T>::build",
    );

    assert_resolved_edge(&generation, &caller, &build, RelationEdgeKindV1::Calls);
}

#[test]
fn rust_qualified_inherent_impl_owner_resolves_to_its_type() {
    let generation = published_rust_workspace(&[
        (
            "file.qualified-owner.lib",
            "crates/widgets/src/lib.rs",
            "pub mod builder;\nmod methods;\npub use builder::Builder;\n",
        ),
        (
            "file.qualified-owner.builder",
            "crates/widgets/src/builder.rs",
            "pub struct Builder;\n",
        ),
        (
            "file.qualified-owner.methods",
            "crates/widgets/src/methods.rs",
            "impl crate::builder::Builder {\n    pub fn build(&self) {}\n}\n",
        ),
        (
            "file.qualified-owner.app",
            "crates/app/src/main.rs",
            "fn assemble(builder: &widgets::Builder) {\n    builder.build();\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let build = symbol_occurrence(
        &generation,
        "crates/widgets/src/methods.rs::crate::builder::Builder::build",
    );

    assert_resolved_edge(&generation, &caller, &build, RelationEdgeKindV1::Calls);
}

#[test]
fn rust_public_trait_methods_are_callable_across_crates() {
    let generation = published_rust_workspace(&[
        (
            "file.public-trait.lib",
            "crates/dep/src/lib.rs",
            "pub trait PublicTrait {\n    fn work(&self);\n}\n",
        ),
        (
            "file.public-trait.app",
            "crates/app/src/main.rs",
            "fn run(value: &dyn dep::PublicTrait) {\n    value.work();\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::run");
    let work = symbol_occurrence(&generation, "crates/dep/src/lib.rs::PublicTrait::work");

    assert_resolved_edge(&generation, &caller, &work, RelationEdgeKindV1::Calls);
}

#[test]
fn rust_trait_impl_methods_do_not_masquerade_as_inherent_methods() {
    let generation = published_rust_workspace(&[
        (
            "file.trait-impl.lib",
            "crates/widgets/src/lib.rs",
            "mod first;\nmod second;\npub struct Builder;\npub trait First { fn build(&self); }\npub trait Second { fn build(&self); }\n",
        ),
        (
            "file.trait-impl.first",
            "crates/widgets/src/first.rs",
            "impl crate::First for crate::Builder { fn build(&self) {} }\n",
        ),
        (
            "file.trait-impl.second",
            "crates/widgets/src/second.rs",
            "impl crate::Second for crate::Builder { fn build(&self) {} }\n",
        ),
        (
            "file.trait-impl.app",
            "crates/app/src/main.rs",
            "use widgets::First;\nfn assemble(builder: &widgets::Builder) {\n    builder.build();\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let first = symbol_occurrence(
        &generation,
        "crates/widgets/src/first.rs::<crate::Builder as crate::First>::build",
    );
    let second = symbol_occurrence(
        &generation,
        "crates/widgets/src/second.rs::<crate::Builder as crate::Second>::build",
    );

    assert!(generation.edges().iter().all(|edge| {
        edge.from_occurrence != caller
            || (edge.to_occurrence != first && edge.to_occurrence != second)
            || edge.kind != RelationEdgeKindV1::Calls
    }));
}

#[test]
fn rust_type_path_call_binds_unique_trait_impl_method() {
    let generation = published_rust_workspace(&[
        (
            "file.ufcs-from.lib",
            "crates/walk/src/lib.rs",
            concat!(
                "mod build;\n",
                "pub struct WalkDir;\n",
                "pub struct WalkEventIter;\n",
                "impl From<WalkDir> for WalkEventIter {\n",
                "    fn from(it: WalkDir) -> Self { let _ = it; WalkEventIter }\n",
                "}\n",
            ),
        ),
        (
            "file.ufcs-from.build",
            "crates/walk/src/build.rs",
            concat!(
                "use crate::{WalkDir, WalkEventIter};\n",
                "pub fn build(wd: WalkDir) {\n",
                "    let _ = WalkEventIter::from(wd);\n",
                "}\n",
            ),
        ),
        (
            "file.ufcs-from.same",
            "crates/app/src/main.rs",
            concat!(
                "struct Local;\n",
                "impl From<u8> for Local {\n",
                "    fn from(value: u8) -> Self { let _ = value; Local }\n",
                "}\n",
                "fn main() {\n",
                "    let _ = Local::from(1u8);\n",
                "}\n",
            ),
        ),
    ]);
    let same_file_caller = symbol_occurrence(&generation, "crates/app/src/main.rs::main");
    let same_crate_caller = symbol_occurrence(&generation, "crates/walk/src/build.rs::build");
    let local_from = symbol_occurrence(
        &generation,
        "crates/app/src/main.rs::<Local as From<u8>>::from",
    );
    let walk_from = symbol_occurrence(
        &generation,
        "crates/walk/src/lib.rs::<WalkEventIter as From<WalkDir>>::from",
    );

    assert!(
        generation.edges().iter().any(|edge| {
            edge.from_occurrence == same_file_caller
                && edge.to_occurrence == local_from
                && edge.kind == RelationEdgeKindV1::Calls
                && edge.authority == EdgeAuthorityV1::SyntaxExact
        }),
        "same-file Local::from must bind the UFCS From impl"
    );
    assert_resolved_edge(
        &generation,
        &same_crate_caller,
        &walk_from,
        RelationEdgeKindV1::Calls,
    );
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
fn rust_restricted_reexport_does_not_escape_its_crate() {
    let generation = published_rust_workspace(&[
        (
            "file.restricted.lib",
            "crates/widgets/src/lib.rs",
            "mod hidden;\npub(crate) use hidden::Builder;\n",
        ),
        (
            "file.restricted.hidden",
            "crates/widgets/src/hidden.rs",
            "pub struct Builder;\nimpl Builder { pub fn build(&self) {} }\n",
        ),
        (
            "file.restricted.app",
            "crates/app/src/main.rs",
            "fn assemble(builder: &widgets::Builder) {\n    builder.build();\n}\nfn main() {}\n",
        ),
    ]);
    let caller = symbol_occurrence(&generation, "crates/app/src/main.rs::assemble");
    let build = symbol_occurrence(&generation, "crates/widgets/src/hidden.rs::Builder::build");

    assert!(generation.edges().iter().all(|edge| {
        edge.from_occurrence != caller
            || edge.to_occurrence != build
            || edge.kind != RelationEdgeKindV1::Calls
    }));
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

fn import_rows_mut(file: &mut Value) -> &mut Vec<Value> {
    file["artifacts"]["imports"]
        .as_array_mut()
        .expect("sealed file imports")
}

/// Tamper the first file's sealed import rows, re-address the segment, and
/// reseal the manifest, so the refusal comes from the restored file payload.
fn tampered_import_payload_error(
    sealed: &PartitionedSealV1,
    mutation: &str,
    mutate: impl FnOnce(&mut Vec<Value>),
) -> CodeIndexProductionErrorV1 {
    let tampered = sealed.with_tampered_file_segment(0, |file| mutate(import_rows_mut(file)));
    match tampered.restore(&tampered.manifest) {
        Ok(_) => panic!("{mutation} restored after the segment and manifest were re-addressed"),
        Err(error) => error,
    }
}

fn file_imports(generation: &CodeIndexPublishedGenerationV1) -> Vec<CodeIndexImportEvidenceV1> {
    generation
        .imports()
        .iter()
        .filter(|row| row.logical_path == "src/a.ts")
        .cloned()
        .collect()
}

#[test]
fn file_import_artifacts_require_nondefault_canonical_rows() {
    let generation = published_import_generation();

    assert_eq!(
        file_imports(&generation)
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

    let sealed = PartitionedSealV1::of(&generation);
    let missing = sealed.with_tampered_file_segment(0, |file| {
        assert!(
            file["artifacts"]
                .as_object_mut()
                .expect("file artifact object")
                .remove("imports")
                .is_some(),
            "the sealed artifact must carry its required imports field"
        );
    });
    let error = missing
        .restore(&missing.manifest)
        .expect_err("imports must be a required field without a serde default");
    assert!(
        error.to_string().contains("missing field `imports`"),
        "unexpected missing-imports error: {error}"
    );
}

#[test]
fn file_import_artifacts_bind_file_consistent_path_and_nonempty_span_to_indexed_extent() {
    let generation = published_import_generation();
    let imports = file_imports(&generation);
    let indexed_end = generation
        .chunks()
        .chunks()
        .iter()
        .filter(|chunk| chunk.anchor.file_occurrence_id.as_str() == "file.import.a")
        .map(|chunk| chunk.anchor.source_span.end_byte)
        .max()
        .expect("complete file has indexed chunks");

    assert!(imports.iter().all(|row| {
        row.file_occurrence_id.as_str() == "file.import.a"
            && !row.span.is_empty()
            && row.span.end_byte <= indexed_end
    }));
    assert_eq!(
        imports
            .iter()
            .map(|row| {
                &FIRST_SOURCE[usize::try_from(row.span.start_byte).expect("span start")
                    ..usize::try_from(row.span.end_byte).expect("span end")]
            })
            .collect::<Vec<_>>(),
        vec!["Foo", "Bar as Baz"]
    );
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
fn sealed_import_generation_round_trips_to_identical_bytes() {
    let first = published_import_generation();
    let first_sealed = PartitionedSealV1::of(&first);
    let second_sealed = PartitionedSealV1::of(&published_import_generation());
    assert_eq!(first_sealed.manifest, second_sealed.manifest);
    assert_eq!(first_sealed.segments, second_sealed.segments);

    assert_eq!(
        first_sealed.envelope()["generation"]["format_revision"],
        SEALED_GENERATION_FORMAT_REVISION_V1
    );
    let restored = first_sealed.restored();
    assert_eq!(restored.imports(), first.imports());
    assert_eq!(
        PartitionedSealV1::of(&restored).manifest,
        first_sealed.manifest
    );
}

#[test]
fn sealed_import_generation_rejects_semantic_tampering_after_segment_readdress() {
    let sealed = PartitionedSealV1::of(&published_import_generation());
    sealed.restored();
    assert!(
        sealed.file_segment_payload(0)["artifacts"]
            .get("import_rows_digest")
            .is_none(),
        "sealed file artifacts must not contain a recomputable self-digest authority"
    );

    let error = tampered_import_payload_error(&sealed, "binding-name tamper", |rows| {
        rows[0]["imported_name"] = Value::String("Forged".to_owned());
    });
    assert!(
        error.to_string().contains("import_authority_mismatch"),
        "semantic tamper reached the wrong authority rejection: {error}"
    );
}

#[test]
fn sealed_import_generation_rejects_reorder_and_duplicate_after_segment_readdress() {
    let sealed = PartitionedSealV1::of(&published_import_generation());
    sealed.restored();

    tampered_import_payload_error(&sealed, "row reorder", |rows| rows.swap(0, 1));
    tampered_import_payload_error(&sealed, "duplicate row", |rows| {
        let duplicate = rows[0].clone();
        rows.insert(1, duplicate);
    });
}

#[test]
fn sealed_import_generation_rejects_wrong_file_path_and_span_after_segment_readdress() {
    let sealed = PartitionedSealV1::of(&published_import_generation());
    sealed.restored();

    tampered_import_payload_error(&sealed, "foreign file occurrence", |rows| {
        for row in rows {
            row["file_occurrence_id"] = Value::String("file.foreign".to_owned());
        }
    });
    tampered_import_payload_error(&sealed, "foreign logical path", |rows| {
        for row in rows {
            row["logical_path"] = Value::String("src/foreign.ts".to_owned());
        }
    });
    tampered_import_payload_error(&sealed, "empty source span", |rows| {
        let start = rows[0]["span"]["start_byte"].clone();
        rows[0]["span"]["end_byte"] = start;
    });
    tampered_import_payload_error(&sealed, "out-of-bounds source span", |rows| {
        rows[0]["span"]["end_byte"] = Value::from(FIRST_SOURCE.len() as u64 + 1);
    });
}

/// starship@cc825b00 `src/modules/mod.rs::handle`: `container::module` has one
/// `#[cfg]` variant per target, and `status::module` goes through the file's
/// own `mod status;` although `status` is also a blocklisted std name and a
/// second `status` module exists under `configs`.
#[test]
fn rust_calls_bind_every_cfg_variant_and_declared_modules_named_like_std() {
    let generation = published_rust_workspace(&[
        (
            "file.starship.main",
            "src/main.rs",
            "mod configs;\nmod modules;\n",
        ),
        (
            "file.starship.configs",
            "src/configs/mod.rs",
            "pub mod status;\n",
        ),
        (
            "file.starship.configs-status",
            "src/configs/status.rs",
            "pub struct StatusConfig;\n",
        ),
        (
            "file.starship.modules",
            "src/modules/mod.rs",
            "mod aws;\nmod container;\nmod status;\n\npub fn handle() -> u32 {\n    \
             aws::module() + container::module() + status::module()\n}\n",
        ),
        (
            "file.starship.aws",
            "src/modules/aws.rs",
            "pub fn module() -> u32 { 1 }\n",
        ),
        (
            "file.starship.container",
            "src/modules/container.rs",
            "#[cfg(not(target_os = \"linux\"))]\npub fn module() -> u32 { 0 }\n\n\
             #[cfg(target_os = \"linux\")]\npub fn module() -> u32 { 2 }\n",
        ),
        (
            "file.starship.status",
            "src/modules/status.rs",
            "pub fn module() -> u32 { 3 }\n",
        ),
    ]);
    let handle = symbol_occurrence(&generation, "src/modules/mod.rs::handle");
    let by_occurrence = generation
        .symbols()
        .symbols
        .iter()
        .map(|symbol| (&symbol.occurrence, symbol))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut callees = generation
        .edges()
        .iter()
        .filter(|edge| edge.from_occurrence == handle && edge.kind == RelationEdgeKindV1::Calls)
        .map(|edge| {
            let symbol = by_occurrence[&edge.to_occurrence];
            (symbol.qualified_name.as_str(), symbol.start_line)
        })
        .collect::<Vec<_>>();
    callees.sort();
    assert_eq!(
        callees,
        vec![
            ("src/modules/aws.rs::module", 0),
            ("src/modules/container.rs::module", 1),
            ("src/modules/container.rs::module", 4),
            ("src/modules/status.rs::module", 0),
        ],
        "handle calls three module identities; the cfg-duplicated one binds at both sites"
    );
}

/// tokio declares modules inside `cfg_rt! { ... }` item lists; a function in
/// such a module is a symbol whose same-file and cross-file callers resolve.
#[test]
fn rust_items_inside_item_list_macros_are_symbols_with_resolved_callers() {
    let generation = published_rust_workspace(&[
        (
            "file.cfg-rt.lib",
            "src/lib.rs",
            "cfg_rt! { pub mod foo { pub fn bar() {} } }\n\
             mod user;\n\
             pub fn local() { foo::bar(); }\n",
        ),
        (
            "file.cfg-rt.user",
            "src/user.rs",
            "pub fn remote() { crate::foo::bar(); }\n",
        ),
    ]);
    let bar = symbol_occurrence(&generation, "src/lib.rs::foo::bar");
    let mut callers = generation
        .edges()
        .iter()
        .filter(|edge| edge.to_occurrence == bar && edge.kind == RelationEdgeKindV1::Calls)
        .map(|edge| {
            generation
                .symbols()
                .symbols
                .iter()
                .find(|symbol| symbol.occurrence == edge.from_occurrence)
                .map(|symbol| (symbol.qualified_name.as_str(), edge.authority))
        })
        .collect::<Vec<_>>();
    callers.sort_by_key(|caller| caller.as_ref().map(|(name, _)| *name));
    assert_eq!(
        callers,
        vec![
            Some(("src/lib.rs::local", EdgeAuthorityV1::SyntaxExact)),
            Some(("src/user.rs::remote", EdgeAuthorityV1::NameResolved)),
        ]
    );
}

fn resolved_callers<'a>(
    generation: &'a CodeIndexPublishedGenerationV1,
    target: &SymbolOccurrenceId,
) -> Vec<&'a str> {
    let mut callers = generation
        .edges()
        .iter()
        .filter(|edge| edge.to_occurrence == *target && edge.kind == RelationEdgeKindV1::Calls)
        .filter_map(|edge| {
            generation
                .symbols()
                .symbols
                .iter()
                .find(|symbol| symbol.occurrence == edge.from_occurrence)
                .map(|symbol| symbol.qualified_name.as_str())
        })
        .collect::<Vec<_>>();
    callers.sort_unstable();
    callers
}

#[test]
fn rust_calls_bind_through_a_use_declared_in_the_calling_block() {
    let generation = published_rust_workspace(&[
        (
            "file.block-use.lib",
            "crates/app/src/lib.rs",
            "mod m;\nmod n;\nmod runtime;\nuse crate::n::g;\n\
             pub fn f() { use crate::m::g; g(); }\n\
             pub fn module_scope() { g(); }\n\
             pub fn spawn_inner() {\n    use crate::runtime::{context, task};\n    context::with_current();\n    task::schedule();\n}\n\
             pub fn timer() {\n    #[cfg(feature = \"rt\")]\n    {\n        use crate::runtime::context;\n        context::with_current();\n    }\n}\n\
             mod inner { pub fn x() {} }\n\
             pub fn same_file() { use self::inner::x; x(); }\n",
        ),
        ("file.block-use.m", "crates/app/src/m.rs", "pub fn g() {}\n"),
        ("file.block-use.n", "crates/app/src/n.rs", "pub fn g() {}\n"),
        (
            "file.block-use.runtime",
            "crates/app/src/runtime/mod.rs",
            "pub mod context;\npub mod task;\n",
        ),
        (
            "file.block-use.context",
            "crates/app/src/runtime/context.rs",
            "mod current;\npub(crate) use current::with_current;\n",
        ),
        (
            "file.block-use.current",
            "crates/app/src/runtime/context/current.rs",
            "pub(crate) fn with_current() {}\n",
        ),
        (
            "file.block-use.task",
            "crates/app/src/runtime/task.rs",
            "pub fn schedule() {}\n",
        ),
    ]);
    let block_g = symbol_occurrence(&generation, "crates/app/src/m.rs::g");
    let module_g = symbol_occurrence(&generation, "crates/app/src/n.rs::g");
    let with_current = symbol_occurrence(
        &generation,
        "crates/app/src/runtime/context/current.rs::with_current",
    );
    let schedule = symbol_occurrence(&generation, "crates/app/src/runtime/task.rs::schedule");
    let inner_x = symbol_occurrence(&generation, "crates/app/src/lib.rs::inner::x");

    // The block's `use` shadows the module-scope import only inside `f`.
    assert_eq!(
        resolved_callers(&generation, &block_g),
        ["crates/app/src/lib.rs::f"]
    );
    assert_eq!(
        resolved_callers(&generation, &module_g),
        ["crates/app/src/lib.rs::module_scope"]
    );
    assert_eq!(
        resolved_callers(&generation, &with_current),
        [
            "crates/app/src/lib.rs::spawn_inner",
            "crates/app/src/lib.rs::timer"
        ]
    );
    assert_eq!(
        resolved_callers(&generation, &schedule),
        ["crates/app/src/lib.rs::spawn_inner"]
    );
    // A block `use` of this file's own item keeps binding it in-file.
    assert_eq!(
        resolved_callers(&generation, &inner_x),
        ["crates/app/src/lib.rs::same_file"]
    );
}
