//! TypeScript projects whose `node_modules/.bin/tsc` runs the pinned
//! typescript@7 Bazel installs for tests.
//!
//! The launcher is the kind of wrapper a workspace installs: it records how
//! it was run, may hold a check until the test releases it, and then runs the
//! real compiler. Every finding a read returns is that compiler's.

use std::fs;
use std::path::{Path, PathBuf};

use tracedecay_runtime_core::test_executable::write_executable_script;

/// Where the fixture launcher records each run: one line with the working
/// directory and the arguments it received.
pub const TYPESCRIPT_FIXTURE_TSC_INVOCATIONS: &str = "node_modules/tsc-invocations.log";

/// The file whose creation lets a [`TypeScriptFixtureCompiler::Held`]
/// compiler start its check.
pub const TYPESCRIPT_FIXTURE_TSC_RELEASE: &str = "node_modules/tsc-release";

/// The package file the monorepo fixture's `TS4023` is reported on.
pub const TYPESCRIPT_MONOREPO_APP_FILE: &str = "packages/app/src/index.ts";

/// A TypeScript file in the monorepo fixture that no tsconfig owns.
pub const TYPESCRIPT_MONOREPO_UNOWNED_FILE: &str = "scripts/release.ts";

/// Whether the TypeScript fixture project carries its own compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeScriptFixtureCompiler {
    /// `node_modules/.bin/tsc` runs the pinned compiler.
    Present,
    /// No `node_modules` at all: a checkout before `npm install`.
    Missing,
    /// Like `Present`, but each check waits for
    /// [`TYPESCRIPT_FIXTURE_TSC_RELEASE`] before the compiler starts, so the
    /// producer stays pending until the test lets it publish.
    Held,
}

/// Sources that genuinely produce `TS4023` under `declaration: true`:
/// `index.ts` exports a value typed by an interface `dep.ts` does not export.
const HIDDEN_DEPENDENCY: &str = "interface Hidden {\n  a: number;\n}\n\nexport function make(): Hidden {\n  return { a: 1 };\n}\n";
const HIDDEN_EXPORT: &str = "import { make } from \"./dep\";\n\nexport const value = make();\n";

/// A small TypeScript project whose own compiler reports `TS4023` on
/// `src/index.ts` line 3 column 14, where `value` is declared.
pub fn write_typescript_diagnostics_fixture(project: &Path, compiler: TypeScriptFixtureCompiler) {
    write_files(
        project,
        &[
            (
                "package.json",
                "{\n  \"name\": \"diagnostics-fixture\",\n  \"private\": true\n}\n",
            ),
            (
                "tsconfig.json",
                "{\n  \"compilerOptions\": {\n    \"strict\": true,\n    \"declaration\": true,\n    \"module\": \"es2022\",\n    \"target\": \"es2022\"\n  },\n  \"include\": [\"src\"]\n}\n",
            ),
            (".gitignore", "node_modules/\n"),
            ("src/dep.ts", HIDDEN_DEPENDENCY),
            ("src/index.ts", HIDDEN_EXPORT),
        ],
    );
    install_compiler(project, compiler);
}

/// The issue #2025 layout: a pnpm workspace whose packages each carry a
/// `tsconfig.json` extending a root `tsconfig.base.json`, with no root
/// `tsconfig.json`. `packages/app` has the same genuine `TS4023` sources as
/// [`write_typescript_diagnostics_fixture`]; `packages/lib` is clean. The
/// compiler is the workspace root's, as pnpm hoists a root dev dependency's
/// binary.
pub fn write_typescript_monorepo_diagnostics_fixture(
    project: &Path,
    compiler: TypeScriptFixtureCompiler,
) {
    write_files(
        project,
        &[
            (
                "package.json",
                "{\n  \"name\": \"monorepo-fixture\",\n  \"private\": true,\n  \"devDependencies\": { \"typescript\": \"7.0.2\" }\n}\n",
            ),
            ("pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n"),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            (".gitignore", "node_modules/\n"),
            (
                "tsconfig.base.json",
                "{\n  // shared by every package\n  \"compilerOptions\": {\n    \"strict\": true,\n    \"declaration\": true,\n    \"module\": \"es2022\",\n    \"target\": \"es2022\",\n  },\n}\n",
            ),
            (
                "packages/app/package.json",
                "{ \"name\": \"@fixture/app\", \"private\": true }\n",
            ),
            (
                "packages/app/tsconfig.json",
                "{ \"extends\": \"../../tsconfig.base.json\", \"include\": [\"src\"] }\n",
            ),
            ("packages/app/src/dep.ts", HIDDEN_DEPENDENCY),
            (TYPESCRIPT_MONOREPO_APP_FILE, HIDDEN_EXPORT),
            (
                "packages/lib/package.json",
                "{ \"name\": \"@fixture/lib\", \"private\": true }\n",
            ),
            (
                "packages/lib/tsconfig.json",
                "{ \"extends\": \"../../tsconfig.base.json\", \"include\": [\"src\"] }\n",
            ),
            (
                "packages/lib/src/lib.ts",
                "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
            ),
            (
                TYPESCRIPT_MONOREPO_UNOWNED_FILE,
                "export const release = \"v1\";\n",
            ),
        ],
    );
    install_compiler(project, compiler);
}

fn write_files(project: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let path = project.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

fn install_compiler(project: &Path, compiler: TypeScriptFixtureCompiler) {
    if compiler == TypeScriptFixtureCompiler::Missing {
        return;
    }
    let pinned = PathBuf::from(
        std::env::var_os("TRACEDECAY_TEST_TYPESCRIPT_COMPILER")
            .expect("set TRACEDECAY_TEST_TYPESCRIPT_COMPILER to the installed pinned tsc"),
    )
    .canonicalize()
    .expect("installed pinned tsc");
    let hold = if compiler == TypeScriptFixtureCompiler::Held {
        format!(
            "case \" $* \" in *\" --noEmit \"*)\n  while [ ! -e \"{}\" ]; do sleep 0.1; done ;;\nesac\n",
            project.join(TYPESCRIPT_FIXTURE_TSC_RELEASE).display()
        )
    } else {
        String::new()
    };
    let bin = project.join("node_modules/.bin");
    fs::create_dir_all(&bin).unwrap();
    write_executable_script(
        &bin.join("tsc"),
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$(pwd)\" \"$*\" >> \"{}\"\n{hold}exec \"{}\" \"$@\"\n",
            project.join(TYPESCRIPT_FIXTURE_TSC_INVOCATIONS).display(),
            pinned.display()
        ),
    )
    .unwrap();
}

/// The build-info file a logged run names when it is the producer's exact
/// incremental check of `tsconfig` from `root`; `None` for a run of any
/// other shape or tsconfig.
pub fn typescript_fixture_build_info(
    invocation: &str,
    root: &Path,
    tsconfig: &str,
) -> Option<PathBuf> {
    let root = root.display();
    let build_info = PathBuf::from(invocation.strip_prefix(&format!(
        "{root} -p {root}/{tsconfig} --noEmit --pretty false --incremental --tsBuildInfoFile "
    ))?);
    (build_info.extension() == Some("tsbuildinfo".as_ref())
        && build_info.parent().and_then(Path::file_name) == Some("typescript-build-info".as_ref()))
    .then_some(build_info)
}

/// Whether a logged run is the producer asking the compiler, from `root`, how
/// it resolves `tsconfig`.
pub fn is_typescript_fixture_configuration_query(
    invocation: &str,
    root: &Path,
    tsconfig: &str,
) -> bool {
    let root = root.display();
    invocation == format!("{root} -p {root}/{tsconfig} --showConfig")
}
