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
    reconcile_detected_scopes, reconcile_scope, remove_materialized_skill, resolve_project_root,
};

/// Stable installation id for the local test profile; distinct from `INSTALL_B`
/// so cross-profile removal protection can be exercised.
const INSTALL: &str = "install-test-a";
const INSTALL_B: &str = "install-test-b";

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
async fn materialize_on_activate_writes_global_scope_only_by_default() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let project = root.join("project");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);
    install_fake_hosts(&project);

    activate_skill(&profile_root, "code-slop-cleanup").await;

    let (results, errors) = reconcile_detected_scopes(&profile_root, &home, &project);
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    // 2 hosts x 2 scopes (project + global) are still detected...
    assert_eq!(results.len(), 4, "expected 4 detected scopes");

    // ...but a default (global-scoped) skill materializes ONLY into the user's
    // global host dirs — never into the project checkout (no repo pollution).
    let global = [
        home.join(".claude/skills/code-slop-cleanup/SKILL.md"),
        home.join(".codex/skills/code-slop-cleanup/SKILL.md"),
    ];
    for path in &global {
        assert!(path.is_file(), "missing global skill at {}", path.display());
    }
    assert!(
        home.join(".claude/skills/code-slop-cleanup/references/checklist.md")
            .is_file()
    );

    let project_paths = [
        project.join(".claude/skills/code-slop-cleanup/SKILL.md"),
        project.join(".codex/skills/code-slop-cleanup/SKILL.md"),
    ];
    for path in &project_paths {
        assert!(
            !path.exists(),
            "global skill must not pollute project at {}",
            path.display()
        );
    }

    // Only the two global scopes wrote a file; project scopes wrote nothing.
    let total_written: usize = results.iter().map(|r| r.report.written_count()).sum();
    assert_eq!(total_written, 2, "only global claude+codex should write");
    for result in &results {
        let expected = match result.scope.describe().as_str() {
            desc if desc.ends_with("/global") => 1,
            _ => 0,
        };
        assert_eq!(
            result.report.written_count(),
            expected,
            "scope {}",
            result.scope.describe()
        );
    }
}

#[tokio::test]
async fn isolated_profile_cannot_remove_user_profile_materializations() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let project = root.join("project");
    let user_profile = home.join(".tracedecay");
    let isolated_profile = root.join("test-profile/.tracedecay");
    install_fake_hosts(&home);

    activate_skill(&user_profile, "code-slop-cleanup").await;
    reconcile_detected_scopes(&user_profile, &home, &project);
    let materialized = home.join(".codex/skills/code-slop-cleanup/SKILL.md");
    assert!(materialized.is_file());

    let (results, errors) = reconcile_detected_scopes(&isolated_profile, &home, &project);
    assert!(results.is_empty());
    assert!(errors.is_empty());
    assert!(materialized.is_file());
}

#[tokio::test]
async fn materialized_file_carries_provenance_frontmatter() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
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
    let profile_root = home.join(".tracedecay");
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
    let profile_root = home.join(".tracedecay");
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
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home.clone());
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    let first = materialize_skill(&scope, &skill, INSTALL).unwrap();
    assert_eq!(first.action, MaterializeAction::Written);

    // Change the body: the content-hash changes, so the reconciler rewrites.
    skill.body_markdown = "# Cleanup v2\n\nNow with extra rigor.".to_string();
    let second = materialize_skill(&scope, &skill, INSTALL).unwrap();
    assert_eq!(second.action, MaterializeAction::Written);
    let contents = std::fs::read_to_string(skill_md(&scope, "code-slop-cleanup")).unwrap();
    assert!(contents.contains("Now with extra rigor."));

    // A third pass with the same content is a no-op.
    let third = materialize_skill(&scope, &skill, INSTALL).unwrap();
    assert_eq!(third.action, MaterializeAction::Unchanged);
}

#[tokio::test]
async fn metadata_update_re_materializes_the_file() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill, INSTALL).unwrap();

    skill.metadata.summary = "Use when performing a strict cleanup before review.".to_string();
    let updated = materialize_skill(&scope, &skill, INSTALL).unwrap();

    assert_eq!(updated.action, MaterializeAction::Written);
    let contents = std::fs::read_to_string(skill_md(&scope, "code-slop-cleanup")).unwrap();
    assert!(contents.contains("performing a strict cleanup"));
    assert_eq!(
        materialize_skill(&scope, &skill, INSTALL).unwrap().action,
        MaterializeAction::Unchanged
    );
}

