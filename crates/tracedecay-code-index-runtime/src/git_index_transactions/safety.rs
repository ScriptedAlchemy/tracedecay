//! Exact native Git safety evidence and executable-policy classification.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::{Command, Output};

use tracedecay_domain::{GitFileModeV1, GitHeadStateV1, ManifestDigest, canonical_sha256};
use tracedecay_runtime_core::git_discovery::{
    GitRepositoryIdentityOutcome, discover_repository_identity_bounded,
};

use super::process::{read_optional_file, worktree_mode};
use super::{FixedGitIndexRunner, NativeGitIndexError};

impl FixedGitIndexRunner {
    #[tracing::instrument(
        name = "daemon.git.index_tx.tracked_worktree",
        level = "trace",
        skip_all
    )]
    /// Bind worktree identity without reading every clean HEAD blob.
    ///
    /// The path domain is still HEAD ∪ index ∪ non-ignored untracked names, so
    /// staging an already-bound path does not change the digest. A HEAD tree
    /// object id is reused only when Git itself still compares that path: the
    /// dirty set comes from `git diff HEAD` with fsmonitor and ignoreStat
    /// disabled, and assume-unchanged / skip-worktree paths are read as
    /// worktree bytes. Request cancel and deadline are checked between Git
    /// children and paths so a hunks deadline can drop the real index lock.
    pub fn tracked_worktree_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        self.check_cancelled()?;
        let (has_head, head_entries) = self.head_tree_entries()?;
        self.check_cancelled()?;
        let mut paths = head_entries.keys().cloned().collect::<BTreeSet<_>>();
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
        self.check_cancelled()?;
        let mut read_worktree = self.dirty_worktree_paths(has_head)?;
        read_worktree.extend(self.worktree_check_suppressed_paths()?);

        let mut manifest = Vec::new();
        for path in paths {
            self.check_cancelled()?;
            let path_text =
                std::str::from_utf8(&path).map_err(|_| NativeGitIndexError::MalformedOutput {
                    operation: "ls-tree",
                })?;
            let entry = if !read_worktree.contains(&path)
                && let Some(head) = head_entries.get(&path)
            {
                if head.kind == "unsupported" {
                    ("unsupported", Vec::new())
                } else {
                    (head.kind, head.oid.clone())
                }
            } else {
                worktree_manifest_bytes(&self.repository_root.join(path_text))?
            };
            manifest.push((path_text.to_owned(), entry.0, entry.1));
        }
        canonical_sha256(&manifest).map_err(Into::into)
    }

    fn head_tree_entries(
        &self,
    ) -> Result<(bool, BTreeMap<Vec<u8>, HeadTreeEntry>), NativeGitIndexError> {
        match self.head_state()? {
            GitHeadStateV1::Unborn { .. } => Ok((false, BTreeMap::new())),
            GitHeadStateV1::Attached { .. } | GitHeadStateV1::Detached { .. } => {
                let output = self.run_git("ls-tree", &["ls-tree", "-r", "-z", "HEAD"])?;
                let mut entries = BTreeMap::new();
                for raw in output
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter(|entry| !entry.is_empty())
                {
                    let (path, entry) = parse_ls_tree_entry(raw)?;
                    entries.insert(path, entry);
                }
                Ok((true, entries))
            }
        }
    }

    fn dirty_worktree_paths(
        &self,
        has_head: bool,
    ) -> Result<BTreeSet<Vec<u8>>, NativeGitIndexError> {
        if !has_head {
            return Ok(BTreeSet::new());
        }
        Ok(nul_paths(
            &self
                .run_git(
                    "diff",
                    &[
                        "-c",
                        "core.fsmonitor=",
                        "-c",
                        "core.ignoreStat=false",
                        "diff",
                        "-z",
                        "--name-only",
                        "--no-renames",
                        "--no-ext-diff",
                        "--no-textconv",
                        "--no-color",
                        "HEAD",
                    ],
                )?
                .stdout,
        ))
    }

    /// Paths whose worktree bytes Git will not compare (`assume-unchanged`,
    /// `skip-worktree`). Those flags are not byte evidence for an exact
    /// snapshot; the digest must read the worktree itself.
    fn worktree_check_suppressed_paths(&self) -> Result<BTreeSet<Vec<u8>>, NativeGitIndexError> {
        let output = self.run_git("ls-files", &["ls-files", "-v", "-z"])?;
        let mut suppressed = BTreeSet::new();
        for entry in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let (tag, path) = parse_ls_files_tag_path(entry)?;
            if git_tag_suppresses_worktree_check(tag) {
                suppressed.insert(path);
            }
        }
        Ok(suppressed)
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
        let mut command = self.command()?;
        command.args(["check-attr", "-z", "-a", "--stdin"]);
        let attributes = self.run_bounded_git_stdin(command, "check-attr", &paths.stdout)?;
        canonical_sha256(&attributes.stdout).map_err(Into::into)
    }

    /// Whether an external driver can rewrite this repository's content.
    ///
    /// `git config --get-regexp` answers over the whole configuration stack,
    /// `/etc/gitconfig` included. `git lfs install --system`, what every
    /// GitHub-hosted runner image and most developer installs do, defines
    /// `filter.lfs.clean|smudge|process` for every repository on the host, so
    /// treating a *defined* driver as an *applied* one refused every preview
    /// and apply on such a machine, permanently and repository-independently.
    ///
    /// A named driver only runs where gitattributes bind its name to a path,
    /// so the fence intersects the configured driver names with the names this
    /// repository's attributes actually bind. The intersection stays
    /// fail-closed: an ambient definition that some attribute *does* bind is
    /// still refused, wherever that definition or that attribute came from.
    /// `diff.external` has no name to bind, it replaces the diff machinery
    /// for every diff, so it refuses unconditionally.
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
    /// The path domain is the same one `tracked_worktree_digest` binds,
    /// tracked entries plus non-ignored untracked entries, because an
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
        let mut command = self.command()?;
        command.args(["check-attr", "-z", "--stdin", "diff", "merge", "filter"]);
        let output = self.run_bounded_git_stdin(command, "check-attr", &stdin)?;
        // `-z` emits `path NUL attribute NUL value NUL` triples.
        let fields = output.stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
        let mut bound = BTreeSet::new();
        for triple in fields.as_chunks::<3>().0 {
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
            r"^(core\.sparsecheckout|core\.sparsecheckoutcone|index\.sparse)$",
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
            .run_git("ls-files", &["ls-files", "-t", "-z"])?
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

    fn run_bounded_git_stdin(
        &self,
        command: Command,
        operation: &'static str,
        input: &[u8],
    ) -> Result<Output, NativeGitIndexError> {
        let output = self.run_bounded_stdin(command, input)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(NativeGitIndexError::GitFailed {
                operation,
                status: output.status.to_string(),
            })
        }
    }
}

