//! The provenance probe `build.rs` bakes into the binary, exercised as the
//! exact code the build script mounts — not a copy that can drift.
//!
//! Covers the three-source contract (verified git worktree, release env sha,
//! `cargo package` VCS journal — in that order, failing when none applies)
//! and the Cargo rerun semantics of the repo watcher: edits that change the
//! answer rerun the script, its own output does not, and probing never
//! rewrites the `.git/index` it hands Cargo as a trigger.

use source_provenance::{ProvenanceOrigin, ResolvedSourceProvenance, resolve, watch_paths};

#[path = "../build-support/source_provenance.rs"]
mod source_provenance;

const ENV_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const VCS_SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";

fn git(root: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git should run");
    assert!(status.status.success(), "git {args:?} failed");
}

fn committed_repo(root: &std::path::Path) {
    git(root, &["init", "--quiet"]);
    std::fs::write(root.join("tracked.txt"), "one").expect("write tracked file");
    git(root, &["add", "tracked.txt"]);
    git(
        root,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "init",
        ],
    );
}

fn head_sha(root: &std::path::Path) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git should run");
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8(output.stdout)
        .expect("sha is UTF-8")
        .trim()
        .to_string()
}

/// `resolve` for a fixture whose repository root and manifest dir coincide,
/// with no release env sha — the shape every git-vs-vcs-info test uses.
fn resolve_local(root: &std::path::Path) -> Result<ResolvedSourceProvenance, String> {
    resolve(root, root, None)
}

fn write_vcs_info(dir: &std::path::Path, contents: &str) {
    std::fs::write(dir.join(".cargo_vcs_info.json"), contents).expect("write vcs info");
}

// ---------------------------------------------------------------------------
// Source order and per-source contracts
// ---------------------------------------------------------------------------

#[test]
fn a_committed_worktree_reports_its_full_head_and_is_clean() {
    let dir = tempfile::tempdir().expect("temp dir");
    committed_repo(dir.path());

    let resolved = resolve_local(dir.path()).expect("a committed worktree resolves");

    assert_eq!(resolved.origin, ProvenanceOrigin::VerifiedGit);
    assert_eq!(resolved.full_sha, head_sha(dir.path()));
    assert_eq!(resolved.full_sha.len(), 40);
    assert!(
        resolved
            .full_sha
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{} is not lowercase hex",
        resolved.full_sha
    );
    assert!(!resolved.dirty, "a freshly committed tree is not dirty");
    assert!(!watch_paths(dir.path()).is_empty());
}

#[test]
fn an_uncommitted_change_marks_the_worktree_dirty() {
    let dir = tempfile::tempdir().expect("temp dir");
    committed_repo(dir.path());

    std::fs::write(dir.path().join("tracked.txt"), "two").expect("modify tracked file");
    assert!(
        resolve_local(dir.path())
            .expect("dirty worktree resolves")
            .dirty,
        "a modified tracked file is dirty"
    );

    git(dir.path(), &["checkout", "--", "tracked.txt"]);
    std::fs::write(dir.path().join("stray.txt"), "stray").expect("write untracked file");
    assert!(
        resolve_local(dir.path()).expect("worktree resolves").dirty,
        "an untracked file is dirty"
    );
}

/// Strict source order: a verified worktree answers even when the release env
/// sha is also present, so a developer checkout can never impersonate a
/// release export.
#[test]
fn a_verified_worktree_outranks_the_release_env_sha() {
    let dir = tempfile::tempdir().expect("temp dir");
    committed_repo(dir.path());

    let resolved = resolve(dir.path(), dir.path(), Some(ENV_SHA)).expect("worktree resolves");

    assert_eq!(resolved.origin, ProvenanceOrigin::VerifiedGit);
    assert_eq!(resolved.full_sha, head_sha(dir.path()));
    assert_ne!(resolved.full_sha, ENV_SHA);
}

#[test]
fn the_release_env_sha_names_a_clean_exact_commit() {
    let dir = tempfile::tempdir().expect("temp dir");

    let resolved =
        resolve(dir.path(), dir.path(), Some(ENV_SHA)).expect("release env sha resolves");

    assert_eq!(resolved.origin, ProvenanceOrigin::ReleaseEnv);
    assert_eq!(resolved.full_sha, ENV_SHA);
    assert!(
        !resolved.dirty,
        "a release sha names an exact commit; there is no worktree to be dirty"
    );
}

