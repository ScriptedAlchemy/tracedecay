//! Bounded co-change mining: files that history says usually change together.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::{GitCommandBounds, GitCommandError, bounded_git_output, try_git_program};

const HISTORY_COMMITS: usize = 1_000;
const MAX_COMMIT_FILES: usize = 50;
const MAX_PARTNERS: usize = 8;
const SINCE: &str = "--since=18 months ago";

struct CouplingGate {
    min_co_changes: usize,
    min_percent: usize,
}

const MULTI_FILE_GATE: CouplingGate = CouplingGate {
    min_co_changes: 3,
    min_percent: 50,
};
const SINGLE_FILE_GATE: CouplingGate = CouplingGate {
    min_co_changes: 5,
    min_percent: 75,
};

/// A typed failure from co-change Git reads.
#[derive(Debug, thiserror::Error)]
pub enum CoChangeError {
    #[error(transparent)]
    Command(#[from] GitCommandError),
    #[error("{0}")]
    InvalidOutput(String),
    #[error("{0}")]
    NonZeroExit(String),
}

/// One changed file and a file outside that change that co-occur in commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoChangePartner {
    pub file: String,
    pub partner: String,
    pub together: usize,
    file_changes: usize,
}

/// Reports files that usually change with `changed` (mined from the history
/// reachable from `history`) but are missing from `changed` and still exist
/// in `tree`. Unavailable history is an error. Every Git read observes
/// `bounds`, including its cancellation.
#[tracing::instrument(
    name = "runtime_core.git.co_change_partners",
    level = "trace",
    skip_all
)]
pub fn co_change_partners(
    root: &Path,
    history: &str,
    tree: &str,
    changed: &[String],
    bounds: &GitCommandBounds,
) -> Result<Vec<CoChangePartner>, CoChangeError> {
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    try_git_program().map_err(GitCommandError::from)?;
    let prefix = git_stdout(root, &["rev-parse", "--show-prefix"], bounds)?;
    let prefix = prefix
        .strip_suffix(b"\n")
        .ok_or_else(|| co_change_error("git rev-parse returned an unterminated project prefix"))?;

    let changed = changed.iter().map(String::as_bytes).collect::<HashSet<_>>();
    let gate = if changed.len() == 1 {
        &SINGLE_FILE_GATE
    } else {
        &MULTI_FILE_GATE
    };
    let max_count = format!("--max-count={HISTORY_COMMITS}");
    let log = git_stdout(
        root,
        &[
            "log",
            "--no-merges",
            "--no-renames",
            "--no-relative",
            "--format=%x00",
            "--name-only",
            "-z",
            &max_count,
            SINCE,
            history,
            "--",
        ],
        bounds,
    )?;
    // Paths stay Git-native bytes: an unrelated non-UTF-8 name in history or
    // the tree must not cost the answer for the paths that are decodable.
    let mut changes = HashMap::<&[u8], usize>::new();
    let mut co_changes = HashMap::<(&[u8], &[u8]), usize>::new();
    for commit in commit_file_sets(&log)? {
        if commit.len() > MAX_COMMIT_FILES {
            continue;
        }
        let files = commit
            .iter()
            .filter_map(|path| path.strip_prefix(prefix))
            .collect::<HashSet<_>>();
        for anchor in files.iter().filter_map(|path| changed.get(path)) {
            *changes.entry(anchor).or_default() += 1;
            for partner in files.iter().filter(|path| !changed.contains(*path)) {
                *co_changes.entry((anchor, partner)).or_default() += 1;
            }
        }
    }

    let mut strongest = HashMap::<String, CoChangePartner>::new();
    for ((anchor, file), together) in co_changes {
        let total = changes[anchor];
        if together < gate.min_co_changes || together * 100 < total * gate.min_percent {
            continue;
        }
        let (Ok(file), Ok(anchor)) = (std::str::from_utf8(file), std::str::from_utf8(anchor))
        else {
            continue;
        };
        let candidate = CoChangePartner {
            file: anchor.to_owned(),
            partner: file.to_owned(),
            together,
            file_changes: total,
        };
        match strongest.get(file) {
            Some(current) if rank(current) <= rank(&candidate) => {}
            _ => {
                strongest.insert(file.to_owned(), candidate);
            }
        }
    }
    if strongest.is_empty() {
        return Ok(Vec::new());
    }

    let listing = git_stdout(root, &["ls-tree", "-r", "--name-only", "-z", tree], bounds)?;
    let present = listing
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect::<HashSet<_>>();
    let mut partners = strongest
        .into_values()
        .filter(|partner| present.contains(partner.partner.as_bytes()))
        .collect::<Vec<_>>();
    partners.sort_by(|left, right| rank(left).cmp(&rank(right)));
    partners.truncate(MAX_PARTNERS);
    Ok(partners)
}

