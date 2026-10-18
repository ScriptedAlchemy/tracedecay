//! Filesystem aliases for isolated test fixtures without administrator privileges.

#[cfg(windows)]
use std::ffi::OsString;
use std::io;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
#[cfg(windows)]
use std::process::Command;

/// Create an alias to an existing directory. Windows junctions exercise real
/// canonicalization without requiring the symlink privilege or Developer Mode.
pub fn create_directory_alias(target: &Path, alias: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, alias)
    }
    #[cfg(windows)]
    {
        let quote = |path: &Path| {
            let mut argument = OsString::from("\"");
            argument.push(path);
            argument.push("\"");
            argument
        };
        let output = Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .raw_arg(quote(alias))
            .raw_arg(quote(target))
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "creating directory alias {} -> {} exited with {}: {} {}",
                alias.display(),
                target.display(),
                output.status,
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim(),
            )))
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, alias);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "directory aliases are not supported",
        ))
    }
}
