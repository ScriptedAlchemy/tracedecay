//! Capability-relative quarantine for whole code-index scope roots.
//!
//! The retention journal stores the exact filesystem identity captured before
//! quarantine. Recovery reopens every component without following symlinks and
//! requires that identity before either restoring or recursively unlinking it.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
use std::ffi::CString;
use std::ffi::OsStr;
use std::io;
use std::path::Path;

#[cfg(not(windows))]
use cap_fs_ext::OpenOptionsMaybeDirExt;
use cap_fs_ext::{DirExt, ambient_authority};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};

use super::{
    CodeGenerationRetentionErrorV1, SCOPE_RETENTION_QUARANTINE_DIRECTORY, StrandedCodeIndexScopeV1,
    is_code_index_scope_hash, storage,
};

/// Rename-stable identity for one scope root. This mirrors the orphan-store
/// retirement fence: inode/device are authoritative on Unix, while other
/// platforms retain the strongest portable creation and modification stamps.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ScopeDirectoryIdentityV1 {
    modified_secs: i64,
    modified_nanos: u32,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    created_100ns: u64,
}

/// Already-open authority for the store root and its exact quarantine tree.
/// Ambient paths are used only to acquire the store handle; every directory
/// below it is opened no-follow and every mutation is relative to a handle.
pub(super) struct ScopeQuarantineAuthority {
    store: Dir,
    quarantine: Option<Dir>,
    stage: Option<Dir>,
    receipt_digest: String,
    scope_identities: BTreeMap<String, ScopeDirectoryIdentityV1>,
    source_handles: BTreeMap<String, Dir>,
}

impl ScopeQuarantineAuthority {
    pub(super) fn prepare(
        store_root: &Path,
        receipt_digest: &str,
        scopes: &[StrandedCodeIndexScopeV1],
    ) -> Result<Self, CodeGenerationRetentionErrorV1> {
        validate_digest(receipt_digest)?;
        let store = open_store_root(store_root)?;
        let mut scope_identities = BTreeMap::new();
        let mut source_handles = BTreeMap::new();
        for scope in scopes {
            validate_scope_name(&scope.scope_hash)?;
            let source = store
                .open_dir_nofollow(&scope.scope_hash)
                .map_err(storage)?;
            let identity = directory_identity(&source).map_err(storage)?;
            scope_identities.insert(scope.scope_hash.clone(), identity);
            source_handles.insert(scope.scope_hash.clone(), source);
        }
        let mut authority = Self {
            store,
            quarantine: None,
            stage: None,
            receipt_digest: receipt_digest.to_owned(),
            scope_identities,
            source_handles,
        };
        authority.open_or_create_stage()?;
        Ok(authority)
    }

    pub(super) fn recover(
        store_root: &Path,
        receipt_digest: &str,
        scope_identities: BTreeMap<String, ScopeDirectoryIdentityV1>,
    ) -> Result<Self, CodeGenerationRetentionErrorV1> {
        validate_digest(receipt_digest)?;
        if scope_identities
            .keys()
            .any(|scope_hash| !is_code_index_scope_hash(scope_hash))
        {
            return Err(unsafe_state(
                "scope quarantine journal contains a non-scope identity",
            ));
        }
        let store = open_store_root(store_root)?;
        let quarantine = open_optional_dir(&store, SCOPE_RETENTION_QUARANTINE_DIRECTORY)?;
        let stage = match quarantine.as_ref() {
            Some(quarantine) => open_optional_dir(quarantine, receipt_digest)?,
            None => None,
        };
        Ok(Self {
            store,
            quarantine,
            stage,
            receipt_digest: receipt_digest.to_owned(),
            scope_identities,
            source_handles: BTreeMap::new(),
        })
    }

    pub(super) fn scope_identities(&self) -> &BTreeMap<String, ScopeDirectoryIdentityV1> {
        &self.scope_identities
    }

