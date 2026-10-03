//! The same sixteen-function call graph written in six languages, each
//! through its own import forms (`fixtures/cross-file-calls/<language>`).
//! Every cross-file call binds through the file's imports, package, or
//! loaded files; a call on a runtime value (`store.add(...)`) binds nothing
//! and stays a disclosed caller gap.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use tracedecay_code_index::{
    chunks::content_digest,
    graph_projection::CodeGraphInteractiveReader,
    production::{
        CodeIndexBuildRequestV1, CodeIndexCapturedFileV1, CodeIndexProductionOwnerV1,
        CodeIndexPublishedGenerationV1,
    },
};
use tracedecay_domain::{
    FileOccurrenceId, LanguageId, RelationEdgeKindV1, SanitizationReceiptId, SanitizedCodeFileV1,
    SensitivityLevelV1, SnapshotFileDispositionV1, SymbolOccurrenceId,
};
use tracedecay_graph_db::NeverCancelled;

use crate::{
    production_orchestration::{
        ActiveControl, ApplyingProjectionSink, SharedPublicationStore, config, request_with_source,
    },
    support::{cold_generation, id},
    typescript_module_resolution::reader,
};

const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../tracedecay-code-extraction/fixtures/cross-file-calls"
);

/// Every call of the designed graph, `(caller, callee)` by role. The two
/// `main -> add|get` calls go through a local `store` value.
const CALLS: [(&str, &str); 22] = [
    ("main", "add"),
    ("main", "get"),
    ("main", "summary"),
    ("main", "upgrade"),
    ("main", "area"),
    ("main", "perimeter"),
    ("main", "old_format"),
    ("main", "scale"),
    ("summary", "format_line"),
    ("summary", "mean"),
    ("format_line", "normalize"),
    ("upgrade", "shim"),
    ("upgrade", "normalize"),
    ("shim", "legacy_normalize"),
    ("old_format", "legacy_normalize"),
    ("area", "clamp"),
    ("perimeter", "clamp"),
    ("perimeter", "total"),
    ("add", "normalize"),
    ("get", "normalize"),
    ("mean", "total"),
    ("scale", "clamp"),
];

/// Calls on a runtime value, which no import or package rule reaches.
const RECEIVER_CALLS: [(&str, &str); 2] = [("main", "add"), ("main", "get")];

fn python() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("normalize", "app/util.py::normalize"),
        ("clamp", "app/util.py::clamp"),
        ("legacy_normalize", "app/legacy.py::normalize"),
        ("old_format", "app/legacy.py::old_format"),
        ("shim", "app/compat.py::shim"),
        ("upgrade", "app/compat.py::upgrade"),
        ("area", "app/shapes.py::area"),
        ("perimeter", "app/shapes.py::perimeter"),
        ("add", "app/store.py::Store::add"),
        ("get", "app/store.py::Store::get"),
        ("format_line", "app/report.py::format_line"),
        ("summary", "app/report.py::summary"),
        ("total", "app/math.py::total"),
        ("mean", "app/math.py::mean"),
        ("scale", "app/math.py::scale"),
        ("main", "app/main.py::main"),
    ])
}

fn go() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("normalize", "util/util.go::Normalize"),
        ("clamp", "util/util.go::Clamp"),
        ("legacy_normalize", "legacy/legacy.go::Normalize"),
        ("old_format", "legacy/legacy.go::OldFormat"),
        ("shim", "compat/compat.go::Shim"),
        ("upgrade", "compat/compat.go::Upgrade"),
        ("area", "shapes/shapes.go::Area"),
        ("perimeter", "shapes/shapes.go::Perimeter"),
        ("add", "store/store.go::Add"),
        ("get", "store/store.go::Get"),
        ("format_line", "report/report.go::FormatLine"),
        ("summary", "report/report.go::Summary"),
        ("total", "mathx/total.go::Total"),
        ("mean", "mathx/stats.go::Mean"),
        ("scale", "mathx/stats.go::Scale"),
        ("main", "main.go::main"),
    ])
}