fn rank(
    partner: &CoChangePartner,
) -> (
    std::cmp::Reverse<u128>,
    std::cmp::Reverse<usize>,
    &str,
    &str,
) {
    let share = (partner.together as u128 * 1_000_000) / partner.file_changes as u128;
    (
        std::cmp::Reverse(share),
        std::cmp::Reverse(partner.together),
        partner.partner.as_str(),
        partner.file.as_str(),
    )
}

fn git_stdout(
    root: &Path,
    args: &[&str],
    bounds: &GitCommandBounds,
) -> Result<Vec<u8>, CoChangeError> {
    let output = bounded_git_output(root, args, bounds)?;
    if !output.status.success() {
        return Err(CoChangeError::NonZeroExit(format!(
            "git {} exited with {}: {}",
            args[0],
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// Empty NUL tokens delimit commits because Git paths cannot be empty or
/// contain NUL. The first path carries one formatting newline.
fn commit_file_sets(log: &[u8]) -> Result<Vec<Vec<&[u8]>>, CoChangeError> {
    let mut commits = Vec::new();
    let mut current: Option<Vec<&[u8]>> = None;
    let mut after_header = false;
    for token in log.split(|byte| *byte == 0) {
        if token.is_empty() {
            commits.extend(current.take());
            after_header = true;
            continue;
        }
        let token = if after_header {
            current = Some(Vec::new());
            after_header = false;
            token
                .strip_prefix(b"\n")
                .ok_or_else(|| co_change_error("git log omitted the commit header separator"))?
        } else {
            token
        };
        current
            .as_mut()
            .ok_or_else(|| co_change_error("git log listed a path before any commit"))?
            .push(token);
    }
    commits.extend(current);
    Ok(commits)
}

fn co_change_error(detail: impl Into<String>) -> CoChangeError {
    CoChangeError::InvalidOutput(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo(tempfile::TempDir);

    impl Repo {
        fn new() -> Self {
            let repo = Self(tempfile::tempdir().unwrap());
            repo.git(&["init", "-b", "master"]);
            repo
        }

        fn git(&self, args: &[&str]) {
            let output = std::process::Command::new(try_git_program().unwrap())
                .args([
                    "-c",
                    "user.name=TraceDecay Test",
                    "-c",
                    "user.email=tracedecay-test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=.git/no-hooks",
                ])
                .args(args)
                .current_dir(self.0.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}: {:?}", output.stderr);
        }

        fn commit(&self, files: &[&str]) {
            for file in files {
                let path = self.0.path().join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let previous = std::fs::read_to_string(&path).unwrap_or_default();
                std::fs::write(&path, format!("{previous}x\n")).unwrap();
            }
            self.git(&[&["add", "--"][..], files].concat());
            self.git(&["commit", "-m", "change"]);
        }

        fn partners(&self, changed: &[&str]) -> Vec<(String, String, usize, usize)> {
            let changed = changed
                .iter()
                .map(|path| (*path).to_owned())
                .collect::<Vec<_>>();
            co_change_partners(
                self.0.path(),
                "HEAD",
                "HEAD",
                &changed,
                &GitCommandBounds::default(),
            )
            .unwrap()
            .into_iter()
            .map(|partner| {
                (
                    partner.partner,
                    partner.file,
                    partner.together,
                    partner.file_changes,
                )
            })
            .collect()
        }
    }

    fn partner(
        file: &str,
        of: &str,
        together: usize,
        total: usize,
    ) -> (String, String, usize, usize) {
        (file.to_owned(), of.to_owned(), together, total)
    }

    #[test]
    fn a_change_missing_its_usual_companion_is_reported() {
        let repo = Repo::new();

        for _ in 0..3 {
            repo.commit(&["schema.rs", "migrations/next.sql"]);
        }
        repo.commit(&["schema.rs"]);
        repo.commit(&["handler.rs", "handler_test.rs"]);
        repo.commit(&["handler.rs", "handler_test.rs", "readme.md"]);
        repo.commit(&["handler.rs", "handler_test.rs"]);
        repo.commit(&["readme.md", "handler.rs"]);

        assert_eq!(
            repo.partners(&["schema.rs", "handler.rs"]),
            vec![
                partner("handler_test.rs", "handler.rs", 3, 4),
                partner("migrations/next.sql", "schema.rs", 3, 4),
            ],
            "partners that clear the gate, strongest coupling first, then by path"
        );
        assert_eq!(
            repo.partners(&["schema.rs", "handler.rs", "migrations/next.sql"]),
            vec![partner("handler_test.rs", "handler.rs", 3, 4)],
            "a partner already in the change set is not missing"
        );

        assert_eq!(repo.partners(&["schema.rs"]), Vec::new());
        for _ in 0..3 {
            repo.commit(&["schema.rs", "migrations/next.sql"]);
        }
        assert_eq!(
            repo.partners(&["schema.rs"]),
            vec![partner("migrations/next.sql", "schema.rs", 6, 7)]
        );

        repo.git(&["rm", "-q", "migrations/next.sql"]);
        repo.git(&["commit", "-m", "drop migration"]);
        assert_eq!(repo.partners(&["schema.rs"]), Vec::new());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unrelated_non_utf8_paths_do_not_hide_partners() {
        use std::os::unix::ffi::OsStrExt;

        let repo = Repo::new();
        let binary = std::ffi::OsStr::from_bytes(b"assets/\xff.bin");
        std::fs::create_dir_all(repo.0.path().join("assets")).unwrap();
        std::fs::write(repo.0.path().join(binary), b"x").unwrap();
        let output = std::process::Command::new(try_git_program().unwrap())
            .args(["add", "--"])
            .arg(binary)
            .current_dir(repo.0.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        repo.git(&["commit", "-m", "binary asset"]);
        for _ in 0..3 {
            repo.commit(&["schema.rs", "migrations/next.sql", "new\nline.rs", "\u{1}"]);
        }
        assert_eq!(
            repo.partners(&["schema.rs", "handler.rs"]),
            vec![
                partner("\u{1}", "schema.rs", 3, 3),
                partner("migrations/next.sql", "schema.rs", 3, 3),
                partner("new\nline.rs", "schema.rs", 3, 3),
            ]
        );
    }

    #[test]
    fn a_cancelled_read_stops_before_mining() {
        let repo = Repo::new();
        for _ in 0..5 {
            repo.commit(&["schema.rs", "migrations/next.sql"]);
        }
        assert_eq!(
            repo.partners(&["schema.rs"]),
            vec![partner("migrations/next.sql", "schema.rs", 5, 5)]
        );
        let cancel = crate::cancellation::CancellationToken::new();
        cancel.cancel();
        let read = co_change_partners(
            repo.0.path(),
            "HEAD",
            "HEAD",
            &["schema.rs".to_owned()],
            &GitCommandBounds {
                cancel: Some(cancel),
                ..GitCommandBounds::default()
            },
        );
        let error = read.unwrap_err();
        assert!(matches!(
            error,
            CoChangeError::Command(GitCommandError::Cancelled)
        ));
    }

    #[test]
    fn bulk_commits_do_not_couple_files() {
        let repo = Repo::new();
        let bulk = (0..=MAX_COMMIT_FILES)
            .map(|index| format!("bulk/{index}.rs"))
            .collect::<Vec<_>>();
        let mut files = bulk.iter().map(String::as_str).collect::<Vec<_>>();
        files.push("lib.rs");
        for _ in 0..6 {
            repo.commit(&files);
        }
        assert_eq!(repo.partners(&["lib.rs", "other.rs"]), Vec::new());
        for _ in 0..3 {
            repo.commit(&["lib.rs", "lib_test.rs"]);
        }
        assert_eq!(
            repo.partners(&["lib.rs", "other.rs"]),
            vec![partner("lib_test.rs", "lib.rs", 3, 3)]
        );
    }

    #[test]
    fn nested_projects_keep_project_relative_paths_and_history_bounds() {
        let repo = Repo::new();
        for _ in 0..5 {
            repo.commit(&[
                "project/src/schema.rs",
                "project/migrations/next.sql",
                "outside.rs",
            ]);
        }
        let root = repo.0.path().join("project");
        let changed = vec!["src/schema.rs".to_owned()];
        let partners = co_change_partners(
            &root,
            "HEAD",
            "HEAD",
            &changed,
            &GitCommandBounds::default(),
        )
        .unwrap();
        assert_eq!(
            partners,
            vec![CoChangePartner {
                file: "src/schema.rs".to_owned(),
                partner: "migrations/next.sql".to_owned(),
                together: 5,
                file_changes: 5,
            }]
        );
        let limited = GitCommandBounds {
            max_stdout_bytes: 1,
            ..GitCommandBounds::default()
        };
        let error = co_change_partners(&root, "HEAD", "HEAD", &changed, &limited).unwrap_err();
        assert!(matches!(
            &error,
            CoChangeError::Command(GitCommandError::OutputLimitExceeded {
                stream: "stdout",
                bound: 1
            })
        ));
        for _ in 0..3 {
            repo.commit(&["project/src/schema.rs"]);
        }
        assert_eq!(
            co_change_partners(
                &root,
                "HEAD",
                "HEAD",
                &changed,
                &GitCommandBounds::default()
            )
            .unwrap(),
            Vec::new(),
        );
        assert_eq!(
            co_change_partners(
                &root,
                "HEAD~3",
                "HEAD",
                &changed,
                &GitCommandBounds::default()
            )
            .unwrap(),
            partners,
        );
    }

    #[test]
    fn unavailable_history_is_not_an_empty_measurement() {
        let repo = Repo::new();
        let changed = vec!["schema.rs".to_owned()];
        let unborn = co_change_partners(
            repo.0.path(),
            "HEAD",
            "HEAD",
            &changed,
            &GitCommandBounds::default(),
        )
        .unwrap_err();
        assert!(matches!(&unborn, CoChangeError::NonZeroExit(_)));
        assert!(
            unborn.to_string().contains("bad revision 'HEAD'"),
            "{unborn}"
        );
        for _ in 0..5 {
            repo.commit(&["schema.rs", "migration.sql"]);
        }
        assert_eq!(
            repo.partners(&["schema.rs"]),
            vec![partner("migration.sql", "schema.rs", 5, 5)]
        );
        let error = co_change_partners(
            repo.0.path(),
            "missing-ref",
            "HEAD",
            &changed,
            &GitCommandBounds::default(),
        )
        .unwrap_err();
        assert!(matches!(&error, CoChangeError::NonZeroExit(_)));
        assert!(
            error.to_string().contains("bad revision 'missing-ref'"),
            "{error}"
        );
    }
}