    pub(super) fn stage(
        &mut self,
        scopes: &[StrandedCodeIndexScopeV1],
    ) -> Result<(), CodeGenerationRetentionErrorV1> {
        self.require_exact_scopes(scopes)?;
        self.open_or_create_stage()?;
        let stage = self.stage.as_ref().ok_or_else(|| {
            unsafe_state("scope reconciliation quarantine stage could not be opened")
        })?;
        for scope in scopes {
            let expected = self.expected_identity(&scope.scope_hash)?.clone();
            let held_source = self.source_handles.get(&scope.scope_hash).ok_or_else(|| {
                unsafe_state("scope quarantine lost its pre-rename source capability")
            })?;
            if directory_identity(held_source).map_err(storage)? != expected {
                return Err(identity_changed(&scope.scope_hash, "before quarantine"));
            }
            let source = open_child_directory(&self.store, &scope.scope_hash)?;
            let staged = open_child_directory(stage, &scope.scope_hash)?;
            match (source, staged) {
                (Some((source, actual)), None) => {
                    if actual != expected
                        || directory_identity(&source).map_err(storage)? != expected
                    {
                        return Err(identity_changed(&scope.scope_hash, "before quarantine"));
                    }
                    rename_noreplace(
                        &self.store,
                        OsStr::new(&scope.scope_hash),
                        stage,
                        OsStr::new(&scope.scope_hash),
                    )
                    .map_err(storage)?;
                    let (moved, moved_identity) = open_child_directory(stage, &scope.scope_hash)?
                        .ok_or_else(|| {
                        unsafe_state("scope quarantine rename did not publish its destination")
                    })?;
                    if moved_identity != expected
                        || directory_identity(&moved).map_err(storage)? != expected
                    {
                        return Err(identity_changed(&scope.scope_hash, "after quarantine"));
                    }
                    sync_directory(&self.store).map_err(storage)?;
                    sync_directory(stage).map_err(storage)?;
                }
                (None, None) => {
                    return Err(unsafe_state(format!(
                        "stranded scope '{}' is missing before quarantine",
                        scope.scope_hash
                    )));
                }
                (None, Some(_)) => {
                    return Err(unsafe_state(format!(
                        "stranded scope '{}' was already quarantined",
                        scope.scope_hash
                    )));
                }
                (Some(_), Some(_)) => {
                    return Err(unsafe_state(format!(
                        "stranded scope '{}' exists in both source and quarantine",
                        scope.scope_hash
                    )));
                }
            }
        }
        self.source_handles.clear();
        Ok(())
    }

    pub(super) fn rollback(
        &mut self,
        scopes: &[StrandedCodeIndexScopeV1],
    ) -> Result<(), CodeGenerationRetentionErrorV1> {
        self.require_exact_scopes(scopes)?;
        for scope in scopes {
            let expected = self.expected_identity(&scope.scope_hash)?.clone();
            let source = open_child_directory(&self.store, &scope.scope_hash)?;
            let staged = match self.stage.as_ref() {
                Some(stage) => open_child_directory(stage, &scope.scope_hash)?,
                None => None,
            };
            match (source, staged) {
                (Some((_, actual)), None) if actual == expected => {}
                (None, Some((staged, actual))) if actual == expected => {
                    if directory_identity(&staged).map_err(storage)? != expected {
                        return Err(identity_changed(&scope.scope_hash, "before rollback"));
                    }
                    let stage = self.stage.as_ref().ok_or_else(|| {
                        unsafe_state("scope rollback lost its quarantine capability")
                    })?;
                    rename_noreplace(
                        stage,
                        OsStr::new(&scope.scope_hash),
                        &self.store,
                        OsStr::new(&scope.scope_hash),
                    )
                    .map_err(storage)?;
                    let (_, restored) = open_child_directory(&self.store, &scope.scope_hash)?
                        .ok_or_else(|| unsafe_state("scope rollback did not restore its source"))?;
                    if restored != expected {
                        return Err(identity_changed(&scope.scope_hash, "after rollback"));
                    }
                    sync_directory(&self.store).map_err(storage)?;
                    sync_directory(stage).map_err(storage)?;
                }
                (None, None) => {
                    return Err(unsafe_state(format!(
                        "scope reconciliation rollback cannot find '{}'",
                        scope.scope_hash
                    )));
                }
                (Some(_), Some(_)) => {
                    return Err(unsafe_state(format!(
                        "scope reconciliation rollback found duplicate '{}'",
                        scope.scope_hash
                    )));
                }
                (Some(_), None) | (None, Some(_)) => {
                    return Err(identity_changed(&scope.scope_hash, "during rollback"));
                }
            }
        }
        self.remove_empty_stage()
    }