fn java() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("normalize", "src/app/util/Util.java::Util::normalize"),
        ("clamp", "src/app/util/Util.java::Util::clamp"),
        (
            "legacy_normalize",
            "src/app/legacy/Legacy.java::Legacy::normalize",
        ),
        (
            "old_format",
            "src/app/legacy/Legacy.java::Legacy::oldFormat",
        ),
        ("shim", "src/app/compat/Compat.java::Compat::shim"),
        ("upgrade", "src/app/compat/Compat.java::Compat::upgrade"),
        ("area", "src/app/shapes/Shapes.java::Shapes::area"),
        ("perimeter", "src/app/shapes/Shapes.java::Shapes::perimeter"),
        ("add", "src/app/store/Store.java::Store::add"),
        ("get", "src/app/store/Store.java::Store::get"),
        (
            "format_line",
            "src/app/report/Report.java::Report::formatLine",
        ),
        ("summary", "src/app/report/Report.java::Report::summary"),
        ("total", "src/app/math/MathOps.java::MathOps::total"),
        ("mean", "src/app/math/Stats.java::Stats::mean"),
        ("scale", "src/app/math/MathOps.java::MathOps::scale"),
        ("main", "src/app/Main.java::Main::main"),
    ])
}

fn ruby() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("normalize", "lib/util.rb::Util::normalize"),
        ("clamp", "lib/util.rb::Util::clamp"),
        ("legacy_normalize", "lib/legacy.rb::Legacy::normalize"),
        ("old_format", "lib/legacy.rb::Legacy::old_format"),
        ("shim", "lib/compat.rb::Compat::shim"),
        ("upgrade", "lib/compat.rb::Compat::upgrade"),
        ("area", "lib/shapes.rb::Shapes::area"),
        ("perimeter", "lib/shapes.rb::Shapes::perimeter"),
        ("add", "lib/store.rb::Store::add"),
        ("get", "lib/store.rb::Store::get"),
        ("format_line", "lib/report.rb::Report::format_line"),
        ("summary", "lib/report.rb::Report::summary"),
        ("total", "lib/math_ops.rb::MathOps::total"),
        ("mean", "lib/math_ops.rb::MathOps::mean"),
        ("scale", "lib/math_ops.rb::MathOps::scale"),
        ("main", "lib/main.rb::main"),
    ])
}

fn rust() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("normalize", "src/util.rs::normalize"),
        ("clamp", "src/util.rs::clamp"),
        ("legacy_normalize", "src/legacy.rs::normalize"),
        ("old_format", "src/legacy.rs::old_format"),
        ("shim", "src/compat.rs::shim"),
        ("upgrade", "src/compat.rs::upgrade"),
        ("area", "src/shapes.rs::area"),
        ("perimeter", "src/shapes.rs::perimeter"),
        ("add", "src/store.rs::Store::add"),
        ("get", "src/store.rs::Store::get"),
        ("format_line", "src/report.rs::format_line"),
        ("summary", "src/report.rs::summary"),
        ("total", "src/math.rs::total"),
        ("mean", "src/math.rs::mean"),
        ("scale", "src/math.rs::scale"),
        ("main", "src/main.rs::main"),
    ])
}

fn typescript() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("normalize", "src/util.ts::normalize"),
        ("clamp", "src/util.ts::clamp"),
        ("legacy_normalize", "src/legacy.ts::normalize"),
        ("old_format", "src/legacy.ts::oldFormat"),
        ("shim", "src/compat.ts::shim"),
        ("upgrade", "src/compat.ts::upgrade"),
        ("area", "src/shapes.ts::area"),
        ("perimeter", "src/shapes.ts::perimeter"),
        ("add", "src/store.ts::Store::add"),
        ("get", "src/store.ts::Store::get"),
        ("format_line", "src/report.ts::formatLine"),
        ("summary", "src/report.ts::summary"),
        ("total", "src/math.ts::total"),
        ("mean", "src/math.ts::mean"),
        ("scale", "src/math.ts::scale"),
        ("main", "src/main.ts::main"),
    ])
}

/// Every file under `root`, path-sorted, as `(root-relative path, source)`.
pub(crate) fn fixture_files(root: &Path) -> Vec<(String, String)> {
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
    walk(root, root, &mut files);
    assert!(!files.is_empty(), "fixture {} is present", root.display());
    files
}

fn language_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("py") => "python",
        Some("go" | "mod") => "go",
        Some("java") => "java",
        Some("rb") => "ruby",
        Some("rs") => "rust",
        Some("toml") => "toml",
        Some("ts" | "tsx") => "typescript",
        Some("json") => "json",
        other => panic!("fixture file {path} has an unexpected extension {other:?}"),
    }
}

