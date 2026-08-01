use super::*;

#[derive(Debug)]
pub(super) struct BootstrapAuthority {
    lock_path: PathBuf,
}

struct BootstrapLease {
    database_key: PathBuf,
    refs: usize,
    held: File,
}

static BOOTSTRAP_LEASES: LazyLock<Mutex<HashMap<PathBuf, BootstrapLease>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn reject_hard_linked_database(path: &Path) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| access_io_error("inspect database links", path, &error))?;
    #[cfg(unix)]
    let has_multiple_links = {
        use std::os::unix::fs::MetadataExt;
        metadata.is_file() && metadata.nlink() > 1
    };
    #[cfg(windows)]
    let has_multiple_links = metadata.is_file() && windows_hard_link_count(path)? > 1;
    #[cfg(not(any(unix, windows)))]
    let has_multiple_links = false;
    if has_multiple_links {
        return Err(access_error(
            "resolve",
            path,
            "hard-linked SQLite databases are unsupported because their WAL/SHM sidecars differ",
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub fn windows_hard_link_count(path: &Path) -> Result<u32> {
    let file = std::fs::File::open(path)
        .map_err(|error| access_io_error("inspect database links", path, &error))?;
    crate::windows_file::information(&file)
        .map(|information| information.number_of_links)
        .map_err(|error| access_io_error("inspect database links", path, &error))
}

pub(super) fn acquire_bootstrap_authority(
    identity: &DatabaseIdentity,
    intent: &str,
) -> Result<Option<BootstrapAuthority>> {
    let Some(lock_path) = identity.bootstrap_lock_path.as_ref() else {
        return Ok(None);
    };
    let mut leases = BOOTSTRAP_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = leases.get_mut(lock_path) {
        if existing.database_key != identity.database_key {
            return Err(access_error(
                intent,
                &identity.database_path,
                "a case-variant first-create database authority is already active",
            ));
        }
        existing.refs += 1;
        return Ok(Some(BootstrapAuthority {
            lock_path: lock_path.clone(),
        }));
    }
    let held = open_lock_file(lock_path)?;
    if let Err(error) = fs2::FileExt::try_lock_exclusive(&held) {
        return if is_lock_contended(&error) {
            Err(access_error(
                intent,
                &identity.database_path,
                "a case-variant first-create database authority is already active",
            ))
        } else {
            Err(access_io_error(
                "acquire first-create database lease",
                lock_path,
                &error,
            ))
        };
    }
    leases.insert(
        lock_path.clone(),
        BootstrapLease {
            database_key: identity.database_key.clone(),
            refs: 1,
            held,
        },
    );
    Ok(Some(BootstrapAuthority {
        lock_path: lock_path.clone(),
    }))
}

impl Drop for BootstrapAuthority {
    fn drop(&mut self) {
        let mut leases = BOOTSTRAP_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_remove = leases.get_mut(&self.lock_path).is_some_and(|lease| {
            lease.refs = lease.refs.saturating_sub(1);
            lease.refs == 0
        });
        if should_remove && let Some(lease) = leases.remove(&self.lock_path) {
            let _ = fs2::FileExt::unlock(&lease.held);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(unix, windows))]
    #[test]
    fn hard_linked_database_paths_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("database.db");
        let alias = temp.path().join("database-hard-link.db");
        std::fs::write(&database, []).unwrap();
        std::fs::hard_link(&database, &alias).unwrap();

        for path in [&database, &alias] {
            let error = DatabaseIdentity::for_path(path).unwrap_err();
            assert!(error.to_string().contains("hard-linked SQLite databases"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_database_symlinks_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("missing.db");
        let alias = temp.path().join("database-alias.db");
        std::os::unix::fs::symlink(&target, &alias).unwrap();

        let error = DatabaseIdentity::for_path(&alias).unwrap_err();
        assert!(error.to_string().contains("database symlink is dangling"));
        assert!(!target.exists());
    }
}
