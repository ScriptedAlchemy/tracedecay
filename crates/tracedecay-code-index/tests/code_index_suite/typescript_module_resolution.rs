//! Cross-file TypeScript edges through the import forms a monorepo actually
//! writes: workspace package names, tsconfig `paths` aliases (with
//! `extends`), dotted extensionless file names, and barrel re-exports. The
//! fixture is the redacted monorepo under the extraction crate's fixtures.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use tracedecay_code_index::{
    chunks::content_digest,
    graph_projection::{
        CODE_GRAPH_PROJECTOR_REVISION, CodeGraphInteractiveReader, CodeGraphProjectionStore,
        build_published_code_graph_manifest_checked, code_graph_projection_identity,
    },
    production::{
        CodeIndexCapturedFileV1, CodeIndexProductionOwnerV1, CodeIndexPublishedGenerationV1,
    },
};
use tracedecay_domain::{
    EdgeAuthorityV1, FileOccurrenceId, LanguageId, RelationEdgeKindV1, SanitizationReceiptId,
    SanitizedCodeFileV1, SensitivityLevelV1, SnapshotFileDispositionV1, SymbolOccurrenceId,
};
use tracedecay_graph_db::{
    GraphNamespace, GraphProjectorRevision, NeverCancelled, VerifiedGraphSnapshot,
};

use crate::{
    production_orchestration::{
        ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
    },
    support::{PartitionedSealV1, id},
};

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tracedecay-code-extraction/fixtures/typescript-monorepo"
);

