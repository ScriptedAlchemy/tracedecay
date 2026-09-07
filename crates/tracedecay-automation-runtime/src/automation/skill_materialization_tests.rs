use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use crate::automation::host_io::{HostIo, ManagedSkillExportReport, PluginFile};
use crate::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, SkillInstallTarget,
};
use crate::errors::TraceDecayError;

use super::*;

/// The production host I/O bundle, expressed in this test crate copy's types.
///
/// This lib-test binary contains two copies of the crate: the `cfg(test)`
/// copy under test and the plain lib copy that `tracedecay-agent-hosts`
/// links. `tracedecay_agent_hosts::host_io()` therefore returns the lib
/// copy's `HostIo`, which is a distinct type here. Every callback below
/// delegates to the same production implementation; nothing is stubbed or
/// re-implemented.
fn production_host_io() -> HostIo {
    fn export_to_agents(home: &Path, profile_root: &Path) -> Vec<ManagedSkillExportReport> {
        convert_reports(
            tracedecay_agent_hosts::host_io().export_managed_skills_to_agents(home, profile_root),
        )
    }

    fn export_to_agent_hosts(
        home: &Path,
        project_root: &Path,
        profile_root: &Path,
    ) -> Vec<ManagedSkillExportReport> {
        convert_reports(
            tracedecay_agent_hosts::host_io().export_managed_skills_to_agent_hosts(
                home,
                project_root,
                profile_root,
            ),
        )
    }

    fn convert_reports(reports: Vec<impl serde::Serialize>) -> Vec<ManagedSkillExportReport> {
        // The two crate copies declare structurally identical serde types.
        serde_json::from_value(serde_json::to_value(reports).expect("serialize export reports"))
            .expect("export reports round-trip between crate copies")
    }

    fn codex_agent_files() -> &'static [PluginFile] {
        static FILES: OnceLock<Vec<PluginFile>> = OnceLock::new();
        FILES
            .get_or_init(|| {
                tracedecay_agent_hosts::host_io()
                    .codex_agent_files()
                    .iter()
                    .map(|file| PluginFile {
                        relative: file.relative,
                        contents: file.contents,
                    })
                    .collect()
            })
            .as_slice()
    }

    HostIo {
        export_to_agents,
        export_to_agent_hosts,
        write_text: tracedecay_agent_hosts::agents::safe_write_text_file,
        write_json: tracedecay_agent_hosts::agents::safe_write_json_file,
        remove_host_file: tracedecay_agent_hosts::agents::safe_remove_host_file,
        codex_agent_files,
    }
}

const INSTALLATION: &str = "test-installation";
const SKILL_ID: &str = "atomic-retry";

fn active_skill(body: &str) -> ManagedSkill {
    let mut skill = ManagedSkillDraft {
        id: SKILL_ID.to_string(),
        title: "Atomic retry".to_string(),
        summary: "Tests materialization recovery after a failed manifest write.".to_string(),
        routing_description: "Repeated repository workflows requiring this maintained procedure."
            .to_owned(),
        category: "testing".to_string(),
        targets: vec![SkillInstallTarget::Claude],
        body_markdown: body.to_string(),
        support_files: Vec::new(),
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "test".to_string(),
            run_id: Some("atomic-retry-run".to_string()),
        },
    }
    .materialize()
    .unwrap();
    skill.set_state(ManagedSkillState::Active);
    skill
}

fn child_names(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn interrupted_manifest_atomic_write_keeps_v1_authoritative_and_public_retry_commits_v2() {
    let host_io = production_host_io();
    let temp = tempfile::tempdir().unwrap();
    let scope = MaterializationScope::global(MaterializationHost::Claude, temp.path().join("home"));
    let v1 = active_skill("# v1\n\nOriginal materialized skill.");
    let first = materialize_skill(&host_io, &scope, &v1, INSTALLATION).unwrap();
    assert_eq!(first.action, MaterializeAction::Written);

    let dir = scope.skill_dir(&v1.host_skill_slug());
    let manifest_path = dir.join(MATERIALIZATION_MANIFEST_FILE);
    let pending_path = dir.join(MATERIALIZATION_PENDING_FILE);
    let v1_manifest_bytes = fs::read(&manifest_path).unwrap();
    let v1_manifest = match read_materialization_manifest(&dir, SKILL_ID).unwrap() {
        ManifestState::Owned(manifest) => manifest,
        ManifestState::Missing | ManifestState::Foreign => panic!("expected v1 manifest"),
    };
    let baseline_children = child_names(&dir);

    let v2 = active_skill("# v2\n\nRecovered materialized skill.");
    let artifacts = desired_artifacts(&v2).unwrap();
    let pending = PendingMaterialization {
        managed_by: MATERIALIZED_SKILL_MANAGED_BY.to_string(),
        skill_id: SKILL_ID.to_string(),
        previous_files: v1_manifest.files.clone(),
        remove_files: BTreeMap::new(),
        next_manifest: build_materialization_manifest(
            &v2,
            v2.materialized_package_hash().unwrap(),
            INSTALLATION,
            &artifacts,
        ),
        artifacts_hex: artifacts
            .iter()
            .map(|(relative, bytes)| (relative.clone(), hex::encode(bytes)))
            .collect(),
    };
    write_pending_materialization(&host_io, &dir, &pending).unwrap();

    let blocked_intent_root = temp.path().join("blocked-write-intent-root");
    fs::write(&blocked_intent_root, b"not a directory").unwrap();
    let error =
        tracedecay_agent_hosts::agents::with_host_config_write_intents(blocked_intent_root, || {
            apply_pending_materialization(&host_io, &dir, &pending)
        })
        .unwrap_err();
    match error {
        TraceDecayError::Config { message } => {
            assert!(
                message.contains("failed to atomically replace"),
                "{message}"
            );
            assert!(
                message.contains("could not create host config write intent directory"),
                "{message}"
            );
        }
        other => panic!("expected typed atomic-write error, got {other:?}"),
    }

    assert_eq!(fs::read(&manifest_path).unwrap(), v1_manifest_bytes);
    assert!(pending_path.is_file());

    let retried = materialize_skill(&host_io, &scope, &v2, INSTALLATION).unwrap();
    assert_ne!(retried.action, MaterializeAction::SkippedForeign);
    assert_ne!(retried.action, MaterializeAction::SkippedForked);
    assert!(
        fs::read_to_string(dir.join(SKILL_FILE))
            .unwrap()
            .contains("Recovered materialized skill.")
    );
    let v2_manifest = match read_materialization_manifest(&dir, SKILL_ID).unwrap() {
        ManifestState::Owned(manifest) => manifest,
        ManifestState::Missing | ManifestState::Foreign => panic!("expected v2 manifest"),
    };
    assert_ne!(v2_manifest.package_hash, v1_manifest.package_hash);
    assert!(!pending_path.exists());
    assert_eq!(child_names(&dir), baseline_children);
}
