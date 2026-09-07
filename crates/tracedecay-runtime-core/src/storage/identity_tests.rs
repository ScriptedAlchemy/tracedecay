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
    fn a_linked_worktree_and_its_primary_checkout_agree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (primary, linked) = repository(temp.path());
        assert_eq!(
            default_profile_project_id(&primary),
            default_profile_project_id(&linked),
        );
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
}
