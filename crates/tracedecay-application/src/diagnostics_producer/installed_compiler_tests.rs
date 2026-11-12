//! The pinned compilers installed the way pnpm installs them, and the
//! producer against one when that installed compiler changes without a
//! version change.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use tracedecay_domain::UtcMicros;
use tracedecay_domain::test_fixtures::id;
use tracedecay_lsp::{TypeScriptCompilerAdmission, typescript_projects};
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;
use tracedecay_runtime_core::db::engine::TestConnection;
use tracedecay_runtime_core::test_executable::write_executable_script;

use super::{
    CompilerProducerRunV1, CompilerReadBudget, TypeScriptProducerStateV1,
    TypeScriptProducerStatusHandleV1, hash_package_tree, run_typescript_producer_v1,
};
use crate::diagnostics_publication::CodeIndexPublicationIdentityV1;
use crate::diagnostics_store::DiagnosticsStore;

const SOURCE: &str =
    "const text: string = \"abc\";\nexport const size: number = text.tracedecayLength;\n";
const MISSING_PROPERTY: &str = "Property 'tracedecayLength' does not exist on type 'string'.";
/// A same-version patch to the compiler's own default library declaring the
/// property the source reads.
const LIBRARY_PATCH: &str = "\ninterface String { readonly tracedecayLength: number; }\n";

/// What pnpm writes as `node_modules/.bin/tsc`: run the package's `bin.tsc`
/// entry with the `node` installed beside the shim when there is one.
const PNPM_SHIM: &str = "#!/bin/sh\n\
    basedir=$(dirname \"$0\")\n\
    if [ -x \"$basedir/node\" ]; then\n\
      exec \"$basedir/node\" \"$basedir/../typescript/bin/tsc\" \"$@\"\n\
    fi\n\
    exec node \"$basedir/../typescript/bin/tsc\" \"$@\"\n";

/// A maintained TypeScript release Bazel installs for these tests.
#[derive(Clone, Copy, Debug)]
pub(super) enum PinnedTypeScript {
    /// typescript@7: a launcher for the native compiler in a platform
    /// package it depends on.
    Seven,
    /// typescript@5: the compiler as JavaScript in the package itself.
    Five,
}

impl PinnedTypeScript {
    fn major(self) -> &'static str {
        match self {
            Self::Seven => "7.",
            Self::Five => "5.",
        }
    }

    fn launcher(self) -> PathBuf {
        let launcher = match self {
            Self::Seven => std::env::var_os("TRACEDECAY_TEST_TYPESCRIPT_COMPILER")
                .expect("set TRACEDECAY_TEST_TYPESCRIPT_COMPILER to the installed pinned tsc"),
            Self::Five => std::env::var_os("TRACEDECAY_TEST_TYPESCRIPT5_COMPILER")
                .expect("set TRACEDECAY_TEST_TYPESCRIPT5_COMPILER to the installed pinned tsc"),
        };
        PathBuf::from(launcher)
            .canonicalize()
            .expect("installed pinned tsc")
    }
}

/// Node, the `typescript` package and each declared dependency installed for
/// it, as the pinned compiler's own Node resolves them.
struct PinnedCompiler {
    node: PathBuf,
    typescript: PathBuf,
    dependencies: Vec<(String, PathBuf)>,
}

impl PinnedCompiler {
    fn resolve(release: PinnedTypeScript) -> Self {
        const RESOLVE: &str = "--node_options=\
            const fs = require('fs'), path = require('path');\
            const typescript = path.dirname(path.dirname(fs.realpathSync(process.argv[1])));\
            const manifest = JSON.parse(fs.readFileSync(path.join(typescript, 'package.json')));\
            const names = Object.keys({ ...manifest.dependencies, ...manifest.optionalDependencies });\
            const dependencies = names.flatMap((name) => {\
              try {\
                return [[name, path.dirname(require.resolve(name + '/package.json', { paths: [typescript] }))]];\
              } catch { return []; }\
            });\
            JSON.stringify({ node: process.argv0, typescript, dependencies })";
        #[derive(serde::Deserialize)]
        struct Resolved {
            node: PathBuf,
            typescript: PathBuf,
            dependencies: Vec<(String, PathBuf)>,
        }

        let probe = tempfile::tempdir().expect("probe directory");
        let output = Command::new(release.launcher())
            .current_dir(probe.path())
            .args(["--node_options=--print", RESOLVE])
            .output()
            .expect("run the pinned compiler's Node");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let resolved: Resolved =
            serde_json::from_slice(&output.stdout).expect("resolved compiler installation");
        Self {
            node: resolved.node,
            typescript: resolved.typescript,
            dependencies: resolved.dependencies,
        }
    }
}

