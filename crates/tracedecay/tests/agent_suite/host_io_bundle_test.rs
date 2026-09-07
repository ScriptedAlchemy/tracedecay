//! The automation host I/O bundle is an explicit value, not a process-global.
//!
//! Every automation entry point that writes host-owned files takes a
//! [`HostIo`], so these tests can hand two differently-composed bundles to the
//! same process and prove that each write, removal, and export sweep reaches
//! exactly the bundle it was given — and that a bundle whose write surface
//! refuses produces a typed error, never an empty or "disabled" success.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tracedecay_automation_runtime::automation::agent_targets::{
    install_codex_managed_agents, remove_managed_agents,
};
use tracedecay_automation_runtime::automation::host_io::{
    HostIo, ManagedSkillExportReport, PluginFile,
};
use tracedecay_automation_runtime::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, create_managed_skill,
    default_managed_skill_targets,
};
use tracedecay_automation_runtime::automation::skill_materialization::{
    MaterializationHost, MaterializationScope, materialize_skill,
};
use tracedecay_automation_runtime::automation::skill_targets::{
    SkillInstallTarget, install_managed_skills,
};
use tracedecay_automation_runtime::automation::skill_writer::{
    ManagedSkillDeploymentStatus, deploy_managed_skills_to_project,
};
use tracedecay_domain::errors::{Result, TraceDecayError};

/// Everything the `A` bundle was asked to do, in order.
static A_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Everything the `B` bundle was asked to do, in order.
static B_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The recording logs are process statics (fn pointers cannot capture), so
/// the tests that read them run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// Taken after any `.await`, so the guard never spans an await point.
fn serial() -> MutexGuard<'static, ()> {
    let guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    drain(&A_LOG);
    drain(&B_LOG);
    guard
}

fn drain(log: &Mutex<Vec<String>>) -> Vec<String> {
    std::mem::take(&mut *log.lock().unwrap())
}

const A_AGENTS: &[PluginFile] = &[PluginFile {
    relative: "tracedecay-alpha.toml",
    contents: "name = \"alpha\"\n",
}];

const B_AGENTS: &[PluginFile] = &[
    PluginFile {
        relative: "tracedecay-beta.toml",
        contents: "name = \"beta\"\n",
    },
    PluginFile {
        relative: "tracedecay-gamma.toml",
        contents: "name = \"gamma\"\n",
    },
];

/// Prompt files carry the writing bundle's marker so cross-talk shows up in
/// the file itself; structured files (agent TOML, manifests) are left intact.
fn stamped(path: &Path, marker: &str, contents: &str) -> String {
    if path.extension().is_some_and(|ext| ext == "md") {
        format!("<!-- {marker} -->\n{contents}")
    } else {
        contents.to_string()
    }
}

/// Two bundles whose every callback records into its own log and stamps its
/// own marker into what it writes, so any cross-talk is visible in the files.
fn bundle_a() -> HostIo {
    fn export_to_agents(_: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        A_LOG.lock().unwrap().push("export_agents".into());
        Vec::new()
    }
    fn export_to_agent_hosts(_: &Path, _: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        A_LOG.lock().unwrap().push("export_hosts".into());
        Vec::new()
    }
    fn write_text(path: &Path, contents: &str, _: Option<&Path>) -> Result<()> {
        A_LOG
            .lock()
            .unwrap()
            .push(format!("write_text {}", path.display()));
        Ok(std::fs::write(path, stamped(path, "A", contents))?)
    }
    fn write_json(path: &Path, value: &serde_json::Value, _: Option<&Path>) -> Result<()> {
        A_LOG
            .lock()
            .unwrap()
            .push(format!("write_json {}", path.display()));
        Ok(std::fs::write(path, serde_json::to_vec_pretty(value)?)?)
    }
    fn remove_host_file(path: &Path) -> std::io::Result<()> {
        A_LOG
            .lock()
            .unwrap()
            .push(format!("remove {}", path.display()));
        std::fs::remove_file(path)
    }
    fn codex_agent_files() -> &'static [PluginFile] {
        A_AGENTS
    }
    HostIo {
        export_to_agents,
        export_to_agent_hosts,
        write_text,
        write_json,
        remove_host_file,
        codex_agent_files,
    }
}

