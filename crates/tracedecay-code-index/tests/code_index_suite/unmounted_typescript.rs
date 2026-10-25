//! Public unmounted-file audit journeys for TypeScript module evidence.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tracedecay_code_extraction::{LanguageExtractor, TypeScriptExtractor};
use tracedecay_code_index::unmounted_files::{EcosystemAudit, audit_project};
use tracedecay_domain::IndexPathPolicyV1;
use tracedecay_runtime_core::logging::set_tracing_capture;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("fixture parent"))
        .expect("create fixture directory");
    std::fs::write(path, contents).expect("write fixture");
}

fn audit_typescript(root: &Path) -> EcosystemAudit {
    let policy = IndexPathPolicyV1::new(Vec::new(), Vec::new()).expect("empty path policy");
    audit_project(root, &policy)
        .expect("project audit")
        .ecosystems
        .into_iter()
        .find(|audit| audit.ecosystem == "typescript")
        .expect("TypeScript ecosystem")
}

fn unmounted_paths(audit: &EcosystemAudit) -> Vec<&str> {
    audit
        .unmounted
        .iter()
        .map(|entry| entry.file.as_str())
        .collect()
}

#[derive(Default)]
struct ExtractionCalls {
    next_span: AtomicU64,
    parses: AtomicUsize,
    artifacts: AtomicUsize,
}

struct CountExtractionCalls(Arc<ExtractionCalls>);

impl tracing::Subscriber for CountExtractionCalls {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        matches!(
            metadata.name(),
            "code_extraction.parse_file" | "code_extraction.traverse_file"
        )
    }

    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        match span.metadata().name() {
            "code_extraction.parse_file" => {
                self.0.parses.fetch_add(1, Ordering::Relaxed);
            }
            "code_extraction.traverse_file" => {
                self.0.artifacts.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
        tracing::span::Id::from_u64(self.0.next_span.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {}

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[test]
fn import_walk_parses_each_source_once_without_building_graph_artifacts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write(root, "package.json", r#"{"name":"app"}"#);
    write(
        root,
        "rsbuild.config.ts",
        "export default { entry: './src/main.ts' };",
    );
    write(
        root,
        "src/main.ts",
        "import './used'; export const main = 1;",
    );
    write(root, "src/used.ts", "export const used = 1;");
    write(root, "src/orphan.ts", "export const orphan = 1;");

    let calls = Arc::new(ExtractionCalls::default());
    let _capture = set_tracing_capture(CountExtractionCalls(Arc::clone(&calls)));
    let control = TypeScriptExtractor.extract_artifact(
        "control.ts",
        "import './used'; export function fullGraph() { return 1; }",
    );
    assert!(control.result.errors.is_empty());
    assert!(!control.result.nodes.is_empty());
    assert_eq!(control.imports[0].module_specifier, "./used");
    assert_eq!(calls.parses.swap(0, Ordering::Relaxed), 1);
    assert_eq!(calls.artifacts.swap(0, Ordering::Relaxed), 1);

    let audit = audit_typescript(root);
    assert_eq!(audit.status.as_str(), "audited", "{:?}", audit.note);
    assert_eq!(audit.entry_point_count, 2);
    assert_eq!(audit.mounted_file_count, 3);
    assert_eq!(unmounted_paths(&audit), vec!["src/orphan.ts"]);
    assert_eq!(
        calls.parses.load(Ordering::Relaxed),
        audit.mounted_file_count,
        "each visited source, including the config, has one parser call"
    );
    assert_eq!(
        calls.artifacts.load(Ordering::Relaxed),
        0,
        "reachability must not construct graph artifacts"
    );
}

#[test]
fn module_forms_mount_across_typescript_and_javascript_suffixes() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write(
        root,
        "package.json",
        r#"{"name":"app","main":"src/main.ts"}"#,
    );
    write(
        root,
        "src/main.ts",
        concat!(
            "import { named } from './esm.mts';\n",
            "export { renamed } from './reexported.cts';\n",
            "import legacy = require('./equals.cjs');\n",
            "const required = require(/* the dependency */ './required.js');\n",
            "export const lazy = () => import(/* the chunk */ './dynamic.mjs');\n",
            "export const view = () => import(`./view.tsx`);\n",
            "// import './comment_only';\n",
            "export { named, legacy, required };\n",
        ),
    );
    write(
        root,
        "src/esm.mts",
        "import './component.jsx'; export const named = 1;",
    );
    write(
        root,
        "src/reexported.cts",
        "export const renamed: number = 1;",
    );
    write(root, "src/equals.cjs", "module.exports = 1;");
    write(root, "src/required.js", "module.exports = 1;");
    write(root, "src/dynamic.mjs", "export const chunk = 1;");
    write(root, "src/view.tsx", "export const view = <section />;");
    write(
        root,
        "src/component.jsx",
        "export const component = <span />;",
    );
    write(root, "src/comment_only.ts", "export const unused = 1;");
    write(root, "src/orphan.ts", "export const unused = 1;");

    let audit = audit_typescript(root);
    assert_eq!(audit.status.as_str(), "audited", "{:?}", audit.note);
    assert_eq!(audit.mounted_file_count, 8);
    assert_eq!(
        unmounted_paths(&audit),
        vec!["src/comment_only.ts", "src/orphan.ts"]
    );
}

#[test]
fn escaped_module_literals_resolve_to_their_decoded_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write(
        root,
        "package.json",
        r#"{"name":"app","main":"src/main.ts"}"#,
    );
    write(
        root,
        "src/main.ts",
        r#"
import './es\x63aped';
export { marker } from './\u0063afé';
import legacy = require('./quo\'ted');
const rocket = require('./\uD83D\uDE80');
export const lazy = () => import(`./\u{1F680}-lazy`);
import './line\
continued';
export { legacy, rocket };
"#,
    );
    for path in [
        "src/escaped.ts",
        "src/café.ts",
        "src/quo'ted.ts",
        "src/🚀.ts",
        "src/🚀-lazy.ts",
        "src/linecontinued.ts",
    ] {
        write(root, path, "export const marker = 1;");
    }
    write(root, "src/orphan.ts", "export const unused = 1;");

    let audit = audit_typescript(root);
    assert_eq!(audit.status.as_str(), "audited", "{:?}", audit.note);
    assert_eq!(audit.mounted_file_count, 7);
    assert_eq!(unmounted_paths(&audit), vec!["src/orphan.ts"]);
}

#[test]
fn undecodable_module_literals_withhold_findings_and_preserve_known_mounts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write(
        root,
        "package.json",
        r#"{"name":"app","main":"src/main.ts"}"#,
    );
    write(
        root,
        "src/main.ts",
        r#"import './known'; export const lazy = () => import('./\uD800');"#,
    );
    write(root, "src/known.ts", "export const known = 1;");
    write(root, "src/orphan.ts", "export const orphan = 1;");

    let audit = audit_typescript(root);
    assert_eq!(audit.status.as_str(), "partial");
    assert_eq!(audit.mounted_file_count, 2);
    assert!(audit.unmounted.is_empty());
    let note = audit.note.expect("literal decoding omission");
    assert!(note.contains("src/main.ts"), "{note}");
    assert!(!note.contains(&root.display().to_string()), "{note}");
}

