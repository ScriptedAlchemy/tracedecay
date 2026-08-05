//! Atomic destination-ref publication with exact namespace revalidation.

use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;

use tracedecay_domain::GitOidV1;

use super::process::parse_git_oid;
use super::{FixedGitIndexRunner, NativeGitIndexError};

#[derive(Clone, Debug)]
pub(super) struct NativeRefState {
    name: String,
    object: GitOidV1,
    symbolic_target: Option<String>,
}

impl FixedGitIndexRunner {
    pub(super) fn require_ref_value(
        &self,
        reference: &str,
        expected: &GitOidV1,
    ) -> Result<(), NativeGitIndexError> {
        let value = self.run_git("rev-parse", &["rev-parse", "--verify", reference])?;
        if parse_git_oid("rev-parse", &value.stdout)? != *expected {
            return Err(NativeGitIndexError::StaleRepositoryState);
        }
        Ok(())
    }

    pub(super) fn ref_snapshot(&self) -> Result<Vec<NativeRefState>, NativeGitIndexError> {
        let output = self.run_git(
            "for-each-ref",
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(symref)",
            ],
        )?;
        let text = std::str::from_utf8(&output.stdout).map_err(|_| {
            NativeGitIndexError::MalformedOutput {
                operation: "for-each-ref",
            }
        })?;
        let mut refs = Vec::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let mut fields = line.split('\0');
            let name = fields.next().filter(|name| !name.is_empty()).ok_or(
                NativeGitIndexError::MalformedOutput {
                    operation: "for-each-ref",
                },
            )?;
            let object = fields.next().ok_or(NativeGitIndexError::MalformedOutput {
                operation: "for-each-ref",
            })?;
            let object = GitOidV1::new(object)?;
            let symbolic_target = fields
                .next()
                .filter(|target| !target.is_empty())
                .map(str::to_owned);
            if fields.next().is_some() {
                return Err(NativeGitIndexError::MalformedOutput {
                    operation: "for-each-ref",
                });
            }
            refs.push(NativeRefState {
                name: name.to_owned(),
                object,
                symbolic_target,
            });
        }
        Ok(refs)
    }

    pub(super) fn ref_namespace_matches_excluding(
        &self,
        expected_refs: &[NativeRefState],
        excluded: &str,
    ) -> Result<bool, NativeGitIndexError> {
        Ok(same_ref_states(
            &refs_excluding(&self.ref_snapshot()?, excluded),
            &refs_excluding(expected_refs, excluded),
        ))
    }

    pub(super) fn update_ref_with_namespace_cas(
        &self,
        target: &str,
        new_value: &GitOidV1,
        old_value: &GitOidV1,
        expected_refs: &[NativeRefState],
    ) -> Result<(), NativeGitIndexError> {
        if !same_ref_states(&self.ref_snapshot()?, expected_refs) {
            return Err(NativeGitIndexError::StaleRepositoryState);
        }

        let mut transaction = String::from("start\n");
        for reference in expected_refs
            .iter()
            .filter(|reference| reference.name != target)
        {
            transaction.push_str("verify ");
            transaction.push_str(&reference.name);
            transaction.push(' ');
            transaction.push_str(reference.object.as_str());
            transaction.push('\n');
        }
        transaction.push_str("update ");
        transaction.push_str(target);
        transaction.push(' ');
        transaction.push_str(new_value.as_str());
        transaction.push(' ');
        transaction.push_str(old_value.as_str());
        transaction.push_str("\nprepare\n");

        let mut command = self.command();
        command
            .args(["update-ref", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| NativeGitIndexError::Io("update-ref stdin unavailable".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| NativeGitIndexError::Io("update-ref stdout unavailable".to_owned()))?;
        let mut stdout = BufReader::new(stdout);
        stdin
            .write_all(transaction.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        if !read_update_ref_response(&mut stdout, "prepare: ok")? {
            drop(stdin);
            let _ = child.wait();
            return Err(NativeGitIndexError::StaleRepositoryState);
        }

        let prepared_state = self.ref_snapshot().and_then(|current_refs| {
            same_ref_states(&current_refs, expected_refs)
                .then_some(())
                .ok_or(NativeGitIndexError::StaleRepositoryState)
        });
        if let Err(error) = prepared_state {
            stdin
                .write_all(b"abort\n")
                .and_then(|()| stdin.flush())
                .map_err(|abort_error| NativeGitIndexError::Io(abort_error.to_string()))?;
            let aborted = read_update_ref_response(&mut stdout, "abort: ok")?;
            drop(stdin);
            let status = child
                .wait()
                .map_err(|wait_error| NativeGitIndexError::Io(wait_error.to_string()))?;
            if !aborted || !status.success() {
                return Err(NativeGitIndexError::GitFailed {
                    operation: "update-ref abort",
                    status: status.to_string(),
                });
            }
            return Err(error);
        }

        stdin
            .write_all(b"commit\n")
            .and_then(|()| stdin.flush())
            .map_err(|error| {
                NativeGitIndexError::Io(error.to_string())
                    .into_commit_boundary_unknown("update-ref")
            })?;
        let committed = read_update_ref_response(&mut stdout, "commit: ok")
            .map_err(|error| error.into_commit_boundary_unknown("update-ref"))?;
        drop(stdin);
        let status = child.wait().map_err(|error| {
            NativeGitIndexError::Io(error.to_string()).into_commit_boundary_unknown("update-ref")
        })?;
        if !committed || !status.success() {
            return Err(NativeGitIndexError::GitFailed {
                operation: "update-ref",
                status: status.to_string(),
            }
            .into_commit_boundary_unknown("update-ref"));
        }
        Ok(())
    }
}

fn refs_excluding(refs: &[NativeRefState], excluded: &str) -> Vec<NativeRefState> {
    refs.iter()
        .filter(|reference| reference.name != excluded)
        .cloned()
        .collect()
}

fn same_ref_states(left: &[NativeRefState], right: &[NativeRefState]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.name == right.name
                && left.object == right.object
                && left.symbolic_target == right.symbolic_target
        })
}

fn read_update_ref_response(
    output: &mut impl BufRead,
    expected: &str,
) -> Result<bool, NativeGitIndexError> {
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = output
            .read_line(&mut line)
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        if bytes == 0 {
            return Ok(false);
        }
        if line.trim_end() == expected {
            return Ok(true);
        }
    }
}