fn bundle_b() -> HostIo {
    fn export_to_agents(_: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        B_LOG.lock().unwrap().push("export_agents".into());
        vec![ManagedSkillExportReport {
            agent: "b-host".into(),
            exports: Vec::new(),
            error: Some("b-host refused".into()),
        }]
    }
    fn export_to_agent_hosts(_: &Path, _: &Path, _: &Path) -> Vec<ManagedSkillExportReport> {
        B_LOG.lock().unwrap().push("export_hosts".into());
        vec![ManagedSkillExportReport {
            agent: "b-host".into(),
            exports: Vec::new(),
            error: Some("b-host refused".into()),
        }]
    }
    fn write_text(path: &Path, contents: &str, _: Option<&Path>) -> Result<()> {
        B_LOG
            .lock()
            .unwrap()
            .push(format!("write_text {}", path.display()));
        Ok(std::fs::write(path, stamped(path, "B", contents))?)
    }
    fn write_json(path: &Path, value: &serde_json::Value, _: Option<&Path>) -> Result<()> {
        B_LOG
            .lock()
            .unwrap()
            .push(format!("write_json {}", path.display()));
        Ok(std::fs::write(path, serde_json::to_vec_pretty(value)?)?)
    }
    fn remove_host_file(path: &Path) -> std::io::Result<()> {
        B_LOG
            .lock()
            .unwrap()
            .push(format!("remove {}", path.display()));
        std::fs::remove_file(path)
    }
    fn codex_agent_files() -> &'static [PluginFile] {
        B_AGENTS
    }
    HostIo {
        export_to_agents,
        export_to_agent_hosts,
        write_text,
        write_json,
        remove_host_file,
        codex_agent_files,
    }
}

/// A bundle whose write surface is refused by the host.
fn refusing_bundle() -> HostIo {
    fn write_text(path: &Path, _: &str, _: Option<&Path>) -> Result<()> {
        Err(TraceDecayError::Config {
            message: format!("host refused write to {}", path.display()),
        })
    }
    fn write_json(path: &Path, _: &serde_json::Value, _: Option<&Path>) -> Result<()> {
        Err(TraceDecayError::Config {
            message: format!("host refused write to {}", path.display()),
        })
    }
    HostIo {
        write_text,
        write_json,
        ..bundle_a()
    }
}

fn draft(id: &str) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: format!("{id} title"),
        summary: format!("{id} summary"),
        routing_description: format!("Use when {id} summary"),
        category: "workflow".to_string(),
        targets: default_managed_skill_targets(),
        body_markdown: format!("Use {id} when the workflow repeats."),
        support_files: Vec::new(),
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::User,
            actor: "test".to_string(),
            run_id: None,
        },
    }
}

async fn profile_with_skill(temp: &Path, id: &str) -> PathBuf {
    let profile_root = temp.join("profile");
    create_managed_skill(&profile_root, draft(id))
        .await
        .unwrap();
    profile_root
}

fn exists(path: &Path) -> bool {
    path.exists()
}

#[tokio::test]
async fn two_bundles_install_prompt_indexes_without_cross_talk() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = profile_with_skill(temp.path(), "repo-hygiene").await;
    let _serial = serial();
    let a_prompt = temp.path().join("a/AGENTS.md");
    let b_prompt = temp.path().join("b/AGENTS.md");
    let (a, b) = (bundle_a(), bundle_b());

    // Interleave the two compositions in one process: each write must land
    // through the bundle it was handed, with that bundle's marker only.
    let a_summary =
        install_managed_skills(&a, &profile_root, SkillInstallTarget::Agents, &a_prompt).unwrap();
    let b_summary =
        install_managed_skills(&b, &profile_root, SkillInstallTarget::Agents, &b_prompt).unwrap();
    let a_again =
        install_managed_skills(&a, &profile_root, SkillInstallTarget::Agents, &a_prompt).unwrap();

    assert_eq!(a_summary.exported_count, 1);
    assert_eq!(b_summary.exported_count, 1);
    assert_eq!(a_again.exported_count, 1);

    let a_text = std::fs::read_to_string(&a_prompt).unwrap();
    let b_text = std::fs::read_to_string(&b_prompt).unwrap();
    assert!(a_text.starts_with("<!-- A -->"), "{a_text}");
    assert!(!a_text.contains("<!-- B -->"), "{a_text}");
    assert!(b_text.starts_with("<!-- B -->"), "{b_text}");
    assert!(!b_text.contains("<!-- A -->"), "{b_text}");
    assert!(a_text.contains("`repo-hygiene`"));
    assert!(b_text.contains("`repo-hygiene`"));

    let a_log = drain(&A_LOG);
    let b_log = drain(&B_LOG);
    assert!(
        a_log
            .iter()
            .all(|entry| entry == &format!("write_text {}", a_prompt.display())),
        "{a_log:?}"
    );
    assert_eq!(b_log, [format!("write_text {}", b_prompt.display())]);
}

