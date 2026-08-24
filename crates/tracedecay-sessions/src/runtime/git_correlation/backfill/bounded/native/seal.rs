use std::path::{Path, PathBuf};

use super::{BoundedBackfillInterruption, BoundedGitControl};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in super::super) struct RepositorySeal {
    pub worktree: PathBuf,
    pub worktree_identity: Vec<u8>,
    pub git_dir: PathBuf,
    pub git_dir_identity: Vec<u8>,
    pub common_dir: PathBuf,
    pub common_dir_identity: Vec<u8>,
}

pub(super) fn capture_repository_seal(
    repository: &gix::Repository,
) -> Result<RepositorySeal, BoundedBackfillInterruption> {
    let worktree = canonical_directory(
        repository
            .workdir()
            .ok_or(BoundedBackfillInterruption::SourceUnavailable)?,
    )?;
    let git_dir = canonical_directory(repository.git_dir())?;
    let common_dir = canonical_directory(repository.common_dir())?;
    Ok(RepositorySeal {
        worktree_identity: stable_filesystem_identity(&worktree)?,
        git_dir_identity: stable_filesystem_identity(&git_dir)?,
        common_dir_identity: stable_filesystem_identity(&common_dir)?,
        worktree,
        git_dir,
        common_dir,
    })
}

pub(super) fn verify_repository_identity(
    repository: &gix::Repository,
    expected: &RepositorySeal,
) -> Result<(), BoundedBackfillInterruption> {
    if capture_repository_seal(repository)? != *expected {
        return Err(BoundedBackfillInterruption::SourceChanged);
    }
    Ok(())
}

pub(in super::super) fn verify_repository_source(
    project_path: &Path,
    seal: &RepositorySeal,
    control: &BoundedGitControl,
) -> Result<(), BoundedBackfillInterruption> {
    control.check()?;
    let repository =
        gix::discover(project_path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    verify_repository_identity(&repository, seal)?;
    control.check()
}

fn canonical_directory(path: &Path) -> Result<PathBuf, BoundedBackfillInterruption> {
    let canonical = path
        .canonicalize()
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    if !canonical.is_dir() {
        return Err(BoundedBackfillInterruption::SourceUnavailable);
    }
    Ok(canonical)
}

#[cfg(unix)]
fn stable_filesystem_identity(path: &Path) -> Result<Vec<u8>, BoundedBackfillInterruption> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata =
        std::fs::metadata(path).map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut identity = b"unix-dev-ino-v1\0".to_vec();
    identity.extend_from_slice(&metadata.dev().to_le_bytes());
    identity.extend_from_slice(&metadata.ino().to_le_bytes());
    Ok(identity)
}

#[cfg(windows)]
fn stable_filesystem_identity(path: &Path) -> Result<Vec<u8>, BoundedBackfillInterruption> {
    use std::os::windows::fs::OpenOptionsExt as _;

    // Directory handles need backup semantics; the identity itself comes from
    // the stable GetFileInformationByHandle authority in runtime-core instead
    // of the unstable `windows_by_handle` metadata surface. The identity byte
    // layout (u32 volume + u64 index, little endian) is unchanged.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let information = tracedecay_runtime_core::windows_file::information(&directory)
        .map_err(|_| BoundedBackfillInterruption::SourceUnavailable)?;
    let mut identity = b"windows-volume-index-v1\0".to_vec();
    identity.extend_from_slice(&information.volume_serial_number.to_le_bytes());
    identity.extend_from_slice(&information.file_index.to_le_bytes());
    Ok(identity)
}

#[cfg(not(any(unix, windows)))]
fn stable_filesystem_identity(_path: &Path) -> Result<Vec<u8>, BoundedBackfillInterruption> {
    Err(BoundedBackfillInterruption::SourceUnavailable)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::observation::ObservationCancellation;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new(tracedecay_runtime_core::git::git_program())
            .current_dir(path)
            .args(args)
            .env("GIT_AUTHOR_NAME", "TraceDecay")
            .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
            .env("GIT_COMMITTER_NAME", "TraceDecay")
            .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fixture() -> tempfile::TempDir {
        let fixture = tempfile::tempdir().unwrap();
        git(fixture.path(), &["init", "-b", "main"]);
        std::fs::write(fixture.path().join("tracked"), "one").unwrap();
        git(fixture.path(), &["add", "tracked"]);
        git(fixture.path(), &["commit", "-m", "initial"]);
        fixture
    }

    #[test]
    fn repository_seal_allows_mutable_git_state_but_rejects_same_path_replacement() {
        let fixture = fixture();
        let repository = gix::discover(fixture.path()).unwrap();
        let seal = capture_repository_seal(&repository).unwrap();

        std::fs::write(fixture.path().join("tracked"), "two").unwrap();
        git(fixture.path(), &["commit", "-am", "advance head"]);
        let moved = gix::discover(fixture.path()).unwrap();
        verify_repository_identity(&moved, &seal).unwrap();

        std::fs::rename(
            fixture.path().join(".git"),
            fixture.path().join(".git.replaced"),
        )
        .unwrap();
        git(fixture.path(), &["init", "-b", "main"]);
        let replacement = gix::discover(fixture.path()).unwrap();
        assert_eq!(
            verify_repository_identity(&replacement, &seal).unwrap_err(),
            BoundedBackfillInterruption::SourceChanged
        );
    }

    #[test]
    fn repository_source_check_observes_control() {
        let fixture = fixture();
        let repository = gix::discover(fixture.path()).unwrap();
        let seal = capture_repository_seal(&repository).unwrap();
        let cancellation = ObservationCancellation::default();
        cancellation.cancel();
        let control = BoundedGitControl::new(cancellation, std::time::Duration::from_secs(1));
        assert_eq!(
            verify_repository_source(fixture.path(), &seal, &control).unwrap_err(),
            BoundedBackfillInterruption::Cancelled
        );
    }
}
