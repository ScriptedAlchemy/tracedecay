#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::Value;
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

    #[test]
    fn durable_private_directory_publish_and_remove_close_the_namespace() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("private").join("response-handles");
        PrivateStoreIo::create_dir_all_durable(&nested).unwrap();
        let record = nested.join("record.json");
        let temporary = nested.join("record.tmp");

        PrivateStoreIo::write_file_atomically_durable(&record, &temporary, b"payload").unwrap();
        assert_eq!(fs::read(&record).unwrap(), b"payload");
        assert!(PrivateStoreIo::remove_file_durable(&record).unwrap());
        assert!(!record.exists());
        assert!(!PrivateStoreIo::remove_file_durable(&record).unwrap());
    }

    #[test]
    fn durable_private_directory_rejects_relative_paths() {
        let error =
            PrivateStoreIo::create_dir_all_durable(std::path::Path::new("relative/private-store"))
                .expect_err("durable directory publication must require an absolute path");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn durable_private_directory_retry_reestablishes_a_failed_parent_barrier() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("response-handles");
        with_durable_namespace_sync_fault_for_test(2, || {
            PrivateStoreIo::create_dir_all_durable(&target)
        })
        .expect_err("the injected post-create parent sync must fail");
        assert!(target.is_dir());

        with_durable_namespace_sync_fault_for_test(1, || {
            PrivateStoreIo::create_dir_all_durable(&target)
        })
        .expect_err("an existing retry must still establish the parent barrier");
        PrivateStoreIo::create_dir_all_durable(&target).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn durable_remove_retry_reestablishes_a_failed_parent_barrier() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("response.json");
        fs::write(&target, b"payload").unwrap();
        with_durable_namespace_sync_fault_for_test(1, || {
            PrivateStoreIo::remove_file_durable(&target)
        })
        .expect_err("the injected post-remove parent sync must fail");
        assert!(!target.exists());

        with_durable_namespace_sync_fault_for_test(1, || {
            PrivateStoreIo::remove_file_durable(&target)
        })
        .expect_err("a missing retry must still establish the parent barrier");
        assert!(!PrivateStoreIo::remove_file_durable(&target).unwrap());
    }

    #[test]
    fn concurrent_durable_first_create_converges_on_one_directory() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("private").join("response-handles");
        let barrier = Arc::new(Barrier::new(2));
        let workers = [(), ()].map(|()| {
            let target = target.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                PrivateStoreIo::create_dir_all_durable(&target)
            })
        });

        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        assert!(target.is_dir());
    }

    #[test]
    fn post_rename_error_retains_the_complete_replacement_for_retry() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("record.json");
        let temporary = root.path().join("record.tmp");
        fs::write(&target, b"prior").unwrap();

        with_durable_atomic_write_fault_for_test(
            DurableAtomicWriteFaultForTest::AfterRename,
            || PrivateStoreIo::write_file_atomically_durable(&target, &temporary, b"replacement"),
        )
        .expect_err("post-rename durability fault must surface");

        assert_eq!(fs::read(&target).unwrap(), b"replacement");
        assert!(!temporary.exists());
    }

    #[test]
    fn durable_atomic_write_fault_is_scoped_to_one_write_attempt() {
        let root = tempfile::tempdir().unwrap();

        for (index, phase) in [
            DurableAtomicWriteFaultForTest::AfterTempSync,
            DurableAtomicWriteFaultForTest::AfterRename,
        ]
        .into_iter()
        .enumerate()
        {
            let target = root.path().join(format!("phase-{index}.json"));
            let temporary = root.path().join(format!("phase-{index}.tmp"));
            with_durable_atomic_write_fault_for_test(phase, || {
                PrivateStoreIo::write_file_atomically_durable(&target, &temporary, b"first")
            })
            .expect_err("the injected write must fail at its requested phase");

            let retry_target = root.path().join(format!("phase-{index}-retry.json"));
            let retry_temporary = root.path().join(format!("phase-{index}-retry.tmp"));
            PrivateStoreIo::write_file_atomically_durable(
                &retry_target,
                &retry_temporary,
                b"retry",
            )
            .expect("the next write must not inherit the consumed fault");
        }

        let invalid_target = root.path().join("invalid-target").join("record.json");
        let invalid_temporary = root.path().join("different-parent").join("record.tmp");
        with_durable_atomic_write_fault_for_test(
            DurableAtomicWriteFaultForTest::AfterRename,
            || {
                PrivateStoreIo::write_file_atomically_durable(
                    &invalid_target,
                    &invalid_temporary,
                    b"invalid",
                )
            },
        )
        .expect_err("an ordinary pre-injection validation error must surface");

        let after_error_target = root.path().join("after-error.json");
        let after_error_temporary = root.path().join("after-error.tmp");
        PrivateStoreIo::write_file_atomically_durable(
            &after_error_target,
            &after_error_temporary,
            b"after-error",
        )
        .expect("an ordinary error must not leak the injected fault to the next write");

        let unwind = std::panic::catch_unwind(|| {
            with_durable_atomic_write_fault_for_test(
                DurableAtomicWriteFaultForTest::AfterRename,
                || panic!("pre-consumption panic"),
            );
        });
        assert!(unwind.is_err());

        let after_panic_target = root.path().join("after-panic.json");
        let after_panic_temporary = root.path().join("after-panic.tmp");
        PrivateStoreIo::write_file_atomically_durable(
            &after_panic_target,
            &after_panic_temporary,
            b"after-panic",
        )
        .expect("a panic must not leak the injected fault to the next write");
    }

    #[test]
    fn durable_atomic_write_fault_is_isolated_to_the_injecting_thread() {
        for injecting_name in ["first", "second"] {
            let root = tempfile::tempdir().unwrap();
            let root = Arc::new(root.path().to_path_buf());
            let start = Arc::new(Barrier::new(2));
            let non_injecting_done = Arc::new(Barrier::new(2));
            let workers = ["first", "second"].map(|name| {
                let root = Arc::clone(&root);
                let start = Arc::clone(&start);
                let non_injecting_done = Arc::clone(&non_injecting_done);
                std::thread::spawn(move || {
                    let write = |suffix: &str| {
                        let target = root.join(format!("{name}-{suffix}.json"));
                        let temporary = root.join(format!("{name}-{suffix}.tmp"));
                        PrivateStoreIo::write_file_atomically_durable(
                            &target,
                            &temporary,
                            name.as_bytes(),
                        )
                    };
                    if name == injecting_name {
                        with_durable_atomic_write_fault_for_test(
                            DurableAtomicWriteFaultForTest::AfterRename,
                            || {
                                start.wait();
                                non_injecting_done.wait();
                                write("injected")
                            },
                        )
                        .expect_err("the injecting thread must receive its fault");
                    } else {
                        start.wait();
                        write("clean").expect("the non-injecting thread must succeed");
                        non_injecting_done.wait();
                    }
                    write("retry").expect("both subsequent writes must succeed");
                })
            });

            for worker in workers {
                worker.join().unwrap();
            }
        }
    }

    #[test]
    fn durable_namespace_sync_fault_scope_clears_on_unwind() {
        let unwind = std::panic::catch_unwind(|| {
            with_durable_namespace_sync_fault_for_test(1, || panic!("before sync"));
        });
        assert!(unwind.is_err());

        let root = tempfile::tempdir().unwrap();
        PrivateStoreIo::create_dir_all_durable(&root.path().join("after-panic")).unwrap();
    }

    #[test]
    fn linked_worktree_resolves_the_repository_identity_marker() {
        let dir = tempfile::tempdir().unwrap();
        let primary = dir.path().join("primary");
        let linked = dir.path().join("linked");
        let profile_root = dir.path().join("profile");
        fs::create_dir_all(&primary).unwrap();
        for args in [
            ["init", "--quiet"].as_slice(),
            ["config", "user.email", "test@example.com"].as_slice(),
            ["config", "user.name", "TraceDecay Test"].as_slice(),
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&primary)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::write(primary.join("file.rs"), "pub fn shared() {}\n").unwrap();
        for args in [["add", "."].as_slice(), ["commit", "-m", "seed"].as_slice()] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&primary)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        assert!(
            std::process::Command::new("git")
                .args(["worktree", "add", "-b", "linked"])
                .arg(&linked)
                .current_dir(&primary)
                .status()
                .unwrap()
                .success()
        );

        let project_id = "proj_primary_store";
        assert!(write_repository_identity_marker(&primary, project_id).unwrap());

        let layout = resolve_persisted_layout(&linked, &profile_root)
            .unwrap()
            .expect("repository identity resolves the canonical project store");
        assert_eq!(layout.identity.project_id.as_deref(), Some(project_id));
        assert_eq!(
            layout.data_root,
            profile_root.join("projects").join(project_id)
        );
        assert_eq!(layout.project_root, linked);
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
