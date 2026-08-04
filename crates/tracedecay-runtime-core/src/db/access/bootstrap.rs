use super::*;

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
}
