//! Host-loadable managed-skill materialization: activation writes real
//! `SKILL.md` files into `.claude`/`.codex` skills dirs (project + global),
//! deactivation removes them, user edits fork-protect the file, reconciles are
//! idempotent, and `doctor` reports drift. Mirrors the install/update lifecycle
//! test patterns in `skill_targets_test.rs`.

use std::path::{Path, PathBuf};

use tracedecay::automation::managed_skills::{
    ManagedSkillDraft, ManagedSkillProvenance, ManagedSkillSource, ManagedSkillState,
    ManagedSupportFile, create_managed_skill_draft, default_managed_skill_targets,
    set_managed_skill_state,
};
use tracedecay::automation::skill_frontmatter::parse_skill_frontmatter;
use tracedecay::automation::skill_materialization::{
    MaterializationHost, MaterializationScope, MaterializeAction, RemoveAction, SkillDrift,
    detect_scopes, doctor_detected_scopes, doctor_scope, materialize_skill,
    reconcile_detected_scopes, reconcile_scope, remove_materialized_skill,
};

/// A canonicalized temp root: on macOS `/tmp` is a symlink to `/private/tmp`,
/// so canonicalizing keeps materialized paths comparable to the profile paths.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    (temp, root)
}

/// Creates the `.claude` and `.codex` host config directories under `base` so
/// `detect_scopes` treats it as an eligible materialization scope.
fn install_fake_hosts(base: &Path) {
    std::fs::create_dir_all(base.join(".claude")).unwrap();
    std::fs::create_dir_all(base.join(".codex")).unwrap();
}

fn draft(id: &str) -> ManagedSkillDraft {
    ManagedSkillDraft {
        id: id.to_string(),
        title: "Code slop cleanup".to_string(),
        summary: "Use when tidying obvious code slop before review.".to_string(),
        category: "maintenance".to_string(),
        targets: default_managed_skill_targets(),
        body_markdown: "# Cleanup\n\nRemove dead code and stray debug prints.".to_string(),
        support_files: vec![
            ManagedSupportFile::new(
                "references/checklist.md",
                b"- drop debug prints\n- delete dead code\n".to_vec(),
            )
            .unwrap(),
        ],
        provenance: ManagedSkillProvenance {
            source: ManagedSkillSource::AutomationRun,
            actor: "tracedecay".to_string(),
            run_id: Some("run_slop".to_string()),
        },
    }
}

/// Drafts a skill in `profile_root` and flips it to `Active`.
async fn activate_skill(profile_root: &Path, id: &str) {
    create_managed_skill_draft(profile_root, draft(id))
        .await
        .unwrap();
    set_managed_skill_state(profile_root, id, ManagedSkillState::Active)
        .await
        .unwrap();
}

fn skill_md(scope: &MaterializationScope, slug: &str) -> PathBuf {
    scope.skills_dir().join(slug).join("SKILL.md")
}

#[tokio::test]
async fn materialize_on_activate_writes_both_hosts_and_scopes() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let project = root.join("project");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);
    install_fake_hosts(&project);

    activate_skill(&profile_root, "code-slop-cleanup").await;

    let (results, errors) = reconcile_detected_scopes(&profile_root, &home, &project);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    // 2 hosts x 2 scopes (project + global) = 4 destinations.
    assert_eq!(results.len(), 4, "expected 4 detected scopes");

    let expected = [
        home.join(".claude/skills/code-slop-cleanup/SKILL.md"),
        home.join(".codex/skills/code-slop-cleanup/SKILL.md"),
        project.join(".claude/skills/code-slop-cleanup/SKILL.md"),
        project.join(".codex/skills/code-slop-cleanup/SKILL.md"),
    ];
    for path in &expected {
        assert!(
            path.is_file(),
            "missing materialized skill at {}",
            path.display()
        );
    }
    // Support files travel with the skill package.
    assert!(
        home.join(".claude/skills/code-slop-cleanup/references/checklist.md")
            .is_file()
    );

    // Every reconcile entry wrote its file.
    for result in &results {
        assert_eq!(
            result.report.written_count(),
            1,
            "scope {}",
            result.scope.describe()
        );
    }
}

#[tokio::test]
async fn materialized_file_carries_provenance_frontmatter() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    reconcile_detected_scopes(&profile_root, &home, &home);

    let path = home.join(".claude/skills/code-slop-cleanup/SKILL.md");
    let contents = std::fs::read_to_string(&path).unwrap();
    let fm = parse_skill_frontmatter(&contents).unwrap();

    assert_eq!(fm["name"].as_scalar(), Some("code-slop-cleanup"));
    assert_eq!(fm["managed-by"].as_scalar(), Some("tracedecay-automation"));
    assert_eq!(fm["skill-id"].as_scalar(), Some("code-slop-cleanup"));
    let content_hash = fm["content-hash"].as_scalar().unwrap();
    assert!(content_hash.starts_with("sha256:"), "hash: {content_hash}");
    assert!(fm.contains_key("skill-version"));
    assert!(fm.contains_key("description"));
    // The host-facing body survives verbatim.
    assert!(contents.contains("Remove dead code and stray debug prints."));
}

