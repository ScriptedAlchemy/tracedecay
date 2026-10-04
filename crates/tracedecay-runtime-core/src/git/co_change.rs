//! Bounded co-change mining: files that history says usually change together.
use std::collections::{HashMap, HashSet};
use std::path::Path;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{GitCommandBounds, bounded_git_output, try_git_program};
use crate::git_repository::{GitRepositoryAuthority, GitRepositoryError};

/// Newest commits mined per read.
const HISTORY_COMMITS: usize = 1_000;
/// Commits touching more files are bulk edits (renames, formatting, vendoring)
/// whose file sets say nothing about which files belong together.
const MAX_COMMIT_FILES: usize = 50;
const MAX_PARTNERS: usize = 20;

/// The degree threshold a partner must clear: the pair changed together in at
/// least `min_co_changes` commits, and in at least `min_percent` of the
/// commits that touched the changed file.
struct CouplingGate {
    min_co_changes: usize,
    min_percent: usize,
}

const MULTI_FILE_GATE: CouplingGate = CouplingGate {
    min_co_changes: 3,
    min_percent: 50,
};
/// A one-file change raises the most false alarms, so it must clear a stricter
/// gate before it is told a partner is missing.
const SINGLE_FILE_GATE: CouplingGate = CouplingGate {
    min_co_changes: 5,
    min_percent: 75,
};

/// A file that historically changes with `partner_of` but is absent from the
/// change set under review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingCoChangePartner {
    pub file: String,
    pub partner_of: String,
    /// Commits that changed both `partner_of` and `file`.
    pub co_changes: usize,
    /// Commits that changed `partner_of`.
    pub partner_of_changes: usize,
}