#[tokio::test]
async fn support_update_removes_only_stale_owned_files() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill, INSTALL).unwrap();
    let dir = scope.skills_dir().join("code-slop-cleanup");
    let stale = dir.join("references/checklist.md");
    let foreign = dir.join("references/user-notes.md");
    std::fs::write(&foreign, "keep me\n").unwrap();

    skill.support_files =
        vec![ManagedSupportFile::new("references/guide.md", b"new guide\n".to_vec()).unwrap()];
    let updated = materialize_skill(&scope, &skill, INSTALL).unwrap();

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
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let mut skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill, INSTALL).unwrap();
    let support = scope
        .skills_dir()
        .join("code-slop-cleanup/references/checklist.md");
    std::fs::write(&support, "user edit\n").unwrap();

    skill.support_files[0].bytes = b"automation update\n".to_vec();
    let updated = materialize_skill(&scope, &skill, INSTALL).unwrap();

    assert_eq!(updated.action, MaterializeAction::SkippedForked);
    assert_eq!(std::fs::read_to_string(support).unwrap(), "user edit\n");
}

#[tokio::test]
async fn deactivation_removes_only_owned_artifacts() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill, INSTALL).unwrap();
    let dir = scope.skills_dir().join("code-slop-cleanup");
    let foreign = dir.join("references/user-notes.md");
    std::fs::write(&foreign, "keep me\n").unwrap();

    let removed = remove_materialized_skill(&scope, "code-slop-cleanup", INSTALL).unwrap();

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
    let profile_root = home.join(".tracedecay");
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

    let error = materialize_skill(&scope, &skill, INSTALL).unwrap_err();

    assert!(error.to_string().contains("symlink"), "{error}");
    assert!(std::fs::read_dir(external).unwrap().next().is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn nested_support_symlink_is_rejected_before_write() {
    use std::os::unix::fs::symlink;

    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
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

    let error = materialize_skill(&scope, &skill, INSTALL).unwrap_err();

    assert!(error.to_string().contains("symlink"), "{error}");
    assert!(!external.join("checklist.md").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn nested_support_symlink_is_rejected_before_remove() {
    use std::os::unix::fs::symlink;

    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
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
    materialize_skill(&scope, &skill, INSTALL).unwrap();
    let dir = scope.skills_dir().join("code-slop-cleanup");
    let support = dir.join("references/checklist.md");
    let contents = std::fs::read(&support).unwrap();
    std::fs::remove_file(&support).unwrap();
    std::fs::remove_dir(dir.join("references")).unwrap();
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(external.join("checklist.md"), &contents).unwrap();
    symlink(&external, dir.join("references")).unwrap();

    let error = remove_materialized_skill(&scope, "code-slop-cleanup", INSTALL).unwrap_err();

    assert!(error.to_string().contains("symlink"), "{error}");
    assert_eq!(
        std::fs::read(external.join("checklist.md")).unwrap(),
        contents
    );
}

#[tokio::test]
async fn fork_protection_leaves_user_edited_file_and_doctor_flags_it() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home.clone());
    let skill = tracedecay::automation::managed_skills::load_managed_skill(
        &profile_root,
        "code-slop-cleanup",
    )
    .await
    .unwrap();
    materialize_skill(&scope, &skill, INSTALL).unwrap();
    let path = skill_md(&scope, "code-slop-cleanup");

    // User edits the materialized body (the content-hash no longer matches).
    let edited = format!(
        "{}\n\n<!-- user note: keep this -->\n",
        std::fs::read_to_string(&path).unwrap()
    );
    std::fs::write(&path, &edited).unwrap();

    // Re-materialize: the reconciler must NOT clobber the fork.
    let action = materialize_skill(&scope, &skill, INSTALL).unwrap();
    assert_eq!(action.action, MaterializeAction::SkippedForked);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);

    // Doctor flags the fork.
    let drift = doctor_scope(&scope, std::slice::from_ref(&skill), INSTALL);
    assert!(
        drift.iter().any(
            |d| matches!(d, SkillDrift::Forked { skill_id, .. } if skill_id == "code-slop-cleanup")
        ),
        "expected Forked drift, got {drift:?}"
    );

    // A deactivate reconcile must also refuse to delete the fork.
    let removed = remove_materialized_skill(&scope, "code-slop-cleanup", INSTALL).unwrap();
    assert_eq!(removed, RemoveAction::SkippedForked);
    assert!(path.is_file(), "forked file must survive removal");
}