fn version(package: &Path) -> String {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(package.join("package.json")).unwrap()).unwrap();
    manifest["version"].as_str().unwrap().to_owned()
}

/// Copies a package as a package manager extracts it. The native compiler is
/// hard-linked where the filesystem allows, and every patch below replaces
/// files rather than writing through a link.
fn install(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let metadata = std::fs::metadata(&source).unwrap();
        if metadata.is_dir() {
            install(&source, &target);
        } else if metadata.len() > 1024 * 1024 {
            let source = source.canonicalize().unwrap();
            if std::fs::hard_link(&source, &target).is_err() {
                std::fs::copy(&source, &target).unwrap();
            }
        } else {
            std::fs::copy(&source, &target).unwrap();
        }
    }
}

fn patch(file: &Path, appended: &str) {
    let mut text = std::fs::read_to_string(file).unwrap();
    text.push_str(appended);
    std::fs::remove_file(file).unwrap();
    std::fs::write(file, text).unwrap();
}

/// One TypeScript project whose `node_modules` holds the pinned compiler in
/// pnpm's layout: the packages in its virtual store, dependencies linked
/// beside the package that declares them, and the `.bin` shim.
pub(super) struct InstalledCompilerProject {
    _temp: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) typescript: PathBuf,
    pub(super) build_info_dir: PathBuf,
    connection: TestConnection,
}

