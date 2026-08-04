use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, SkillInstallTarget,
};
use crate::errors::TraceDecayError;

use super::*;

const INSTALLATION: &str = "test-installation";
const SKILL_ID: &str = "atomic-retry";

fn active_skill(body: &str) -> ManagedSkill {
    let mut skill = ManagedSkillDraft {
        id: SKILL_ID.to_string(),
        title: "Atomic retry".to_string(),
        summary: "Tests materialization recovery after a failed manifest write.".to_string(),
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
    let temp = tempfile::tempdir().unwrap();
    let scope = MaterializationScope::global(MaterializationHost::Claude, temp.path().join("home"));
    let v1 = active_skill("# v1\n\nOriginal materialized skill.");
    let first = materialize_skill(&scope, &v1, INSTALLATION).unwrap();
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
    write_pending_materialization(&dir, &pending).unwrap();

    let blocked_intent_root = temp.path().join("blocked-write-intent-root");
    fs::write(&blocked_intent_root, b"not a directory").unwrap();
    let error = crate::agents::with_host_config_write_intents(blocked_intent_root, || {
        apply_pending_materialization(&dir, &pending)
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

    let retried = materialize_skill(&scope, &v2, INSTALLATION).unwrap();
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