#[tokio::test]
async fn foreign_file_is_never_touched_and_doctor_reports_conflict() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
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

    let action = materialize_skill(&scope, &skill, INSTALL).unwrap();
    assert_eq!(action.action, MaterializeAction::SkippedForeign);
    assert_eq!(
        std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
        foreign
    );

    // Removal never touches a foreign file either.
    assert_eq!(
        remove_materialized_skill(&scope, "code-slop-cleanup", INSTALL).unwrap(),
        RemoveAction::SkippedForeign
    );

    let drift = doctor_scope(&scope, std::slice::from_ref(&skill), INSTALL);
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
    let profile_root = home.join(".tracedecay");
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
    materialize_skill(&scope, &skill, INSTALL).unwrap();
    let orphan_drift = doctor_scope(&scope, &[], INSTALL);
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
    let profile_root = home.join(".tracedecay");
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

    let report = reconcile_scope(&scope, &[], INSTALL).unwrap();
    assert!(
        report.removed.is_empty(),
        "foreign skill must not be enumerated for removal"
    );
    assert!(foreign_dir.join("SKILL.md").is_file());
}

// ---------------------------------------------------------------------------
// Adversarial-review findings on #362/#366 (skill-materialization hardening).
// ---------------------------------------------------------------------------

use tracedecay::automation::managed_skills::ManagedSkill;

async fn load_skill(profile_root: &Path, id: &str) -> ManagedSkill {
    tracedecay::automation::managed_skills::load_managed_skill(profile_root, id)
        .await
        .unwrap()
}

/// #1 A pristine materialized file whose manifest sidecar is lost (fresh clone,
/// gitignored dotfile, dotfile-sync) must be re-derived from disk — not frozen
/// as `SkippedForked` and mislabeled by doctor.
#[tokio::test]
async fn lost_manifest_pristine_file_is_rederived_not_forked() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let skill = load_skill(&profile_root, "code-slop-cleanup").await;
    assert_eq!(
        materialize_skill(&scope, &skill, INSTALL).unwrap().action,
        MaterializeAction::Written
    );

    // Simulate a lost manifest sidecar (SKILL.md committed, dot-manifest not).
    let dir = scope.skills_dir().join("code-slop-cleanup");
    let manifest = dir.join(".tracedecay-materialization.json");
    std::fs::remove_file(&manifest).unwrap();

    // Re-materialize: the reconciler must recognize the pristine package and
    // re-derive the manifest, never SkippedForked.
    let again = materialize_skill(&scope, &skill, INSTALL).unwrap();
    assert_ne!(again.action, MaterializeAction::SkippedForked);
    assert!(manifest.is_file(), "manifest should be re-derived");

    // Doctor must NOT report the pristine file as a user fork.
    let drift = doctor_scope(&scope, std::slice::from_ref(&skill), INSTALL);
    assert!(
        !drift.iter().any(|d| matches!(d, SkillDrift::Forked { .. })),
        "pristine file wrongly flagged as forked: {drift:?}"
    );

    // And it stays removable (no doctor-nag / update-refuses loop).
    std::fs::remove_file(&manifest).unwrap();
    assert_eq!(
        remove_materialized_skill(&scope, "code-slop-cleanup", INSTALL).unwrap(),
        RemoveAction::Removed
    );
    assert!(!dir.join("SKILL.md").exists());
}

/// #2 Materialization must resolve the enclosing project root, so running from a
/// subdirectory targets the repo root rather than the subdir.
#[tokio::test]
async fn resolve_project_root_finds_enclosing_repo_root() {
    let (_temp, root) = canonical_tempdir();
    let repo = root.join("repo");
    let subdir = repo.join("crates/inner/src");
    std::fs::create_dir_all(&subdir).unwrap();
    std::fs::create_dir_all(repo.join(".tracedecay")).unwrap();
    std::fs::write(repo.join(".tracedecay/tracedecay.db"), b"stub").unwrap();

    assert_eq!(resolve_project_root(&subdir), repo);
    assert_eq!(resolve_project_root(&repo), repo);
}

