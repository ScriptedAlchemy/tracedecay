//! Exact native Git safety evidence and executable-policy classification.

use std::process::{Command, Stdio};

use tracedecay_domain::{
    GitFileModeV1, GitHeadStateV1, GitOidV1, ManifestDigest, canonical_sha256,
};
use tracedecay_runtime_core::git_discovery::{
    GitRepositoryIdentityOutcome, discover_repository_identity_bounded,
};

use super::process::{
    is_executable_hook, read_optional_file, run_command_with_stdin, worktree_mode,
};
use super::{FixedGitIndexRunner, NativeGitIndexError};

impl FixedGitIndexRunner {
    pub(crate) fn has_applicable_commit_hooks(&self) -> Result<bool, NativeGitIndexError> {
        match self.ensure_no_applicable_hooks() {
            Ok(()) => Ok(false),
            Err(NativeGitIndexError::UnsupportedHookPolicy) => Ok(true),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn signing_key_available(
        &self,
        key_reference: &str,
    ) -> Result<bool, NativeGitIndexError> {
        let format = self.run_git_output(&["config", "--get", "gpg.format"])?;
        let format = if format.status.success() {
            String::from_utf8(format.stdout)
                .map_err(|_| NativeGitIndexError::MalformedOutput {
                    operation: "config",
                })?
                .trim()
                .to_owned()
        } else if format.status.code() == Some(1) {
            "openpgp".to_owned()
        } else {
            return Err(NativeGitIndexError::GitFailed {
                operation: "config",
                status: format.status.to_string(),
            });
        };
        if format != "openpgp" {
            return Ok(false);
        }
        for key in ["gpg.program", "gpg.openpgp.program"] {
            let configured = self.run_git_output(&["config", "--get", key])?;
            if configured.status.success() && !configured.stdout.is_empty() {
                return Ok(false);
            }
            if !configured.status.success() && configured.status.code() != Some(1) {
                return Err(NativeGitIndexError::GitFailed {
                    operation: "config",
                    status: configured.status.to_string(),
                });
            }
        }
        let output = Command::new("gpg")
            .args([
                "--batch",
                "--list-secret-keys",
                "--with-colons",
                "--fingerprint",
                "--",
                key_reference,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let Ok(output) = output else {
            return Ok(false);
        };
        if !output.status.success() {
            return Ok(false);
        }
        let listing = String::from_utf8(output.stdout)
            .map_err(|_| NativeGitIndexError::MalformedOutput { operation: "gpg" })?;
        let mut expect_primary_fingerprint = false;
        let mut fingerprints = std::collections::BTreeSet::new();
        for line in listing.lines() {
            let fields = line.split(':').collect::<Vec<_>>();
            match fields.first().copied() {
                Some("sec") => expect_primary_fingerprint = true,
                Some("ssb") => expect_primary_fingerprint = false,
                Some("fpr") if expect_primary_fingerprint => {
                    if let Some(fingerprint) =
                        fields.get(9).filter(|fingerprint| !fingerprint.is_empty())
                    {
                        fingerprints.insert(fingerprint.to_ascii_uppercase());
                    }
                    expect_primary_fingerprint = false;
                }
                _ => {}
            }
        }
        Ok(fingerprints.len() == 1
            && fingerprints
                .first()
                .is_some_and(|fingerprint| fingerprint == &key_reference.to_ascii_uppercase()))
    }

    pub(super) fn verify_created_commit_signature(
        &self,
        commit: &GitOidV1,
        expected_fingerprint: &str,
    ) -> Result<(), NativeGitIndexError> {
        let output = self
            .command()
            .args(["verify-commit", "--raw", commit.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        let status = [output.stdout, output.stderr].concat();
        let expected = expected_fingerprint.to_ascii_uppercase();
        let valid = String::from_utf8(status).ok().is_some_and(|status| {
            status.lines().any(|line| {
                line.strip_prefix("[GNUPG:] VALIDSIG ")
                    .map(|fields| fields.split_whitespace().collect::<Vec<_>>())
                    .is_some_and(|fields| {
                        fields
                            .first()
                            .is_some_and(|fingerprint| fingerprint.to_ascii_uppercase() == expected)
                            || fields.last().is_some_and(|fingerprint| {
                                fingerprint.to_ascii_uppercase() == expected
                            })
                    })
            })
        });
        if !output.status.success() || !valid {
            return Err(NativeGitIndexError::CommitStateUnsupported);
        }
        Ok(())
    }

    pub(crate) fn tracked_worktree_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let paths = match self.head_state()? {
            GitHeadStateV1::Unborn { .. } => Vec::new(),
            GitHeadStateV1::Attached { .. } | GitHeadStateV1::Detached { .. } => {
                self.run_git("ls-tree", &["ls-tree", "-r", "-z", "--name-only", "HEAD"])?
                    .stdout
            }
        };
        let mut manifest = Vec::new();
        for path in paths
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            let path =
                std::str::from_utf8(path).map_err(|_| NativeGitIndexError::MalformedOutput {
                    operation: "ls-tree",
                })?;
            let absolute = self.repository_root.join(path);
            let entry = match std::fs::symlink_metadata(&absolute) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    let target = std::fs::read_link(&absolute)
                        .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
                    (
                        "symlink",
                        target.to_string_lossy().into_owned().into_bytes(),
                    )
                }
                Ok(metadata) if metadata.is_file() => (
                    if worktree_mode(&absolute)
                        .is_some_and(|mode| mode.as_str() == GitFileModeV1::EXECUTABLE)
                    {
                        "executable"
                    } else {
                        "file"
                    },
                    std::fs::read(&absolute)
                        .map_err(|error| NativeGitIndexError::Io(error.to_string()))?,
                ),
                Ok(_) => ("unsupported", Vec::new()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ("absent", Vec::new())
                }
                Err(error) => return Err(NativeGitIndexError::Io(error.to_string())),
            };
            manifest.push((path.to_owned(), entry.0, entry.1));
        }
        canonical_sha256(&manifest).map_err(Into::into)
    }

    pub(crate) fn configuration_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let output = self.run_git("config", &["config", "--null", "--show-origin", "--list"])?;
        canonical_sha256(&output.stdout).map_err(Into::into)
    }

    pub(crate) fn filesystem_capabilities_digest(
        &self,
    ) -> Result<ManifestDigest, NativeGitIndexError> {
        let output = self.run_git_output(&[
            "config",
            "--null",
            "--get-regexp",
            r"^core\.(filemode|symlinks|ignorecase|precomposeunicode|protecthfs|protectntfs)$",
        ])?;
        let capabilities = if output.status.success() {
            output.stdout
        } else if output.status.code() == Some(1) {
            Vec::new()
        } else {
            return Err(NativeGitIndexError::GitFailed {
                operation: "config",
                status: output.status.to_string(),
            });
        };
        canonical_sha256(&capabilities).map_err(Into::into)
    }

    pub(crate) fn attributes_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let paths = self.run_git("ls-files", &["ls-files", "-z"])?;
        let mut command = self.command();
        command
            .args(["check-attr", "-z", "-a", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let attributes = run_command_with_stdin(command, "check-attr", &paths.stdout)?;
        canonical_sha256(&attributes.stdout).map_err(Into::into)
    }

    pub(crate) fn has_external_drivers(&self) -> Result<bool, NativeGitIndexError> {
        let output = self.run_git_output(&[
            "config",
            "--get-regexp",
            r"^(diff\.external|merge\..*\.driver|diff\..*\.(command|textconv)|filter\..*\.(clean|smudge|process))$",
        ])?;
        if output.status.success() {
            return Ok(!output.stdout.is_empty());
        }
        if output.status.code() == Some(1) {
            return Ok(false);
        }
        Err(NativeGitIndexError::GitFailed {
            operation: "config",
            status: output.status.to_string(),
        })
    }

    pub(crate) fn sparse_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let sparse_path = self.git_dir.join("info").join("sparse-checkout");
        let sparse_bytes = read_optional_file(&sparse_path)?;
        let config = self.run_git_output(&[
            "config",
            "--null",
            "--get-regexp",
            r"^(core\.sparseCheckout|core\.sparseCheckoutCone|index\.sparse)$",
        ])?;
        let config = if config.status.success() {
            config.stdout
        } else if config.status.code() == Some(1) {
            Vec::new()
        } else {
            return Err(NativeGitIndexError::GitFailed {
                operation: "config",
                status: config.status.to_string(),
            });
        };
        let sparse_entries = self
            .run_git("ls-files", &["ls-files", "--sparse", "-t", "-z"])?
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| entry.starts_with(b"S "))
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        canonical_sha256(&(sparse_bytes, config, sparse_entries)).map_err(Into::into)
    }

