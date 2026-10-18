/// The Bugbot review asked whether the two id paths can disagree about one
/// repository: `default_profile_project_id` hashes what
/// `repository_identity_root` returns, while the primary-checkout fallback
/// hashes an explicitly canonicalized path. These exercise the ways a caller
/// can hand in a path that is spelled differently from its canonical form.
#[cfg(test)]
mod identity_root_canonicalization_tests {
    use super::*;

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

    /// A repository with one linked worktree, returned as (primary, linked).
    fn repository(temp: &Path) -> (PathBuf, PathBuf) {
        let primary = temp.join("primary");
        fs::create_dir_all(&primary).expect("create primary");
        git(&primary, &["init", "--initial-branch=main"]);
        git(&primary, &["config", "user.email", "test@example.com"]);
        git(&primary, &["config", "user.name", "test"]);
        fs::write(primary.join("file.txt"), "x").expect("seed file");
        git(&primary, &["add", "file.txt"]);
        git(&primary, &["commit", "-m", "seed"]);

        let linked = temp.join("linked");
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
        (primary, linked)
    }

    #[test]
    fn a_trailing_separator_does_not_change_the_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, linked) = repository(temp.path());
        let expected = default_profile_project_id(&primary);

        for root in [&primary, &linked] {
            let mut spelled = root.as_os_str().to_os_string();
            spelled.push("/");
            assert_eq!(
                default_profile_project_id(Path::new(&spelled)),
                expected,
                "trailing separator changed the id for {}",
                root.display()
            );
        }
    }

    #[test]
    fn a_dot_dot_segment_does_not_change_the_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, linked) = repository(temp.path());
        let expected = default_profile_project_id(&primary);

        for root in [&primary, &linked] {
            let name = root.file_name().expect("checkout name");
            let indirect = root.join("..").join(name);
            assert_eq!(
                default_profile_project_id(&indirect),
                expected,
                "a .. segment changed the id for {}",
                root.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_checkout_does_not_change_the_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, linked) = repository(temp.path());
        let expected = default_profile_project_id(&primary);

        for (root, link_name) in [(&primary, "primary-link"), (&linked, "linked-link")] {
            let link = temp.path().join(link_name);
            std::os::unix::fs::symlink(root, &link).expect("create symlink");
            assert_eq!(
                default_profile_project_id(&link),
                expected,
                "a symlinked spelling changed the id for {}",
                root.display()
            );
        }
    }

    #[test]
    fn a_subdirectory_is_not_absorbed_into_the_repository() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, _linked) = repository(temp.path());
        let nested = primary.join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        assert_ne!(
            default_profile_project_id(&nested),
            default_profile_project_id(&primary),
        );
    }
    #[test]
    fn enrolled_roots_share_exact_identity_across_linked_paths_and_refuse_foreign_markers() {
        let temp = tempfile::tempdir().unwrap();
        let (primary, linked) = repository(temp.path());
        let project = tracedecay_domain::ProjectId::new("project-enrolled".to_owned()).unwrap();
        assert!(write_repository_identity_marker(&primary, project.as_str()).unwrap());
        let roots = enrolled_project_roots(
            [
                linked,
                primary.clone(),
                primary.join("."),
                temp.path().join("missing"),
            ],
            &project,
        )
        .unwrap();
        assert_eq!(roots, vec![primary.canonicalize().unwrap()]);
        let foreign = tracedecay_domain::ProjectId::new("project-foreign".to_owned()).unwrap();
        assert_eq!(
            enrolled_project_roots([primary.clone()], &foreign)
                .unwrap()
                .len(),
            0
        );
        let marker = repository_identity_path(&primary).unwrap();
        fs::write(&marker, b"invalid identity marker").unwrap();
        assert!(enrolled_project_roots([primary], &project).is_err());
        assert_eq!(fs::read(marker).unwrap(), b"invalid identity marker");
    }

    #[test]
    fn enrolled_roots_allow_only_exact_path_fallback_without_creating_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let project = tracedecay_domain::ProjectId::new(default_profile_project_id(&root)).unwrap();
        assert_eq!(
            enrolled_project_roots([root.clone()], &project).unwrap(),
            vec![root.clone()]
        );
        let foreign = tracedecay_domain::ProjectId::new("project-foreign".to_owned()).unwrap();
        assert_eq!(
            enrolled_project_roots([root.clone()], &foreign)
                .unwrap()
                .len(),
            0
        );
        assert!(!root.join(".git").exists());
    }
}