#[test]
fn a_malformed_release_env_sha_fails_quoting_the_value() {
    let dir = tempfile::tempdir().expect("temp dir");
    for malformed in [
        "",
        "ab12cd34ef56",
        "0123456789ABCDEF0123456789ABCDEF01234567",
        "g123456789abcdef0123456789abcdef01234567",
        "0123456789abcdef0123456789abcdef012345678",
    ] {
        let error = resolve(dir.path(), dir.path(), Some(malformed))
            .expect_err("a malformed release sha must fail the build");
        assert!(
            error.contains(&format!("{malformed:?}")),
            "error must quote the malformed value {malformed:?}: {error}"
        );
        assert!(error.contains("TRACEDECAY_RELEASE_GIT_SHA"), "{error}");
    }
}

#[test]
fn packaged_vcs_info_names_its_commit_and_absent_dirty_means_clean() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_vcs_info(
        dir.path(),
        &format!(r#"{{"git":{{"sha1":"{VCS_SHA}"}},"path_in_vcs":"crates/tracedecay-cli"}}"#),
    );

    let resolved = resolve_local(dir.path()).expect("vcs info resolves");

    assert_eq!(resolved.full_sha, VCS_SHA);
    assert!(!resolved.dirty, "Cargo omits `dirty` for clean packages");
    assert_eq!(
        resolved.origin,
        ProvenanceOrigin::PackagedVcsInfo {
            manifest_file: dir.path().join(".cargo_vcs_info.json"),
        }
    );
}

#[test]
fn packaged_vcs_info_honors_the_allow_dirty_flag() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_vcs_info(
        dir.path(),
        &format!(r#"{{"git":{{"sha1":"{VCS_SHA}","dirty":true}}}}"#),
    );

    let resolved = resolve_local(dir.path()).expect("vcs info resolves");

    assert!(resolved.dirty, "`git.dirty: true` must be honored");
    assert_eq!(resolved.full_sha, VCS_SHA);
}

