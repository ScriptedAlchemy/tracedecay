use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::backup::StepResult;
use rusqlite::{Connection, OpenFlags};

use super::control::SnapshotReadControl;
use super::{family_paths, with_suffix};

pub(super) async fn materialize(path: &Path, control: SnapshotReadControl) -> io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        control.checkpoint()?;
        let standalone = with_suffix(&path, ".standalone");
        match fs::remove_file(&standalone) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let source = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(io::Error::other)?;
        let mut destination = Connection::open_with_flags(
            &standalone,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(io::Error::other)?;
        {
            let backup = rusqlite::backup::Backup::new(&source, &mut destination)
                .map_err(io::Error::other)?;
            loop {
                control.checkpoint()?;
                match backup.step(128).map_err(io::Error::other)? {
                    StepResult::Done => break,
                    StepResult::More => {}
                    StepResult::Busy | StepResult::Locked => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    _ => {
                        return Err(io::Error::other(
                            "SQLite returned an unknown snapshot backup step result",
                        ));
                    }
                }
            }
        }
        control.checkpoint()?;
        drop(destination);
        drop(source);
        for member in family_paths(&path) {
            match fs::remove_file(member) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        fs::rename(standalone, path)
    })
    .await
    .map_err(|error| io::Error::other(format!("snapshot materialization task failed: {error}")))?
}
