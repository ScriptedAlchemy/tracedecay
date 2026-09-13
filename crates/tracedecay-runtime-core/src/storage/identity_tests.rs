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
        assert!(
            enrolled_project_roots([primary.clone()], &foreign)
                .unwrap()
                .is_empty()
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
        assert!(
            enrolled_project_roots([root.clone()], &foreign)
                .unwrap()
                .is_empty()
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
        assert!(roots.is_empty());
    }
}
