use std::{ffi::OsStr, fs, io, path::Path};

use super::{Dir, rename_noreplace, replace_file};

const FAMILY: [&str; 4] = [
    "sessions.db",
    "sessions.db-wal",
    "sessions.db-shm",
    "sessions.db-journal",
];

fn open(path: &Path) -> Dir {
    Dir::open_ambient_dir(path, cap_std::ambient_authority()).unwrap()
}

#[test]
fn publication_refuses_escaped_sources_and_destinations_without_moving_store_files() {
    let root = tempfile::tempdir().unwrap();
    let stage_path = root.path().join("stage");
    let target_path = root.path().join("target");
    let foreign_path = root.path().join("foreign");
    for path in [&stage_path, &target_path, &foreign_path] {
        fs::create_dir(path).unwrap();
    }
    let stage = open(&stage_path);
    let target = open(&target_path);

    for name in FAMILY {
        let owned = format!("owned {name}");
        let foreign = format!("foreign {name}");
        fs::write(stage_path.join(name), owned.as_bytes()).unwrap();
        fs::write(foreign_path.join(name), foreign.as_bytes()).unwrap();
        for source in [Path::new("../foreign").join(name), foreign_path.join(name)] {
            let error = rename_noreplace(&stage, source.as_os_str(), &target, OsStr::new(name))
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(
                fs::read(foreign_path.join(name)).unwrap(),
                foreign.as_bytes()
            );
            assert!(!target_path.join(name).exists());
        }
        let destination_name = format!("published-{name}");
        for destination in [
            Path::new("../foreign").join(&destination_name),
            foreign_path.join(&destination_name),
        ] {
            let error =
                rename_noreplace(&stage, OsStr::new(name), &target, destination.as_os_str())
                    .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert_eq!(fs::read(stage_path.join(name)).unwrap(), owned.as_bytes());
            assert_eq!(
                fs::read(foreign_path.join(name)).unwrap(),
                foreign.as_bytes()
            );
            assert!(!foreign_path.join(&destination_name).exists());
        }
    }
}

#[test]
fn publication_rejects_normalized_aliases_and_nul_before_either_rename() {
    let root = tempfile::tempdir().unwrap();
    let parent = open(root.path());
    fs::write(root.path().join("payload"), b"owned").unwrap();
    fs::write(root.path().join("published"), b"foreign").unwrap();

    for name in [
        "",
        ".",
        "..",
        "./payload",
        "payload/",
        "payload/.",
        "child/payload",
        "payload\0ignored",
    ] {
        let invalid = OsStr::new(name);
        for result in [
            rename_noreplace(&parent, invalid, &parent, OsStr::new("absent")),
            rename_noreplace(&parent, OsStr::new("payload"), &parent, invalid),
            replace_file(&parent, invalid, OsStr::new("published")),
            replace_file(&parent, OsStr::new("payload"), invalid),
        ] {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
        assert_eq!(fs::read(root.path().join("payload")).unwrap(), b"owned");
        assert_eq!(fs::read(root.path().join("published")).unwrap(), b"foreign");
        assert!(!root.path().join("absent").exists());
    }
}

#[cfg(unix)]
mod process_replacement {
    use std::{env, os::unix::fs::symlink, process::Command};

    use super::{FAMILY, OsStr, Path, fs, io, open, rename_noreplace};

    const ROOT: &str = "TRACEDECAY_CAPABILITY_RENAME_TEST_ROOT";
    const COMPLETE: &str = "capability-rename-intermediate-replaced";

    #[test]
    fn replace_intermediate_directory_in_child() {
        let Some(root) = env::var_os(ROOT) else {
            return;
        };
        let root = Path::new(&root);
        fs::remove_dir(root.join("stage/bridge")).unwrap();
        symlink(root.join("foreign"), root.join("stage/bridge")).unwrap();
        println!("{COMPLETE}");
    }

    #[test]
    fn child_replacing_an_intermediate_directory_cannot_redirect_publication() {
        let root = tempfile::tempdir().unwrap();
        let stage_path = root.path().join("stage");
        let foreign_path = root.path().join("foreign");
        fs::create_dir_all(stage_path.join("bridge")).unwrap();
        fs::create_dir(&foreign_path).unwrap();
        for name in FAMILY {
            fs::write(foreign_path.join(name), name.as_bytes()).unwrap();
        }
        fs::write(stage_path.join("owned.sqlite"), b"owned").unwrap();
        let stage = open(&stage_path);

        // The child changes the namespace after the parent capability is held.
        let (_, module) = module_path!().split_once("::").unwrap();
        let output = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{module}::replace_intermediate_directory_in_child"),
                "--nocapture",
            ])
            .env(ROOT, root.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed") && stdout.contains(COMPLETE),
            "namespace replacement child failed: {stdout} {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rebound = fs::symlink_metadata(stage_path.join("bridge")).unwrap();
        assert!(rebound.is_symlink());

        for (source, destination) in [
            ("bridge/sessions.db", "published.sqlite"),
            ("owned.sqlite", "bridge/published.sqlite"),
        ] {
            let error =
                rename_noreplace(&stage, OsStr::new(source), &stage, OsStr::new(destination))
                    .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        for name in FAMILY {
            assert_eq!(fs::read(foreign_path.join(name)).unwrap(), name.as_bytes());
        }
        assert_eq!(fs::read(stage_path.join("owned.sqlite")).unwrap(), b"owned");
        assert!(!stage_path.join("published.sqlite").exists());
        assert!(!foreign_path.join("published.sqlite").exists());
    }
}