struct HeadTreeEntry {
    kind: &'static str,
    oid: Vec<u8>,
}

fn worktree_manifest_bytes(
    absolute: &Path,
) -> Result<(&'static str, Vec<u8>), NativeGitIndexError> {
    match std::fs::symlink_metadata(absolute) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = std::fs::read_link(absolute)
                .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
            Ok((
                "symlink",
                target.to_string_lossy().into_owned().into_bytes(),
            ))
        }
        Ok(metadata) if metadata.is_file() => Ok((
            if worktree_mode(absolute)
                .is_some_and(|mode| mode.as_str() == GitFileModeV1::EXECUTABLE)
            {
                "executable"
            } else {
                "file"
            },
            std::fs::read(absolute).map_err(|error| NativeGitIndexError::Io(error.to_string()))?,
        )),
        Ok(_) => Ok(("unsupported", Vec::new())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(("absent", Vec::new())),
        Err(error) => Err(NativeGitIndexError::Io(error.to_string())),
    }
}

fn parse_ls_tree_entry(entry: &[u8]) -> Result<(Vec<u8>, HeadTreeEntry), NativeGitIndexError> {
    let delimiter = entry.iter().position(|byte| *byte == b'\t').ok_or(
        NativeGitIndexError::MalformedOutput {
            operation: "ls-tree",
        },
    )?;
    let (metadata, path_with_delimiter) = entry.split_at(delimiter);
    let Some(path) = path_with_delimiter.get(1..).filter(|path| !path.is_empty()) else {
        return Err(NativeGitIndexError::MalformedOutput {
            operation: "ls-tree",
        });
    };
    let mut fields = metadata.split(|byte| *byte == b' ');
    let mode = fields.next().unwrap_or_default();
    let _object_type = fields.next();
    let Some(oid) = fields.next().filter(|oid| !oid.is_empty()) else {
        return Err(NativeGitIndexError::MalformedOutput {
            operation: "ls-tree",
        });
    };
    let kind = match mode {
        b"100644" => "file",
        b"100755" => "executable",
        b"120000" => "symlink",
        _ => "unsupported",
    };
    Ok((
        path.to_vec(),
        HeadTreeEntry {
            kind,
            oid: oid.to_vec(),
        },
    ))
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

fn parse_ls_files_tag_path(entry: &[u8]) -> Result<(u8, Vec<u8>), NativeGitIndexError> {
    let (tag, rest) = entry
        .split_first()
        .ok_or(NativeGitIndexError::MalformedOutput {
            operation: "ls-files",
        })?;
    let Some(path) = rest.strip_prefix(b" ").filter(|path| !path.is_empty()) else {
        return Err(NativeGitIndexError::MalformedOutput {
            operation: "ls-files",
        });
    };
    Ok((*tag, path.to_vec()))
}

fn git_tag_suppresses_worktree_check(tag: u8) -> bool {
    tag.is_ascii_lowercase() || tag == b'S'
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
    use super::{
        NativeGitIndexError, git_tag_suppresses_worktree_check, index_entry_path,
        parse_ls_files_tag_path, parse_ls_tree_entry,
    };

    #[test]
    fn ls_tree_entry_binds_mode_oid_and_path() {
        let (path, entry) =
            parse_ls_tree_entry(b"100644 blob deadbeefcafebabe\tcrates/app.rs").expect("entry");
        assert_eq!(path, b"crates/app.rs");
        assert_eq!(entry.kind, "file");
        assert_eq!(entry.oid, b"deadbeefcafebabe");
        let (path, entry) = parse_ls_tree_entry(b"100755 blob abc\tbin/tool").expect("executable");
        assert_eq!(path, b"bin/tool");
        assert_eq!(entry.kind, "executable");
        let (path, entry) = parse_ls_tree_entry(b"160000 commit def\tvendor/lib").expect("gitlink");
        assert_eq!(path, b"vendor/lib");
        assert_eq!(entry.kind, "unsupported");
        assert!(parse_ls_tree_entry(b"100644 blob deadbeef").is_err());
    }

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

    #[test]
    fn ls_files_status_tag_marks_assume_unchanged_and_skip_worktree() {
        let (tag, path) = parse_ls_files_tag_path(b"H source.rs").expect("cached");
        assert_eq!(tag, b'H');
        assert_eq!(path, b"source.rs");
        assert!(!git_tag_suppresses_worktree_check(tag));
        let (tag, path) = parse_ls_files_tag_path(b"h source.rs").expect("assume-unchanged");
        assert_eq!(path, b"source.rs");
        assert!(git_tag_suppresses_worktree_check(tag));
        let (tag, _) = parse_ls_files_tag_path(b"S source.rs").expect("skip-worktree");
        assert!(git_tag_suppresses_worktree_check(tag));
        let (tag, _) = parse_ls_files_tag_path(b"s source.rs").expect("both flags");
        assert!(git_tag_suppresses_worktree_check(tag));
        assert!(parse_ls_files_tag_path(b"H").is_err());
        assert!(parse_ls_files_tag_path(b"Hsource.rs").is_err());
    }
}
