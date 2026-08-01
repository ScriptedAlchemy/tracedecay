//! Capability-scoped source-edit file authority: opens a candidate file
//! beneath the project root without following symlinks, tracks its identity
//! (device/inode) between the preview read and the atomic publish, and
//! performs the crash-safe temp-file-then-rename publication every edit
//! primitive shares.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, ambient_authority};
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use same_file::Handle;

use crate::errors::{Result, TraceDecayError};

static SOURCE_EDIT_TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct SourceEditFileAuthority {
    root: Dir,
    parent: Dir,
    parent_relative: PathBuf,
    name: OsString,
}

impl SourceEditFileAuthority {
    pub(super) fn open(project_root: &Path, relative: &Path) -> Result<Self> {
        let relative = normalize_source_edit_relative_path(relative)?;
        let root = Dir::open_ambient_dir(project_root, ambient_authority())
            .map_err(|error| source_edit_path_error("open authorized source edit root", error))?;
        let components = relative.components().collect::<Vec<_>>();
        let Some(Component::Normal(name)) = components.last() else {
            return Err(source_edit_unsafe_path());
        };
        let mut parent = root
            .open_dir_nofollow(".")
            .map_err(|error| source_edit_path_error("open source edit root", error))?;
        let mut parent_relative = PathBuf::new();
        for component in &components[..components.len().saturating_sub(1)] {
            let Component::Normal(component) = component else {
                return Err(source_edit_unsafe_path());
            };
            parent = parent.open_dir_nofollow(component).map_err(|error| {
                source_edit_path_error("open source edit parent without following symlinks", error)
            })?;
            parent_relative.push(component);
        }
        Ok(Self {
            root,
            parent,
            parent_relative,
            name: name.to_os_string(),
        })
    }