#[cfg(test)]
mod enrolled_project_roots_tests {
    use super::*;
    use tracedecay_domain::ProjectId;

    #[test]
    fn empty_candidates_yield_no_roots() {
        let project_id = ProjectId::new("proj_0123456789abcdef").expect("project id");
        let roots = enrolled_project_roots(Vec::<PathBuf>::new(), &project_id).expect("filter");
        assert_eq!(roots.len(), 0);
    }
}

#[cfg(test)]
mod identity_anchor_tests {
    use super::*;
    use tracedecay_domain::git::GitOidV1;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf-8 git output")
    }

    fn commit(dir: &Path, file: &str) {
        fs::write(dir.join(file), file).expect("write fixture file");
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", file]);
    }

    /// A repository whose `main` merges two unrelated histories.
    fn two_root_repository(temp: &Path) -> PathBuf {
        let root = temp.join("two-roots");
        fs::create_dir_all(root.join("nested")).expect("create repository");
        git(&root, &["init", "-q", "--initial-branch=main"]);
        commit(&root, "first.txt");
        git(&root, &["checkout", "-q", "--orphan", "other"]);
        git(&root, &["rm", "-q", "-r", "--cached", "."]);
        commit(&root, "second.txt");
        git(&root, &["checkout", "-q", "-f", "main"]);
        git(
            &root,
            &[
                "merge",
                "-q",
                "--allow-unrelated-histories",
                "-m",
                "merge",
                "other",
            ],
        );
        root
    }

    fn root_commits(root: &Path) -> Vec<GitOidV1> {
        let mut roots = git(root, &["rev-list", "--max-parents=0", "HEAD"])
            .lines()
            .map(|line| GitOidV1::new(line.to_owned()).expect("root oid"))
            .collect::<Vec<_>>();
        roots.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        roots
    }

    fn anchors(root: &Path) -> Vec<IdentityAnchor> {
        repository_identity_anchors(root, crate::git::git_remote_url(root).as_deref())
    }

    fn remote(url: &str, relative: &str) -> IdentityAnchor {
        IdentityAnchor::Remote {
            url: crate::git::normalize_git_remote_url(url).expect("normalized remote"),
            relative: PathBuf::from(relative),
        }
    }

    #[test]
    fn every_root_commit_of_a_merged_history_is_an_anchor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = two_root_repository(temp.path());
        let roots = root_commits(&root);
        assert_eq!(roots.len(), 2, "fixture must carry two unrelated roots");

        assert_eq!(
            anchors(&root),
            roots
                .iter()
                .cloned()
                .map(IdentityAnchor::RootCommit)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            IdentityAnchor::RootCommit(roots[0].clone()).alias_key(),
            format!("git-root-commit:{}", roots[0].as_str())
        );
    }

    #[test]
    fn a_subdirectory_project_is_anchored_by_root_and_relative_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = two_root_repository(temp.path());
        let anchors = repository_identity_anchors(&root.join("nested"), Some("git@h:o/r.git"));

        let mut expected = root_commits(&root)
            .iter()
            .map(|root| format!("git-subproject:{}:nested", root.as_str()))
            .collect::<Vec<_>>();
        expected.push("git-remote-url:https://h/o/r#nested".to_owned());
        assert_eq!(
            anchors
                .iter()
                .map(IdentityAnchor::alias_key)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn a_shallow_clone_is_anchored_by_its_normalized_remote() {
        let temp = tempfile::tempdir().expect("tempdir");
        let origin = two_root_repository(temp.path());
        let url = format!("file://{}/", origin.display());
        git(
            temp.path(),
            &["clone", "-q", "--depth", "1", &url, "shallow"],
        );
        let shallow = temp.path().join("shallow");
        fs::create_dir(shallow.join("nested")).expect("create subproject");

        assert_eq!(anchors(&shallow), vec![remote(&url, "")]);
        assert_eq!(
            anchors(&shallow.join("nested")),
            vec![remote(&url, "nested")],
            "a subproject's remote anchor names its path in the checkout"
        );

        git(temp.path(), &["clone", "-q", &url, "full"]);
        let full = temp.path().join("full");
        let mut expected = root_commits(&full)
            .into_iter()
            .map(IdentityAnchor::RootCommit)
            .collect::<Vec<_>>();
        expected.push(remote(&url, ""));
        assert_eq!(
            anchors(&full),
            expected,
            "a full clone records its remote for later shallow clones"
        );
    }

    #[test]
    fn one_remote_normalizes_the_same_however_it_was_cloned() {
        let expected = Some("https://github.com/owner/repo".to_owned());
        for spelling in [
            "https://github.com/Owner/Repo.git",
            "https://github.com/Owner/Repo.GIT/",
            "https://github.com/owner/repo/",
            "ssh://git@github.com/owner/repo.git",
            "ssh://git@github.com/owner/repo",
            "git@github.com:owner/repo.git",
            "git@github.com:/owner/repo",
        ] {
            assert_eq!(
                crate::git::normalize_git_remote_url(spelling),
                expected,
                "{spelling}"
            );
        }
        assert_eq!(
            crate::git::normalize_git_remote_url("file:///srv/repos/remote.git/"),
            crate::git::normalize_git_remote_url("/srv/repos/remote.git"),
        );
        assert_eq!(
            crate::git::local_remote_path("file:///srv/repos/checkout"),
            Some(PathBuf::from("/srv/repos/checkout"))
        );
        assert_eq!(
            crate::git::local_remote_path("git@github.com:owner/repo.git"),
            None
        );
    }

    #[test]
    fn a_checkout_walks_its_history_once_per_head() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = two_root_repository(temp.path());
        let walks = root_commit_walk_count_for_test(&root);

        let first = anchors(&root);
        assert_eq!(anchors(&root), first);
        assert_eq!(
            root_commit_walk_count_for_test(&root),
            walks + 1,
            "a second lookup at the same HEAD reuses the walk"
        );
        commit(&root, "third.txt");
        assert_eq!(anchors(&root), first);
        assert_eq!(
            root_commit_walk_count_for_test(&root),
            walks + 2,
            "a new HEAD is walked again"
        );
    }

    #[test]
    fn an_unborn_repository_has_no_anchors() {
        let temp = tempfile::tempdir().expect("tempdir");
        git(temp.path(), &["init", "-q"]);
        assert_eq!(
            repository_identity_anchors(temp.path(), Some("git@h:o/r")),
            Vec::new()
        );
    }

    #[test]
    fn a_marker_naming_a_held_store_survives_a_claim_and_an_unheld_one_is_replaced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = two_root_repository(temp.path());
        let profile = temp.path().join("profile");
        let marked = |project_id| {
            read_repository_identity_marker(&root)
                .unwrap()
                .expect("marker")
                .project_id
                == project_id
        };
        assert!(write_repository_identity_marker(&root, "proj_held").unwrap());
        fs::create_dir_all(profile_sharded_data_root(&profile, "proj_held")).unwrap();

        assert!(!claim_repository_identity_marker(&root, "proj_local", &profile).unwrap());
        assert!(marked("proj_held"), "a held project's marker is kept");

        assert!(write_repository_identity_marker(&root, "proj_stale").unwrap());
        assert!(claim_repository_identity_marker(&root, "proj_local", &profile).unwrap());
        assert!(marked("proj_local"), "a marker naming no held store yields");
    }
}
