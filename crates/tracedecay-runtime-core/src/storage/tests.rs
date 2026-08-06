#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::cell::RefCell;
    use std::sync::{Arc, Barrier};

    #[test]
    fn repository_marker_keeps_the_existing_store_when_fallback_identity_differs() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("repo");
        let profile_root = dir.path().join("profile");
        fs::create_dir_all(&project_root).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&project_root)
            .status()
            .unwrap();
        assert!(init.success(), "the fixture repository must initialize");

        let fallback_id = default_profile_project_id(&project_root);
        let existing_id = "proj_existing_marker_store";
        assert_ne!(
            fallback_id, existing_id,
            "the fixture must model a changed fallback derivation"
        );
        let existing_store = profile_root.join("projects").join(existing_id);
        fs::create_dir_all(&existing_store).unwrap();
        let sentinel = existing_store.join("existing-store-sentinel");
        fs::write(&sentinel, "do not orphan").unwrap();
        assert!(write_repository_identity_marker(&project_root, existing_id).unwrap());

        let resolved = resolve_layout(&project_root, &profile_root).unwrap();

        assert_eq!(
            resolved.identity.project_id.as_deref(),
            Some(existing_id),
            "persisted repository identity must outrank a newly-derived fallback id"
        );
        assert_eq!(resolved.data_root, existing_store);
        assert!(
            sentinel.is_file(),
            "the selected existing store must stay intact"
        );
    }

    /// Writes a profile-sharded `CodeProject` manifest for `project_id`
    /// pointing at `project_root`, the fixture every store-selection test
    /// below builds its profile out of.
    fn write_manifest(profile_root: &Path, project_id: &str, project_root: &Path) {
        let data_root = profile_root.join("projects").join(project_id);
        fs::create_dir_all(&data_root).unwrap();
        write_store_manifest_to_path(
            &data_root.join(STORE_MANIFEST_FILENAME),
            &StoreManifest {
                schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                project_id: Some(project_id.to_string()),
                store_kind: StoreKind::CodeProject,
                storage_mode: StorageMode::ProfileSharded,
                project_root: project_root.to_path_buf(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn exact_root_manifest_overrides_shared_git_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("repo");
        let unrelated_root = dir.path().join("unrelated");
        let profile_root = dir.path().join("profile");
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(&unrelated_root).unwrap();
        write_manifest(&profile_root, "proj_exact", &project_root);
        write_manifest(&profile_root, "proj_unrelated", &unrelated_root);

        let resolver_calls = RefCell::new(Vec::new());
        let (layouts, selected_is_sole_exact_root) =
            matching_legacy_profile_layouts_with_git_resolver(
                &project_root,
                &profile_root,
                None,
                |root| {
                    resolver_calls.borrow_mut().push(root.to_path_buf());
                    Some(dir.path().join("shared.git"))
                },
            )
            .unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(
            layouts[0].identity.project_id.as_deref(),
            Some("proj_exact")
        );
        assert!(!selected_is_sole_exact_root);
        assert!(
            resolver_calls.borrow().is_empty(),
            "exact-root selection must not invoke shared-Git discovery"
        );

        resolver_calls.borrow_mut().clear();
        let (layouts, selected_is_sole_exact_root) =
            matching_legacy_profile_layouts_with_git_resolver(
                &project_root,
                &profile_root,
                Some("proj_exact"),
                |root| {
                    resolver_calls.borrow_mut().push(root.to_path_buf());
                    Some(dir.path().join("shared.git"))
                },
            )
            .unwrap();
        assert_eq!(layouts.len(), 1);
        assert_eq!(
            layouts[0].identity.project_id.as_deref(),
            Some("proj_unrelated")
        );
        assert!(
            selected_is_sole_exact_root,
            "the caller decides whether the selected exact root outranks recovery"
        );
        assert_eq!(
            resolver_calls.borrow().as_slice(),
            [project_root, unrelated_root],
            "an excluded selected exact root must retain shared-Git recovery"
        );
    }

    #[test]
    fn exact_root_manifest_without_project_id_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("repo");
        let profile_root = dir.path().join("profile");
        let data_root = profile_root.join("projects").join("legacy-missing-id");
        fs::create_dir_all(&project_root).unwrap();
        fs::create_dir_all(&data_root).unwrap();
        write_store_manifest_to_path(
            &data_root.join(STORE_MANIFEST_FILENAME),
            &StoreManifest {
                schema_version: STORE_MANIFEST_SCHEMA_VERSION,
                project_id: None,
                store_kind: StoreKind::CodeProject,
                storage_mode: StorageMode::ProfileSharded,
                project_root: project_root.clone(),
                data_root,
                graph_db_relpath: "tracedecay.db".into(),
                sessions_db_relpath: "sessions.db".into(),
                branch_meta_relpath: "branch-meta.json".into(),
            },
        )
        .unwrap();

        let error = matching_legacy_profile_layouts_with_git_resolver(
            &project_root,
            &profile_root,
            None,
            |_| None,
        )
        .expect_err("missing project_id must fail closed");
        assert!(error.to_string().contains("project_id is missing"));
    }

    #[test]
    fn non_exact_identity_retains_historical_git_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let main_root = dir.path().join("repo");
        let worktree_root = dir.path().join("repo-worktree");
        let historical_root = dir.path().join("historical-worktree");
        let profile_root = dir.path().join("profile");
        for root in [&main_root, &worktree_root, &historical_root] {
            fs::create_dir_all(root).unwrap();
        }
        write_manifest(&profile_root, "proj_selected", &main_root);
        write_manifest(&profile_root, "proj_historical", &historical_root);

        let resolver_calls = RefCell::new(Vec::new());
        let (layouts, selected_is_sole_exact_root) =
            matching_legacy_profile_layouts_with_git_resolver(
                &worktree_root,
                &profile_root,
                Some("proj_selected"),
                |root| {
                    resolver_calls.borrow_mut().push(root.to_path_buf());
                    Some(dir.path().join("shared.git"))
                },
            )
            .unwrap();

        assert_eq!(layouts.len(), 1);
        assert!(!selected_is_sole_exact_root);
        assert_eq!(
            resolver_calls.borrow().as_slice(),
            [worktree_root, historical_root],
            "a selected identity from a sibling root must retain shared-Git recovery"
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn detached_linked_worktree_recovers_its_shared_legacy_profile_store() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary");
        let linked = dir.path().join("linked");
        let profile_root = dir.path().join("profile");
        fs::create_dir_all(&primary).unwrap();
        git(&primary, &["init", "--initial-branch=main"]);
        git(&primary, &["config", "user.email", "test@example.com"]);
        git(&primary, &["config", "user.name", "test"]);
        fs::write(primary.join("file.txt"), "x").unwrap();
        git(&primary, &["add", "file.txt"]);
        git(&primary, &["commit", "-m", "seed"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-b",
                "linked",
                linked.to_str().expect("utf-8 path"),
            ],
        );
        git(&linked, &["checkout", "--detach"]);
        assert!(crate::worktree::is_detached_linked_worktree(&linked));

        write_manifest(&profile_root, "proj_historical", &primary);

        let (layouts, selected_is_sole_exact_root) =
            matching_legacy_profile_layouts(&linked, &profile_root, None).unwrap();

        assert!(!selected_is_sole_exact_root);
        assert_eq!(layouts.len(), 1);
        assert_eq!(
            layouts[0].identity.project_id.as_deref(),
            Some("proj_historical")
        );
        assert_eq!(
            layouts[0].data_root,
            profile_root.join("projects").join("proj_historical")
        );
    }

    #[test]
    fn append_line_keeps_concurrent_jsonl_writes_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(
            dir.path()
                .canonicalize()
                .unwrap()
                .join("hook_analytics.jsonl"),
        );
        let writers = 8;
        let lines_per_writer = 100;
        let barrier = Arc::new(Barrier::new(writers));
        let mut handles = Vec::new();

        for writer in 0..writers {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for line in 0..lines_per_writer {
                    let payload = serde_json::json!({
                        "event": "hook_invoked",
                        "writer": writer,
                        "line": line,
                        "padding": "x".repeat(4096),
                    });
                    PrivateStoreIo::append_line(&path, &payload.to_string()).unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let contents = std::fs::read_to_string(&*path).unwrap();
        let rows = contents.lines().collect::<Vec<_>>();
        assert_eq!(rows.len(), writers * lines_per_writer);
        for row in rows {
            serde_json::from_str::<Value>(row).unwrap();
        }
        assert!(append_lock_path(&path).is_file());
    }

    #[test]
    #[cfg(unix)]
    fn symlink_guard_skips_leading_system_alias_but_rejects_managed_tail() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();

        // A normal store path below a possibly symlinked system temp root
        // (macOS /var -> /private/var) must be tolerated.
        let real = root.join("real");
        std::fs::create_dir_all(real.join("store")).unwrap();
        PrivateStoreIo::append_line(&real.join("store").join("f.jsonl"), "{\"n\":1}")
            .expect("normal store path must not be rejected");

        // A symlinked directory is caught when the write path ensures it:
        // the directory is then the checked final component.
        let parent_link = root.join("plink");
        symlink(real.join("store"), &parent_link).unwrap();
        let err = PrivateStoreIo::create_dir_all(&parent_link).unwrap_err();
        assert!(
            err.to_string().contains("must not contain symlinks"),
            "{err}"
        );

        // A symlinked final component is rejected.
        let target = real.join("store").join("h.jsonl");
        std::fs::write(&target, "").unwrap();
        let file_link = real.join("store").join("h-link.jsonl");
        symlink(&target, &file_link).unwrap();
        let err = PrivateStoreIo::append_line(&file_link, "{}").unwrap_err();
        assert!(
            err.to_string().contains("must not contain symlinks"),
            "{err}"
        );
    }

    #[test]
    fn append_line_uses_a_reusable_sidecar_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize: on macOS the tempdir lives under /var -> /private/var,
        // which the symlink guard would otherwise reject.
        let path = dir.path().canonicalize().unwrap().join("ledger.jsonl");
        let lock_path = append_lock_path(&path);
        assert_eq!(lock_path.file_name().unwrap(), "ledger.jsonl.lock");

        PrivateStoreIo::append_line(&path, "{\"n\":1}").unwrap();
        assert!(lock_path.is_file(), "sidecar lock file should be created");

        // A second append reuses the same sidecar and never locks the data
        // handle, so it must succeed and leave both entries intact.
        PrivateStoreIo::append_line(&path, "{\"n\":2}").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 2);
        assert!(lock_path.is_file());
        // The lock file is metadata only; it must not accumulate ledger bytes.
        assert_eq!(std::fs::metadata(&lock_path).unwrap().len(), 0);
    }

    #[test]
    #[cfg(unix)]
    fn private_lock_file_is_created_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().canonicalize().unwrap().join("private.lock");
        let file = open_lock_file(&lock_path, true).unwrap();
        drop(file);

        assert_eq!(
            std::fs::metadata(lock_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn append_line_leaves_data_file_writable() {
        let dir = tempfile::tempdir().unwrap();
        // Canonicalize: on macOS the tempdir lives under /var -> /private/var,
        // which the symlink guard would otherwise reject.
        let path = dir.path().canonicalize().unwrap().join("perms.jsonl");

        PrivateStoreIo::append_line(&path, "{\"a\":1}").unwrap();
        PrivateStoreIo::append_line(&path, "{\"a\":2}").unwrap();

        let meta = std::fs::metadata(&path).unwrap();
        // Guards against any Windows FILE_ATTRIBUTE_READONLY regression and any
        // Unix mode regression that would strip the owner write bit.
        assert!(
            !meta.permissions().readonly(),
            "appended data file must stay writable"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o600,
                "private data file must retain owner-only 0o600 permissions"
            );
        }

        // The file must still be openable for a further append after the cycle.
        PrivateStoreIo::append_line(&path, "{\"a\":3}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
    }
}