#[tokio::test]
async fn remove_on_deactivate_deletes_materialized_file() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    reconcile_detected_scopes(&profile_root, &home, &home);
    let path = home.join(".claude/skills/code-slop-cleanup/SKILL.md");
    assert!(path.is_file());

    // Deactivate: the skill drops out of the active set.
    set_managed_skill_state(
        &profile_root,
        "code-slop-cleanup",
        ManagedSkillState::Disabled,
    )
    .await
    .unwrap();
    let (results, errors) = reconcile_detected_scopes(&profile_root, &home, &home);
    assert!(errors.is_empty(), "errors: {errors:?}");
    assert!(
        !path.exists(),
        "materialized file should be removed on deactivate"
    );
    // The package directory is pruned too.
    assert!(!home.join(".claude/skills/code-slop-cleanup").exists());
    let removed: usize = results.iter().map(|r| r.report.removed_count()).sum();
    assert_eq!(removed, 2, "both claude+codex managed files removed");
}

#[tokio::test]
async fn idempotent_reconcile_is_unchanged_on_rerun() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    reconcile_detected_scopes(&profile_root, &home, &home);
    let path = home.join(".claude/skills/code-slop-cleanup/SKILL.md");
    let first = std::fs::read_to_string(&path).unwrap();

    let (results, errors) = reconcile_detected_scopes(&profile_root, &home, &home);
    assert!(errors.is_empty(), "errors: {errors:?}");
    for result in &results {
        assert_eq!(result.report.written_count(), 0, "rerun rewrote a file");
        for entry in &result.report.materialized {
            assert_eq!(entry.action, MaterializeAction::Unchanged);
        }
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
}

#[tokio::test]
async fn body_update_re_materializes_the_file() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home.clone());
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    let first = materialize_skill(&scope, &skill).unwrap();
    assert_eq!(first.action, MaterializeAction::Written);

    // Change the body: the content-hash changes, so the reconciler rewrites.
    skill.body_markdown = "# Cleanup v2\n\nNow with extra rigor.".to_string();
    let second = materialize_skill(&scope, &skill).unwrap();
    assert_eq!(second.action, MaterializeAction::Written);
    let contents = std::fs::read_to_string(skill_md(&scope, "code-slop-cleanup")).unwrap();
    assert!(contents.contains("Now with extra rigor."));

    // A third pass with the same content is a no-op.
    let third = materialize_skill(&scope, &skill).unwrap();
    assert_eq!(third.action, MaterializeAction::Unchanged);
}

#[tokio::test]
async fn metadata_update_re_materializes_the_file() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill).unwrap();

    skill.metadata.summary = "Use when performing a strict cleanup before review.".to_string();
    let updated = materialize_skill(&scope, &skill).unwrap();

    assert_eq!(updated.action, MaterializeAction::Written);
    let contents = std::fs::read_to_string(skill_md(&scope, "code-slop-cleanup")).unwrap();
    assert!(contents.contains("performing a strict cleanup"));
    assert_eq!(
        materialize_skill(&scope, &skill).unwrap().action,
        MaterializeAction::Unchanged
    );
}

#[tokio::test]
async fn support_update_removes_only_stale_owned_files() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill).unwrap();
    let dir = scope.skills_dir().join("code-slop-cleanup");
    let stale = dir.join("references/checklist.md");
    let foreign = dir.join("references/user-notes.md");
    std::fs::write(&foreign, "keep me\n").unwrap();

    skill.support_files =
        vec![ManagedSupportFile::new("references/guide.md", b"new guide\n".to_vec()).unwrap()];
    let updated = materialize_skill(&scope, &skill).unwrap();

    assert_eq!(updated.action, MaterializeAction::Written);
    assert!(
        !stale.exists(),
        "stale owned support file should be removed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("references/guide.md")).unwrap(),
        "new guide\n"
    );
    assert_eq!(std::fs::read_to_string(foreign).unwrap(), "keep me\n");
}

