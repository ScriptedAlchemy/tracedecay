//! Read-only discovery of processes holding `TraceDecay` `SQLite` store files.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenStoreHolder {
    pub(crate) pid: u32,
    pub(crate) command: String,
    pub(crate) executable: Option<PathBuf>,
    pub(crate) version: Option<String>,
    pub(crate) paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenStoreHolderScan {
    Supported(Vec<OpenStoreHolder>),
    Unsupported { reason: String },
}

/// Finds processes that currently hold any member of the supplied `SQLite`
/// database families. The scan never signals or terminates a process.
pub(crate) fn scan(database_paths: &[PathBuf]) -> io::Result<OpenStoreHolderScan> {
    #[cfg(target_os = "linux")]
    {
        if !Path::new("/proc").is_dir() {
            return Ok(OpenStoreHolderScan::Unsupported {
                reason: "open-store process discovery requires a mounted Linux /proc filesystem"
                    .to_string(),
            });
        }
        scan_linux(
            Path::new("/proc"),
            database_paths,
            std::process::id(),
            probe_tracedecay_version,
        )
        .map(OpenStoreHolderScan::Supported)
    }
    #[cfg(target_os = "macos")]
    {
        scan_macos(database_paths).map(OpenStoreHolderScan::Supported)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = database_paths;
        Ok(OpenStoreHolderScan::Unsupported {
            reason: format!(
                "open-store process discovery is unavailable on {}",
                std::env::consts::OS
            ),
        })
    }
}

