//! OpenCode one-analyzer journey (Plan 27 / Plan 35 host-proof minimum).
//!
//! Plan 27 "Direct acceptance": *"OpenCode conformance starts the TraceDecay
//! custom LSP with an existing language analyzer present and proves exactly one
//! analyzer owns that language before, during, and after install, repair,
//! rollback, and uninstall while TraceDecay findings still project."*
//! Plan 35 "Direct acceptance": *"OpenCode retains exactly one analyzer per
//! language through install, repair, rollback, and uninstall."*
//!
//! The proof is a real spawn counter, not a configuration assertion: the fake
//! analyzer appends one line every time its process starts, so "a second
//! analyzer is prevented" is observed as the counter not moving. The
//! registration written here is byte-shaped exactly like the one
//! `crates/tracedecay-agent-hosts/src/agents/opencode.rs::install_registration_entries`
//! writes to `<project>/opencode.json`.

use super::*;

use tracedecay_lsp::analyzer::HostAnalyzerOwnership;

const HOST_RETAINED_ANALYZER: &str = "host-fake-analyzer";

/// The exact `opencode.json` shape the TraceDecay OpenCode installer writes
/// when the host already runs its own analyzer for `.fake` files.
fn installed_opencode_registration(retained: bool) -> serde_json::Value {
    let retained_by_extension = if retained {
        serde_json::json!({ ".fake": [HOST_RETAINED_ANALYZER] })
    } else {
        serde_json::json!({})
    };
    serde_json::json!({
        "lsp": {
            HOST_RETAINED_ANALYZER: {
                "command": [HOST_RETAINED_ANALYZER],
                "extensions": [".fake"]
            },
            "tracedecay": {
                "command": ["tracedecay", "lsp", "bridge", "--stdio"],
                "extensions": [".fake"],
                "env": { "TRACEDECAY_LSP_BROKER_UPSTREAM": "0" },
                "initialization": {
                    "tracedecay": {
                        "brokerUpstream": false,
                        "duplicateAnalyzerAvoidance": true,
                        "analyzerOwnership": {
                            "mode": "projection_only",
                            "retainedByExtension": retained_by_extension
                        }
                    }
                }
            }
        }
    })
}

fn install_opencode_registration(project_root: &std::path::Path, retained: bool) {
    std::fs::write(
        project_root.join("opencode.json"),
        serde_json::to_vec_pretty(&installed_opencode_registration(retained)).unwrap(),
    )
    .unwrap();
}

fn uninstall_opencode_registration(project_root: &std::path::Path) {
    std::fs::remove_file(project_root.join("opencode.json")).unwrap();
}

fn analyzer_starts(counter_path: &std::path::Path) -> usize {
    match std::fs::read_to_string(counter_path) {
        Ok(contents) => contents.lines().count(),
        Err(_) => 0,
    }
}

struct OneAnalyzerProject {
    project: tempfile::TempDir,
    script_path: std::path::PathBuf,
    counter_path: std::path::PathBuf,
}

impl OneAnalyzerProject {
    fn new() -> Self {
        let project = tempfile::tempdir().unwrap();
        let script_path = project.path().join("fake_lsp.py");
        let counter_path = project.path().join("analyzer-starts.txt");
        std::fs::write(
            &script_path,
            fake_lsp_script_that_records_start(&counter_path),
        )
        .unwrap();
        Self {
            project,
            script_path,
            counter_path,
        }
    }

    fn root(&self) -> &std::path::Path {
        self.project.path()
    }

    fn broker(&self) -> lsp::broker::DiagnosticBroker {
        lsp::broker::DiagnosticBroker::new_for_test(
            self.project.path(),
            vec![fake_python_adapter(
                FAKE_LANGUAGE,
                "fake",
                &self.script_path,
            )],
        )
    }

    fn starts(&self) -> usize {
        analyzer_starts(&self.counter_path)
    }
}

