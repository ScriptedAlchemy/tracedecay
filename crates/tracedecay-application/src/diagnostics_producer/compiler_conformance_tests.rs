//! What the installed compiler, not a reading of its configuration, decides:
//! which project a file belongs to, when a project must be checked again, and
//! how a project that references others is checked. Each case runs against
//! both maintained compiler lines, whose reports and records differ.

use std::num::NonZeroUsize;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;

use super::installed_compiler_tests::{InstalledCompilerProject, PinnedTypeScript};
use super::{
    CompilerProducerRunV1, TypeScriptDiagnosticsAvailabilityV1, TypeScriptProducerStateV1,
    TypeScriptProducerStatusHandleV1, TypeScriptProjectCheckV1, test_compiler_admission,
    typescript_diagnostics_availability,
};

fn producer(status: &TypeScriptProducerStatusHandleV1) -> TypeScriptProducerStateV1 {
    TypeScriptProducerStateV1::new(
        status.clone(),
        test_compiler_admission(Arc::new(ProcessBackgroundCpuV1::new(
            NonZeroUsize::new(2).unwrap(),
        ))),
    )
}

fn owner(
    project: &InstalledCompilerProject,
    status: &TypeScriptProducerStatusHandleV1,
    file: &str,
) -> Option<(String, Option<TypeScriptProjectCheckV1>)> {
    let generation = status
        .current()
        .and_then(|status| status.generation.clone());
    match typescript_diagnostics_availability(
        &project.root,
        Some(Path::new(file)),
        status,
        generation.as_ref(),
    ) {
        TypeScriptDiagnosticsAvailabilityV1::Configured {
            tsconfig, check, ..
        } => Some((
            tsconfig
                .strip_prefix(&project.root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
            check,
        )),
        TypeScriptDiagnosticsAvailabilityV1::NoTsconfig { .. } => None,
        other => panic!("{file}: {other:?}"),
    }
}

fn codes(findings: &[(String, String)]) -> Vec<&str> {
    findings.iter().map(|(code, _)| code.as_str()).collect()
}

const RELEASES: [PinnedTypeScript; 2] = [PinnedTypeScript::Seven, PinnedTypeScript::Five];

/// A shared base names its sources with `${configDir}`, which the compiler
/// resolves against each extending project. The package's sources are that
/// project's, and a source added there is checked in the next generation.
#[tokio::test]
async fn a_config_dir_base_owns_and_admits_each_extending_projects_sources() {
    for release in RELEASES {
        config_dir_base(release).await;
    }
}

async fn config_dir_base(release: PinnedTypeScript) {
    let project = InstalledCompilerProject::installing(
        release,
        &[
            (
                "tsconfig.base.json",
                r#"{"compilerOptions":{"strict":true},"include":["${configDir}/src"]}"#,
            ),
            (
                "packages/app/tsconfig.json",
                r#"{"extends":"../../tsconfig.base.json"}"#,
            ),
            (
                "packages/app/src/index.ts",
                "export const value: number = 1;\n",
            ),
        ],
    );
    let status = TypeScriptProducerStatusHandleV1::default();
    let mut state = producer(&status);
    let run = project.run("generation.config-dir.1", &mut state).await;
    assert!(
        matches!(run, CompilerProducerRunV1::Published { checked: 1, .. }),
        "{release:?}: {run:?}"
    );
    assert_eq!(
        owner(&project, &status, "packages/app/src/index.ts"),
        Some((
            "packages/app/tsconfig.json".to_owned(),
            Some(TypeScriptProjectCheckV1::Checked)
        )),
        "{release:?}: the compiler checked the package source"
    );

    std::fs::write(
        project.root.join("packages/app/src/added.ts"),
        "export const added: string = 1;\n",
    )
    .unwrap();
    project.run("generation.config-dir.2", &mut state).await;
    assert_eq!(
        codes(&project.findings("generation.config-dir.2").await),
        ["TS2322"],
        "{release:?}: the compiler's own include admits the added source"
    );
}

/// The compiler's wildcards skip hidden directories and nested
/// `node_modules` even under an explicit `exclude`, and a file the program
/// imports is checked although no pattern names it.
#[tokio::test]
async fn membership_is_the_program_the_compiler_checked() {
    for release in RELEASES {
        program_membership(release).await;
    }
}

async fn program_membership(release: PinnedTypeScript) {
    let project = InstalledCompilerProject::installing(
        release,
        &[
            (
                "tsconfig.json",
                r#"{"include":["src"],"exclude":["src/generated"],"files":["tools/entry.ts"]}"#,
            ),
            ("src/index.ts", "export const value = 1;\n"),
            ("src/.cache/stale.ts", "export const stale: string = 1;\n"),
            (
                "src/vendor/node_modules/pkg/index.ts",
                "export const vendored: string = 1;\n",
            ),
            (
                "tools/entry.ts",
                "import { helper } from \"./helper\";\nhelper();\n",
            ),
            ("tools/helper.ts", "export function helper() {}\n"),
        ],
    );
    let status = TypeScriptProducerStatusHandleV1::default();
    let mut state = producer(&status);
    project.run("generation.membership.1", &mut state).await;
    assert_eq!(
        project.findings("generation.membership.1").await,
        Vec::new(),
        "{release:?}: the compiler never checked the hidden or vendored source"
    );
    for file in [
        "src/.cache/stale.ts",
        "src/vendor/node_modules/pkg/index.ts",
    ] {
        assert_eq!(owner(&project, &status, file), None, "{release:?}: {file}");
    }
    assert_eq!(
        owner(&project, &status, "tools/helper.ts"),
        Some((
            "tsconfig.json".to_owned(),
            Some(TypeScriptProjectCheckV1::Checked)
        )),
        "{release:?}: the program the compiler checked imports the helper"
    );
}

/// Package and relative bases participate in the compiler's multiple-extends
/// precedence. Their inherited JavaScript admission, include and exclude
/// decisions remain authoritative as the source roster changes.
#[tokio::test]
async fn package_and_multiple_extends_preserve_javascript_membership_and_reuse() {
    for release in RELEASES {
        package_and_multiple_extends(release).await;
    }
}

async fn package_and_multiple_extends(release: PinnedTypeScript) {
    let project = InstalledCompilerProject::installing(
        release,
        &[
            (
                "node_modules/@fixture/tsconfig/package.json",
                r#"{"name":"@fixture/tsconfig","version":"1.0.0","tsconfig":"base.json"}"#,
            ),
            (
                "node_modules/@fixture/tsconfig/base.json",
                r#"{"compilerOptions":{"allowJs":true,"checkJs":false}}"#,
            ),
            (
                "tsconfig.shared.json",
                r#"{"compilerOptions":{"checkJs":true,"outDir":"./src/generated"},"include":["src/**/*"],"exclude":["src/excluded","src/generated"]}"#,
            ),
            (
                "tsconfig.json",
                r#"{"extends":["@fixture/tsconfig","./tsconfig.shared.json"]}"#,
            ),
            (
                "src/index.ts",
                "const unused = 0;\nexport const value: number = 1;\n",
            ),
            (
                "src/checked.js",
                "/** @type {string} */\nexport const label = 7;\n",
            ),
            (
                "src/excluded/hidden.js",
                "/** @type {string} */\nexport const hidden = 8;\n",
            ),
            (
                "src/generated/output.js",
                "/** @type {string} */\nexport const generated = 9;\n",
            ),
        ],
    );
    project.log_compiler_runs();
    let status = TypeScriptProducerStatusHandleV1::default();
    let mut state = producer(&status);
    let started = Instant::now();
    let run = project
        .run("generation.package-extends.1", &mut state)
        .await;
    let cold = started.elapsed();
    assert!(
        matches!(run, CompilerProducerRunV1::Published { checked: 1, .. }),
        "{release:?}: {run:?}"
    );
    assert_eq!(project.checked(), ["tsconfig.json"]);
    assert_eq!(
        codes(&project.findings("generation.package-extends.1").await),
        ["TS2322"],
        "{release:?}: the later base enables checks of package-admitted JavaScript"
    );
    for file in ["src/index.ts", "src/checked.js"] {
        assert_eq!(
            owner(&project, &status, file),
            Some((
                "tsconfig.json".to_owned(),
                Some(TypeScriptProjectCheckV1::Checked)
            )),
            "{release:?}: {file}"
        );
    }
    for file in ["src/excluded/hidden.js", "src/generated/output.js"] {
        assert_eq!(owner(&project, &status, file), None, "{release:?}: {file}");
    }
    assert_eq!(
        project.runs(),
        0,
        "{release:?}: ordinary availability reads never invoke a compiler"
    );

    let started = Instant::now();
    let run = project
        .run("generation.package-extends.2", &mut state)
        .await;
    println!(
        "{release:?}: cold producer {cold:?}, unchanged generation reuse {:?}",
        started.elapsed()
    );
    assert!(
        matches!(
            run,
            CompilerProducerRunV1::Published {
                checked: 0,
                reused: 1,
                ..
            }
        ),
        "{release:?}: unchanged authoritative inputs reuse the prior check: {run:?}"
    );
    assert!(project.checked().is_empty());

    std::fs::write(
        project.root.join("src/added.js"),
        "/** @type {string} */\nexport const added = 10;\n",
    )
    .unwrap();
    project
        .run("generation.package-extends.3", &mut state)
        .await;
    assert_eq!(project.checked(), ["tsconfig.json"]);
    assert_eq!(
        codes(&project.findings("generation.package-extends.3").await),
        ["TS2322", "TS2322"],
        "{release:?}: inherited includes admit the new JavaScript source"
    );
    std::fs::remove_file(project.root.join("src/checked.js")).unwrap();
    project
        .run("generation.package-extends.4", &mut state)
        .await;
    assert_eq!(project.checked(), ["tsconfig.json"]);
    assert_eq!(
        codes(&project.findings("generation.package-extends.4").await),
        ["TS2322"],
        "{release:?}: a removed member cannot retain its earlier diagnostic"
    );
    assert_eq!(owner(&project, &status, "src/checked.js"), None);
    assert_eq!(
        owner(&project, &status, "src/added.js"),
        Some((
            "tsconfig.json".to_owned(),
            Some(TypeScriptProjectCheckV1::Checked)
        ))
    );
    assert_eq!(project.runs(), 0);

    std::fs::write(
        project
            .root
            .join("node_modules/@fixture/tsconfig/base.json"),
        r#"{"compilerOptions":{"allowJs":true,"checkJs":false,"noUnusedLocals":true}}"#,
    )
    .unwrap();
    let run = project
        .run("generation.package-extends.5", &mut state)
        .await;
    assert!(
        matches!(run, CompilerProducerRunV1::Published { checked: 1, .. }),
        "{release:?}: the changed package base invalidates the check: {run:?}"
    );
    assert_eq!(project.checked(), ["tsconfig.json"]);
    let findings = project.findings("generation.package-extends.5").await;
    let mut codes = codes(&findings);
    codes.sort_unstable();
    assert_eq!(
        codes,
        ["TS2322", "TS6133"],
        "{release:?}: a changed package's base config changes the actual findings"
    );
}

#[tokio::test]
async fn an_unindexed_dependency_edit_invalidates_clean_diagnostics() {
    for release in RELEASES {
        let project = InstalledCompilerProject::installing(
            release,
            &[
                (
                    "tsconfig.json",
                    r#"{"include":["src"],"compilerOptions":{"strict":true,"module":"NodeNext"}}"#,
                ),
                (
                    "node_modules/fixture/package.json",
                    r#"{"name":"fixture","version":"1.0.0","types":"./index.d.ts"}"#,
                ),
                (
                    "node_modules/fixture/index.d.ts",
                    "export const value: string;\n",
                ),
                (
                    "src/index.ts",
                    "import { value } from 'fixture';\nexport const label: string = value;\n",
                ),
            ],
        );
        let status = TypeScriptProducerStatusHandleV1::default();
        let mut state = producer(&status);
        let run = project.run("generation.dependency.1", &mut state).await;
        assert!(
            matches!(run, CompilerProducerRunV1::Published { checked: 1, .. }),
            "{release:?}: {run:?}"
        );
        assert_eq!(
            owner(&project, &status, "src/index.ts"),
            Some((
                "tsconfig.json".to_owned(),
                Some(TypeScriptProjectCheckV1::Checked)
            ))
        );
        let findings = project.findings("generation.dependency.1").await;
        assert!(findings.is_empty(), "{release:?}: {findings:?}");

        std::fs::write(
            project.root.join("node_modules/fixture/index.d.ts"),
            "export const value: number;\n",
        )
        .unwrap();
        let run = project.run("generation.dependency.2", &mut state).await;
        assert_eq!(
            codes(&project.findings("generation.dependency.2").await),
            ["TS2322"],
            "{release:?}: the installed declaration changed without an indexed edit: {run:?}"
        );
    }
}

const SOLUTION: &[(&str, &str)] = &[
    (
        "tsconfig.json",
        r#"{"files":[],"references":[{"path":"./packages/lib"},{"path":"./packages/app"}]}"#,
    ),
    (
        "packages/lib/tsconfig.json",
        r#"{"compilerOptions":{"composite":true,"rootDir":"src","outDir":"dist","strict":true},"include":["src"]}"#,
    ),
    (
        "packages/lib/src/index.ts",
        "export function size(text: string): number { return text.length; }\n",
    ),
    (
        "packages/app/tsconfig.json",
        r#"{"compilerOptions":{"composite":true,"rootDir":"src","outDir":"dist","strict":true},"include":["src"],"references":[{"path":"../lib"}]}"#,
    ),
    (
        "packages/app/src/main.ts",
        "import { size } from \"../../lib/src/index\";\nexport const label: string = size(\"abc\");\n",
    ),
];

/// What the project's own `tsc -b` writes into its tree.
fn build_solution(project: &InstalledCompilerProject) {
    let output = Command::new(project.launcher())
        .current_dir(&project.root)
        .args(["-b", "tsconfig.json", "--pretty", "false"])
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(report.contains("TS2322"), "{report}");
}

/// tsc checks a project against the declarations its references emit. Until
/// they are built, its own run reports their absence rather than the
/// project's findings, which is no evidence about its sources.
#[tokio::test]
async fn unbuilt_references_are_not_published_as_the_referencing_projects_findings() {
    for release in RELEASES {
        unbuilt_references(release).await;
    }
}

async fn unbuilt_references(release: PinnedTypeScript) {
    let project = InstalledCompilerProject::installing(release, SOLUTION);
    let status = TypeScriptProducerStatusHandleV1::default();
    let mut state = producer(&status);
    project.run("generation.references.1", &mut state).await;
    let findings = project.findings("generation.references.1").await;
    assert!(
        !codes(&findings).contains(&"TS6305"),
        "{release:?}: an unbuilt reference is not a finding in app: {findings:?}"
    );
    let Some((tsconfig, Some(TypeScriptProjectCheckV1::Failed { reason }))) =
        owner(&project, &status, "packages/app/src/main.ts")
    else {
        panic!("{release:?}: app could not be checked without its references' outputs");
    };
    assert_eq!(tsconfig, "packages/app/tsconfig.json");
    assert!(reason.contains("packages/lib/tsconfig.json"), "{reason}");
}

/// Once built, app's check reads lib's declarations. Editing lib's source
/// leaves them out of date, so app's earlier finding is not republished;
/// restoring the input and rebuilding makes app's finding evidence again.
#[tokio::test]
async fn out_of_date_references_invalidate_the_referencing_projects_check() {
    for release in RELEASES {
        out_of_date_references(release).await;
    }
}

async fn out_of_date_references(release: PinnedTypeScript) {
    let project = InstalledCompilerProject::installing(release, SOLUTION);
    build_solution(&project);
    let lib = project.root.join("packages/lib/src/index.ts");
    let built = std::fs::read_to_string(&lib).unwrap();
    let status = TypeScriptProducerStatusHandleV1::default();
    let mut state = producer(&status);
    project.run("generation.references.1", &mut state).await;
    assert_eq!(
        codes(&project.findings("generation.references.1").await),
        ["TS2322"],
        "{release:?}: app's real finding against lib's built declarations"
    );

    std::fs::write(
        project.root.join("packages/lib/src/index.ts"),
        "export function size(text: string): string { return text; }\n",
    )
    .unwrap();
    project.run("generation.references.2", &mut state).await;
    assert_eq!(
        project.findings("generation.references.2").await,
        Vec::new(),
        "{release:?}: lib's stale declarations must not keep app's finding"
    );
    let Some((_, Some(TypeScriptProjectCheckV1::Failed { reason }))) =
        owner(&project, &status, "packages/app/src/main.ts")
    else {
        panic!("{release:?}: app's references are out of date");
    };
    assert!(reason.contains("packages/lib/tsconfig.json"), "{reason}");

    std::fs::write(&lib, built).unwrap();
    build_solution(&project);
    let run = project.run("generation.references.3", &mut state).await;
    let app = owner(&project, &status, "packages/app/src/main.ts");
    assert_eq!(
        codes(&project.findings("generation.references.3").await),
        ["TS2322"],
        "{release:?}: lib's declarations match its source again: {run:?}, {app:?}"
    );
    assert_eq!(
        app,
        Some((
            "packages/app/tsconfig.json".to_owned(),
            Some(TypeScriptProjectCheckV1::Checked)
        ))
    );
}

/// A new indexed source changes a referenced project's fresh program listing,
/// even though it is absent from the previously recorded membership. The
/// dependent must recheck and report stale reference outputs rather than
/// reusing its earlier clean result.
#[tokio::test]
async fn an_added_source_in_a_reference_invalidates_dependents() {
    for release in RELEASES {
        let project = InstalledCompilerProject::installing(release, SOLUTION);
        std::fs::write(
            project.root.join("packages/app/src/main.ts"),
            "import { size } from \"../../lib/src/index\";\nexport const label: number = size(\"abc\");\n",
        )
        .unwrap();
        let output = Command::new(project.launcher())
            .current_dir(&project.root)
            .args(["-b", "tsconfig.json", "--pretty", "false"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{release:?}: clean solution did not build: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let status = TypeScriptProducerStatusHandleV1::default();
        let mut state = producer(&status);
        project.run("generation.reference-add.1", &mut state).await;
        assert_eq!(
            owner(&project, &status, "packages/app/src/main.ts"),
            Some((
                "packages/app/tsconfig.json".to_owned(),
                Some(TypeScriptProjectCheckV1::Checked)
            )),
            "{release:?}: the initial clean app must have a reusable result"
        );

        std::fs::write(
            project.root.join("packages/lib/src/generated.ts"),
            "export const generated: string = 1;\n",
        )
        .unwrap();
        let run = project.run("generation.reference-add.2", &mut state).await;
        let Some((_, Some(TypeScriptProjectCheckV1::Failed { reason }))) =
            owner(&project, &status, "packages/app/src/main.ts")
        else {
            panic!(
                "{release:?}: app reused clean diagnostics after the reference changed: {run:?}"
            );
        };
        assert!(reason.contains("packages/lib/tsconfig.json"), "{reason}");
    }
}

/// A base the configuration extends is missing. typescript@5 refuses the
/// configuration and typescript@7 reports it without the error, then fails
/// the check; either way the project is a typed failure naming the base,
/// never a clean page, until an edit repairs it.
#[tokio::test]
async fn an_unreadable_configuration_is_a_failure_until_repaired() {
    for release in RELEASES {
        unreadable_configuration(release).await;
    }
}

async fn unreadable_configuration(release: PinnedTypeScript) {
    let project = InstalledCompilerProject::installing(
        release,
        &[
            (
                "tsconfig.json",
                r#"{"extends":"./node_modules/fixture-base.json","include":["src"]}"#,
            ),
            ("src/index.ts", "export const value: string = 1;\n"),
        ],
    );
    let status = TypeScriptProducerStatusHandleV1::default();
    let mut state = producer(&status);
    let run = project.run("generation.unreadable.1", &mut state).await;
    let CompilerProducerRunV1::CompilerFailed { reason } = run else {
        panic!("{release:?}: nothing was checked: {run:?}");
    };
    assert!(reason.contains("base.json"), "{release:?}: {reason}");
    assert!(
        matches!(
            owner(&project, &status, "src/index.ts"),
            Some((_, Some(TypeScriptProjectCheckV1::Failed { .. })))
        ),
        "{release:?}"
    );

    std::fs::write(project.root.join("node_modules/fixture-base.json"), "{}").unwrap();
    project.run("generation.unreadable.2", &mut state).await;
    assert_eq!(
        codes(&project.findings("generation.unreadable.2").await),
        ["TS2322"],
        "{release:?}: the repaired configuration is checked"
    );
}

/// Preparation cost and incremental reuse, matched across both compiler
/// lines. Preparation is the generation that lists the program and checks
/// it; reuse is the generation after it that changed nothing and so lists the
/// program again without checking. The listing is the compiler's own answer
/// to what the program holds, paid once per project per generation in place
/// of the sealed-digest guess it replaces.
#[tokio::test]
async fn preparation_and_reuse_cost() {
    const SOURCES: usize = 200;
    let mut files: Vec<(String, String)> = vec![(
        "tsconfig.json".to_owned(),
        r#"{"include":["src/**/*.ts"]}"#.to_owned(),
    )];
    for index in 0..SOURCES {
        files.push((
            format!("src/mod{index}.ts"),
            format!("export const value{index}: number = {index};\n"),
        ));
    }
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_str()))
        .collect();
    for release in RELEASES {
        let project = InstalledCompilerProject::installing(release, &files);
        let status = TypeScriptProducerStatusHandleV1::default();
        let mut state = producer(&status);
        let started = Instant::now();
        let prepared = project.run("generation.cost.1", &mut state).await;
        let preparation = started.elapsed();
        let started = Instant::now();
        let reused = project.run("generation.cost.2", &mut state).await;
        let reuse = started.elapsed();
        assert!(
            matches!(
                prepared,
                CompilerProducerRunV1::Published {
                    checked: 1,
                    reused: 0,
                    ..
                }
            ),
            "{release:?}: {prepared:?}"
        );
        assert!(
            matches!(
                reused,
                CompilerProducerRunV1::Published {
                    checked: 0,
                    reused: 1,
                    ..
                }
            ),
            "{release:?}: {reused:?}"
        );
        println!(
            "{release:?}: {SOURCES} sources, preparation {preparation:?}, unchanged reuse {reuse:?}"
        );
    }
}