    pub(super) fn cleanup_committed(
        &mut self,
        scopes: &[StrandedCodeIndexScopeV1],
    ) -> Result<(), CodeGenerationRetentionErrorV1> {
        self.require_exact_scopes(scopes)?;
        for scope in scopes {
            let expected = self.expected_identity(&scope.scope_hash)?.clone();
            if open_child_directory(&self.store, &scope.scope_hash)?.is_some() {
                return Err(unsafe_state(format!(
                    "scope reconciliation receipt is durable but '{}' returned to the store root",
                    scope.scope_hash
                )));
            }
            let staged = match self.stage.as_ref() {
                Some(stage) => open_child_directory(stage, &scope.scope_hash)?,
                None => None,
            };
            if let Some((staged, actual)) = staged {
                if actual != expected || directory_identity(&staged).map_err(storage)? != expected {
                    return Err(identity_changed(&scope.scope_hash, "before unlink"));
                }
                remove_open_dir_all_nofollow(staged).map_err(storage)?;
                if let Some(stage) = self.stage.as_ref() {
                    sync_directory(stage).map_err(storage)?;
                }
            }
        }
        self.remove_empty_stage()
    }

    fn expected_identity(
        &self,
        scope_hash: &str,
    ) -> Result<&ScopeDirectoryIdentityV1, CodeGenerationRetentionErrorV1> {
        validate_scope_name(scope_hash)?;
        self.scope_identities.get(scope_hash).ok_or_else(|| {
            unsafe_state(format!(
                "scope quarantine journal has no filesystem identity for '{scope_hash}'"
            ))
        })
    }

    fn require_exact_scopes(
        &self,
        scopes: &[StrandedCodeIndexScopeV1],
    ) -> Result<(), CodeGenerationRetentionErrorV1> {
        let requested = scopes
            .iter()
            .map(|scope| scope.scope_hash.as_str())
            .collect::<BTreeSet<_>>();
        let fenced = self
            .scope_identities
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if requested.len() != scopes.len() || requested != fenced {
            return Err(unsafe_state(
                "scope quarantine candidates do not match the durable identity fence",
            ));
        }
        Ok(())
    }

    fn open_or_create_stage(&mut self) -> Result<(), CodeGenerationRetentionErrorV1> {
        if self.stage.is_some() {
            return Ok(());
        }
        let quarantine = open_or_create_dir(&self.store, SCOPE_RETENTION_QUARANTINE_DIRECTORY)?;
        let stage = open_or_create_dir(&quarantine, &self.receipt_digest)?;
        sync_directory(&quarantine).map_err(storage)?;
        sync_directory(&self.store).map_err(storage)?;
        self.quarantine = Some(quarantine);
        self.stage = Some(stage);
        Ok(())
    }

    fn remove_empty_stage(&mut self) -> Result<(), CodeGenerationRetentionErrorV1> {
        let Some(stage) = self.stage.take() else {
            return Ok(());
        };
        if stage.read_dir(".").map_err(storage)?.next().is_some() {
            self.stage = Some(stage);
            return Err(unsafe_state(
                "scope reconciliation quarantine contains unexpected entries",
            ));
        }
        stage.remove_open_dir().map_err(storage)?;
        if let Some(quarantine) = self.quarantine.as_ref() {
            sync_directory(quarantine).map_err(storage)?;
        }
        Ok(())
    }
}

fn open_store_root(store_root: &Path) -> Result<Dir, CodeGenerationRetentionErrorV1> {
    let canonical = store_root.canonicalize().map_err(storage)?;
    Dir::open_ambient_dir(canonical, ambient_authority()).map_err(storage)
}

fn open_or_create_dir(parent: &Dir, name: &str) -> Result<Dir, CodeGenerationRetentionErrorV1> {
    match parent.open_dir_nofollow(name) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match parent.create_dir(name) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(storage(error)),
            }
            parent.open_dir_nofollow(name).map_err(storage)
        }
        Err(error) => Err(storage(error)),
    }
}