#[cfg(target_os = "macos")]
fn scan_macos(database_paths: &[PathBuf]) -> io::Result<Vec<OpenStoreHolder>> {
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;
    use std::process::Command;

    let mut targets = database_paths
        .iter()
        .flat_map(|path| sqlite_family_paths(path))
        .filter(|path| path.is_file())
        .map(|path| path.canonicalize().unwrap_or(path))
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    let mut identities = BTreeMap::<(u64, u64), Vec<PathBuf>>::new();
    for target in &targets {
        let metadata = target.metadata()?;
        identities
            .entry((metadata.dev(), metadata.ino()))
            .or_default()
            .push(target.clone());
    }

    let run = |program: &str| {
        Command::new(program)
            .args(["-nP", "-FpcfDi0", "--"])
            .args(&targets)
            .output()
    };
    let output = match run("lsof") {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => run("/usr/sbin/lsof")?,
        Err(error) => return Err(error),
    };
    let stderr_has_content = output.stderr.iter().any(|byte| !byte.is_ascii_whitespace());
    if (!output.status.success() && output.status.code() != Some(1)) || stderr_has_content {
        return Err(io::Error::other(format!(
            "lsof failed while checking open TraceDecay stores: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    parse_lsof_output(&output.stdout, &identities, std::process::id())
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn parse_lsof_output(
    output: &[u8],
    targets: &std::collections::BTreeMap<(u64, u64), Vec<PathBuf>>,
    own_pid: u32,
) -> io::Result<Vec<OpenStoreHolder>> {
    use std::collections::BTreeSet;

    let mut holders = Vec::new();
    let mut pid = None;
    let mut command = String::new();
    let mut paths = BTreeSet::new();
    let mut file_open = false;
    let mut device = None;
    let mut inode = None;
    let finish_file = |file_open: &mut bool,
                       device: &mut Option<u64>,
                       inode: &mut Option<u64>,
                       paths: &mut BTreeSet<PathBuf>|
     -> io::Result<()> {
        if !*file_open {
            return Ok(());
        }
        let identity = match (device.take(), inode.take()) {
            (Some(device), Some(inode)) => (device, inode),
            _ => {
                return Err(io::Error::other(
                    "lsof returned a matching file without device and inode identity",
                ));
            }
        };
        let Some(matched) = targets.get(&identity) else {
            return Err(io::Error::other(format!(
                "lsof returned unexpected file identity {:#x}:{}",
                identity.0, identity.1
            )));
        };
        paths.extend(matched.iter().cloned());
        *file_open = false;
        Ok(())
    };
    let finish = |pid: &mut Option<u32>,
                  command: &mut String,
                  paths: &mut BTreeSet<PathBuf>,
                  holders: &mut Vec<OpenStoreHolder>| {
        let Some(current) = pid.take() else {
            return;
        };
        if current != own_pid && !paths.is_empty() {
            holders.push(OpenStoreHolder {
                pid: current,
                command: std::mem::take(command),
                executable: None,
                version: None,
                paths: std::mem::take(paths).into_iter().collect(),
            });
        } else {
            command.clear();
            paths.clear();
        }
    };
    for field in output.split(|byte| *byte == 0) {
        let field = field.strip_prefix(b"\n").unwrap_or(field);
        let Some((&kind, value)) = field.split_first() else {
            continue;
        };
        match kind {
            b'p' => {
                finish_file(&mut file_open, &mut device, &mut inode, &mut paths)?;
                finish(&mut pid, &mut command, &mut paths, &mut holders);
                pid = Some(
                    parse_decimal_field(value)
                        .and_then(|value| u32::try_from(value).ok())
                        .ok_or_else(|| io::Error::other("lsof returned an invalid process ID"))?,
                );
            }
            b'c' => command = String::from_utf8_lossy(value).into_owned(),
            b'f' => {
                if pid.is_none() {
                    return Err(io::Error::other(
                        "lsof returned a matching file without a process ID",
                    ));
                }
                finish_file(&mut file_open, &mut device, &mut inode, &mut paths)?;
                file_open = true;
            }
            b'D' => device = parse_hex_field(value),
            b'i' => inode = parse_decimal_field(value),
            _ => {}
        }
    }
    finish_file(&mut file_open, &mut device, &mut inode, &mut paths)?;
    finish(&mut pid, &mut command, &mut paths, &mut holders);
    holders.sort_by_key(|holder| holder.pid);
    Ok(holders)
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn parse_hex_field(value: &[u8]) -> Option<u64> {
    let value = value.strip_prefix(b"0x").unwrap_or(value);
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| u64::from_str_radix(value, 16).ok())
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn parse_decimal_field(value: &[u8]) -> Option<u64> {
    std::str::from_utf8(value)
        .ok()
        .and_then(|value| value.parse().ok())
}

#[cfg(target_os = "linux")]
fn scan_linux<F>(
    proc_root: &Path,
    database_paths: &[PathBuf],
    own_pid: u32,
    mut version_probe: F,
) -> io::Result<Vec<OpenStoreHolder>>
where
    F: FnMut(u32, &Path, &str) -> Option<String>,
{
    use std::collections::{BTreeMap, BTreeSet};
    use std::os::unix::fs::MetadataExt;

    let mut targets = BTreeMap::<(u64, u64), BTreeSet<PathBuf>>::new();
    for database in database_paths {
        for path in sqlite_family_paths(database) {
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    targets
                        .entry((metadata.dev(), metadata.ino()))
                        .or_default()
                        .insert(path.canonicalize().unwrap_or(path));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let mut holders = Vec::new();
    for entry in std::fs::read_dir(proc_root)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if transient_proc_error(&error) => continue,
            Err(error) => return Err(error),
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let process_root = entry.path();
        let fds = match std::fs::read_dir(process_root.join("fd")) {
            Ok(fds) => fds,
            Err(error) if transient_proc_error(&error) => continue,
            Err(error) => return Err(error),
        };
        let mut paths = BTreeSet::new();
        for fd in fds {
            let fd = match fd {
                Ok(fd) => fd,
                Err(error) if transient_proc_error(&error) => continue,
                Err(error) => return Err(error),
            };
            let metadata = match std::fs::metadata(fd.path()) {
                Ok(metadata) => metadata,
                Err(error) if transient_proc_error(&error) => continue,
                Err(error) => return Err(error),
            };
            if let Some(matched) = targets.get(&(metadata.dev(), metadata.ino())) {
                paths.extend(matched.iter().cloned());
            }
        }
        if paths.is_empty() {
            continue;
        }

        let command = process_comm(&process_root, pid);
        let executable = std::fs::read_link(process_root.join("exe")).ok();
        let version = if is_tracedecay_process(&command, executable.as_deref()) {
            version_probe(pid, proc_root, &command)
        } else {
            None
        };
        holders.push(OpenStoreHolder {
            pid,
            command,
            executable,
            version,
            paths: paths.into_iter().collect(),
        });
    }
    holders.sort_by_key(|holder| holder.pid);
    Ok(holders)
}

#[cfg(target_os = "linux")]
fn transient_proc_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sqlite_family_paths(path: &Path) -> [PathBuf; 3] {
    [
        path.to_path_buf(),
        with_suffix(path, "-wal"),
        with_suffix(path, "-shm"),
    ]
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(all(test, unix))]
mod lsof_tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn lsof_field_output_is_bounded_to_targets_and_excludes_self() {
        let target = PathBuf::from("/stores/sessions.db");
        let targets = BTreeMap::from([((0x2a, 7), vec![target.clone()])]);
        let holders = parse_lsof_output(
            b"p42\0ctracedecay\0f7\0D0x2a\0i7\0\np43\0cself\0f8\0D0x2a\0i7\0\n",
            &targets,
            43,
        )
        .unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].pid, 42);
        assert_eq!(holders[0].command, "tracedecay");
        assert_eq!(holders[0].paths, vec![target]);
    }

    #[test]
    fn lsof_field_output_preserves_non_utf8_and_newline_paths() {
        let target = PathBuf::from(OsString::from_vec(b"/stores/odd\n\xff.db".to_vec()));
        let targets = BTreeMap::from([((0x2a, 7), vec![target.clone()])]);
        let output = b"p42\0ctracedecay\0f7\0D0x2a\0i7\0n/stores/odd\\n\\xff.db\0\n";

        let holders = parse_lsof_output(output, &targets, 43).unwrap();
        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].paths, vec![target]);
    }

    #[test]
    fn lsof_field_output_rejects_missing_file_identity() {
        let targets = BTreeMap::from([((0x2a, 7), vec![PathBuf::from("/stores/db")])]);
        let error = parse_lsof_output(b"p42\0ctracedecay\0f7\0\n", &targets, 43).unwrap_err();
        assert!(error.to_string().contains("without device and inode"));
    }

    #[test]
    fn lsof_field_output_rejects_missing_process_identity() {
        let targets = BTreeMap::from([((0x2a, 7), vec![PathBuf::from("/stores/db")])]);
        let error = parse_lsof_output(b"f7\0D0x2a\0i7\0\n", &targets, 43).unwrap_err();
        assert!(error.to_string().contains("without a process ID"));
    }
}

#[cfg(target_os = "linux")]
fn process_comm(process_root: &Path, pid: u32) -> String {
    std::fs::read_to_string(process_root.join("comm"))
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|_| format!("pid {pid}"))
}

#[cfg(target_os = "linux")]
fn is_tracedecay_process(command: &str, executable: Option<&Path>) -> bool {
    let executable_matches = executable
        .and_then(Path::file_name)
        .is_some_and(|name| name.to_string_lossy().contains("tracedecay"));
    let command_matches = command
        .split_whitespace()
        .next()
        .and_then(|value| Path::new(value).file_name())
        .is_some_and(|name| name.to_string_lossy().contains("tracedecay"));
    executable_matches || command_matches
}

#[cfg(target_os = "linux")]
fn probe_tracedecay_version(pid: u32, proc_root: &Path, _command: &str) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    let mut child = Command::new(proc_root.join(pid.to_string()).join("exe"))
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let output = child.wait_with_output().ok()?;
                return String::from_utf8(output.stdout)
                    .ok()?
                    .lines()
                    .next()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned);
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn linux_scan_matches_open_sidecars_by_file_identity() {
        let temp = TempDir::new().unwrap();
        let proc_root = temp.path().join("proc");
        let database = temp.path().join("store/sessions.db");
        std::fs::create_dir_all(database.parent().unwrap()).unwrap();
        std::fs::write(&database, b"db").unwrap();
        let wal = with_suffix(&database, "-wal");
        std::fs::write(&wal, b"wal").unwrap();

        let process = proc_root.join("42");
        std::fs::create_dir_all(process.join("fd")).unwrap();
        std::fs::write(
            process.join("cmdline"),
            b"/opt/tracedecay\0serve\0--token\0secret-value\0",
        )
        .unwrap();
        std::fs::write(process.join("comm"), b"tracedecay\n").unwrap();
        symlink("/opt/tracedecay", process.join("exe")).unwrap();
        symlink(&wal, process.join("fd/7")).unwrap();

        let holders = scan_linux(&proc_root, &[database], 9000, |pid, _, command| {
            assert_eq!(pid, 42);
            assert_eq!(command, "tracedecay");
            Some("tracedecay 0.0.45".to_string())
        })
        .unwrap();

        assert_eq!(holders.len(), 1);
        assert_eq!(holders[0].pid, 42);
        assert_eq!(holders[0].version.as_deref(), Some("tracedecay 0.0.45"));
        assert_eq!(holders[0].paths, vec![wal.canonicalize().unwrap()]);
        assert!(!format!("{:?}", holders[0]).contains("secret-value"));
    }

    #[test]
    fn linux_scan_ignores_its_own_pid_and_unrelated_files() {
        let temp = TempDir::new().unwrap();
        let proc_root = temp.path().join("proc");
        let database = temp.path().join("sessions.db");
        let unrelated = temp.path().join("other.db");
        std::fs::write(&database, b"db").unwrap();
        std::fs::write(&unrelated, b"other").unwrap();
        for (pid, path) in [(42_u32, &database), (43_u32, &unrelated)] {
            let process = proc_root.join(pid.to_string());
            std::fs::create_dir_all(process.join("fd")).unwrap();
            std::fs::write(process.join("comm"), b"tracedecay\n").unwrap();
            symlink(path, process.join("fd/1")).unwrap();
        }

        let holders = scan_linux(&proc_root, &[database], 42, |_, _, _| {
            panic!("excluded processes must not be probed")
        })
        .unwrap();

        assert!(holders.is_empty());
    }
}