/// Reports files that usually change with `changed` (mined from the history
/// reachable from `history`) but are missing from `changed` and still exist
/// in `tree`. Missing/unborn repositories have no history; unreadable history
/// is an error. Every Git read observes `bounds`, including its cancellation.
#[tracing::instrument(
    name = "runtime_core.git.missing_co_change_partners",
    level = "trace",
    skip_all
)]
pub fn missing_co_change_partners(
    root: &Path,
    history: &str,
    tree: &str,
    changed: &[String],
    bounds: &GitCommandBounds,
) -> Result<Vec<MissingCoChangePartner>> {
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    try_git_program().map_err(|_| TraceDecayError::HostCliUnavailable {
        program: "git".to_owned(),
        lifecycle: "Git co-change analysis".to_owned(),
    })?;
    match std::fs::metadata(root) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(TraceDecayError::Io(error)),
        Ok(_) => {}
    }
    let repository = match GitRepositoryAuthority::discover(root) {
        Ok(repository) => repository,
        Err(GitRepositoryError::NotARepository { .. }) => return Ok(Vec::new()),
        Err(error) => return Err(co_change_error(error.to_string())),
    };
    if repository
        .head()
        .map_err(|error| co_change_error(error.to_string()))?
        .commit()
        .is_none()
    {
        return Ok(Vec::new());
    }

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
            "--format=%x01",
            "--name-only",
            "-z",
            &max_count,
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
        for anchor in commit.iter().filter_map(|path| changed.get(path)) {
            *changes.entry(anchor).or_default() += 1;
            for partner in commit.iter().filter(|path| !changed.contains(*path)) {
                *co_changes.entry((anchor, partner)).or_default() += 1;
            }
        }
    }

    let mut strongest = HashMap::<String, MissingCoChangePartner>::new();
    for ((anchor, file), together) in co_changes {
        let total = changes[anchor];
        if together < gate.min_co_changes || together * 100 < total * gate.min_percent {
            continue;
        }
        let (Ok(file), Ok(anchor)) = (std::str::from_utf8(file), std::str::from_utf8(anchor))
        else {
            continue;
        };
        let candidate = MissingCoChangePartner {
            file: file.to_owned(),
            partner_of: anchor.to_owned(),
            co_changes: together,
            partner_of_changes: total,
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
        .filter(|partner| present.contains(partner.file.as_bytes()))
        .collect::<Vec<_>>();
    partners.sort_by(|left, right| rank(left).cmp(&rank(right)));
    partners.truncate(MAX_PARTNERS);
    Ok(partners)
}

/// Strongest coupling first: higher co-change share, then more co-changes,
/// then path order for a deterministic answer.
fn rank(
    partner: &MissingCoChangePartner,
) -> (
    std::cmp::Reverse<u128>,
    std::cmp::Reverse<usize>,
    &str,
    &str,
) {
    let share =
        (partner.co_changes as u128 * 1_000_000) / partner.partner_of_changes.max(1) as u128;
    (
        std::cmp::Reverse(share),
        std::cmp::Reverse(partner.co_changes),
        partner.file.as_str(),
        partner.partner_of.as_str(),
    )
}

fn git_stdout(root: &Path, args: &[&str], bounds: &GitCommandBounds) -> Result<Vec<u8>> {
    let output = bounded_git_output(root, args, bounds)
        .map_err(|error| co_change_error(error.to_string()))?;
    if !output.status.success() {
        return Err(co_change_error(format!(
            "git {} exited with {}: {}",
            args[0],
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// Splits `git log --format=%x01 --name-only -z` output into per-commit file
/// sets. Each commit is a `\x01` header token; the first path after it carries
/// the one `\n` separator Git writes between the header and the name list.
fn commit_file_sets(log: &[u8]) -> Result<Vec<Vec<&[u8]>>> {
    let mut commits = Vec::new();
    let mut current: Option<Vec<&[u8]>> = None;
    let mut after_header = false;
    for token in log.split(|byte| *byte == 0) {
        let token = if after_header {
            token.strip_prefix(b"\n").unwrap_or(token)
        } else {
            token
        };
        after_header = false;
        if token == b"\x01" {
            commits.extend(current.take());
            current = Some(Vec::new());
            after_header = true;
            continue;
        }
        if token.is_empty() {
            continue;
        }
        current
            .as_mut()
            .ok_or_else(|| co_change_error("git log listed a path before any commit"))?
            .push(token);
    }
    commits.extend(current);
    Ok(commits)
}

fn co_change_error(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("git-co-change-unavailable", false, detail.into())
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
            missing_co_change_partners(
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
                    partner.file,
                    partner.partner_of,
                    partner.co_changes,
                    partner.partner_of_changes,
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
        let missing = tempfile::tempdir().unwrap();
        assert_eq!(
            missing_co_change_partners(
                &missing.path().join("absent"),
                "HEAD",
                "HEAD",
                &["schema.rs".to_owned()],
                &GitCommandBounds::default(),
            )
            .unwrap(),
            Vec::new()
        );
        let repo = Repo::new();
        assert_eq!(repo.partners(&["schema.rs", "handler.rs"]), Vec::new());

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

        // A one-file change must clear the stricter single-file gate.
        assert_eq!(repo.partners(&["schema.rs"]), Vec::new());
        for _ in 0..3 {
            repo.commit(&["schema.rs", "migrations/next.sql"]);
        }
        assert_eq!(
            repo.partners(&["schema.rs"]),
            vec![partner("migrations/next.sql", "schema.rs", 6, 7)]
        );

        // A partner that no longer exists cannot be forgotten.
        repo.git(&["rm", "-q", "migrations/next.sql"]);
        repo.git(&["commit", "-m", "drop migration"]);
        assert_eq!(repo.partners(&["schema.rs"]), Vec::new());
    }

    #[cfg(unix)]
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
            repo.commit(&["schema.rs", "migrations/next.sql"]);
        }
        assert_eq!(
            repo.partners(&["schema.rs", "handler.rs"]),
            vec![partner("migrations/next.sql", "schema.rs", 3, 3)]
        );
    }

    #[test]
    fn a_cancelled_read_stops_before_mining() {
        let repo = Repo::new();
        repo.commit(&["schema.rs"]);
        let cancel = crate::cancellation::CancellationToken::new();
        cancel.cancel();
        let read = missing_co_change_partners(
            repo.0.path(),
            "HEAD",
            "HEAD",
            &["schema.rs".to_owned()],
            &GitCommandBounds {
                cancel: Some(cancel),
                ..GitCommandBounds::default()
            },
        );
        assert!(
            read.as_ref()
                .is_err_and(|error| error.to_string().contains("cancelled")),
            "{read:?}"
        );
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
    }
}
