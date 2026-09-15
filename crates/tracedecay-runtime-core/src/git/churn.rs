//! Bounded, read-only per-file commit counts shared by graph health consumers.
use std::collections::HashMap;
use std::path::Path;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{GitCommandBounds, bounded_git_output, try_git_program};
use crate::git_repository::{GitRepositoryAuthority, GitRepositoryError};

/// Counts commits touching each exact UTF-8 Git path during the requested window.
/// Missing/unborn repositories have no history. Unreadable history and paths that
/// cannot be represented by the graph's string identity are errors, not zero churn.
#[hotpath::measure(label = "runtime_core.git.file_churn", future = true)]
pub async fn file_churn(project_root: &Path, days: u32) -> Result<HashMap<String, usize>> {
    let root = project_root.to_owned();
    tokio::task::spawn_blocking(move || read_file_churn(&root, days, &GitCommandBounds::default()))
        .await
        .map_err(|error| churn_error(error.to_string()))?
}

fn churn_error(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("git-churn-unavailable", false, detail.into())
}

fn read_file_churn(
    root: &Path,
    days: u32,
    bounds: &GitCommandBounds,
) -> Result<HashMap<String, usize>> {
    try_git_program().map_err(|_| TraceDecayError::HostCliUnavailable {
        program: "git".to_owned(),
        lifecycle: "Git churn analysis".to_owned(),
    })?;
    match std::fs::metadata(root) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(HashMap::new());
        }
        Err(error) => return Err(TraceDecayError::Io(error)),
        Ok(_) => {}
    }
    let repository = match GitRepositoryAuthority::discover(root) {
        Ok(repository) => repository,
        Err(GitRepositoryError::NotARepository { .. }) => return Ok(HashMap::new()),
        Err(error) => return Err(churn_error(error.to_string())),
    };
    if repository
        .head()
        .map_err(|error| churn_error(error.to_string()))?
        .commit()
        .is_none()
    {
        return Ok(HashMap::new());
    }
    let output = bounded_git_output(
        root,
        &[
            "log",
            "--format=",
            "--name-only",
            "-z",
            &format!("--since={days} days ago"),
        ],
        bounds,
    )
    .map_err(|error| churn_error(error.to_string()))?;
    if !output.status.success() {
        return Err(churn_error(format!(
            "git log exited with {}",
            output.status
        )));
    }
    let mut churn = HashMap::new();
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let path = std::str::from_utf8(path).map_err(|error| churn_error(error.to_string()))?;
        *churn.entry(path.to_owned()).or_insert(0) += 1;
    }
    Ok(churn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_paths_and_bounded_history() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let output = std::process::Command::new(try_git_program().unwrap())
                .args(args)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}: {:?}", output.stderr);
        };
        assert!(
            read_file_churn(root, 90, &GitCommandBounds::default())
                .unwrap()
                .is_empty()
        );
        assert!(
            read_file_churn(&root.join("missing"), 90, &GitCommandBounds::default())
                .unwrap()
                .is_empty()
        );
        git(&["init"]);
        assert!(
            read_file_churn(root, 90, &GitCommandBounds::default())
                .unwrap()
                .is_empty()
        );
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        #[cfg(unix)]
        let paths = [
            " leading.rs",
            "trailing.rs ",
            "line\nbreak.rs",
            "tab\tname.rs",
            "utf8-λ.rs",
        ];
        #[cfg(not(unix))]
        let paths = [" leading.rs", "utf8-λ.rs"];
        for path in paths {
            std::fs::write(root.join(path), "one").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-m", "first"]);
        std::fs::write(root.join(paths[0]), "two").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "second"]);
        let counts = read_file_churn(root, 90, &GitCommandBounds::default()).unwrap();
        assert_eq!(counts.len(), paths.len());
        for (index, path) in paths.iter().enumerate() {
            assert_eq!(counts[*path], if index == 0 { 2 } else { 1 });
        }
        let limited = GitCommandBounds {
            max_stdout_bytes: 1,
            ..Default::default()
        };
        assert!(read_file_churn(root, 90, &limited).is_err());
        let expired = GitCommandBounds {
            deadline: std::time::Instant::now(),
            ..Default::default()
        };
        assert!(read_file_churn(root, 90, &expired).is_err());
        #[cfg(unix)]
        if non_utf8_file_names_supported(root)
            .expect("probe non-UTF-8 file-name support without hiding storage failures")
        {
            use std::os::unix::ffi::OsStrExt;
            let path = std::ffi::OsStr::from_bytes(b"invalid-\xff.rs");
            std::fs::write(root.join(path), "invalid UTF-8 identity").unwrap();
            git(&["add", "."]);
            git(&["commit", "-m", "invalid path"]);
            assert!(read_file_churn(root, 90, &GitCommandBounds::default()).is_err());
        }
    }

    /// Report whether `directory`'s filesystem accepts a name that is not
    /// valid UTF-8.
    ///
    /// `cfg(unix)` is a compile gate, not a filesystem capability: APFS
    /// refuses such a name outright with `EILSEQ`, so a macOS run failed at
    /// the fixture instead of exercising the churn refusal. Probing keeps the
    /// coverage everywhere the bytes are accepted while propagating unrelated
    /// storage failures.
    #[cfg(unix)]
    fn non_utf8_file_names_supported(directory: &std::path::Path) -> std::io::Result<bool> {
        use std::os::unix::ffi::OsStringExt as _;

        let probe = directory.join(std::ffi::OsString::from_vec(b"probe-\xff".to_vec()));
        match std::fs::write(&probe, b"") {
            Ok(()) => {
                std::fs::remove_file(&probe)?;
                Ok(true)
            }
            // Darwin exposes an invalid byte sequence as EILSEQ. Rust
            // deliberately leaves that errno uncategorized, so keep this
            // capability exception local to the one platform whose filesystem
            // rejects the probe name.
            #[cfg(target_os = "macos")]
            Err(error) if error.raw_os_error() == Some(92) => Ok(false),
            Err(error) => Err(error),
        }
    }
}
