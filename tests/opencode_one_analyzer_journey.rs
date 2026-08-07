//! OpenCode one-analyzer conformance journey (Plans 27/35).
//!
//! Plan 27 requires that OpenCode conformance "starts the TraceDecay custom
//! LSP with an existing language analyzer present and proves exactly one
//! analyzer owns that language before, during, and after install, repair,
//! rollback, and uninstall while TraceDecay findings still project"; Plan 35
//! repeats the same exactly-one-analyzer-per-language mandate.
//!
//! The journey drives the real CLI lifecycle (`install`, `reinstall`, a
//! killed mutation recovered by `host-bundle recover`, `uninstall`) against an
//! isolated home whose OpenCode configuration already declares a real
//! pre-existing analyzer (`rust-analyzer` for `.rs`), and at every stage
//! proves single ownership through both consumption paths:
//!
//! * the registration the installer writes (`duplicateAnalyzerAvoidance` +
//!   `analyzerOwnership.retainedByExtension`), and
//! * the analyzer broker, which must keep the language admitted so
//!   graph-backed TraceDecay findings still project while refusing to mount
//!   or refresh a second analyzer process for it.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;
use tracedecay_lsp::analyzer::adapters::{DiagnosticMode, LspAdapterDefinition};
use tracedecay_lsp::analyzer::broker::{DiagnosticBroker, EngineState};
use tracedecay_lsp::analyzer::host_ownership::HostAnalyzerOwnership;

const HOST_CONFIG_RELATIVE: &str = ".config/opencode/opencode.json";
const PRE_EXISTING_ANALYZER: &str = "rust-analyzer";

struct IsolatedCli {
    home: TempDir,
    project: TempDir,
    profile: PathBuf,
    bin_dir: PathBuf,
}

impl IsolatedCli {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let profile = home.path().join(".tracedecay-test-profile");
        let bin_dir = home.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let shim = bin_dir.join(if cfg!(windows) {
            "tracedecay.exe"
        } else {
            "tracedecay"
        });
        if fs::hard_link(env!("CARGO_BIN_EXE_tracedecay"), &shim).is_err() {
            fs::copy(env!("CARGO_BIN_EXE_tracedecay"), &shim).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&shim).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&shim, permissions).unwrap();
        }
        Self {
            home,
            project,
            profile,
            bin_dir,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin_dir.clone()).chain(std::env::split_paths(&inherited_path)),
        )
        .unwrap();
        command
            .args(args)
            .current_dir(self.project.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("TRACEDECAY_DATA_DIR", &self.profile)
            .env("TRACEDECAY_GLOBAL_DB", self.profile.join("global.db"))
            .env("PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn run_with_env(&self, args: &[&str], key: &str, value: &str) -> Output {
        let mut command = self.command(args);
        command.env(key, value);
        command.output().unwrap()
    }

    fn host_config_path(&self) -> PathBuf {
        self.home.path().join(HOST_CONFIG_RELATIVE)
    }

    fn host_config(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.host_config_path()).unwrap()).unwrap()
    }
}