#[test]
fn malformed_vcs_info_fails_instead_of_defaulting() {
    let cases: &[(&str, &str)] = &[
        ("not json at all", "failed to parse"),
        (r#"{"path_in_vcs":"x"}"#, "no `git` object"),
        (r#"{"git":{}}"#, "no `git.sha1` string"),
        (
            r#"{"git":{"sha1":"tooshort"}}"#,
            "not a 40-character lowercase hex",
        ),
        (
            r#"{"git":{"sha1":"0123456789ABCDEF0123456789ABCDEF01234567"}}"#,
            "not a 40-character lowercase hex",
        ),
    ];
    for (contents, expected_fragment) in cases {
        let dir = tempfile::tempdir().expect("temp dir");
        write_vcs_info(dir.path(), contents);
        let error = resolve_local(dir.path())
            .expect_err("malformed vcs info must fail instead of defaulting");
        assert!(
            error.contains(expected_fragment),
            "vcs info {contents:?}: expected {expected_fragment:?} in {error}"
        );
    }
}

#[test]
fn a_non_boolean_vcs_dirty_flag_fails() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_vcs_info(
        dir.path(),
        &format!(r#"{{"git":{{"sha1":"{VCS_SHA}","dirty":"yes"}}}}"#),
    );
    let error = resolve_local(dir.path()).expect_err("a non-boolean dirty flag must fail");
    assert!(error.contains("git.dirty"), "{error}");
}

#[test]
fn no_source_fails_naming_all_three_sources() {
    let dir = tempfile::tempdir().expect("temp dir");

    let error = resolve_local(dir.path()).expect_err("a bare directory has no provenance");

    assert!(error.contains("git worktree"), "{error}");
    assert!(error.contains("TRACEDECAY_RELEASE_GIT_SHA"), "{error}");
    assert!(error.contains(".cargo_vcs_info.json"), "{error}");
    assert!(watch_paths(dir.path()).is_empty());
}

/// A crate unpacked below an unrelated repository must not inherit that
/// repository's commit — the exact shape of a `cargo install` from the
/// registry inside a developer's checkout.
#[test]
fn a_subdirectory_of_another_repository_has_no_git_identity() {
    let dir = tempfile::tempdir().expect("temp dir");
    committed_repo(dir.path());
    let unpacked = dir.path().join("registry/tracedecay-0.0.0");
    std::fs::create_dir_all(&unpacked).expect("create unpacked crate dir");

    resolve_local(&unpacked).expect_err("an enclosing repository must not supply provenance");
    assert!(
        watch_paths(&unpacked).is_empty(),
        "an unrelated repository must not drive this crate's rebuilds"
    );

    // With packaged VCS metadata present, the same nested unpack resolves —
    // from Cargo's journal, not from the enclosing repository.
    write_vcs_info(&unpacked, &format!(r#"{{"git":{{"sha1":"{VCS_SHA}"}}}}"#));
    let resolved = resolve_local(&unpacked).expect("packaged metadata resolves");
    assert_eq!(resolved.full_sha, VCS_SHA);
    assert_ne!(resolved.full_sha, head_sha(dir.path()));
}

/// `watch_paths` hands `.git/index` to Cargo as a rebuild trigger, and any
/// build-script rerun recompiles the whole crate. Probing identity must
/// therefore leave the index alone: a plain `git status` refreshes its stat
/// cache and rewrites it, which would arm that trigger on every build that
/// follows an edit.
#[test]
fn probing_identity_does_not_rewrite_the_index_it_watches() {
    let dir = tempfile::tempdir().expect("temp dir");
    committed_repo(dir.path());
    let index = dir.path().join(".git/index");
    // A stale stat cache is what tempts git into rewriting the index.
    std::fs::write(dir.path().join("tracked.txt"), "modified").expect("modify tracked file");
    let before = std::fs::metadata(&index)
        .and_then(|meta| meta.modified())
        .expect("index mtime");

    assert!(
        resolve_local(dir.path()).expect("worktree resolves").dirty,
        "the fixture must read as dirty"
    );

    let after = std::fs::metadata(&index)
        .and_then(|meta| meta.modified())
        .expect("index mtime");
    assert_eq!(
        before, after,
        "resolve() rewrote .git/index, which watch_paths registers as a rebuild trigger"
    );
}

// ---------------------------------------------------------------------------
// Cargo rerun semantics, exercised through a real nested cargo build
// ---------------------------------------------------------------------------

const SOURCE_PROVENANCE_CARGO_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/source-provenance-cargo"
);

fn source_provenance_cargo_fixture() -> &'static std::path::Path {
    std::path::Path::new(SOURCE_PROVENANCE_CARGO_FIXTURE)
}

fn cargo_config_directory(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn workspace_rustup_toolchain() -> Option<String> {
    if let Ok(existing) = std::env::var("RUSTUP_TOOLCHAIN") {
        return Some(existing);
    }
    let toolchain_file =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rust-toolchain.toml");
    let contents = std::fs::read_to_string(toolchain_file).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if let Some(channel) = line
            .strip_prefix("channel = \"")
            .and_then(|rest| rest.strip_suffix('"'))
        {
            return Some(channel.to_owned());
        }
    }
    None
}

struct CargoFixture {
    _directory: tempfile::TempDir,
    root: std::path::PathBuf,
    target: std::path::PathBuf,
    cargo_home: std::path::PathBuf,
    rustup_toolchain: Option<String>,
}

impl CargoFixture {
    fn new() -> Self {
        let fixture = source_provenance_cargo_fixture();
        let directory = tempfile::tempdir().expect("fixture directory");
        let root = directory.path().join("repository");
        let target = directory.path().join("target");
        let cargo_home = directory.path().join("cargo-home");
        std::fs::create_dir_all(&cargo_home).expect("fixture cargo home");
        std::fs::create_dir_all(root.join("src")).expect("fixture source directory");

        for relative in ["Cargo.toml", "Cargo.lock", "README.md"] {
            std::fs::copy(fixture.join(relative), root.join(relative))
                .unwrap_or_else(|error| panic!("copy checked-in fixture {relative}: {error}"));
        }
        std::fs::copy(fixture.join("src/main.rs"), root.join("src/main.rs"))
            .expect("copy checked-in fixture src/main.rs");

        let vendor_directory = cargo_config_directory(&fixture.join("vendor"));
        std::fs::write(
            cargo_home.join("config.toml"),
            format!(
                "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n\
                 [source.vendored-sources]\n\
                 directory = \"{vendor_directory}\"\n"
            ),
        )
        .expect("fixture cargo home config");

        let shared_provenance = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("build-support/source_provenance.rs");
        std::fs::write(
            root.join("build.rs"),
            format!(
                "#[path = {shared_provenance:?}]\n\
                 mod source_provenance;\n\
                 \n\
                 fn main() {{\n\
                     let manifest_dir = std::env::var(\"CARGO_MANIFEST_DIR\").expect(\"manifest directory\");\n\
                     let root = std::path::Path::new(&manifest_dir);\n\
                     let resolved = source_provenance::resolve(root, root, None).expect(\"fixture provenance\");\n\
                     for path in source_provenance::watch_paths(root) {{\n\
                         println!(\"cargo::rerun-if-changed={{}}\", path.display());\n\
                     }}\n\
                     let counter = std::path::PathBuf::from(std::env::var(\"OUT_DIR\").expect(\"output directory\")).join(\"build-script-runs\");\n\
                     let runs = std::fs::read_to_string(&counter).ok().and_then(|value| value.parse::<u32>().ok()).unwrap_or(0) + 1;\n\
                     std::fs::write(counter, runs.to_string()).expect(\"write run count\");\n\
                     println!(\"cargo::rustc-env=FIXTURE_IDENTITY={{}}\", if resolved.dirty {{ \"dirty\" }} else {{ \"clean\" }});\n\
                 }}\n"
            ),
        )
        .expect("fixture build script");

        Self {
            _directory: directory,
            root,
            target,
            cargo_home,
            rustup_toolchain: workspace_rustup_toolchain(),
        }
    }

    fn root(&self) -> &std::path::Path {
        &self.root
    }

    fn commit_sources(&self) {
        self.dependency_preflight();
        git(&self.root, &["init", "--quiet"]);
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "build.rs",
            "src/main.rs",
            "README.md",
        ] {
            git(&self.root, &["add", path]);
        }
        git(
            &self.root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        );
    }

    fn build(&self) {
        self.cargo(&["build", "--locked", "--offline", "--quiet"]);
    }

    fn dependency_preflight(&self) {
        let output = self.cargo_output(&["fetch", "--locked", "--offline"]);
        if output.status.success() {
            return;
        }
        panic!(
            "source-provenance dependency preflight failed\n\
             command: cargo fetch --locked --offline\n\
             status: {}\n\
             stdout:\n{}\n\
             stderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn reported_identity(&self) -> String {
        let output = std::process::Command::new(
            self.target
                .join("debug")
                .join(format!("identity-fixture{}", std::env::consts::EXE_SUFFIX)),
        )
        .output()
        .expect("fixture binary should run");
        assert!(
            output.status.success(),
            "fixture binary should exit successfully"
        );
        String::from_utf8(output.stdout)
            .expect("fixture binary should print UTF-8")
            .trim()
            .to_string()
    }

    fn build_script_runs(&self) -> u32 {
        let build_directory = self.target.join("debug/build");
        let counters = std::fs::read_dir(build_directory)
            .expect("fixture build output directory")
            .flatten()
            .map(|entry| entry.path().join("out/build-script-runs"))
            .filter(|path| path.is_file())
            .collect::<Vec<_>>();
        assert_eq!(
            counters.len(),
            1,
            "fixture should have one build-script counter"
        );
        std::fs::read_to_string(&counters[0])
            .expect("fixture build-script counter")
            .trim()
            .parse()
            .expect("fixture build-script counter should be an integer")
    }

    fn cargo(&self, args: &[&str]) {
        let output = self.cargo_output(args);
        assert!(
            output.status.success(),
            "fixture cargo {args:?} failed\n\
             stdout:\n{}\n\
             stderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    fn cargo_output(&self, args: &[&str]) -> std::process::Output {
        let mut command =
            std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        command
            .args(args)
            .current_dir(&self.root)
            .env("CARGO_HOME", &self.cargo_home)
            .env("CARGO_TARGET_DIR", &self.target);
        if let Some(toolchain) = &self.rustup_toolchain {
            command.env("RUSTUP_TOOLCHAIN", toolchain);
        }
        command.output().expect("fixture cargo should run")
    }
}

#[test]
fn cargo_rebuilds_the_identity_after_a_tracked_non_source_edit() {
    let fixture = CargoFixture::new();
    fixture.commit_sources();
    fixture.build();
    assert_eq!(fixture.reported_identity(), "clean");
    assert_eq!(fixture.build_script_runs(), 1);

    std::fs::write(fixture.root().join("README.md"), "modified\n")
        .expect("modify tracked non-source file");
    fixture.build();

    assert_eq!(fixture.reported_identity(), "dirty");
    assert_eq!(fixture.build_script_runs(), 2);
}

#[test]
fn cargo_rebuilds_the_identity_after_an_existing_untracked_file_changes() {
    let fixture = CargoFixture::new();
    fixture.commit_sources();
    std::fs::write(fixture.root().join("loose.txt"), "one\n").expect("create untracked file");
    fixture.build();
    assert_eq!(fixture.reported_identity(), "dirty");
    assert_eq!(fixture.build_script_runs(), 1);

    std::fs::write(fixture.root().join("loose.txt"), "two\n").expect("modify untracked file");
    fixture.build();

    assert_eq!(fixture.build_script_runs(), 2);
}

#[test]
fn a_noop_build_does_not_rerun_the_identity_script_from_its_output() {
    let fixture = CargoFixture::new();
    fixture.commit_sources();
    fixture.build();
    assert_eq!(fixture.build_script_runs(), 1);

    fixture.build();

    assert_eq!(fixture.build_script_runs(), 1);
}