fn fixture_files() -> Vec<(String, String)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let mut entries = std::fs::read_dir(dir)
            .expect("fixture directory")
            .map(|entry| entry.expect("fixture entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("fixture path under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((
                    relative,
                    std::fs::read_to_string(&path).expect("fixture source"),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(Path::new(FIXTURE_ROOT), Path::new(FIXTURE_ROOT), &mut files);
    assert!(files.len() >= 10, "fixture monorepo is present: {files:?}");
    files
}

fn language_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("ts") => "typescript",
        Some("json") => "json",
        other => panic!("fixture file {path} has an unexpected extension {other:?}"),
    }
}

fn published_fixture() -> Arc<CodeIndexPublishedGenerationV1> {
    let mut request = request_with_source(
        "file.ts-monorepo.seed",
        1_700_000,
        "commit.ts-monorepo.1",
        "tree.ts-monorepo.1",
        "",
    );
    request.snapshot.files.clear();
    request.snapshot.sanitization_receipts.clear();
    request.captured_files.clear();
    request.changed_files.clear();
    let mut identity = Vec::new();
    for (ordinal, (path, source)) in fixture_files().into_iter().enumerate() {
        let file_occurrence_id = id::<FileOccurrenceId>(&format!("file.ts-monorepo.{ordinal:02}"));
        let bytes = source.as_bytes();
        request.snapshot.files.push(SanitizedCodeFileV1 {
            file_occurrence_id: file_occurrence_id.clone(),
            logical_path: path.clone(),
            language: Some(id::<LanguageId>(language_for(&path))),
            content_digest: content_digest(bytes),
            disposition: SnapshotFileDispositionV1::Present,
        });
        request
            .snapshot
            .sanitization_receipts
            .push(id::<SanitizationReceiptId>(&format!(
                "receipt.ts-monorepo.{ordinal:02}"
            )));
        request.captured_files.push(CodeIndexCapturedFileV1 {
            file_occurrence_id,
            sanitized_bytes: Arc::from(bytes),
            sensitivity_level: SensitivityLevelV1::Public,
        });
        request.changed_files.insert(path.clone());
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
        .expect("TypeScript monorepo snapshot is canonical");
    CodeIndexProductionOwnerV1::new(
        config(),
        SharedPublicationStore::default(),
        ApplyingProjectionSink,
    )
    .expect("production owner")
    .build_and_publish(request, &ActiveControl)
    .expect("TypeScript monorepo generation publishes")
}

fn symbol(generation: &CodeIndexPublishedGenerationV1, qualified_name: &str) -> SymbolOccurrenceId {
    generation
        .symbols()
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == qualified_name)
        .unwrap_or_else(|| panic!("missing symbol {qualified_name}"))
        .occurrence
        .clone()
}

/// `(caller qualified name, call-site count)` for every name-resolved call
/// edge into `target`.
fn resolved_callers(
    generation: &CodeIndexPublishedGenerationV1,
    target: &SymbolOccurrenceId,
) -> BTreeMap<String, usize> {
    callers_with_authority(generation, target, EdgeAuthorityV1::NameResolved)
}

fn callers_with_authority(
    generation: &CodeIndexPublishedGenerationV1,
    target: &SymbolOccurrenceId,
    authority: EdgeAuthorityV1,
) -> BTreeMap<String, usize> {
    let names = generation
        .symbols()
        .symbols
        .iter()
        .map(|symbol| (symbol.occurrence.clone(), symbol.qualified_name.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut callers = BTreeMap::new();
    for edge in generation.edges().iter().filter(|edge| {
        edge.to_occurrence == *target
            && edge.kind == RelationEdgeKindV1::Calls
            && edge.authority == authority
    }) {
        *callers
            .entry(names[&edge.from_occurrence].clone())
            .or_default() += 1;
    }
    callers
}

fn reader(generation: &CodeIndexPublishedGenerationV1) -> CodeGraphInteractiveReader {
    let manifest = build_published_code_graph_manifest_checked(
        code_graph_projection_identity(
            GraphNamespace::new("code-graph-ts-monorepo").expect("graph namespace"),
        )
        .expect("projection identity"),
        generation,
        &GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision"),
        &|| Ok(()),
    )
    .expect("published generation projects");
    let snapshot =
        VerifiedGraphSnapshot::memory(Arc::unwrap_or_clone(manifest), Arc::new(NeverCancelled))
            .expect("verified graph snapshot");
    CodeGraphProjectionStore::from_verified_snapshot(
        snapshot,
        generation.manifest().generation_id.clone(),
    )
    .expect("verified graph store")
    .interactive_reader_with_cancellation(
        &generation.manifest().generation_id,
        Arc::new(NeverCancelled),
    )
    .expect("generation-pinned reader")
}

/// The `file_dependents` seam: files whose symbols call or use symbols in
/// `file`, plus whether the graph holds call sites it could not bind to them.
fn file_dependents(reader: &CodeGraphInteractiveReader, file: &str) -> (BTreeSet<String>, bool) {
    let seeds = reader
        .symbols_in_logical_file(file, 1_000, Arc::new(NeverCancelled))
        .expect("file symbols")
        .into_iter()
        .map(|symbol| symbol.occurrence)
        .collect::<Vec<_>>();
    assert!(!seeds.is_empty(), "{file} publishes symbols");
    let unresolved = reader
        .has_unresolved_callers(&seeds, None, Arc::new(NeverCancelled))
        .expect("unresolved caller probe");
    let dependents = reader
        .callers(
            &seeds,
            &[RelationEdgeKindV1::Calls, RelationEdgeKindV1::Uses],
            10_000,
            Arc::new(NeverCancelled),
        )
        .expect("incoming edges")
        .into_iter()
        .flatten()
        .filter_map(|edge| edge.neighbor.binding?.logical_path)
        .filter(|path| path != file)
        .collect::<BTreeSet<_>>();
    (dependents, unresolved)
}

#[test]
fn workspace_package_alias_dotted_and_barrel_imports_bind_every_call_site() {
    let generation = published_fixture();
    let main = "apps/web/src/main.ts::main".to_owned();
    let test_case = "apps/web/test/report.helpers.test.ts::report helpers::joins rows".to_owned();

    // (a) `@fixture/shared` by manifest name, through `exports["."]` mapped
    // from `dist/` to `src/`, then through the barrel: `export * from` for
    // `format` (a blocklisted ubiquitous name that an explicit import still
    // binds), `export { sum as add } from` for `add`, and the `./math`
    // subpath export for `sum`.
    let format = symbol(&generation, "packages/shared/src/format.ts::format");
    assert_eq!(
        resolved_callers(&generation, &format),
        BTreeMap::from([(main.clone(), 2)])
    );
    let sum = symbol(&generation, "packages/shared/src/math.ts::sum");
    assert_eq!(
        resolved_callers(&generation, &sum),
        BTreeMap::from([(main.clone(), 2)])
    );

    // (b) `./report.helpers` and `../src/report.helpers` name
    // `report.helpers.ts`; four call sites across two files.
    let build_report = symbol(&generation, "apps/web/src/report.helpers.ts::buildReport");
    assert_eq!(
        resolved_callers(&generation, &build_report),
        BTreeMap::from([(main.clone(), 1), (test_case.clone(), 2)])
    );

    // (c) `./lib` resolves to `lib/index.ts`, whose `export { reexported }
    // from "./x"` forwards to the defining file.
    let reexported = symbol(&generation, "apps/web/src/lib/x.ts::reexported");
    assert_eq!(
        resolved_callers(&generation, &reexported),
        BTreeMap::from([(main.clone(), 1)])
    );

    // tsconfig `paths` from the nearest config, `extends` for the base.
    let widget = symbol(&generation, "apps/web/src/widgets/widget.ts::widget");
    assert_eq!(
        resolved_callers(&generation, &widget),
        BTreeMap::from([(main.clone(), 1)])
    );

    // The decoys share names with the unresolvable imports in `gaps.ts`; a
    // module-resolved import never falls back to name matching.
    for decoy in ["missing", "gone", "useState"] {
        let target = symbol(&generation, &format!("apps/web/src/decoys.ts::{decoy}"));
        assert!(
            resolved_callers(&generation, &target).is_empty(),
            "{decoy} binds nothing"
        );
    }
}

/// `hops.ts` forwards its own bindings through a same-module export clause:
/// a declaration (`hopped`), a renamed declaration beside a same-named local
/// (`inner as renamed`), a local import (`relayTarget as relayed`), and a
/// local import of an unindexed module (`missing as relayedMissing`).
#[test]
fn same_module_export_clauses_bind_the_forwarded_binding() {
    let generation = published_fixture();
    let consume = "apps/web/src/hop-consumer.ts::consumeHops".to_owned();
    let test_file = "apps/web/test/shadowing.test.ts";

    let hopped = symbol(&generation, "apps/web/src/hops.ts::hopped");
    assert_eq!(
        resolved_callers(&generation, &hopped),
        BTreeMap::from([
            (consume.clone(), 1),
            (format!("{test_file}::hopped::calls the import"), 1),
        ])
    );
    let inner = symbol(&generation, "apps/web/src/hops.ts::inner");
    assert_eq!(
        resolved_callers(&generation, &inner),
        BTreeMap::from([(consume.clone(), 1)])
    );
    // The local `renamed` is not what `export { inner as renamed }` exports.
    let renamed = symbol(&generation, "apps/web/src/hops.ts::renamed");
    assert!(resolved_callers(&generation, &renamed).is_empty());
    let relay_target = symbol(&generation, "apps/web/src/hop-target.ts::relayTarget");
    assert_eq!(
        resolved_callers(&generation, &relay_target),
        BTreeMap::from([
            (consume.clone(), 1),
            (format!("{test_file}::calls the imported relay"), 1),
        ])
    );

    // `relayedMissing` forwards an import of a module that is not indexed:
    // no edge, and the call site stays a disclosed gap.
    let gaps = generation.unresolved_typescript_import_calls();
    assert!(
        gaps.iter()
            .any(|gap| gap.reference_name == "relayedMissing"),
        "{gaps:?}"
    );
}

/// A test title is not a declaration, and a same-file declaration shadows an
/// import only inside the function scope that declares it.
#[test]
fn test_titles_and_out_of_scope_helpers_do_not_shadow_imports() {
    let generation = published_fixture();
    let test_file = "apps/web/test/shadowing.test.ts";

    for title in ["hopped", "relayed"] {
        let describe = symbol(&generation, &format!("{test_file}::{title}"));
        for authority in [EdgeAuthorityV1::SyntaxExact, EdgeAuthorityV1::NameResolved] {
            assert!(
                callers_with_authority(&generation, &describe, authority).is_empty(),
                "describe({title:?}) is no call target"
            );
        }
    }
    let helper = symbol(&generation, &format!("{test_file}::relayed::relayed"));
    assert_eq!(
        callers_with_authority(&generation, &helper, EdgeAuthorityV1::SyntaxExact),
        BTreeMap::from([(format!("{test_file}::relayed::calls the local helper"), 1)])
    );
    let relay_target = symbol(&generation, "apps/web/src/hop-target.ts::relayTarget");
    assert!(
        !resolved_callers(&generation, &relay_target)
            .contains_key(&format!("{test_file}::relayed::calls the local helper")),
        "the in-scope helper shadows the import"
    );
}

#[test]
fn default_and_namespace_imports_bind_every_call_site() {
    let generation = published_fixture();
    let defaults = "apps/web/src/consumers.ts::consumeDefaults".to_owned();
    let namespaces = "apps/web/src/consumers.ts::consumeNamespaces".to_owned();

    // Default imports: `export default function` (directly and through
    // `relay.ts`, which default-exports its own default import of it),
    // `export default <name>`, `export { impl as default }`, and
    // `export { default as welcome } from` reached both by name and as a
    // namespace member of the barrel.
    for (target, expected) in [
        ("apps/web/src/defaults/greet.ts::greet", 2),
        ("apps/web/src/defaults/farewell.ts::farewell", 1),
        ("apps/web/src/defaults/aliased.ts::aliasedImpl", 1),
        ("apps/web/src/defaults/welcome.ts::welcome", 2),
    ] {
        assert_eq!(
            resolved_callers(&generation, &symbol(&generation, target)),
            BTreeMap::from([(defaults.clone(), expected)]),
            "{target}"
        );
    }

    // Namespace members: a relative `import * as`, a nested `export * as`
    // namespace behind a workspace package, and a named import of that
    // namespace.
    for target in [
        "apps/web/src/tools.ts::sharpen",
        "packages/shared/src/strings.ts::upper",
        "packages/shared/src/strings.ts::lower",
    ] {
        assert_eq!(
            resolved_callers(&generation, &symbol(&generation, target)),
            BTreeMap::from([(namespaces.clone(), 1)]),
            "{target}"
        );
    }

    // `export * from` never forwards `default`, a namespace without the
    // member binds nothing, and a member of an external namespace is not
    // bound to the same-named project decoy.
    for target in [
        "packages/shared/src/defaulted.ts::defaulted",
        "apps/web/src/decoys.ts::absentMember",
        "apps/web/src/decoys.ts::useState",
    ] {
        assert!(
            resolved_callers(&generation, &symbol(&generation, target)).is_empty(),
            "{target} binds nothing"
        );
    }
}

#[test]
fn file_dependents_list_every_importing_file_and_disclose_unbound_imports() {
    let generation = published_fixture();
    let reader = reader(&generation);

    let (dependents, unresolved) = file_dependents(&reader, "packages/shared/src/format.ts");
    assert_eq!(
        dependents,
        BTreeSet::from(["apps/web/src/main.ts".to_owned()])
    );
    assert!(!unresolved);

    let (dependents, unresolved) = file_dependents(&reader, "apps/web/src/report.helpers.ts");
    assert_eq!(
        dependents,
        BTreeSet::from([
            "apps/web/src/main.ts".to_owned(),
            "apps/web/test/report.helpers.test.ts".to_owned(),
        ])
    );
    assert!(!unresolved);

    let (dependents, unresolved) = file_dependents(&reader, "apps/web/src/lib/x.ts");
    assert_eq!(
        dependents,
        BTreeSet::from(["apps/web/src/main.ts".to_owned()])
    );
    assert!(!unresolved);

    // `gaps.ts` imports `missing` from a module that is not indexed and
    // `gone` from a workspace subpath the package does not export: the empty
    // dependent list for the same-named decoys is disclosed as incomplete.
    // Its `react` import is an external dependency and no gap at all.
    let (dependents, unresolved) = file_dependents(&reader, "apps/web/src/decoys.ts");
    assert!(dependents.is_empty());
    assert!(
        unresolved,
        "unbound project imports must surface as a coverage gap"
    );
    // `consumers.ts` adds a namespace without the member (`absentMember`) and
    // an external namespace member (`React.useState`).
    for (name, expected) in [
        ("missing", true),
        ("gone", true),
        ("absentMember", true),
        ("useState", false),
    ] {
        let target = symbol(&generation, &format!("apps/web/src/decoys.ts::{name}"));
        assert_eq!(
            reader
                .has_unresolved_callers(&[target], None, Arc::new(NeverCancelled))
                .expect("probe"),
            expected,
            "{name}"
        );
    }

    for file in [
        "apps/web/src/defaults/welcome.ts",
        "apps/web/src/tools.ts",
        "packages/shared/src/strings.ts",
    ] {
        let (dependents, unresolved) = file_dependents(&reader, file);
        assert_eq!(
            dependents,
            BTreeSet::from(["apps/web/src/consumers.ts".to_owned()]),
            "{file}"
        );
        assert!(!unresolved, "{file}");
    }

    // A default import through a barrel's `export *` names project code that
    // does not export it: no dependent, disclosed as a gap.
    let (dependents, unresolved) = file_dependents(&reader, "packages/shared/src/defaulted.ts");
    assert!(dependents.is_empty());
    assert!(unresolved);
}

#[test]
fn sealed_replay_recomputes_identical_typescript_edges() {
    let generation = published_fixture();
    let restored = PartitionedSealV1::of(&generation).restored();
    assert_eq!(restored.edges(), generation.edges());
    assert_eq!(
        restored.unresolved_typescript_import_calls(),
        generation.unresolved_typescript_import_calls()
    );
    let mut unresolved = generation
        .unresolved_typescript_import_calls()
        .into_iter()
        .map(|reference| reference.reference_name)
        .collect::<Vec<_>>();
    unresolved.sort();
    assert_eq!(
        unresolved,
        [
            "defaulted",
            "gone",
            "missing",
            "relayedMissing",
            "tools.absentMember"
        ]
    );
}