#[tokio::test]
async fn user_edited_support_file_is_fork_protected() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill).unwrap();
    let support = scope
        .skills_dir()
        .join("code-slop-cleanup/references/checklist.md");
    std::fs::write(&support, "user edit\n").unwrap();

    skill.support_files[0].bytes = b"automation update\n".to_vec();
    let updated = materialize_skill(&scope, &skill).unwrap();

    assert_eq!(updated.action, MaterializeAction::SkippedForked);
    assert_eq!(std::fs::read_to_string(support).unwrap(), "user edit\n");
}

#[tokio::test]
async fn deactivation_removes_only_owned_artifacts() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill).unwrap();
    let dir = scope.skills_dir().join("code-slop-cleanup");
    let foreign = dir.join("references/user-notes.md");
    std::fs::write(&foreign, "keep me\n").unwrap();

    let removed = remove_materialized_skill(&scope, "code-slop-cleanup").unwrap();

    assert_eq!(removed, RemoveAction::Removed);
    assert!(!dir.join("SKILL.md").exists());
    assert!(!dir.join("references/checklist.md").exists());
    assert_eq!(std::fs::read_to_string(foreign).unwrap(), "keep me\n");
    assert!(dir.is_dir(), "directory with foreign files must remain");
}

#[cfg(unix)]
#[tokio::test]
async fn package_symlink_is_rejected_before_materialization() {
    use std::os::unix::fs::symlink;

    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    let external = root.join("external");
    install_fake_hosts(&home);
    std::fs::create_dir_all(&external).unwrap();

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    std::fs::create_dir_all(scope.skills_dir()).unwrap();
    symlink(&external, scope.skills_dir().join("code-slop-cleanup")).unwrap();
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();

    let error = materialize_skill(&scope, &skill).unwrap_err();

    assert!(error.to_string().contains("symlink"), "{error}");
    assert!(std::fs::read_dir(external).unwrap().next().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn nested_support_symlink_is_rejected_before_write() {
    use std::os::unix::fs::symlink;

    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    let external = root.join("external");
    install_fake_hosts(&home);
    std::fs::create_dir_all(&external).unwrap();

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let dir = scope.skills_dir().join("code-slop-cleanup");
    std::fs::create_dir_all(&dir).unwrap();
    symlink(&external, dir.join("references")).unwrap();
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();

    let error = materialize_skill(&scope, &skill).unwrap_err();

    assert!(error.to_string().contains("symlink"), "{error}");
    assert!(!external.join("checklist.md").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn nested_support_symlink_is_rejected_before_remove() {
    use std::os::unix::fs::symlink;

    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    let external = root.join("external");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill).unwrap();
    let dir = scope.skills_dir().join("code-slop-cleanup");
    let support = dir.join("references/checklist.md");
    let contents = std::fs::read(&support).unwrap();
    std::fs::remove_file(&support).unwrap();
    std::fs::remove_dir(dir.join("references")).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(external.join("checklist.md"), &contents).unwrap();
    symlink(&external, dir.join("references")).unwrap();

    let error = remove_materialized_skill(&scope, "code-slop-cleanup").unwrap_err();

    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(
        std::fs::read(external.join("checklist.md")).unwrap(),
        contents
    );
}

#[tokio::test]
async fn interrupted_manifest_commit_recovers_on_retry() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill).unwrap();
    skill.body_markdown = "# Cleanup v2\n\nRecover this update.".to_string();
    skill.support_files[0].bytes = b"updated checklist\n".to_vec();

    let dir = scope.skills_dir().join("code-slop-cleanup");
    let manifest = dir.join(".tracedecay-materialization.json");
    let blocked_staging = PathBuf::from(format!("{}.new", manifest.display()));
    std::fs::create_dir(&blocked_staging).unwrap();
    assert!(materialize_skill(&scope, &skill).is_err());
    std::fs::remove_dir(blocked_staging).unwrap();

    let retried = materialize_skill(&scope, &skill).unwrap();

    assert_ne!(retried.action, MaterializeAction::SkippedForked);
    assert!(
        std::fs::read_to_string(dir.join("SKILL.md"))
            .unwrap()
            .contains("Recover this update.")
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("references/checklist.md")).unwrap(),
        "updated checklist\n"
    );
}