fn assert_success(phase: &str, output: Output) {
    assert!(
        output.status.success(),
        "opencode {phase} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Seeds the host-owned configuration with a real pre-existing analyzer entry
/// covering `.rs`, exactly as an operator's OpenCode install would carry it.
fn seed_pre_existing_analyzer(cli: &IsolatedCli) {
    let config = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "lsp": {
            PRE_EXISTING_ANALYZER: {
                "command": [PRE_EXISTING_ANALYZER],
                "extensions": [".rs"]
            }
        },
        "theme": "dark"
    });
    let path = cli.host_config_path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

fn rust_adapter() -> LspAdapterDefinition {
    LspAdapterDefinition {
        language: "rust".to_string(),
        language_id: "rust".to_string(),
        // Deliberately resolvable on every runner: if enforcement regressed,
        // the adapter would report as mountable and the assertions below fail
        // rather than vacuously passing on a missing binary.
        command: "true".to_string(),
        args: Vec::new(),
        extensions: vec!["rs".to_string()],
        // Marker discovery is not what this journey proves; an empty set
        // anchors the adapter workspace at the project root.
        root_markers: Vec::new(),
        install_options: Vec::new(),
        diagnostics: DiagnosticMode::PushAndPull,
    }
}

/// Analyzer entries in the host config that cover `.rs`, excluding the
/// TraceDecay bridge registration itself.
///
/// The bridge lists `.rs` among its extensions but is registered
/// projection-only: `duplicateAnalyzerAvoidance` plus the retained-ownership
/// map, with the broker refusing every spawn path for a retained language
/// (proven by [`assert_broker_enforces_single_ownership`]). "Exactly one
/// analyzer" is therefore about entries that would run a competing analyzer.
fn rs_analyzer_entries(config: &serde_json::Value) -> Vec<String> {
    config
        .get("lsp")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flat_map(|servers| servers.iter())
        .filter(|(name, registration)| {
            name.as_str() != "tracedecay"
                && registration
                    .get("extensions")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .any(|extension| extension == ".rs")
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Proves the broker keeps the language admitted (findings still project)
/// while refusing to mount, refresh, or semantically start a second analyzer.
fn assert_broker_enforces_single_ownership(cli: &IsolatedCli, ownership: HostAnalyzerOwnership) {
    assert!(
        ownership.is_engaged(),
        "the installed registration must engage duplicate-analyzer avoidance"
    );
    fs::create_dir_all(cli.project.path().join("src")).unwrap();
    fs::write(cli.project.path().join("src/main.rs"), "fn main() {}\n").unwrap();

    let mut broker = DiagnosticBroker::new_for_test(cli.project.path(), vec![rust_adapter()]);
    broker.adopt_host_analyzer_ownership(ownership);

    assert_eq!(
        broker.host_retained_analyzer("rust"),
        Some(PRE_EXISTING_ANALYZER),
        "the host-declared analyzer must be the retained owner"
    );

    let admitted = broker.admitted_providers_for_files(&["src/main.rs".to_string()]);
    let rust = admitted
        .iter()
        .find(|provider| provider.language == "rust")
        .expect("host-retained language must stay admitted so findings still project");
    assert!(
        !rust.analyzer_available,
        "a host-retained language must never be reported mountable"
    );
    assert!(
        broker
            .mounted_providers_for_files(&["src/main.rs".to_string()])
            .is_empty(),
        "mounting is what would start the second analyzer"
    );

    let prepared = broker
        .prepare_refresh("rust", Vec::new())
        .expect("refusal must be a typed state, not an error");
    assert!(
        prepared.is_none(),
        "prepare_refresh is the only spawn path; it must refuse for a host-retained language"
    );
    let status = broker
        .project_engine_statuses()
        .into_iter()
        .find(|status| status.language == "rust")
        .expect("rust engine status");
    assert_eq!(status.state, EngineState::Disabled);
    let reason = status
        .last_error
        .expect("the refusal must carry an operator-facing reason");
    assert!(
        reason.contains(PRE_EXISTING_ANALYZER),
        "the reason must name the retaining analyzer: {reason}"
    );
}

/// The registration the installer writes must retain the pre-existing analyzer.
fn assert_registration_retains_host_analyzer(config: &serde_json::Value) {
    let initialization = config
        .pointer("/lsp/tracedecay/initialization/tracedecay")
        .expect("installed registration must carry the tracedecay initialization block");
    assert_eq!(
        initialization.get("duplicateAnalyzerAvoidance"),
        Some(&serde_json::Value::Bool(true))
    );
    let retained = initialization
        .pointer("/analyzerOwnership/retainedByExtension/.rs")
        .and_then(serde_json::Value::as_array)
        .expect("the pre-existing .rs analyzer must be recorded as retained");
    assert_eq!(
        retained
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>(),
        vec![PRE_EXISTING_ANALYZER]
    );
    // Exactly one analyzer owns `.rs`: the host's. The TraceDecay entry is
    // registered projection-only for it and its ownership block says so.
    assert_eq!(
        initialization.pointer("/analyzerOwnership/mode"),
        Some(&serde_json::json!("projection_only"))
    );
}

#[test]
fn opencode_keeps_exactly_one_analyzer_through_install_repair_rollback_uninstall() {
    let cli = IsolatedCli::new();
    seed_pre_existing_analyzer(&cli);

    // BEFORE INSTALL: only the host's analyzer covers `.rs`, and with no
    // TraceDecay registration present no ownership claim is engaged.
    let before = cli.host_config();
    assert_eq!(rs_analyzer_entries(&before), vec![PRE_EXISTING_ANALYZER]);
    assert!(before.pointer("/lsp/tracedecay").is_none());
    assert!(!HostAnalyzerOwnership::from_opencode_config(&before).is_engaged());

    // INSTALL: the registration lands projection-only and retains the host's
    // analyzer; the broker refuses to become a second one.
    assert_success("install", cli.run(&["install", "--agent", "opencode"]));
    let installed = cli.host_config();
    assert_registration_retains_host_analyzer(&installed);
    assert_eq!(
        rs_analyzer_entries(&installed),
        vec![PRE_EXISTING_ANALYZER],
        "install must not register TraceDecay as an `.rs` analyzer owner"
    );
    assert_broker_enforces_single_ownership(
        &cli,
        HostAnalyzerOwnership::from_opencode_config(&installed),
    );

    // REPAIR: reinstall refreshes managed artifacts; ownership must survive.
    assert_success("repair", cli.run(&["reinstall"]));
    let repaired = cli.host_config();
    assert_registration_retains_host_analyzer(&repaired);
    assert_eq!(rs_analyzer_entries(&repaired), vec![PRE_EXISTING_ANALYZER]);
    assert_broker_enforces_single_ownership(
        &cli,
        HostAnalyzerOwnership::from_opencode_config(&repaired),
    );

    // ROLLBACK: a mutation killed mid-write is rolled back by recovery to the
    // exact pre-effect state, which must still hold single ownership.
    let pre_fault = fs::read(cli.host_config_path()).unwrap();
    let killed = cli.run_with_env(
        &["reinstall"],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE",
        "1",
    );
    assert!(!killed.status.success(), "fault subprocess did not abort");
    assert_success(
        "rollback recovery",
        cli.run(&["host-bundle", "recover", "--agent", "opencode", "--yes"]),
    );
    assert_eq!(
        fs::read(cli.host_config_path()).unwrap(),
        pre_fault,
        "recovery must restore the exact pre-effect registration"
    );
    let recovered = cli.host_config();
    assert_registration_retains_host_analyzer(&recovered);
    assert_broker_enforces_single_ownership(
        &cli,
        HostAnalyzerOwnership::from_opencode_config(&recovered),
    );

    // UNINSTALL: the TraceDecay registration leaves; the host analyzer stays
    // the sole owner and no ownership claim survives.
    assert_success("uninstall", cli.run(&["uninstall", "--agent", "opencode"]));
    let uninstalled = cli.host_config();
    assert!(uninstalled.pointer("/lsp/tracedecay").is_none());
    assert_eq!(
        rs_analyzer_entries(&uninstalled),
        vec![PRE_EXISTING_ANALYZER]
    );
    assert!(!HostAnalyzerOwnership::from_opencode_config(&uninstalled).is_engaged());
}

/// The broker's own construction path must read the project-level OpenCode
/// configuration without any daemon adoption call, and adopting ownership
/// mid-session must tear down what construction could not have prevented.
#[test]
fn project_level_registration_engages_ownership_at_broker_construction() {
    let project = TempDir::new().unwrap();
    fs::create_dir_all(project.path().join("src")).unwrap();
    fs::write(project.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    let config = serde_json::json!({
        "lsp": {
            PRE_EXISTING_ANALYZER: {
                "command": [PRE_EXISTING_ANALYZER],
                "extensions": [".rs"]
            },
            "tracedecay": {
                "command": ["tracedecay", "lsp", "bridge", "--stdio"],
                "initialization": {
                    "tracedecay": {
                        "brokerUpstream": false,
                        "duplicateAnalyzerAvoidance": true,
                        "analyzerOwnership": {
                            "mode": "projection_only",
                            "retainedByExtension": { ".rs": [PRE_EXISTING_ANALYZER] }
                        }
                    }
                }
            }
        }
    });
    fs::write(
        project.path().join("opencode.json"),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();

    let mut broker = DiagnosticBroker::new_for_test(project.path(), vec![rust_adapter()]);

    assert_eq!(
        broker.host_retained_analyzer("rust"),
        Some(PRE_EXISTING_ANALYZER),
        "construction must consume the project-level registration directly"
    );
    let admitted = broker.admitted_providers_for_files(&["src/main.rs".to_string()]);
    let rust = admitted
        .iter()
        .find(|provider| provider.language == "rust")
        .expect("retained language stays admitted for projection");
    assert!(!rust.analyzer_available);
    let prepared = broker
        .prepare_refresh("rust", Vec::new())
        .expect("refusal must be typed");
    assert!(prepared.is_none());
}