    pub(crate) fn submodule_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let gitlinks = self
            .run_git("ls-files", &["ls-files", "--stage", "-z"])?
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| entry.starts_with(b"160000 "))
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        let gitmodules = read_optional_file(&self.repository_root.join(".gitmodules"))?;
        let nested = if gitlinks.is_empty() {
            Vec::new()
        } else {
            self.run_git(
                "submodule",
                &[
                    "-c",
                    "protocol.file.allow=never",
                    "submodule",
                    "status",
                    "--recursive",
                ],
            )?
            .stdout
        };
        canonical_sha256(&(gitlinks, gitmodules, nested)).map_err(Into::into)
    }

    pub(super) fn ensure_no_applicable_hooks(&self) -> Result<(), NativeGitIndexError> {
        let configured = self.run_git_output(&["config", "--get", "core.hooksPath"])?;
        if configured.status.success() && !configured.stdout.is_empty() {
            return Err(NativeGitIndexError::UnsupportedHookPolicy);
        }
        if !configured.status.success() && configured.status.code() != Some(1) {
            return Err(NativeGitIndexError::GitFailed {
                operation: "config",
                status: configured.status.to_string(),
            });
        }
        for hook in [
            "pre-commit",
            "pre-merge-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
            "reference-transaction",
        ] {
            if is_executable_hook(&self.common_dir.join("hooks").join(hook)) {
                return Err(NativeGitIndexError::UnsupportedHookPolicy);
            }
        }
        Ok(())
    }

    pub(super) fn repository_identity_unchanged(&self) -> bool {
        matches!(
            discover_repository_identity_bounded(&self.repository_root),
            GitRepositoryIdentityOutcome::Resolved(identity)
                if identity.worktree_root == self.repository_root
                    && identity.git_dir == self.git_dir
                    && identity.common_dir == self.common_dir
        )
    }
}