    pub(super) fn open_optional(&self) -> Result<Option<cap_std::fs::File>> {
        match self.parent.symlink_metadata(&self.name) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => return Err(source_edit_unsafe_path()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(source_edit_path_error(
                    "inspect source edit candidate",
                    error,
                ));
            }
        }
        let mut options = CapOpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let input = self
            .parent
            .open_with(&self.name, &options)
            .map_err(|error| {
                source_edit_path_error(
                    "open source edit candidate without following symlinks",
                    error,
                )
            })?;
        if !input
            .metadata()
            .map_err(|error| source_edit_path_error("inspect opened source edit candidate", error))?
            .is_file()
        {
            return Err(source_edit_unsafe_path());
        }
        Ok(Some(input))
    }

    pub(super) fn read_optional_with_identity(&self) -> Result<(Option<Vec<u8>>, Option<Handle>)> {
        let Some(mut input) = self.open_optional()? else {
            return Ok((None, None));
        };
        let identity = Handle::from_file(
            input
                .try_clone()
                .map_err(|error| source_edit_path_error("clone source edit candidate", error))?
                .into_std(),
        )
        .map_err(|error| source_edit_path_error("identify source edit candidate", error))?;
        let mut bytes = Vec::new();
        input
            .read_to_end(&mut bytes)
            .map_err(|error| source_edit_path_error("read source edit candidate", error))?;
        let current = self
            .current_identity()?
            .ok_or_else(source_edit_unsafe_path)?;
        if current != identity {
            return Err(TraceDecayError::Config {
                message: "source edit candidate changed while it was read".to_owned(),
            });
        }
        Ok((Some(bytes), Some(identity)))
    }

    pub(super) fn read_optional(&self) -> Result<Option<Vec<u8>>> {
        self.read_optional_with_identity().map(|(bytes, _)| bytes)
    }

    pub(super) fn read_to_string(&self, label: &str) -> Result<(String, Handle)> {
        let (bytes, identity) = self.read_optional_with_identity()?;
        let bytes = bytes.ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to read {label}: file was not found"),
        })?;
        let source = String::from_utf8(bytes).map_err(|error| TraceDecayError::Config {
            message: format!("failed to read {label}: {error}"),
        })?;
        let identity = identity.ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to read {label}: opened file identity was missing"),
        })?;
        Ok((source, identity))
    }

    pub(super) fn current_identity(&self) -> Result<Option<Handle>> {
        self.open_optional()?
            .map(|file| {
                Handle::from_file(file.into_std()).map_err(|error| {
                    source_edit_path_error("identify current source edit candidate", error)
                })
            })
            .transpose()
    }

    fn verify_parent_binding(&self) -> Result<()> {
        let current = if self.parent_relative.as_os_str().is_empty() {
            self.root.open_dir_nofollow(".")
        } else {
            let mut current = self.root.open_dir_nofollow(".");
            for component in self.parent_relative.components() {
                let Component::Normal(component) = component else {
                    return Err(source_edit_unsafe_path());
                };
                current = current.and_then(|directory| directory.open_dir_nofollow(component));
            }
            current
        }
        .map_err(|error| {
            source_edit_path_error(
                "revalidate source edit parent without following symlinks",
                error,
            )
        })?;
        let expected = Handle::from_file(
            self.parent
                .try_clone()
                .map_err(|error| source_edit_path_error("clone source edit parent", error))?
                .into_std_file(),
        )
        .map_err(|error| source_edit_path_error("identify source edit parent", error))?;
        let observed = Handle::from_file(current.into_std_file()).map_err(|error| {
            source_edit_path_error("identify rebound source edit parent", error)
        })?;
        if expected != observed {
            return Err(TraceDecayError::Config {
                message: "source edit parent changed before atomic publication".to_owned(),
            });
        }
        Ok(())
    }

    pub(super) fn publish(
        &self,
        relative_path: &str,
        expected: Option<&str>,
        expected_identity: Option<&Handle>,
        intended: &str,
        before_compare: impl FnOnce(),
    ) -> Result<()> {
        self.verify_parent_binding()?;
        // Capture the candidate's current permission bits so the atomic replace
        // does not silently downgrade them. The temporary is still created
        // 0o600 so it is never briefly more permissive than the final file; the
        // original mode is restored on the open handle below.
        #[cfg(unix)]
        let published_mode = {
            use cap_std::fs::PermissionsExt;
            self.metadata()
                .ok()
                .map(|metadata| metadata.permissions().mode())
        };
        let mut before_compare = Some(before_compare);
        for _ in 0..64 {
            let temporary = OsString::from(format!(
                ".tracedecay-source-edit.{}.{}.tmp",
                std::process::id(),
                SOURCE_EDIT_TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = CapOpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .follow(FollowSymlinks::No);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut output = match self.parent.open_with(&temporary, &options) {
                Ok(output) => output,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(source_edit_path_error(
                        "create source edit temporary file",
                        error,
                    ));
                }
            };
            let result = (|| {
                output.write_all(intended.as_bytes()).map_err(|error| {
                    source_edit_path_error("write source edit temporary file", error)
                })?;
                // fchmod on the open handle rather than the path: it is immune
                // to umask (unlike the create mode) and cannot race a swap of
                // the temporary name.
                #[cfg(unix)]
                if let Some(mode) = published_mode {
                    use cap_std::fs::{Permissions, PermissionsExt};
                    output
                        .set_permissions(Permissions::from_mode(mode))
                        .map_err(|error| {
                            source_edit_path_error("preserve source edit permissions", error)
                        })?;
                }
                output.sync_all().map_err(|error| {
                    source_edit_path_error("sync source edit temporary file", error)
                })?;
                drop(output);
                let Some(before_compare) = before_compare.take() else {
                    return Err(TraceDecayError::Config {
                        message: "source edit comparison hook already ran".to_owned(),
                    });
                };
                before_compare();
                self.verify_parent_binding()?;
                let (current, current_identity) = self.read_optional_with_identity()?;
                if current.as_deref() != expected.map(str::as_bytes) {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "source edit candidate {relative_path} changed before atomic publication"
                        ),
                    });
                }
                if current_identity.as_ref() != expected_identity {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "source edit candidate {relative_path} was replaced before atomic publication"
                        ),
                    });
                }
                self.parent
                    .rename(&temporary, &self.parent, &self.name)
                    .map_err(|error| {
                        source_edit_path_error("atomically publish source edit candidate", error)
                    })?;
                sync_source_edit_directory(&self.parent)
            })();
            if result.is_err() {
                let _ = self.parent.remove_file(&temporary);
            }
            return result;
        }
        Err(TraceDecayError::Config {
            message: "could not allocate source edit temporary file".to_owned(),
        })
    }

    pub(super) fn metadata(&self) -> Result<cap_std::fs::Metadata> {
        self.parent
            .symlink_metadata(&self.name)
            .map_err(|error| source_edit_path_error("inspect source edit candidate", error))
    }
}