/// Publish one generation of every file under `root` through the production
/// owner, with identities derived from `tag`.
pub(crate) fn publish_fixture_tree(root: &Path, tag: &str) -> Arc<CodeIndexPublishedGenerationV1> {
    let files = fixture_files(root);
    let changed = files.iter().map(|(path, _)| path.clone()).collect();
    cold_generation(
        &CodeIndexProductionOwnerV1::new(
            config(),
            SharedPublicationStore::default(),
            ApplyingProjectionSink,
        )
        .expect("production owner")
        .build_and_publish(
            fixture_tree_request(tag, 1, &files, &changed),
            &ActiveControl,
        )
        .expect("fixture generation publishes"),
    )
}

/// A build request for `files` in `round`, with identities derived from `tag`
/// and each file's position. A file in `changed` past round 1 gets a fresh
/// occurrence, as an edit does.
pub(crate) fn fixture_tree_request(
    tag: &str,
    round: u32,
    files: &[(String, String)],
    changed: &BTreeSet<String>,
) -> CodeIndexBuildRequestV1 {
    let mut request = request_with_source(
        &format!("file.{tag}.seed"),
        1_600_000 + i64::from(round) * 100_000,
        &format!("commit.{tag}.{round}"),
        &format!("tree.{tag}.{round}"),
        "",
    );
    request.snapshot.files.clear();
    request.snapshot.sanitization_receipts.clear();
    request.captured_files.clear();
    request.changed_files.clone_from(changed);
    let mut identity = Vec::new();
    for (ordinal, (path, source)) in files.iter().enumerate() {
        let suffix = if round > 1 && changed.contains(path) {
            format!("{ordinal:02}.{round}")
        } else {
            format!("{ordinal:02}")
        };
        let file_occurrence_id = id::<FileOccurrenceId>(&format!("file.{tag}.{suffix}"));
        let bytes = source.as_bytes();
        request.snapshot.files.push(SanitizedCodeFileV1 {
            file_occurrence_id: file_occurrence_id.clone(),
            logical_path: path.clone(),
            language: Some(id::<LanguageId>(language_for(path))),
            content_digest: content_digest(bytes),
            disposition: SnapshotFileDispositionV1::Present,
        });
        request
            .snapshot
            .sanitization_receipts
            .push(id::<SanitizationReceiptId>(&format!(
                "receipt.{tag}.{suffix}"
            )));
        request.captured_files.push(CodeIndexCapturedFileV1 {
            file_occurrence_id,
            sanitized_bytes: Arc::from(bytes),
            sensitivity_level: SensitivityLevelV1::Public,
        });
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
        .expect("fixture snapshot is canonical");
    request
}

fn published(language: &str) -> Arc<CodeIndexPublishedGenerationV1> {
    publish_fixture_tree(
        &Path::new(FIXTURE_ROOT).join(language),
        &format!("calls-{language}"),
    )
}

struct CallGraphV1 {
    generation: Arc<CodeIndexPublishedGenerationV1>,
    reader: CodeGraphInteractiveReader,
    roles: BTreeMap<&'static str, &'static str>,
}

impl CallGraphV1 {
    fn new(language: &str, roles: BTreeMap<&'static str, &'static str>) -> Self {
        let generation = published(language);
        let reader = reader(&generation);
        Self {
            generation,
            reader,
            roles,
        }
    }

    fn occurrence(&self, role: &str) -> SymbolOccurrenceId {
        let qualified_name = self.roles[role];
        self.generation
            .symbols()
            .symbols
            .iter()
            // A Go `package main` shares `main.go::main` with `func main`.
            .find(|symbol| symbol.qualified_name == qualified_name && symbol.kind != "go_package")
            .unwrap_or_else(|| panic!("missing symbol {qualified_name}"))
            .occurrence
            .clone()
    }

    /// Every call edge between two of the graph's sixteen functions, by role.
    fn calls(&self) -> BTreeSet<(&'static str, &'static str)> {
        let roles = self
            .roles
            .keys()
            .map(|role| (self.occurrence(role), *role))
            .collect::<BTreeMap<_, _>>();
        self.generation
            .edges()
            .iter()
            .filter(|edge| edge.kind == RelationEdgeKindV1::Calls)
            .filter_map(|edge| {
                Some((
                    *roles.get(&edge.from_occurrence)?,
                    *roles.get(&edge.to_occurrence)?,
                ))
            })
            .collect()
    }

    /// Whether `callers(role)` discloses call sites it could not bind.
    fn callers_partial(&self, role: &str) -> bool {
        self.reader
            .has_unresolved_callers(&[self.occurrence(role)], None, Arc::new(NeverCancelled))
            .expect("unresolved caller probe")
    }

    /// The other files whose calls reach symbols of `file`, and whether
    /// `file_dependents` discloses unbound call sites for it.
    fn file_dependents(&self, file: &str) -> (BTreeSet<String>, bool) {
        let seeds = self
            .reader
            .symbols_in_logical_file(file, 1_000, Arc::new(NeverCancelled))
            .expect("file symbols")
            .into_iter()
            .map(|symbol| symbol.occurrence)
            .collect::<Vec<_>>();
        assert!(!seeds.is_empty(), "{file} publishes symbols");
        let unresolved = self
            .reader
            .has_unresolved_callers(&seeds, None, Arc::new(NeverCancelled))
            .expect("unresolved caller probe");
        let dependents = self
            .reader
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
}

fn bindable_calls() -> BTreeSet<(&'static str, &'static str)> {
    CALLS
        .into_iter()
        .filter(|call| !RECEIVER_CALLS.contains(call))
        .collect()
}

/// Every import-bound call binds, nothing else does, and coverage is
/// complete exactly where every call site bound: the receiver calls leave
/// `add` and `get` partial while `normalize` and `clamp`, called from five
/// files through five import forms, report complete.
fn assert_import_language_graph(graph: &CallGraphV1, util_file: &str, dependents: &[&str]) {
    assert_eq!(graph.calls(), bindable_calls());
    for role in ["add", "get"] {
        assert!(
            graph.callers_partial(role),
            "callers({role}) discloses main"
        );
    }
    for role in ["normalize", "clamp", "legacy_normalize", "total", "shim"] {
        assert!(!graph.callers_partial(role), "callers({role}) is complete");
    }
    assert_eq!(
        graph.file_dependents(util_file),
        (
            dependents.iter().copied().map(str::to_owned).collect(),
            false
        )
    );
}

#[test]
fn python_calls_bind_through_from_module_alias_and_relative_imports() {
    let graph = CallGraphV1::new("python", python());
    assert_import_language_graph(
        &graph,
        "app/util.py",
        &[
            "app/compat.py",
            "app/math.py",
            "app/report.py",
            "app/shapes.py",
            "app/store.py",
        ],
    );
}

#[test]
fn go_calls_bind_through_go_mod_import_paths_and_the_same_package() {
    let graph = CallGraphV1::new("go", go());
    assert_import_language_graph(
        &graph,
        "util/util.go",
        &[
            "compat/compat.go",
            "mathx/stats.go",
            "report/report.go",
            "shapes/shapes.go",
            "store/store.go",
        ],
    );
}

#[test]
fn java_calls_bind_through_class_static_glob_and_same_package_imports() {
    let graph = CallGraphV1::new("java", java());
    assert_import_language_graph(
        &graph,
        "src/app/util/Util.java",
        &[
            "src/app/compat/Compat.java",
            "src/app/math/MathOps.java",
            "src/app/report/Report.java",
            "src/app/shapes/Shapes.java",
            "src/app/store/Store.java",
        ],
    );
}

#[test]
fn ruby_constant_calls_bind_through_require_relative_chains() {
    let graph = CallGraphV1::new("ruby", ruby());
    assert_import_language_graph(
        &graph,
        "lib/util.rb",
        &[
            "lib/compat.rb",
            "lib/math_ops.rb",
            "lib/nested.rb",
            "lib/report.rb",
            "lib/shapes.rb",
            "lib/store.rb",
        ],
    );
    // Inside `module App`, `Tools` names `App::Tools` and `::Util` the
    // top-level module.
    let nested = CallGraphV1::new(
        "ruby",
        BTreeMap::from([
            ("run", "lib/nested.rb::App::Runner::run"),
            ("helper", "lib/nested.rb::App::Tools::helper"),
            ("normalize", "lib/util.rb::Util::normalize"),
        ]),
    );
    assert_eq!(
        nested.calls(),
        BTreeSet::from([("run", "helper"), ("run", "normalize")])
    );
}

/// Rust's `main.rs` calls through a grouped crate-name
/// `use fixture::{compat, legacy, report, shapes}` inside `println!`
/// arguments, which bind like the same calls outside a macro.
#[test]
fn typescript_and_rust_graphs_keep_their_bindings() {
    for (language, symbols) in [("typescript", typescript()), ("rust", rust())] {
        let graph = CallGraphV1::new(language, symbols);
        assert_eq!(graph.calls(), bindable_calls(), "{language}");
    }
}

/// A Java call binds the one overload whose parameters accept its argument
/// count, within the file and across files; overloads the count cannot tell
/// apart (equal arity, a variadic tail) leave the call a disclosed gap.
#[test]
fn java_calls_bind_the_overload_their_argument_count_selects() {
    let generation = publish_fixture_tree(
        &Path::new(FIXTURE_ROOT).join("../java-overloads"),
        "java-overloads",
    );
    let symbols = &generation.symbols().symbols;
    let at = |name: &str, line: u32| {
        symbols
            .iter()
            .find(|symbol| symbol.qualified_name.ends_with(name) && symbol.start_line == line)
            .unwrap_or_else(|| panic!("missing {name} at line {line}"))
    };
    let roles = [
        ("run", at("Caller::run", 3)),
        ("notNull(obj)", at("Validate::notNull", 3)),
        ("notNull(obj, msg)", at("Validate::notNull", 7)),
        ("check(int)", at("Validate::check", 13)),
        ("check(String)", at("Validate::check", 15)),
        ("format(pattern, args...)", at("Validate::format", 17)),
        ("format(pattern, arg)", at("Validate::format", 21)),
        ("selfCheck", at("Validate::selfCheck", 25)),
    ];
    let role = |occurrence: &SymbolOccurrenceId| {
        roles
            .iter()
            .find(|(_, symbol)| &symbol.occurrence == occurrence)
            .map(|(role, _)| *role)
    };
    let calls = generation
        .edges()
        .iter()
        .filter(|edge| edge.kind == RelationEdgeKindV1::Calls)
        .filter_map(|edge| Some((role(&edge.from_occurrence)?, role(&edge.to_occurrence)?)))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        calls,
        BTreeSet::from([
            ("notNull(obj)", "notNull(obj, msg)"),
            ("run", "notNull(obj)"),
            ("run", "notNull(obj, msg)"),
            ("run", "format(pattern, args...)"),
        ])
    );
    assert_eq!(
        disclosed_gaps(&generation, "src/app/Caller.java"),
        BTreeSet::from(["Validate.check".to_owned(), "Validate.format".to_owned()])
    );
    assert_eq!(
        disclosed_gaps(&generation, "src/app/Validate.java"),
        BTreeSet::from(["check".to_owned()])
    );
    let reader = reader(&generation);
    for (name, partial) in [
        ("notNull(obj)", false),
        ("notNull(obj, msg)", false),
        ("check(int)", true),
        ("check(String)", true),
        ("format(pattern, arg)", true),
    ] {
        let symbol = roles
            .iter()
            .find(|(role, _)| *role == name)
            .expect("role")
            .1;
        assert_eq!(
            reader
                .has_unresolved_callers(
                    std::slice::from_ref(&symbol.occurrence),
                    None,
                    Arc::new(NeverCancelled)
                )
                .expect("unresolved caller probe"),
            partial,
            "callers({name}) discloses a gap"
        );
    }
}

/// The retained call sites the seal discloses as gaps inside `file`.
fn disclosed_gaps(generation: &CodeIndexPublishedGenerationV1, file: &str) -> BTreeSet<String> {
    let prefix = format!("{file}::");
    let in_file = generation
        .symbols()
        .symbols
        .iter()
        .filter(|symbol| symbol.qualified_name.starts_with(&prefix))
        .map(|symbol| symbol.occurrence.clone())
        .collect::<BTreeSet<_>>();
    generation
        .unresolved_import_calls()
        .into_iter()
        .filter(|gap| in_file.contains(&gap.from_occurrence))
        .map(|gap| gap.reference_name)
        .collect()
}

/// A binding into project code that names no definition is a disclosed gap
/// (a missing project module, a module or class without the member, a
/// project constant the file never loads); a call into an external module
/// (`os.path`, `strings`, `java.util.List`, `JSON`) is none.
#[test]
fn unbindable_project_calls_are_disclosed_and_external_calls_are_not() {
    for (language, file, expected) in [
        ("python", "app/gaps.py", vec!["absent", "vanish"]),
        ("go", "gaps/gaps.go", vec!["missing.Vanish", "util.Absent"]),
        ("java", "src/app/gaps/Gaps.java", vec!["Util.absent"]),
        ("ruby", "lib/gaps.rb", vec!["Util.absent"]),
    ] {
        assert_eq!(
            disclosed_gaps(&published(language), file),
            expected.into_iter().map(str::to_owned).collect(),
            "{language}"
        );
    }
}