impl InstalledCompilerProject {
    fn new() -> Self {
        Self::with_files(&[
            ("tsconfig.json", r#"{"include":["src/**/*.ts"]}"#),
            ("src/index.ts", SOURCE),
            ("README.md", "# first\n"),
        ])
    }

    /// A project of `files` (relative path, contents) beside the installed
    /// pinned typescript@7.
    pub(super) fn with_files(files: &[(&str, &str)]) -> Self {
        Self::installing(PinnedTypeScript::Seven, files)
    }

    /// A project of `files` (relative path, contents) beside `release`.
    pub(super) fn installing(release: PinnedTypeScript, files: &[(&str, &str)]) -> Self {
        let pinned = PinnedCompiler::resolve(release);
        assert!(
            version(&pinned.typescript).starts_with(release.major()),
            "{release:?} resolved typescript@{}",
            version(&pinned.typescript)
        );
        let temp = tempfile::tempdir().expect("isolated compiler project");
        let root = temp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        for (path, contents) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        let store = root.join("node_modules/.pnpm");
        let linked = store.join(format!(
            "typescript@{}/node_modules",
            version(&pinned.typescript)
        ));
        let typescript = linked.join("typescript");
        install(&pinned.typescript, &typescript);
        for (name, package) in &pinned.dependencies {
            let installed = store
                .join(format!("{}@{}", name.replace('/', "+"), version(package)))
                .join("node_modules")
                .join(name);
            install(package, &installed);
            let link = linked.join(name);
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&installed, link).unwrap();
        }
        std::os::unix::fs::symlink(&typescript, root.join("node_modules/typescript")).unwrap();
        let bin = root.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&pinned.node, bin.join("node")).unwrap();
        write_executable_script(&bin.join("tsc"), PNPM_SHIM).unwrap();
        Self {
            build_info_dir: temp.path().join("store/typescript-build-info"),
            connection: TestConnection::open(&temp.path().join("diagnostics.db")),
            _temp: temp,
            root,
            typescript,
        }
    }

    /// Where the installed compiler reads one of its default libraries from,
    /// as it lists its own program.
    fn default_library(&self, name: &str) -> PathBuf {
        let output = Command::new(self.root.join("node_modules/.bin/tsc"))
            .current_dir(&self.root)
            .args(["-p", "tsconfig.json", "--listFilesOnly"])
            .output()
            .expect("list the compiler's program");
        let listed = String::from_utf8(output.stdout).unwrap();
        let library = listed
            .lines()
            .map(Path::new)
            .find(|file| file.file_name() == Some(name.as_ref()))
            .unwrap_or_else(|| panic!("{name} is not in the program:\n{listed}"))
            .canonicalize()
            .unwrap();
        assert!(
            library.starts_with(self.root.join("node_modules/.pnpm")),
            "{} is not part of the installed compiler",
            library.display()
        );
        library
    }

    pub(super) fn edit_readme(&self, text: &str) {
        std::fs::write(self.root.join("README.md"), text).unwrap();
    }

    /// The installed launcher, which [`Self::log_compiler_runs`] wraps.
    pub(super) fn launcher(&self) -> PathBuf {
        self.root.join("node_modules/.bin/tsc")
    }

    /// Makes the installed launcher record the arguments of every run before
    /// it starts the compiler, as a user's wrapper script would.
    pub(super) fn log_compiler_runs(&self) {
        let shim = std::fs::read_to_string(self.launcher()).unwrap();
        let logged = shim.replacen(
            "#!/bin/sh\n",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
                self.compiler_log().display()
            ),
            1,
        );
        std::fs::remove_file(self.launcher()).unwrap();
        write_executable_script(&self.launcher(), logged).unwrap();
    }

    fn compiler_log(&self) -> PathBuf {
        self._temp.path().join("tsc.log")
    }

    /// The tsconfigs, relative to the root, the compiler checked since the
    /// last call; configuration and build-state queries are not checks.
    pub(super) fn checked(&self) -> Vec<String> {
        let log = std::fs::read_to_string(self.compiler_log()).unwrap_or_default();
        std::fs::write(self.compiler_log(), "").unwrap();
        let mut checked = log
            .lines()
            .filter(|run| run.split(' ').any(|arg| arg == "--noEmit"))
            .map(|run| {
                let mut args = run.split(' ');
                args.find(|arg| *arg == "-p").unwrap();
                Path::new(args.next().unwrap())
                    .strip_prefix(&self.root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        checked.sort();
        checked
    }

    /// Every compiler run since the last call to [`Self::checked`] or this.
    pub(super) fn runs(&self) -> usize {
        let log = std::fs::read_to_string(self.compiler_log()).unwrap_or_default();
        std::fs::write(self.compiler_log(), "").unwrap();
        log.lines().count()
    }

    /// The project's sources as the code index seals them: every file
    /// outside installed packages and the compiler's build outputs.
    pub(super) fn identity(&self, generation: &str) -> CodeIndexPublicationIdentityV1 {
        fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if !matches!(
                        path.file_name().and_then(|name| name.to_str()),
                        Some("node_modules" | "dist")
                    ) {
                        sources(&path, found);
                    }
                } else if path.extension() != Some("tsbuildinfo".as_ref()) {
                    found.push(path);
                }
            }
        }
        let mut paths = Vec::new();
        sources(&self.root, &mut paths);
        let files = paths
            .into_iter()
            .map(|file| {
                let path = file
                    .strip_prefix(&self.root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                let bytes = std::fs::read(&file).unwrap();
                (
                    path.clone(),
                    id(&format!("file.{}", path.replace('/', "."))),
                    tracedecay_code_index::intake::content_digest(&bytes),
                )
            })
            .collect::<Vec<_>>();
        CodeIndexPublicationIdentityV1::new(
            id(generation),
            id("repository.installed-compiler"),
            Some(id("worktree.installed-compiler")),
            Some(id("ref.main")),
            None,
            files,
        )
    }

    pub(super) async fn run(
        &self,
        generation: &str,
        state: &mut TypeScriptProducerStateV1,
    ) -> CompilerProducerRunV1 {
        run_typescript_producer_v1(
            &self.root,
            &typescript_projects(&self.root).expect("tsconfig discovery"),
            &self.identity(generation),
            &DiagnosticsStore::new_runtime(&self.connection),
            &self.build_info_dir,
            state,
            UtcMicros(1_700_000_000_000_000),
        )
        .await
    }

    pub(super) async fn findings(&self, generation: &str) -> Vec<(String, String)> {
        DiagnosticsStore::new_runtime(&self.connection)
            .current_records(&id(generation))
            .await
            .unwrap()
            .into_iter()
            .map(|record| (record.code, record.message))
            .collect()
    }

    /// Each published finding as the logical path it is on and its code.
    pub(super) async fn located_findings(&self, generation: &str) -> Vec<(String, String)> {
        let mut findings = DiagnosticsStore::new_runtime(&self.connection)
            .current_records(&id(generation))
            .await
            .unwrap()
            .into_iter()
            .map(|record| {
                let file = record.file_occurrence_id.as_str();
                (
                    file.strip_prefix("file.").unwrap_or(file).to_owned(),
                    record.code,
                )
            })
            .collect::<Vec<_>>();
        findings.sort();
        findings
    }
}