#[test]
fn computed_templates_remain_named_blind_spots_without_fabricated_mounts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write(
        root,
        "package.json",
        r#"{"name":"app","main":"src/main.ts"}"#,
    );
    write(
        root,
        "src/main.ts",
        r#"
export const literal = () => import(`./static`);
export const computed = (name: string) => import(`./${name}`);
export const required = (name: string) => require(`./${name}`);
const moduleName = './computed';
export const indirect = () => import(moduleName);
const data = "import './data_only'";
// require('./comment_only');
throw new Error('auditing source must not execute it');
"#,
    );
    for path in [
        "src/static.ts",
        "src/${name}.ts",
        "src/computed.ts",
        "src/data_only.ts",
        "src/comment_only.ts",
    ] {
        write(root, path, "export const marker = 1;");
    }

    let audit = audit_typescript(root);
    assert_eq!(audit.status.as_str(), "audited", "{:?}", audit.note);
    assert_eq!(audit.mounted_file_count, 2);
    assert_eq!(
        unmounted_paths(&audit),
        vec![
            "src/${name}.ts",
            "src/comment_only.ts",
            "src/computed.ts",
            "src/data_only.ts",
        ]
    );
    assert!(
        audit
            .blind_spots
            .iter()
            .any(|spot| { spot.contains("import(expr)") && spot.contains("require(expr)") })
    );
}

#[test]
fn escaped_config_entry_literals_are_decoded_without_executing_config() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write(root, "package.json", r#"{"name":"app"}"#);
    write(
        root,
        "vite.config.mjs",
        r#"
export default { input: './src/\u0061pp/main.tsx' };
throw new Error('auditing config must not execute it');
"#,
    );
    write(
        root,
        "src/app/main.tsx",
        "import './boot'; export const app = <main />;",
    );
    write(root, "src/app/boot.ts", "export const boot = 1;");
    write(root, "src/app/orphan.ts", "export const orphan = 1;");

    let audit = audit_typescript(root);
    assert_eq!(audit.status.as_str(), "audited", "{:?}", audit.note);
    assert_eq!(audit.mounted_file_count, 3);
    assert_eq!(unmounted_paths(&audit), vec!["src/app/orphan.ts"]);
}

#[test]
fn config_entries_reach_other_packages_and_count_duplicate_paths_once() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    write(root, "app/package.json", r#"{"name":"app"}"#);
    write(
        root,
        "app/vite.config.mts",
        "export default { entries: ['../shared/src/feature.ts', '../shared/src/feature.ts'] };",
    );
    write(root, "app/src/orphan.ts", "export const orphan = 1;");
    write(root, "shared/package.json", r#"{"name":"shared"}"#);
    // This file is reached only through the config, not an index/main convention.
    write(root, "shared/src/feature.ts", "import './dependency';");
    write(
        root,
        "shared/src/dependency.ts",
        "export const dependency = 1;",
    );
    write(root, "shared/src/orphan.ts", "export const orphan = 1;");

    let audit = audit_typescript(root);
    assert_eq!(audit.status.as_str(), "audited", "{:?}", audit.note);
    assert_eq!(audit.package_count, 2);
    assert_eq!(audit.entry_point_count, 2);
    assert_eq!(audit.mounted_file_count, 3);
    assert_eq!(
        unmounted_paths(&audit),
        vec!["app/src/orphan.ts", "shared/src/orphan.ts"]
    );
}
