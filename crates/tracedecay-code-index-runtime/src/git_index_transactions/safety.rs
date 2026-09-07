//! Exact native Git safety evidence and executable-policy classification.

use std::collections::BTreeSet;
use std::process::Stdio;

use tracedecay_domain::{GitFileModeV1, GitHeadStateV1, ManifestDigest, canonical_sha256};
use tracedecay_runtime_core::git_discovery::{
    GitRepositoryIdentityOutcome, discover_repository_identity_bounded,
};

use super::process::{read_optional_file, run_command_with_stdin, worktree_mode};
use super::{FixedGitIndexRunner, NativeGitIndexError};

impl FixedGitIndexRunner {
    #[hotpath::measure(label = "daemon.git.index_tx.tracked_worktree")]
    pub fn tracked_worktree_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let head_paths = match self.head_state()? {
            GitHeadStateV1::Unborn { .. } => Vec::new(),
            GitHeadStateV1::Attached { .. } | GitHeadStateV1::Detached { .. } => {
                self.run_git("ls-tree", &["ls-tree", "-r", "-z", "--name-only", "HEAD"])?
                    .stdout
            }
        };
        let mut paths = nul_paths(&head_paths);
        let index = self.run_git("ls-files", &["ls-files", "--stage", "-z"])?;
        for entry in index
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let path = index_entry_path(entry)?;
            paths.insert(path.to_vec());
        }
        // An untracked path may become an index entry during the intended
        // publication. Including it in the same manifest before and after
        // staging binds its bytes without making the digest index-relative.
        paths.extend(self.other_paths(false)?);

        let mut manifest = Vec::new();
        for path in paths {
            let path =
                std::str::from_utf8(&path).map_err(|_| NativeGitIndexError::MalformedOutput {
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

    pub fn untracked_name_digest(&self) -> Result<Option<ManifestDigest>, NativeGitIndexError> {
        self.other_name_digest(false)
    }

    pub fn ignored_name_digest(&self) -> Result<Option<ManifestDigest>, NativeGitIndexError> {
        self.other_name_digest(true)
    }

    fn other_name_digest(
        &self,
        ignored: bool,
    ) -> Result<Option<ManifestDigest>, NativeGitIndexError> {
        let paths = self.other_paths(ignored)?;
        (!paths.is_empty())
            .then(|| canonical_sha256(&paths))
            .transpose()
            .map_err(Into::into)
    }

    fn other_paths(&self, ignored: bool) -> Result<BTreeSet<Vec<u8>>, NativeGitIndexError> {
        let mut args = vec!["ls-files", "--others"];
        if ignored {
            args.push("--ignored");
        }
        args.extend(["--exclude-standard", "-z"]);
        Ok(nul_paths(&self.run_git("ls-files", &args)?.stdout))
    }

    pub fn configuration_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let output = self.run_git("config", &["config", "--null", "--show-origin", "--list"])?;
        canonical_sha256(&output.stdout).map_err(Into::into)
    }

    pub fn filesystem_capabilities_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
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

    pub fn attributes_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
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

    /// Whether an external driver can rewrite this repository's content.
    ///
    /// `git config --get-regexp` answers over the whole configuration stack,
    /// `/etc/gitconfig` included. `git lfs install --system` — what every
    /// GitHub-hosted runner image and most developer installs do — defines
    /// `filter.lfs.clean|smudge|process` for every repository on the host, so
    /// treating a *defined* driver as an *applied* one refused every preview
    /// and apply on such a machine, permanently and repository-independently.
    ///
    /// A named driver only runs where gitattributes bind its name to a path,
    /// so the fence intersects the configured driver names with the names this
    /// repository's attributes actually bind. The intersection stays
    /// fail-closed: an ambient definition that some attribute *does* bind is
    /// still refused, wherever that definition or that attribute came from.
    /// `diff.external` has no name to bind — it replaces the diff machinery
    /// for every diff — so it refuses unconditionally.
    pub fn has_external_drivers(&self) -> Result<bool, NativeGitIndexError> {
        let (external_diff_driver, named_drivers) = self.configured_drivers()?;
        if external_diff_driver {
            return Ok(true);
        }
        if named_drivers.is_empty() {
            return Ok(false);
        }
        let bound = self.attribute_bound_driver_names()?;
        Ok(!named_drivers.is_disjoint(&bound))
    }

    /// `(diff.external is configured, configured named driver subsections)`.
    fn configured_drivers(&self) -> Result<(bool, BTreeSet<String>), NativeGitIndexError> {
        let output = self.run_git_output(&[
            "config",
            "--null",
            "--get-regexp",
            r"^(diff\.external|merge\..*\.driver|diff\..*\.(command|textconv)|filter\..*\.(clean|smudge|process))$",
        ])?;
        let records = if output.status.success() {
            output.stdout
        } else if output.status.code() == Some(1) {
            Vec::new()
        } else {
            return Err(NativeGitIndexError::GitFailed {
                operation: "config",
                status: output.status.to_string(),
            });
        };
        let mut external_diff_driver = false;
        let mut named_drivers = BTreeSet::new();
        // `--null` emits `key NL value NUL` per record; a valueless key emits
        // `key NUL`. Only the key selects the driver, so the value is ignored.
        for record in records
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let key = record
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap_or_default();
            let key =
                std::str::from_utf8(key).map_err(|_| NativeGitIndexError::MalformedOutput {
                    operation: "config",
                })?;
            if key == "diff.external" {
                external_diff_driver = true;
            } else if let Some(name) = driver_subsection_name(key) {
                named_drivers.insert(name.to_owned());
            }
        }
        Ok((external_diff_driver, named_drivers))
    }

    /// Driver names this repository's gitattributes bind to a path.
    ///
    /// The path domain is the same one `tracked_worktree_digest` binds —
    /// tracked entries plus non-ignored untracked entries — because an
    /// untracked path may become an index entry during the intended
    /// publication and must be fenced before, not after, it is staged.
    fn attribute_bound_driver_names(&self) -> Result<BTreeSet<String>, NativeGitIndexError> {
        let mut paths = nul_paths(&self.run_git("ls-files", &["ls-files", "-z"])?.stdout);
        paths.extend(self.other_paths(false)?);
        if paths.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut stdin = Vec::new();
        for path in &paths {
            stdin.extend_from_slice(path);
            stdin.push(0);
        }
        let mut command = self.command();
        command
            .args(["check-attr", "-z", "--stdin", "diff", "merge", "filter"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let output = run_command_with_stdin(command, "check-attr", &stdin)?;
        // `-z` emits `path NUL attribute NUL value NUL` triples.
        let fields = output.stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
        let mut bound = BTreeSet::new();
        for triple in fields.chunks_exact(3) {
            let Ok(value) = std::str::from_utf8(triple[2]) else {
                continue;
            };
            if matches!(value, "unspecified" | "unset" | "set" | "") {
                continue;
            }
            bound.insert(value.to_owned());
        }
        Ok(bound)
    }

    pub fn sparse_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
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

    pub fn submodule_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
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

    pub fn repository_identity_unchanged(&self) -> bool {
        matches!(
            discover_repository_identity_bounded(&self.repository_root),
            GitRepositoryIdentityOutcome::Resolved(identity)
                if identity.worktree_root == self.repository_root
                    && identity.git_dir == self.git_dir
                    && identity.common_dir == self.common_dir
        )
    }
}

/// The subsection of a named-driver configuration key, or `None` when the key
/// names no driver. Git subsection names may themselves contain `.`, so the
/// name is whatever the fixed prefix and suffix leave behind.
fn driver_subsection_name(key: &str) -> Option<&str> {
    for (prefix, suffix) in [
        ("merge.", ".driver"),
        ("diff.", ".command"),
        ("diff.", ".textconv"),
        ("filter.", ".clean"),
        ("filter.", ".smudge"),
        ("filter.", ".process"),
    ] {
        if let Some(name) = key
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            && !name.is_empty()
        {
            return Some(name);
        }
    }
    None
}

fn nul_paths(bytes: &[u8]) -> BTreeSet<Vec<u8>> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

fn index_entry_path(entry: &[u8]) -> Result<&[u8], NativeGitIndexError> {
    let delimiter = entry.iter().position(|byte| *byte == b'\t').ok_or(
        NativeGitIndexError::MalformedOutput {
            operation: "ls-files",
        },
    )?;
    let (metadata, path_with_delimiter) = entry.split_at(delimiter);
    let Some(path) = path_with_delimiter.get(1..).filter(|path| !path.is_empty()) else {
        return Err(NativeGitIndexError::MalformedOutput {
            operation: "ls-files",
        });
    };
    if metadata.is_empty() {
        return Err(NativeGitIndexError::MalformedOutput {
            operation: "ls-files",
        });
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{NativeGitIndexError, index_entry_path};

    #[test]
    fn index_entry_path_rejects_missing_or_empty_path_bytes() {
        for malformed in [
            b"100644 deadbeef 0 path.txt".as_slice(),
            b"100644 deadbeef 0\t".as_slice(),
            b"\tpath.txt".as_slice(),
        ] {
            assert!(matches!(
                index_entry_path(malformed),
                Err(NativeGitIndexError::MalformedOutput {
                    operation: "ls-files"
                })
            ));
        }
        assert_eq!(
            index_entry_path(b"100644 deadbeef 0\tpath\twith-tab.txt").expect("valid entry"),
            b"path\twith-tab.txt"
        );
    }
}
