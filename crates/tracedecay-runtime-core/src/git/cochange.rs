//! Files that share commits with a diff, read from a bounded `git log`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{GitCommandBounds, bounded_git_output, try_git_program};

const MIN_COMMITS_TOGETHER: usize = 3;
const MAX_PARTNERS: usize = 8;
const SINCE: &str = "--since=18 months ago";

/// One changed file and a file outside that change that co-occur in commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoChangePartner {
    pub file: String,
    pub partner: String,
    pub together: usize,
}

/// Partners of `changed_files` that are not themselves in that set.
///
/// A missing or unborn repository has no history, so the measurement is empty.
/// Any other git failure is an error. Diff context does not treat that
/// failure as an omitted field, so the handler propagates it.
pub fn co_change_partners(
    project_root: &Path,
    changed_files: &[String],
    bounds: &GitCommandBounds,
) -> Result<Vec<CoChangePartner>> {
    if changed_files.is_empty() {
        return Ok(Vec::new());
    }
    let commits = read_commit_files(project_root, bounds)?;
    Ok(partners_outside(&commits, changed_files))
}

fn cochange_error(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route("git-co-change-unavailable", false, detail.into())
}

fn read_commit_files(root: &Path, bounds: &GitCommandBounds) -> Result<Vec<Vec<String>>> {
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
        Ok(metadata) if !metadata.is_dir() => return Ok(Vec::new()),
        Ok(_) => {}
    }
    try_git_program().map_err(|_| TraceDecayError::HostCliUnavailable {
        program: "git".to_owned(),
        lifecycle: "Git co-change partners".to_owned(),
    })?;
    let output = bounded_git_output(
        root,
        &["log", "--name-only", "--pretty=format:%x1e", "-z", SINCE],
        bounds,
    )
    .map_err(|error| cochange_error(error.to_string()))?;
    if !output.status.success() {
        // Unborn and non-repositories have nothing to count. Any other git
        // failure stays an error so the diff handler can fail the request.
        if history_unavailable(&output.stderr) {
            return Ok(Vec::new());
        }
        return Err(cochange_error(format!(
            "git log exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    commits_from_log(&output.stdout)
}

fn history_unavailable(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr);
    text.contains("not a git repository") || text.contains("does not have any commits yet")
}

/// `%x1e` splits commits and `-z` splits paths, so a name that contains a
/// newline stays one path. Git writes a newline after the record separator.
fn commits_from_log(stdout: &[u8]) -> Result<Vec<Vec<String>>> {
    let mut commits = Vec::new();
    for record in stdout.split(|byte| *byte == 0x1e) {
        let record = record.strip_prefix(b"\n").unwrap_or(record);
        let mut files = Vec::new();
        for path in record.split(|byte| *byte == 0) {
            if path.is_empty() {
                continue;
            }
            let path =
                std::str::from_utf8(path).map_err(|error| cochange_error(error.to_string()))?;
            files.push(path.to_owned());
        }
        if !files.is_empty() {
            commits.push(files);
        }
    }
    Ok(commits)
}

fn partners_outside(commits: &[Vec<String>], changed_files: &[String]) -> Vec<CoChangePartner> {
    let changed: HashSet<&str> = changed_files.iter().map(String::as_str).collect();
    let mut together_counts: HashMap<(String, String), usize> = HashMap::new();
    for commit in commits {
        let files: HashSet<&str> = commit.iter().map(String::as_str).collect();
        for file in changed.iter().copied() {
            if !files.contains(file) {
                continue;
            }
            for partner in &files {
                if *partner == file || changed.contains(partner) {
                    continue;
                }
                *together_counts
                    .entry((file.to_owned(), (*partner).to_owned()))
                    .or_insert(0) += 1;
            }
        }
    }
    let mut partners: Vec<CoChangePartner> = together_counts
        .into_iter()
        .filter(|(_, together)| *together >= MIN_COMMITS_TOGETHER)
        .map(|((file, partner), together)| CoChangePartner {
            file,
            partner,
            together,
        })
        .collect();
    partners.sort_by(|left, right| {
        right
            .together
            .cmp(&left.together)
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.partner.cmp(&right.partner))
    });
    partners.truncate(MAX_PARTNERS);
    partners
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partners_need_three_shared_commits_outside_the_diff() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(
            co_change_partners(
                &root.join("missing"),
                &["a.rs".to_owned()],
                &GitCommandBounds::default(),
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            co_change_partners(root, &["a.rs".to_owned()], &GitCommandBounds::default())
                .unwrap()
                .is_empty()
        );
        let git = |args: &[&str], date: Option<&str>| {
            let mut command = std::process::Command::new(try_git_program().unwrap());
            command.args(args).current_dir(root);
            if let Some(date) = date {
                command.env("GIT_AUTHOR_DATE", date);
                command.env("GIT_COMMITTER_DATE", date);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init"], None);
        git(&["config", "user.email", "test@example.com"], None);
        git(&["config", "user.name", "Test"], None);
        assert!(
            co_change_partners(root, &["a.rs".to_owned()], &GitCommandBounds::default())
                .unwrap()
                .is_empty()
        );
        let commit = |paths: &[&str], body: &str, date: Option<&str>| {
            for path in paths {
                std::fs::write(root.join(path), body).unwrap();
            }
            git(&["add", "."], None);
            git(&["commit", "-m", body], date);
        };
        for index in 1..=3 {
            commit(
                &["a.rs", "stale.rs"],
                &format!("stale-{index}"),
                Some("2020-01-01T00:00:00Z"),
            );
        }
        for index in 1..=4 {
            commit(&["a.rs", "p0.rs"], &format!("p0-{index}"), None);
        }
        for partner in 1..=9 {
            let name = format!("p{partner}.rs");
            for index in 1..=3 {
                commit(
                    &["a.rs", name.as_str()],
                    &format!("p{partner}-{index}"),
                    None,
                );
            }
        }
        for index in 1..=2 {
            commit(&["a.rs", "once.rs"], &format!("once-{index}"), None);
        }
        for index in 1..=3 {
            commit(&["a.rs", "also.rs"], &format!("also-{index}"), None);
        }
        assert!(
            co_change_partners(root, &[], &GitCommandBounds::default())
                .unwrap()
                .is_empty()
        );
        let partners = co_change_partners(
            root,
            &["a.rs".to_owned(), "also.rs".to_owned()],
            &GitCommandBounds::default(),
        )
        .unwrap();
        let expected: Vec<CoChangePartner> = std::iter::once(CoChangePartner {
            file: "a.rs".to_owned(),
            partner: "p0.rs".to_owned(),
            together: 4,
        })
        .chain((1..=7).map(|partner| CoChangePartner {
            file: "a.rs".to_owned(),
            partner: format!("p{partner}.rs"),
            together: 3,
        }))
        .collect();
        assert_eq!(partners, expected);
        let limited = GitCommandBounds {
            max_stdout_bytes: 1,
            ..GitCommandBounds::default()
        };
        assert!(co_change_partners(root, &["a.rs".to_owned()], &limited).is_err());
    }
}