fn open_optional_dir(
    parent: &Dir,
    name: &str,
) -> Result<Option<Dir>, CodeGenerationRetentionErrorV1> {
    match parent.open_dir_nofollow(name) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

fn open_child_directory(
    parent: &Dir,
    name: &str,
) -> Result<Option<(Dir, ScopeDirectoryIdentityV1)>, CodeGenerationRetentionErrorV1> {
    match parent.open_dir_nofollow(name) {
        Ok(directory) => {
            let identity = directory_identity(&directory).map_err(storage)?;
            Ok(Some((directory, identity)))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage(error)),
    }
}

fn directory_identity(directory: &Dir) -> io::Result<ScopeDirectoryIdentityV1> {
    let metadata = directory.metadata(".")?;
    #[cfg(unix)]
    let (modified_secs, modified_nanos) = {
        use cap_std::fs::MetadataExt;
        (
            metadata.mtime(),
            u32::try_from(metadata.mtime_nsec())
                .map_err(|_| io::Error::other("invalid scope mtime nanoseconds"))?,
        )
    };
    #[cfg(not(unix))]
    let (modified_secs, modified_nanos) = {
        let modified = metadata
            .modified()?
            .into_std()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| io::Error::other("scope metadata precedes the Unix epoch"))?;
        (
            i64::try_from(modified.as_secs())
                .map_err(|_| io::Error::other("scope mtime exceeds supported range"))?,
            modified.subsec_nanos(),
        )
    };
    Ok(ScopeDirectoryIdentityV1 {
        modified_secs,
        modified_nanos,
        #[cfg(unix)]
        device: {
            use cap_std::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        inode: {
            use cap_std::fs::MetadataExt;
            metadata.ino()
        },
        #[cfg(windows)]
        created_100ns: {
            use cap_std::fs::MetadataExt;
            metadata.creation_time()
        },
    })
}

fn remove_open_dir_all_nofollow(directory: Dir) -> io::Result<()> {
    for entry in directory.read_dir(".")? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_dir() {
            let child = directory.open_dir_nofollow(&name)?;
            remove_open_dir_all_nofollow(child)?;
        } else {
            entry.remove_file()?;
        }
    }
    directory.remove_open_dir()
}

fn sync_directory(directory: &Dir) -> io::Result<()> {
    #[cfg(windows)]
    {
        directory.dir_metadata().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.read(true).maybe_dir(true);
        directory
            .open_with(".", &options)
            .and_then(|file| file.sync_all())
    }
}

fn validate_scope_name(scope_hash: &str) -> Result<(), CodeGenerationRetentionErrorV1> {
    if is_code_index_scope_hash(scope_hash) {
        Ok(())
    } else {
        Err(unsafe_state(
            "scope quarantine received a non-scope directory name",
        ))
    }
}

fn validate_digest(receipt_digest: &str) -> Result<(), CodeGenerationRetentionErrorV1> {
    if receipt_digest.len() == 64
        && receipt_digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(unsafe_state(
            "scope quarantine received an invalid receipt digest",
        ))
    }
}

fn identity_changed(scope_hash: &str, boundary: &str) -> CodeGenerationRetentionErrorV1 {
    unsafe_state(format!(
        "stranded scope '{scope_hash}' changed filesystem identity {boundary}"
    ))
}

fn unsafe_state(message: impl Into<String>) -> CodeGenerationRetentionErrorV1 {
    CodeGenerationRetentionErrorV1::UnsafeState(message.into())
}

