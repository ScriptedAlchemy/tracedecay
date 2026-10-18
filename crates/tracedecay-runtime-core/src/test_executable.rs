//! Executable test doubles (fake host CLIs, `systemctl`, sentinels) that a
//! test can run immediately after writing them.
//!
//! Linux refuses `execve` with `ETXTBSY` while any process holds the image
//! open for writing. A sibling test thread that forks while this process
//! holds a write descriptor carries a copy into its child until that child
//! execs, so a script written with `std::fs::write` and run right after fails
//! intermittently under the parallel test harness. On Unix the script is
//! therefore written by a single-threaded `sh` that has no sibling to fork
//! and has exited before the caller runs it: this process never holds the
//! file open for writing. Other hosts have neither the race nor an exec bit,
//! so the file is written directly.

#[cfg(unix)]
use std::ffi::OsStr;
use std::io;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
#[cfg(unix)]
use std::process::{Command, Stdio};

/// Writes `contents` to `path`, replacing any existing file, and makes it
/// executable (mode `0755` on Unix).
#[cfg(unix)]
pub fn write_executable_script(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    run_sh_writer(
        r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#,
        OsStr::from_bytes(contents.as_ref()),
        path,
    )
}

/// Writes `contents` to `path`, replacing any existing file.
#[cfg(not(unix))]
pub fn write_executable_script(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    std::fs::write(path, contents)
}

/// Places the executable `source` at `destination`: a hard link when both
/// share a filesystem, otherwise a copy that this process never holds open
/// for writing.
#[cfg(unix)]
pub fn link_or_copy_executable(source: &Path, destination: &Path) -> io::Result<()> {
    if std::fs::hard_link(source, destination).is_ok() {
        return Ok(());
    }
    run_sh_writer(
        r#"cp "$1" "$2" && chmod 755 "$2""#,
        source.as_os_str(),
        destination,
    )
}

/// Places the executable `source` at `destination`, as a hard link when both
/// share a filesystem.
#[cfg(not(unix))]
pub fn link_or_copy_executable(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::hard_link(source, destination)
        .or_else(|_| std::fs::copy(source, destination).map(drop))
}

/// Runs `script` in a fresh `sh` with `$1 = input` and `$2 = destination`.
#[cfg(unix)]
fn run_sh_writer(script: &str, input: &OsStr, destination: &Path) -> io::Result<()> {
    let output = Command::new("/bin/sh")
        .args(["-c", script, "sh"])
        .arg(input)
        .arg(destination)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "writing executable {} exited with {}: {}",
            destination.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::{link_or_copy_executable, write_executable_script};

    #[test]
    fn written_script_runs_with_its_exact_contents() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-cli");
        std::fs::write(&script, "stale").unwrap();

        write_executable_script(&script, "#!/bin/sh\nprintf '%s|' \"$@\"\n").unwrap();

        assert_eq!(
            std::fs::read_to_string(&script).unwrap(),
            "#!/bin/sh\nprintf '%s|' \"$@\"\n"
        );
        assert_eq!(
            std::fs::metadata(&script).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let output = std::process::Command::new(&script)
            .args(["-n", "a b"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "-n|a b|");
    }

    #[test]
    fn unwritable_destination_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = write_executable_script(&dir.path().join("missing/fake-cli"), "#!/bin/sh\n")
            .unwrap_err();
        assert!(
            error.to_string().contains("missing/fake-cli"),
            "the refusal names the destination: {error}"
        );
    }

    #[test]
    fn copied_executable_runs_and_a_missing_source_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        write_executable_script(&source, "#!/bin/sh\necho linked\n").unwrap();
        // An existing destination refuses the hard link, so this takes the copy.
        let destination = dir.path().join("destination");
        std::fs::write(&destination, "stale").unwrap();

        link_or_copy_executable(&source, &destination).unwrap();

        let output = std::process::Command::new(&destination).output().unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "linked\n");
        let error = link_or_copy_executable(&dir.path().join("absent"), &dir.path().join("copy"))
            .unwrap_err();
        assert!(
            error.to_string().contains("copy"),
            "the refusal names the destination: {error}"
        );
    }
}