/// #3 A default (global) skill materializes into the global scope but not into a
/// project scope, even when both host dirs exist. (Complements
/// `materialize_on_activate_writes_global_scope_only_by_default`.)
#[tokio::test]
async fn project_scope_filters_out_global_skills() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let project = root.join("project");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);
    install_fake_hosts(&project);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    reconcile_detected_scopes(&profile_root, &home, &project);

    assert!(
        home.join(".claude/skills/code-slop-cleanup/SKILL.md")
            .is_file()
    );
    assert!(
        !project.join(".claude/skills/code-slop-cleanup").exists(),
        "no project package dir should be created for a global skill"
    );
    // Doctor must not report Missing project drift for a global skill.
    let scopes = doctor_detected_scopes(&profile_root, &home, &project).unwrap();
    for (scope, drift) in &scopes {
        if scope.describe().ends_with("/project") {
            assert!(drift.is_empty(), "unexpected project drift: {drift:?}");
        }
    }
}

/// #4 Project-scope orphan cleanup must never delete a committed package another
/// installation authored; the same file in a global scope is still removable.
#[tokio::test]
async fn project_scope_protects_another_installations_committed_package() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let project = root.join("project");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);
    install_fake_hosts(&project);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let skill = load_skill(&profile_root, "code-slop-cleanup").await;

    // Installation A materializes into a project checkout and "commits" it.
    let project_scope = MaterializationScope::project(MaterializationHost::Claude, project);
    materialize_skill(&project_scope, &skill, INSTALL).unwrap();
    let committed = project_scope
        .skills_dir()
        .join("code-slop-cleanup/SKILL.md");
    assert!(committed.is_file());

    // Installation B (skill not in its profile) must NOT delete A's committed
    // file — it is reported, never auto-removed.
    assert_eq!(
        remove_materialized_skill(&project_scope, "code-slop-cleanup", INSTALL_B).unwrap(),
        RemoveAction::SkippedForeign
    );
    assert!(committed.is_file(), "another user's file must survive");

    // Installation A can still remove its own file.
    assert_eq!(
        remove_materialized_skill(&project_scope, "code-slop-cleanup", INSTALL).unwrap(),
        RemoveAction::Removed
    );

    // Global scope is the user's own home: cross-installation removal is fine.
    let global_scope = MaterializationScope::global(MaterializationHost::Claude, home);
    materialize_skill(&global_scope, &skill, INSTALL).unwrap();
    assert_eq!(
        remove_materialized_skill(&global_scope, "code-slop-cleanup", INSTALL_B).unwrap(),
        RemoveAction::Removed
    );
}

/// #4b A committed project package another installation authored (provenance
/// clean, just a different `materialized_by`) must be reported as
/// `ForeignOrphan`, never `Orphan` — doctor must not prescribe an `update`
/// remove that the remove path refuses to perform. Predicate agreement is
/// checked against `remove_materialized_skill`.
#[tokio::test]
async fn doctor_reports_foreign_installation_package_as_foreign_orphan() {
    let (_temp, root) = canonical_tempdir();
    let project = root.join("project");
    let profile_root = root.join("profile");
    install_fake_hosts(&project);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let skill = load_skill(&profile_root, "code-slop-cleanup").await;

    // Installation B authored (and "committed") the package into the checkout.
    let scope = MaterializationScope::project(MaterializationHost::Claude, project);
    materialize_skill(&scope, &skill, INSTALL_B).unwrap();

    // Installation A runs doctor with the skill inactive: foreign orphan, not
    // a plain orphan (which would nag to run `tracedecay update`).
    let drift = doctor_scope(&scope, &[], INSTALL);
    assert_eq!(
        drift
            .iter()
            .filter(|d| matches!(
                d,
                SkillDrift::ForeignOrphan { skill_id, .. } if skill_id == "code-slop-cleanup"
            ))
            .count(),
        1,
        "expected exactly one ForeignOrphan, got {drift:?}"
    );
    assert!(
        !drift.iter().any(|d| matches!(d, SkillDrift::Orphan { .. })),
        "foreign package must never be a plain Orphan: {drift:?}"
    );

    // Predicate agreement: the remove path also refuses this package.
    assert_eq!(
        remove_materialized_skill(&scope, "code-slop-cleanup", INSTALL).unwrap(),
        RemoveAction::SkippedForeign
    );
}