pub(super) fn producer_state() -> TypeScriptProducerStateV1 {
    TypeScriptProducerStateV1::new(
        TypeScriptProducerStatusHandleV1::default(),
        TypeScriptCompilerAdmission::new(Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(2).unwrap(),
        ))),
    )
}

fn published(generation: &str, inserted: u64, checked: usize) -> CompilerProducerRunV1 {
    CompilerProducerRunV1::Published {
        generation: id(generation),
        inserted,
        unresolved: 0,
        checked,
        reused: 1 - checked,
    }
}

fn missing_property() -> Vec<(String, String)> {
    vec![("TS2339".to_owned(), MISSING_PROPERTY.to_owned())]
}

/// The installed compiler lies outside the indexed sources. A same-version
/// change to it, its own default library or a file its launcher loads, is a
/// different compiler: the producer checks again, from scratch, though the
/// generation changed no source the project reads.
#[tokio::test]
async fn a_same_version_compiler_change_is_checked_afresh() {
    for release in [PinnedTypeScript::Seven, PinnedTypeScript::Five] {
        same_version_compiler_change(release).await;
    }
}

async fn same_version_compiler_change(release: PinnedTypeScript) {
    let project = InstalledCompilerProject::installing(
        release,
        &[
            ("tsconfig.json", r#"{"include":["src/**/*.ts"]}"#),
            ("src/index.ts", SOURCE),
            ("README.md", "# first\n"),
        ],
    );
    let mut state = producer_state();
    assert_eq!(
        project.run("generation.installed.1", &mut state).await,
        published("generation.installed.1", 1, 1)
    );
    assert_eq!(
        project.findings("generation.installed.1").await,
        missing_property()
    );

    project.edit_readme("# second\n");
    assert_eq!(
        project.run("generation.installed.2", &mut state).await,
        published("generation.installed.2", 1, 0),
        "an unchanged compiler and program reuse the check"
    );
    assert_eq!(
        project.findings("generation.installed.2").await,
        missing_property()
    );

    patch(&project.default_library("lib.es5.d.ts"), LIBRARY_PATCH);
    project.edit_readme("# third\n");
    assert_eq!(
        project.run("generation.installed.3", &mut state).await,
        published("generation.installed.3", 0, 1)
    );
    assert_eq!(
        project.findings("generation.installed.3").await,
        Vec::new(),
        "the patched compiler declares the property"
    );

    patch(&project.typescript.join("lib/tsc.js"), "\n// patched\n");
    project.edit_readme("# fourth\n");
    assert_eq!(
        project.run("generation.installed.4", &mut state).await,
        published("generation.installed.4", 0, 1),
        "a launcher implementation file is part of the compiler"
    );
}

/// The build-info outlives the producer. A restarted producer has no previous
/// result to compare, so only the revision recorded beside the build-info
/// keeps tsc from replaying what the replaced compiler cached; another
/// worktree's build-info in the same store directory is left alone.
#[tokio::test]
async fn a_restarted_producer_does_not_replay_a_replaced_compilers_build_info() {
    let project = InstalledCompilerProject::new();
    let mut state = producer_state();
    assert_eq!(
        project.run("generation.restart.1", &mut state).await,
        published("generation.restart.1", 1, 1)
    );
    drop(state);
    let peer = project.build_info_dir.join("peer.tsbuildinfo");
    std::fs::write(&peer, "{}").unwrap();
    std::fs::write(peer.with_extension("compiler-revision"), [7; 32]).unwrap();

    patch(&project.default_library("lib.es5.d.ts"), LIBRARY_PATCH);
    let mut restarted = producer_state();
    assert_eq!(
        project.run("generation.restart.2", &mut restarted).await,
        published("generation.restart.2", 0, 1)
    );
    assert_eq!(
        project.findings("generation.restart.2").await,
        Vec::new(),
        "the replaced compiler's cached finding must not be replayed"
    );
    assert_eq!(std::fs::read_to_string(&peer).unwrap(), "{}");
    assert_eq!(
        std::fs::read(peer.with_extension("compiler-revision")).unwrap(),
        [7; 32]
    );
}

#[test]
fn compiler_identity_refuses_an_oversized_directory_only_package() {
    let root = tempfile::tempdir().unwrap();
    for name in ["one", "two", "three"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    let mut budget = CompilerReadBudget {
        bytes: 1024,
        entries: 2,
        buffer: vec![0; 64],
    };
    assert!(
        hash_package_tree(
            &mut Sha256::new(),
            root.path(),
            Path::new(""),
            0,
            &mut budget
        )
        .is_none(),
        "directory entries consume the identity read budget even without payload files"
    );
}