#[tokio::test]
async fn fork_protection_leaves_user_edited_file_and_doctor_flags_it() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home.clone());
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill).unwrap();
    let path = skill_md(&scope, "code-slop-cleanup");

    // User edits the materialized body (the content-hash no longer matches).
    let edited = format!(
        "{}\n\n<!-- user note: keep this -->\n",
        std::fs::read_to_string(&path).unwrap()
    );
    std::fs::write(&path, &edited).unwrap();

    // Re-materialize: the reconciler must NOT clobber the fork.
    let action = materialize_skill(&scope, &skill).unwrap();
    assert_eq!(action.action, MaterializeAction::SkippedForked);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    // Doctor flags the fork.
    let drift = doctor_scope(&scope, std::slice::from_ref(&skill)).unwrap();
    assert!(
        drift.iter().any(
            |d| matches!(d, SkillDrift::Forked { skill_id, .. } if skill_id == "code-slop-cleanup")
        ),
        "expected Forked drift, got {drift:?}"
    );

    // A deactivate reconcile must also refuse to delete the fork.
    let removed = remove_materialized_skill(&scope, "code-slop-cleanup").unwrap();
    assert_eq!(removed, RemoveAction::SkippedForked);
    assert!(path.is_file(), "forked file must survive removal");
}

#[tokio::test]
async fn foreign_file_is_never_touched_and_doctor_reports_conflict() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    // A user (or repo-local dev skill) already owns this slug — no provenance.
    let scope = MaterializationScope::global(MaterializationHost::Claude, home.clone());
    let dir = scope.skills_dir().join("code-slop-cleanup");
    std::fs::create_dir_all(&dir).unwrap();
    let foreign = "---\nname: code-slop-cleanup\ndescription: hand-written\n---\n\nMine.\n";
    std::fs::write(dir.join("SKILL.md"), foreign).unwrap();

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();

    let action = materialize_skill(&scope, &skill).unwrap();
    assert_eq!(action.action, MaterializeAction::SkippedForeign);
    assert_eq!(
        std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
        foreign
    );

    // Removal never touches a foreign file either.
    assert_eq!(
        remove_materialized_skill(&scope, "code-slop-cleanup").unwrap(),
        RemoveAction::SkippedForeign
    );

    let drift = doctor_scope(&scope, std::slice::from_ref(&skill)).unwrap();
    assert!(
        drift
            .iter()
            .any(|d| matches!(d, SkillDrift::Conflict { .. })),
        "expected Conflict drift, got {drift:?}"
    );
}

#[tokio::test]
async fn doctor_reports_missing_and_orphan_drift() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);

    // Active skill, nothing materialized yet -> Missing.
    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scopes = doctor_detected_scopes(&profile_root, &home, &home).unwrap();
    let claude = scopes
        .iter()
        .find(|(scope, _)| scope.host == MaterializationHost::Claude)
        .map(|(_, drift)| drift)
        .unwrap();
    assert!(
        claude
            .iter()
            .any(|d| matches!(d, SkillDrift::Missing { .. })),
        "expected Missing drift, got {claude:?}"
    );

    // Materialize, then deactivate WITHOUT reconciling -> Orphan on disk.
    let scope = MaterializationScope::global(MaterializationHost::Claude, home.clone());
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill).unwrap();
    let orphan_drift = doctor_scope(&scope, &[]).unwrap();
    assert!(
        orphan_drift.iter().any(
            |d| matches!(d, SkillDrift::Orphan { skill_id, .. } if skill_id == "code-slop-cleanup")
        ),
        "expected Orphan drift, got {orphan_drift:?}"
    );
}

#[tokio::test]
async fn detect_scopes_only_covers_installed_hosts() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let project = root.join("project");
    // Only Claude is installed globally; only Codex in the project.
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(project.join(".codex")).unwrap();

    let scopes = detect_scopes(&home, &project);
    let described: Vec<String> = scopes.iter().map(MaterializationScope::describe).collect();
    assert!(
        described.contains(&"claude/global".to_string()),
        "{described:?}"
    );
    assert!(
        described.contains(&"codex/project".to_string()),
        "{described:?}"
    );
    assert!(
        !described.contains(&"codex/global".to_string()),
        "{described:?}"
    );
    assert!(
        !described.contains(&"claude/project".to_string()),
        "{described:?}"
    );
}

#[tokio::test]
async fn reconcile_scope_removes_only_managed_orphans() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = root.join("profile");
    install_fake_hosts(&home);
    let _ = &profile_root;

    let scope = MaterializationScope::global(MaterializationHost::Claude, home.clone());
    // A foreign dev skill sits alongside; reconcile with no active skills must
    // leave it untouched and report nothing removed for it.
    let foreign_dir = scope.skills_dir().join("dev-only");
    std::fs::create_dir_all(&foreign_dir).unwrap();
    std::fs::write(
        foreign_dir.join("SKILL.md"),
        "---\nname: dev-only\ndescription: repo dev skill\n---\n\nDev.\n",
    )
    .unwrap();

    let report = reconcile_scope(&scope, &[]).unwrap();
    assert!(
        report.removed.is_empty(),
        "foreign skill must not be enumerated for removal"
    );
    assert!(foreign_dir.join("SKILL.md").is_file());
}