/// #4c A legacy manifest with no `materialized_by` field (pre-provenance shape),
/// or no manifest at all, has an unknown author; doctor must treat it as a
/// `ForeignOrphan` because the remove path also refuses to delete it.
#[tokio::test]
async fn doctor_reports_legacy_manifestless_author_as_foreign_orphan() {
    let (_temp, root) = canonical_tempdir();
    let project = root.join("project");
    let profile_root = root.join("profile");
    install_fake_hosts(&project);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let skill = load_skill(&profile_root, "code-slop-cleanup").await;

    let scope = MaterializationScope::project(MaterializationHost::Claude, project);
    materialize_skill(&scope, &skill, INSTALL).unwrap();
    let dir = scope.skills_dir().join("code-slop-cleanup");
    let manifest = dir.join(".tracedecay-materialization.json");

    // Legacy shape: strip `materialized_by` but keep the manifest otherwise
    // valid (serde default makes the field optional).
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("materialized_by");
    std::fs::write(&manifest, serde_json::to_string_pretty(&json).unwrap()).unwrap();

    let drift = doctor_scope(&scope, &[], INSTALL);
    assert!(
        drift.iter().any(|d| matches!(
            d,
            SkillDrift::ForeignOrphan { skill_id, .. } if skill_id == "code-slop-cleanup"
        )),
        "legacy manifest-less author must be ForeignOrphan, got {drift:?}"
    );

    // Removing the manifest entirely (lost sidecar) is also unknown-author.
    std::fs::remove_file(&manifest).unwrap();
    let drift = doctor_scope(&scope, &[], INSTALL);
    assert!(
        drift.iter().any(|d| matches!(
            d,
            SkillDrift::ForeignOrphan { skill_id, .. } if skill_id == "code-slop-cleanup"
        )),
        "missing manifest must be ForeignOrphan, got {drift:?}"
    );
}

/// #4d A package this installation authored, now inactive, remains a plain
/// `Orphan` — `tracedecay update` correctly removes self-authored packages.
#[tokio::test]
async fn doctor_reports_own_inactive_skill_as_plain_orphan() {
    let (_temp, root) = canonical_tempdir();
    let project = root.join("project");
    let profile_root = root.join("profile");
    install_fake_hosts(&project);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let skill = load_skill(&profile_root, "code-slop-cleanup").await;

    let scope = MaterializationScope::project(MaterializationHost::Claude, project);
    materialize_skill(&scope, &skill, INSTALL).unwrap();

    let drift = doctor_scope(&scope, &[], INSTALL);
    assert!(
        drift.iter().any(|d| matches!(
            d,
            SkillDrift::Orphan { skill_id, .. } if skill_id == "code-slop-cleanup"
        )),
        "self-authored inactive package must stay a plain Orphan, got {drift:?}"
    );
    assert!(
        !drift
            .iter()
            .any(|d| matches!(d, SkillDrift::ForeignOrphan { .. })),
        "self-authored package must never be ForeignOrphan: {drift:?}"
    );
}

/// #5 Concurrent transactions on the same package must serialize and never wedge
/// it as forked or leave a half-applied manifest.
#[tokio::test]
async fn concurrent_materialize_does_not_wedge_forked() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let skill = load_skill(&profile_root, "code-slop-cleanup").await;

    let actions: Vec<MaterializeAction> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| s.spawn(|| materialize_skill(&scope, &skill, INSTALL).unwrap().action))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    for action in &actions {
        assert_ne!(
            *action,
            MaterializeAction::SkippedForked,
            "a concurrent transaction wedged the package as forked"
        );
    }
    // Final state is a clean, unchanged, fork-free package.
    let final_pass = materialize_skill(&scope, &skill, INSTALL).unwrap();
    assert_eq!(final_pass.action, MaterializeAction::Unchanged);
    let drift = doctor_scope(&scope, std::slice::from_ref(&skill), INSTALL);
    assert!(
        drift.is_empty(),
        "unexpected drift after concurrency: {drift:?}"
    );
}

/// #6 A symlinked scope root (stow/chezmoi dotfiles) must be followed, not
/// rejected: materialization writes through the link.
#[cfg(unix)]
#[tokio::test]
async fn symlinked_scope_root_materializes_through_link() {
    use std::os::unix::fs::symlink;

    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let real_claude = root.join("dotfiles/claude");
    let profile_root = home.join(".tracedecay");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&real_claude).unwrap();
    // ~/.claude is a symlink into a dotfiles repo — a normal setup.
    symlink(&real_claude, home.join(".claude")).unwrap();

    activate_skill(&profile_root, "code-slop-cleanup").await;
    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let skill = load_skill(&profile_root, "code-slop-cleanup").await;

    let action = materialize_skill(&scope, &skill, INSTALL).unwrap();
    assert_eq!(action.action, MaterializeAction::Written);
    assert!(
        real_claude
            .join("skills/code-slop-cleanup/SKILL.md")
            .is_file(),
        "file should be written through the symlinked scope root"
    );
}

