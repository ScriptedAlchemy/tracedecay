//! What the installed compiler, not a reading of its configuration, decides:
//! which project a file belongs to, when a project must be checked again, and
//! how a project that references others is checked. Each case runs against
//! both maintained compiler lines, whose reports and records differ.

use std::num::NonZeroUsize;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tracedecay_lsp::TypeScriptCompilerAdmission;
use tracedecay_runtime_core::background_cpu::ProcessBackgroundCpuV1;

use super::installed_compiler_tests::{InstalledCompilerProject, PinnedTypeScript};
use super::{
    CompilerProducerRunV1, TypeScriptDiagnosticsAvailabilityV1, TypeScriptProducerStateV1,
    TypeScriptProducerStatusHandleV1, TypeScriptProjectCheckV1,
    typescript_diagnostics_availability,
};

fn producer(status: &TypeScriptProducerStatusHandleV1) -> TypeScriptProducerStateV1 {
    TypeScriptProducerStateV1::new(
        status.clone(),
        TypeScriptCompilerAdmission::new(Arc::new(ProcessBackgroundCpuV1::new(
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
/// reverting the edit leaves them current but for their timestamps, and app's
/// finding is evidence again.
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
    project.run("generation.references.3", &mut state).await;
    assert_eq!(
        codes(&project.findings("generation.references.3").await),
        ["TS2322"],
        "{release:?}: lib's declarations match its source again"
    );
    assert_eq!(
        owner(&project, &status, "packages/app/src/main.ts"),
        Some((
            "packages/app/tsconfig.json".to_owned(),
            Some(TypeScriptProjectCheckV1::Checked)
        ))
    );
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
                r#"{"extends":"./base.json","include":["src"]}"#,
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

    std::fs::write(project.root.join("tsconfig.json"), r#"{"include":["src"]}"#).unwrap();
    project.run("generation.unreadable.2", &mut state).await;
    assert_eq!(
        codes(&project.findings("generation.unreadable.2").await),
        ["TS2322"],
        "{release:?}: the repaired configuration is checked"
    );
}