/// Capability-relative no-replace rename, matching orphan-store retirement.
fn rename_noreplace(
    from_parent: &Dir,
    from: &OsStr,
    to_parent: &Dir,
    to: &OsStr,
) -> io::Result<()> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(from.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
        let to = CString::new(to.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
        // SAFETY: both names are single component C strings and both fds are
        // already-open directory capabilities.
        let result = unsafe {
            libc::renameat2(
                from_parent.as_raw_fd(),
                from.as_ptr(),
                to_parent.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        return if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(from.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
        let to = CString::new(to.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
        // SAFETY: both names are single component C strings and RENAME_EXCL
        // refuses an occupied destination.
        let result = unsafe {
            libc::renameatx_np(
                from_parent.as_raw_fd(),
                from.as_ptr(),
                to_parent.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        return if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
    }
    #[cfg(windows)]
    {
        return from_parent.rename(from, to_parent, to);
    }
    #[cfg(not(any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        windows
    )))]
    {
        let _ = (from_parent, from, to_parent, to);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable on this platform",
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    const SCOPE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const RECEIPT_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn fixture() -> (tempfile::TempDir, StrandedCodeIndexScopeV1) {
        let store = tempfile::TempDir::new().expect("create scope quarantine store");
        std::fs::create_dir(store.path().join(SCOPE_HASH)).expect("create scope root");
        std::fs::write(store.path().join(SCOPE_HASH).join("payload"), b"owned")
            .expect("write scope payload");
        (
            store,
            StrandedCodeIndexScopeV1 {
                scope_hash: SCOPE_HASH.to_owned(),
                size_bytes: 5,
                newest_mtime_secs: 0,
            },
        )
    }

    #[test]
    fn rename_uses_open_quarantine_parent_after_ambient_parent_becomes_symlink() {
        let (store, scope) = fixture();
        let external = tempfile::TempDir::new().expect("create external target");
        let mut authority =
            ScopeQuarantineAuthority::prepare(store.path(), RECEIPT_DIGEST, &[scope.clone()])
                .expect("open quarantine authority");
        let quarantine = store.path().join(SCOPE_RETENTION_QUARANTINE_DIRECTORY);
        let held = store.path().join("quarantine-held");
        std::fs::rename(&quarantine, &held).expect("swap quarantine parent");
        symlink(external.path(), &quarantine).expect("replace quarantine parent with symlink");

        authority
            .stage(std::slice::from_ref(&scope))
            .expect("rename remains bound to the already-open quarantine parent");

        assert!(!store.path().join(SCOPE_HASH).exists());
        assert!(held.join(RECEIPT_DIGEST).join(SCOPE_HASH).is_dir());
        assert!(
            std::fs::read_dir(external.path())
                .expect("read external target")
                .next()
                .is_none()
        );
    }

    #[test]
    fn unlink_uses_open_stage_after_ambient_parent_becomes_symlink() {
        let (store, scope) = fixture();
        let external = tempfile::TempDir::new().expect("create external target");
        let external_scope = external.path().join(RECEIPT_DIGEST).join(SCOPE_HASH);
        std::fs::create_dir_all(&external_scope).expect("create external sentinel tree");
        std::fs::write(external_scope.join("sentinel"), b"preserve")
            .expect("write external sentinel");
        let mut authority =
            ScopeQuarantineAuthority::prepare(store.path(), RECEIPT_DIGEST, &[scope.clone()])
                .expect("open quarantine authority");
        authority
            .stage(std::slice::from_ref(&scope))
            .expect("stage scope");
        let quarantine = store.path().join(SCOPE_RETENTION_QUARANTINE_DIRECTORY);
        let held = store.path().join("quarantine-held");
        std::fs::rename(&quarantine, &held).expect("swap quarantine parent");
        symlink(external.path(), &quarantine).expect("replace quarantine parent with symlink");

        authority
            .cleanup_committed(std::slice::from_ref(&scope))
            .expect("unlink remains bound to the already-open stage");

        assert!(!held.join(RECEIPT_DIGEST).exists());
        assert_eq!(
            std::fs::read(external_scope.join("sentinel")).expect("external sentinel survives"),
            b"preserve"
        );
    }

    #[test]
    fn rename_refuses_a_replacement_scope_with_the_same_name() {
        let (store, scope) = fixture();
        let mut authority =
            ScopeQuarantineAuthority::prepare(store.path(), RECEIPT_DIGEST, &[scope.clone()])
                .expect("open quarantine authority");
        let source = store.path().join(SCOPE_HASH);
        let displaced = store.path().join("scope-held");
        std::fs::rename(&source, &displaced).expect("displace fenced scope");
        std::fs::create_dir(&source).expect("create replacement scope");
        std::fs::write(source.join("payload"), b"replacement").expect("write replacement scope");

        let error = authority
            .stage(std::slice::from_ref(&scope))
            .expect_err("replacement identity must not inherit the collection decision");

        assert!(matches!(
            error,
            CodeGenerationRetentionErrorV1::UnsafeState(_)
        ));
        assert_eq!(
            std::fs::read(source.join("payload")).expect("replacement survives"),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(displaced.join("payload")).expect("fenced scope survives"),
            b"owned"
        );
    }

    #[test]
    fn unlink_refuses_a_replacement_at_the_quarantined_name() {
        let (store, scope) = fixture();
        let mut authority =
            ScopeQuarantineAuthority::prepare(store.path(), RECEIPT_DIGEST, &[scope.clone()])
                .expect("open quarantine authority");
        authority
            .stage(std::slice::from_ref(&scope))
            .expect("stage scope");
        let staged = store
            .path()
            .join(SCOPE_RETENTION_QUARANTINE_DIRECTORY)
            .join(RECEIPT_DIGEST)
            .join(SCOPE_HASH);
        let displaced = store.path().join("staged-held");
        std::fs::rename(&staged, &displaced).expect("displace fenced quarantine");
        std::fs::create_dir(&staged).expect("create replacement quarantine");
        std::fs::write(staged.join("payload"), b"replacement")
            .expect("write replacement quarantine");

        let error = authority
            .cleanup_committed(std::slice::from_ref(&scope))
            .expect_err("replacement quarantine must never be unlinked");

        assert!(matches!(
            error,
            CodeGenerationRetentionErrorV1::UnsafeState(_)
        ));
        assert_eq!(
            std::fs::read(staged.join("payload")).expect("replacement survives"),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(displaced.join("payload")).expect("fenced scope survives"),
            b"owned"
        );
    }
}
