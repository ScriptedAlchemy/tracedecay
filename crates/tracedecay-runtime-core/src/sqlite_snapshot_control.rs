use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
pub struct SnapshotReadControl {
    deadline: Option<Instant>,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl SnapshotReadControl {
    pub fn new(deadline: Instant, cancelled: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            deadline: Some(deadline),
            cancelled: Arc::new(cancelled),
        }
    }

    pub fn unlimited() -> Self {
        Self {
            deadline: None,
            cancelled: Arc::new(|| false),
        }
    }

    pub(super) const fn is_unlimited(&self) -> bool {
        self.deadline.is_none()
    }

    pub(super) fn checkpoint(&self) -> io::Result<()> {
        if (self.cancelled)() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "SQLite read snapshot cancelled",
            ));
        }
        if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SQLite read snapshot deadline elapsed",
            ));
        }
        Ok(())
    }

    pub(super) fn copy_file(&self, source: &Path, target: &Path) -> io::Result<()> {
        let mut source = File::open(source)?;
        let mut destination = File::create(target)?;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            self.checkpoint()?;
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            destination.write_all(&buffer[..read])?;
        }
        self.checkpoint()
    }
}