/// #7/#8 (doctor) One package whose internal check errors must not suppress
/// drift reporting for the rest of the scope.
#[cfg(unix)]
#[tokio::test]
async fn doctor_reports_other_drift_when_one_package_errors() {
    use std::os::unix::fs::symlink;

    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let external = root.join("external");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);
    std::fs::create_dir_all(&external).unwrap();

    activate_skill(&profile_root, "good-skill").await;
    activate_skill(&profile_root, "broken-skill").await;
    let good = load_skill(&profile_root, "good-skill").await;
    let broken = load_skill(&profile_root, "broken-skill").await;

    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    materialize_skill(&scope, &broken, INSTALL).unwrap();

    // Replace broken's support dir with a symlink so its check errors.
    let dir = scope.skills_dir().join("broken-skill");
    std::fs::remove_file(dir.join("references/checklist.md")).unwrap();
    std::fs::remove_dir(dir.join("references")).unwrap();
    symlink(&external, dir.join("references")).unwrap();

    // good-skill is active but not materialized -> Missing; broken errors.
    let drift = doctor_scope(&scope, &[good, broken], INSTALL);
    assert!(
        drift
            .iter()
            .any(|d| matches!(d, SkillDrift::Missing { skill_id, .. } if skill_id == "good-skill")),
        "healthy Missing drift suppressed by a broken package: {drift:?}"
    );
    assert!(
        drift
            .iter()
            .any(|d| matches!(d, SkillDrift::Warning { .. })),
        "broken package should surface as a Warning: {drift:?}"
    );
}

/// #8 (reconcile) One poisoned package must not block materialization of the
/// rest of the scope; its failure is recorded, not fatal.
#[cfg(unix)]
#[tokio::test]
async fn reconcile_continues_past_one_poisoned_package() {
    use std::os::unix::fs::symlink;

    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let external = root.join("external");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);
    std::fs::create_dir_all(&external).unwrap();

    activate_skill(&profile_root, "good-skill").await;
    activate_skill(&profile_root, "broken-skill").await;
    let good = load_skill(&profile_root, "good-skill").await;
    let broken = load_skill(&profile_root, "broken-skill").await;

    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    // Poison broken-skill's package slot with a symlink escaping the tree.
    std::fs::create_dir_all(scope.skills_dir()).unwrap();
    symlink(&external, scope.skills_dir().join("broken-skill")).unwrap();

    let report = reconcile_scope(&scope, &[good, broken], INSTALL).unwrap();

    assert_eq!(report.written_count(), 1, "good skill should still write");
    assert_eq!(
        report.errors.len(),
        1,
        "one poisoned package recorded: {:?}",
        report.errors
    );
    assert!(scope.skills_dir().join("good-skill/SKILL.md").is_file());
}

/// #9 Distinct ids that normalize to the same host slug are disambiguated (each
/// materializes to its own dir) and doctor warns instead of silently dropping
/// the second one.
#[tokio::test]
async fn colliding_host_slugs_are_disambiguated_and_warned() {
    let (_temp, root) = canonical_tempdir();
    let home = root.join("home");
    let profile_root = home.join(".tracedecay");
    install_fake_hosts(&home);

    // "team-sync" and "team_sync" both normalize to slug "team-sync".
    activate_skill(&profile_root, "team-sync").await;
    activate_skill(&profile_root, "team_sync").await;
    let a = load_skill(&profile_root, "team-sync").await;
    let b = load_skill(&profile_root, "team_sync").await;

    let scope = MaterializationScope::global(MaterializationHost::Claude, home);
    let report = reconcile_scope(&scope, &[a.clone(), b.clone()], INSTALL).unwrap();

    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert_eq!(report.materialized.len(), 2);
    for entry in &report.materialized {
        assert_eq!(
            entry.action,
            MaterializeAction::Written,
            "colliding skill silently dropped: {entry:?}"
        );
    }

    // Two distinct managed packages exist on disk.
    let managed: Vec<_> = std::fs::read_dir(scope.skills_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("SKILL.md").is_file())
        .collect();
    assert_eq!(managed.len(), 2, "both colliding skills must materialize");

    // Doctor warns about the collision for both.
    let drift = doctor_scope(&scope, &[a, b], INSTALL);
    let warnings = drift
        .iter()
        .filter(|d| matches!(d, SkillDrift::Warning { .. }))
        .count();
    assert!(warnings >= 2, "expected collision warnings, got {drift:?}");
}