pub(super) fn read_source_edit_candidate(
    project_root: &Path,
    relative: &Path,
) -> Result<Option<Vec<u8>>> {
    SourceEditFileAuthority::open(project_root, relative)?.read_optional()
}

pub(super) fn normalize_source_edit_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(source_edit_unsafe_path());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => normalized.push(component),
            Component::CurDir => {}
            _ => return Err(source_edit_unsafe_path()),
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(source_edit_unsafe_path());
    }
    Ok(normalized)
}

fn source_edit_unsafe_path() -> TraceDecayError {
    TraceDecayError::Config {
        message: "source edit path is not a regular file beneath the authorized worktree"
            .to_owned(),
    }
}

fn source_edit_path_error(operation: &'static str, error: io::Error) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("{operation}: {error}"),
    }
}

fn sync_source_edit_directory(directory: &Dir) -> Result<()> {
    #[cfg(windows)]
    {
        directory
            .dir_metadata()
            .map(|_| ())
            .map_err(|error| source_edit_path_error("sync source edit parent directory", error))
    }
    #[cfg(not(windows))]
    {
        let mut options = CapOpenOptions::new();
        options.read(true).maybe_dir(true);
        directory
            .open_with(".", &options)
            .and_then(|file| file.sync_all())
            .map_err(|error| source_edit_path_error("sync source edit parent directory", error))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;

    use super::SourceEditFileAuthority;
    #[cfg(unix)]
    use super::read_source_edit_candidate;

    #[test]
    fn atomic_publication_rejects_same_content_inode_swap() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("lib.rs");
        let replacement = directory.path().join("replacement.rs");
        std::fs::write(&path, "previewed\n").unwrap();
        std::fs::write(&replacement, "previewed\n").unwrap();
        let file = SourceEditFileAuthority::open(directory.path(), Path::new("lib.rs")).unwrap();
        let (_, identity) = file.read_to_string("lib.rs").unwrap();

        assert!(
            file.publish(
                "lib.rs",
                Some("previewed\n"),
                Some(&identity),
                "intended\n",
                || std::fs::rename(&replacement, &path).unwrap(),
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "previewed\n");
    }

    #[test]
    fn atomic_publication_rejects_parent_directory_rebinding() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("src");
        let moved = directory.path().join("moved");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("lib.rs"), "previewed\n").unwrap();
        let file =
            SourceEditFileAuthority::open(directory.path(), Path::new("src/lib.rs")).unwrap();
        let (_, identity) = file.read_to_string("src/lib.rs").unwrap();

        assert!(
            file.publish(
                "src/lib.rs",
                Some("previewed\n"),
                Some(&identity),
                "intended\n",
                || {
                    std::fs::rename(&source, &moved).unwrap();
                    std::fs::create_dir(&source).unwrap();
                    std::fs::write(source.join("lib.rs"), "replacement\n").unwrap();
                },
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(moved.join("lib.rs")).unwrap(),
            "previewed\n"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("lib.rs")).unwrap(),
            "replacement\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_read_rejects_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::write(outside.path().join("lib.rs"), "outside\n").unwrap();
        symlink(outside.path(), directory.path().join("src")).unwrap();

        assert!(read_source_edit_candidate(directory.path(), Path::new("src/lib.rs")).is_err());
        assert_eq!(
            std::fs::read_to_string(outside.path().join("lib.rs")).unwrap(),
            "outside\n"
        );
    }
}
