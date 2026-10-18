//! Names the other processes that hold a database file open.
//!
//! SQLite does not report which connection holds a WAL read slot. When no
//! reader of this runtime holds one and the checkpoint still cannot restart
//! the log, the holder is another process; on Linux its open descriptors on
//! the database or its `-wal`/`-shm` sidecars name it.

use std::path::Path;

use super::types::{CheckpointBlocker, CheckpointBlockers};

const MAX_FOREIGN_HOLDERS: usize = 16;

/// Other processes with the database or a WAL sidecar open, bounded to
/// [`MAX_FOREIGN_HOLDERS`]. Empty where `/proc` is unavailable.
pub(crate) fn foreign_wal_holders(database: &Path) -> CheckpointBlockers {
    let mut blockers = CheckpointBlockers::default();
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return blockers;
    };
    let targets = ["", "-wal", "-shm"].map(|suffix| {
        let mut name = database.as_os_str().to_owned();
        name.push(suffix);
        std::path::PathBuf::from(name)
    });
    let own = std::process::id();
    for process in processes.flatten() {
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        let Ok(descriptors) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        let holds = descriptors.flatten().any(|descriptor| {
            std::fs::read_link(descriptor.path()).is_ok_and(|target| targets.contains(&target))
        });
        if !holds {
            continue;
        }
        if blockers.blockers.len() == MAX_FOREIGN_HOLDERS {
            blockers.omitted += 1;
            continue;
        }
        let name = std::fs::read_to_string(process.path().join("comm"))
            .map(|name| name.trim_end().to_owned())
            .unwrap_or_default();
        blockers
            .blockers
            .push(CheckpointBlocker::ForeignConnection { pid, process: name });
    }
    blockers
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_process_holding_a_wal_sidecar_open_is_named() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("held.sqlite3");
        let mut shm = database.as_os_str().to_owned();
        shm.push("-shm");
        let shm = std::fs::File::create(std::path::PathBuf::from(shm)).unwrap();
        let mut holder = std::process::Command::new("sleep")
            .arg("30")
            .stdin(shm)
            .spawn()
            .unwrap();

        let holders = foreign_wal_holders(&database);
        holder.kill().unwrap();
        holder.wait().unwrap();

        assert_eq!(
            holders.blockers,
            vec![CheckpointBlocker::ForeignConnection {
                pid: holder.id(),
                process: "sleep".to_owned(),
            }]
        );
        assert!(foreign_wal_holders(&directory.path().join("other.sqlite3")).is_clear());
    }
}
