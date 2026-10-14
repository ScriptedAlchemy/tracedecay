//! Wake a child-pipe reader when the supervisor is done with it.
//!
//! A descendant can inherit the write end and hold it open after the direct
//! child exits. An infinite `poll` on the pipe plus a stop pipe wakes the
//! reader without a timer, which a long-lived analyzer cannot afford.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Signals the paired [`ChildPipeWatch`] to stop.
pub struct ChildPipeStop {
    flag: Arc<AtomicBool>,
    wake: std::io::PipeWriter,
}

/// Read end of a [`ChildPipeStop`].
pub struct ChildPipeWatch {
    flag: Arc<AtomicBool>,
    wake: std::io::PipeReader,
    /// ponytail: after stop, one readable chunk is returned and the next read
    /// is EOF. A descendant that writes continuously cannot hold the reader
    /// past that chunk. Upgrade: drain the kernel buffer with a nonblocking
    /// read before returning EOF.
    yielded_after_stop: bool,
}

/// A pipe the watch can `poll` on Unix. Windows child pipes cannot be polled,
/// so the watch falls back to a blocking read.
#[cfg(unix)]
pub trait WatchedPipe: Read + std::os::fd::AsFd {}
#[cfg(unix)]
impl<T: Read + std::os::fd::AsFd> WatchedPipe for T {}

#[cfg(not(unix))]
pub trait WatchedPipe: Read {}
#[cfg(not(unix))]
impl<T: Read> WatchedPipe for T {}

/// Builds a stop handle and the watch the reader thread holds.
pub fn child_pipe_stop() -> io::Result<(ChildPipeStop, ChildPipeWatch)> {
    let (wake, writer) = std::io::pipe()?;
    let flag = Arc::new(AtomicBool::new(false));
    Ok((
        ChildPipeStop {
            flag: Arc::clone(&flag),
            wake: writer,
        },
        ChildPipeWatch {
            flag,
            wake,
            yielded_after_stop: false,
        },
    ))
}

impl ChildPipeStop {
    /// Wakes the reader. Drop does this too, so every exit path stops it.
    pub fn abandon(&mut self) {
        self.flag.store(true, Ordering::Release);
        let _ = self.wake.write_all(&[1]);
    }
}

impl Drop for ChildPipeStop {
    fn drop(&mut self) {
        self.abandon();
    }
}

impl ChildPipeWatch {
    /// Reads one chunk, or `Ok(0)` at EOF or once the stop has been abandoned
    /// and any already-readable chunk has been delivered.
    pub fn read(&mut self, pipe: &mut impl WatchedPipe, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.read_unix(pipe, buf)
        }
        #[cfg(not(unix))]
        {
            // Windows child pipes cannot be polled. The flag is recorded, and
            // this read blocks until the write end closes.
            let _ = self.flag.load(Ordering::Acquire);
            let _ = &self.wake;
            let _ = self.yielded_after_stop;
            pipe.read(buf)
        }
    }

    #[cfg(unix)]
    fn read_unix(&mut self, pipe: &mut impl WatchedPipe, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::fd::AsRawFd;

        loop {
            let mut fds = [
                libc::pollfd {
                    fd: pipe.as_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: both descriptors are open and exclusively borrowed for
            // this call. `nfds` is the length of `fds`.
            let polled = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
            if polled == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let abandoned = self.flag.load(Ordering::Acquire);
            let data_ready = (fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0;
            let wake_ready = (fds[1].revents & libc::POLLIN) != 0;
            if data_ready && !(abandoned && self.yielded_after_stop) {
                match pipe.read(buf) {
                    Ok(0) => return Ok(0),
                    Ok(read) => {
                        if abandoned {
                            self.yielded_after_stop = true;
                        }
                        return Ok(read);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            if abandoned || wake_ready {
                return Ok(0);
            }
        }
    }
}