#[test]
fn codex_agent_installer_and_remover_use_only_the_bundle_they_are_given() {
    let _serial = serial();
    let temp = tempfile::tempdir().unwrap();
    let a_home = temp.path().join("a");
    let b_home = temp.path().join("b");
    let a = install_codex_managed_agents(&bundle_a(), &a_home).unwrap();
    let b = install_codex_managed_agents(&bundle_b(), &b_home).unwrap();
    assert_eq!(a.exported_count, 1);
    assert_eq!(b.exported_count, 2);
    assert!(exists(&a_home.join(".codex/agents/tracedecay-alpha.toml")));
    assert!(!exists(&a_home.join(".codex/agents/tracedecay-beta.toml")));
    assert!(exists(&b_home.join(".codex/agents/tracedecay-beta.toml")));
    assert!(exists(&b_home.join(".codex/agents/tracedecay-gamma.toml")));
    assert!(!exists(&b_home.join(".codex/agents/tracedecay-alpha.toml")));

    let a_writes = drain(&A_LOG);
    let b_writes = drain(&B_LOG);
    assert_eq!(a_writes.len(), 2, "alpha agent + manifest: {a_writes:?}");
    assert_eq!(b_writes.len(), 3, "beta, gamma + manifest: {b_writes:?}");
    assert!(
        a_writes
            .iter()
            .all(|entry| entry.contains("/a/.codex/agents/")),
        "{a_writes:?}"
    );
    assert!(
        b_writes
            .iter()
            .all(|entry| entry.contains("/b/.codex/agents/")),
        "{b_writes:?}"
    );

    // Removal reaches the bundle's removal surface, and only for its own home.
    remove_managed_agents(&bundle_b(), &b_home.join(".codex/agents")).unwrap();
    assert!(drain(&A_LOG).is_empty());
    let b_removes = drain(&B_LOG);
    assert_eq!(b_removes.len(), 3, "beta, gamma + manifest: {b_removes:?}");
    assert!(b_removes.iter().all(|entry| entry.starts_with("remove ")));
    assert!(!exists(&b_home.join(".codex/agents")));
    assert!(exists(&a_home.join(".codex/agents/tracedecay-alpha.toml")));
}

#[tokio::test]
async fn materialization_writes_its_manifest_through_the_given_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = profile_with_skill(temp.path(), "code-slop-cleanup").await;
    let _serial = serial();
    let skills =
        tracedecay_automation_runtime::automation::skill_targets::load_active_managed_skills(
            &profile_root,
        )
        .unwrap();
    let skill = &skills[0];
    let scope = MaterializationScope::global(MaterializationHost::Claude, temp.path().join("home"));
    materialize_skill(&bundle_a(), &scope, skill, "install-a").unwrap();
    let a_writes = drain(&A_LOG);
    assert!(
        a_writes
            .iter()
            .any(|entry| entry.starts_with("write_json ")),
        "manifest and pending transaction land through the bundle: {a_writes:?}"
    );
    assert!(drain(&B_LOG).is_empty());

    // A refused manifest write is a typed error and leaves no half-materialized
    // package behind.
    let refused_scope =
        MaterializationScope::global(MaterializationHost::Codex, temp.path().join("home"));
    let error =
        materialize_skill(&refusing_bundle(), &refused_scope, skill, "install-a").unwrap_err();
    assert!(
        matches!(&error, TraceDecayError::Config { message } if message.contains("host refused write")),
        "{error}"
    );
    assert!(!exists(
        &refused_scope
            .skills_dir()
            .join(skill.host_skill_slug())
            .join("SKILL.md")
    ));
}

#[tokio::test]
async fn refused_host_writes_are_typed_errors_not_empty_success() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = profile_with_skill(temp.path(), "repo-hygiene").await;
    let _serial = serial();
    let prompt = temp.path().join("AGENTS.md");
    std::fs::write(&prompt, "# Keep me\n").unwrap();

    let error = install_managed_skills(
        &refusing_bundle(),
        &profile_root,
        SkillInstallTarget::Agents,
        &prompt,
    )
    .unwrap_err();
    assert!(
        matches!(&error, TraceDecayError::Config { message } if message.contains("host refused write")),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&prompt).unwrap(), "# Keep me\n");
}

#[tokio::test]
async fn deployment_reports_the_given_bundle_export_failures_as_partial_not_complete() {
    let temp = tempfile::tempdir().unwrap();
    let profile_root = profile_with_skill(temp.path(), "repo-hygiene").await;
    let _serial = serial();
    let receipt = deploy_managed_skills_to_project(&bundle_b(), &profile_root, temp.path());
    assert_eq!(drain(&B_LOG), ["export_hosts"]);
    assert!(drain(&A_LOG).is_empty());
    assert_eq!(receipt.status, ManagedSkillDeploymentStatus::PartialFailure);
    assert!(receipt.retry_required);
    assert_eq!(receipt.exports.len(), 1);
    assert_eq!(receipt.exports[0].agent, "b-host");
    assert_eq!(receipt.exports[0].error.as_deref(), Some("b-host refused"));
}