async fn refresh_once(broker: &mut lsp::broker::DiagnosticBroker) {
    broker
        .refresh_documents(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            FAKE_LSP_TIMEOUT,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn opencode_install_prevents_a_second_analyzer_and_uninstall_restores_it() {
    let workspace = OneAnalyzerProject::new();

    // Before install: TraceDecay owns the only analyzer for the language.
    let mut before_install = workspace.broker();
    refresh_once(&mut before_install).await;
    let before_snapshot = before_install.snapshot();
    assert_eq!(
        workspace.starts(),
        1,
        "without a host claim TraceDecay is the one analyzer for the language"
    );
    assert_eq!(
        before_snapshot.summary.total_errors, 1,
        "the baseline journey must really have collected analyzer findings"
    );
    assert_engine_state(
        &before_snapshot,
        FAKE_LANGUAGE,
        lsp::broker::EngineState::Ready,
    );
    assert!(before_install.host_retained_languages().is_empty());

    // Install: the host declares it already runs an analyzer for `.fake`.
    install_opencode_registration(workspace.root(), true);
    let mut after_install = workspace.broker();
    assert_eq!(
        after_install.host_retained_analyzer(FAKE_LANGUAGE),
        Some(HOST_RETAINED_ANALYZER),
        "the installed registration must be the broker's ownership authority"
    );

    refresh_once(&mut after_install).await;

    assert_eq!(
        workspace.starts(),
        1,
        "a second analyzer for a host-retained language must never be spawned"
    );
    let installed_snapshot = after_install.snapshot();
    assert_engine_state(
        &installed_snapshot,
        FAKE_LANGUAGE,
        lsp::broker::EngineState::Disabled,
    );
    let reason = engine_status(&installed_snapshot, FAKE_LANGUAGE)
        .last_error
        .as_deref()
        .expect("the refusal must name the analyzer the host retained");
    assert!(
        reason.contains(HOST_RETAINED_ANALYZER),
        "refusal reason must name the retained host analyzer: {reason}"
    );

    // Semantic requests share the same stdio client slot, so they are the other
    // way a second analyzer could be started.
    let root_uri = url::Url::from_directory_path(workspace.root())
        .unwrap()
        .to_string();
    assert!(
        after_install
            .semantic_authority_if_available(
                FAKE_LANGUAGE,
                workspace.root().to_path_buf(),
                root_uri,
                bounded_fake_lsp_timeouts(),
            )
            .unwrap()
            .is_none(),
        "semantic authority must not start the analyzer the host already owns"
    );
    assert_eq!(workspace.starts(), 1);

    // TraceDecay findings still project: the language stays admitted, it is
    // only never mounted as a second analyzer process.
    let files = vec![FAKE_PATH.to_string()];
    let admitted = after_install.admitted_providers_for_files(&files);
    let admitted_language = admitted
        .iter()
        .find(|provider| provider.language == FAKE_LANGUAGE)
        .expect("a host-retained language stays admitted for graph-backed projection");
    assert!(
        !admitted_language.analyzer_available,
        "a host-retained language must never be reported as analyzer-backed"
    );
    assert!(
        after_install
            .mounted_providers_for_files(&files)
            .iter()
            .all(|provider| provider.language != FAKE_LANGUAGE),
        "mounting is what starts the analyzer process, so it must be refused"
    );

    // Uninstall: the host withdraws its claim and TraceDecay owns it again.
    uninstall_opencode_registration(workspace.root());
    let mut after_uninstall = workspace.broker();
    assert_eq!(after_uninstall.host_retained_analyzer(FAKE_LANGUAGE), None);

    refresh_once(&mut after_uninstall).await;

    assert_eq!(
        workspace.starts(),
        2,
        "after uninstall exactly one analyzer owns the language again — TraceDecay's"
    );
    assert_engine_state(
        &after_uninstall.snapshot(),
        FAKE_LANGUAGE,
        lsp::broker::EngineState::Ready,
    );
}

#[tokio::test]
async fn adopting_host_ownership_mid_session_stops_the_running_second_analyzer() {
    let workspace = OneAnalyzerProject::new();
    let mut broker = workspace.broker();

    refresh_once(&mut broker).await;
    assert_eq!(workspace.starts(), 1, "the analyzer is warm before install");

    // Install happens while the session is live (the repair/rollback case).
    let ownership =
        HostAnalyzerOwnership::from_opencode_config(&installed_opencode_registration(true));
    broker.adopt_host_analyzer_ownership(ownership);

    assert_eq!(
        broker.host_retained_languages(),
        vec![FAKE_LANGUAGE.to_string()]
    );
    refresh_once(&mut broker).await;
    assert_eq!(
        workspace.starts(),
        1,
        "adopting ownership mid-session must drop the warm client, not keep a \
         second analyzer alive for the rest of the session"
    );
    assert_engine_state(
        &broker.snapshot(),
        FAKE_LANGUAGE,
        lsp::broker::EngineState::Disabled,
    );

    // Rollback: withdrawing the claim re-admits TraceDecay's analyzer, and it
    // is a cold start because the previous client really was torn down.
    broker.adopt_host_analyzer_ownership(HostAnalyzerOwnership::default());
    refresh_once(&mut broker).await;
    assert_eq!(workspace.starts(), 2);
}

#[tokio::test]
async fn an_installed_registration_without_a_retained_analyzer_keeps_tracedecay_mounted() {
    let workspace = OneAnalyzerProject::new();
    // OpenCode installed with duplicate-analyzer avoidance on, but the host
    // runs no analyzer for this language: there is no second analyzer to
    // prevent, so refusing here would leave the language with zero.
    install_opencode_registration(workspace.root(), false);
    let mut broker = workspace.broker();

    assert_eq!(broker.host_retained_analyzer(FAKE_LANGUAGE), None);
    refresh_once(&mut broker).await;

    assert_eq!(
        workspace.starts(),
        1,
        "avoidance without a retained analyzer must not disable the only analyzer"
    );
    assert_engine_state(
        &broker.snapshot(),
        FAKE_LANGUAGE,
        lsp::broker::EngineState::Ready,
    );
}
