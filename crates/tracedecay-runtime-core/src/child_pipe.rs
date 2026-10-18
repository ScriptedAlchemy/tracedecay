//! Wake a child-pipe reader when the supervisor is done with it.
//!
//! A descendant can inherit the write end and hold it open after the direct
//! child exits. An infinite `poll` on the pipe plus a stop pipe wakes the
//! reader without a timer, which a long-lived analyzer cannot afford.
//!
//! Windows cannot do that. Its anonymous pipes have no `poll`, and a blocked
//! read has no safe wake, so the reader sleeps between `PeekNamedPipe` looks
//! ([`wait_readable`]) and pays a bounded wake-up rate while idle.

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
}

/// A pipe the watch can look into without blocking: `poll` on Unix,
/// `PeekNamedPipe` on Windows. Other targets get a blocking read.
#[cfg(unix)]
pub trait WatchedPipe: Read + std::os::fd::AsFd {}
#[cfg(unix)]
impl<T: Read + std::os::fd::AsFd> WatchedPipe for T {}

#[cfg(windows)]
pub trait WatchedPipe: Read + std::os::windows::io::AsRawHandle {}
#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle> WatchedPipe for T {}

#[cfg(not(any(unix, windows)))]
pub trait WatchedPipe: Read {}
#[cfg(not(any(unix, windows)))]
impl<T: Read> WatchedPipe for T {}

/// What a non-blocking look at the read end of a pipe found.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadState {
    /// Bytes are waiting, so a read returns without blocking.
    Buffered,
    /// Every write end is closed, so a read reports EOF without blocking.
    Closed,
    /// Nothing is waiting and a write end is still open.
    Idle,
}

/// Windows has no `poll` for anonymous pipes and no way to wake a thread
/// blocked in a read, so readers there sleep between non-blocking looks. The
/// pause grows while a pipe stays idle and resets for each wait.
#[cfg(any(windows, test))]
const IDLE_POLL_FLOOR: std::time::Duration = std::time::Duration::from_millis(1);
#[cfg(any(windows, test))]
const IDLE_POLL_CEILING: std::time::Duration = std::time::Duration::from_millis(25);

/// Waits until `probe` reports bytes or a closed pipe. `Ok(false)` means
/// `abandoned` was raised first, so the caller must stop without reading.
///
/// With `drain_after_abandon` set, bytes already in the pipe at abandonment
/// are still reported ready and only an idle pipe ends the wait, so a stop
/// never drops what the child wrote before it. Without it, abandonment ends
/// the wait immediately.
///
/// The loop is the whole bound: nothing in it blocks, so a descendant holding
/// the write end open cannot hold the reader past the abandonment.
#[cfg(any(windows, test))]
pub fn wait_readable(
    mut probe: impl FnMut() -> io::Result<ReadState>,
    abandoned: &AtomicBool,
    drain_after_abandon: bool,
) -> io::Result<bool> {
    let mut pause = IDLE_POLL_FLOOR;
    loop {
        let stopped = abandoned.load(Ordering::Acquire);
        if stopped && !drain_after_abandon {
            return Ok(false);
        }
        // Probing after loading the flag keeps every byte written before the
        // abandonment visible to the drain.
        match probe()? {
            ReadState::Buffered | ReadState::Closed => return Ok(true),
            ReadState::Idle if stopped => return Ok(false),
            ReadState::Idle => {}
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(IDLE_POLL_CEILING);
    }
}

/// Looks into the read end of a Windows child pipe without blocking.
#[cfg(windows)]
pub fn peek_read_state(pipe: &impl std::os::windows::io::AsRawHandle) -> io::Result<ReadState> {
    use windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE;
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let mut available = 0_u32;
    // SAFETY: the handle is an open pipe end borrowed for the call. No buffer
    // is requested, and `available` is the only out value, valid for writes.
    let peeked = unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &raw mut available,
            std::ptr::null_mut(),
        )
    };
    if peeked != 0 {
        return Ok(if available > 0 {
            ReadState::Buffered
        } else {
            ReadState::Idle
        });
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == i32::try_from(ERROR_BROKEN_PIPE).ok() {
        return Ok(ReadState::Closed);
    }
    Err(error)
}

/// Builds a stop handle and the watch the reader thread holds.
pub fn child_pipe_stop() -> io::Result<(ChildPipeStop, ChildPipeWatch)> {
    let (wake, writer) = std::io::pipe()?;
    let flag = Arc::new(AtomicBool::new(false));
    Ok((
        ChildPipeStop {
            flag: Arc::clone(&flag),
            wake: writer,
        },
        ChildPipeWatch { flag, wake },
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
    /// Reads one chunk, or `Ok(0)` at EOF.
    ///
    /// After the stop is abandoned, bytes already sitting in the pipe are
    /// still returned. `Ok(0)` starts once a read would have to wait, so a
    /// descendant that is only holding the write end cannot block the caller.
    pub fn read(&mut self, pipe: &mut impl WatchedPipe, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.read_unix(pipe, buf)
        }
        #[cfg(windows)]
        {
            // The read below runs only once a look at the pipe shows bytes or
            // EOF, so it never blocks. After abandonment, bytes already in
            // the pipe still come out and an idle pipe reads as EOF.
            let _ = &self.wake;
            if wait_readable(|| peek_read_state(pipe), &self.flag, true)? {
                pipe.read(buf)
            } else {
                Ok(0)
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            // No non-blocking look exists here: the read blocks until the
            // write end closes.
            let _ = (&self.flag, &self.wake);
            pipe.read(buf)
        }
    }

    #[cfg(unix)]
    fn read_unix(&mut self, pipe: &mut impl WatchedPipe, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::fd::AsRawFd;

        loop {
            let abandoned = self.flag.load(Ordering::Acquire);
            // After stop, do not wait for a descendant to write or exit.
            // Timeout 0 reports only what is already buffered.
            let timeout = if abandoned { 0 } else { -1 };
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
            let polled =
                unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if polled == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if polled == 0 {
                return Ok(0);
            }
            let abandoned = self.flag.load(Ordering::Acquire);
            let data_ready = (fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0;
            let wake_ready = (fds[1].revents & libc::POLLIN) != 0;
            if data_ready {
                match pipe.read(buf) {
                    Ok(0) => return Ok(0),
                    Ok(read) => return Ok(read),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if abandoned && error.kind() == io::ErrorKind::WouldBlock => {
                        return Ok(0);
                    }
                    Err(error) => return Err(error),
                }
            }
            if abandoned || wake_ready {
                return Ok(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::{Duration, Instant};

    use super::*;

    #[cfg(unix)]
    #[test]
    fn abandon_returns_bytes_already_buffered() {
        let (mut reader, mut writer) = std::io::pipe().unwrap();
        let (mut stop, mut watch) = child_pipe_stop().unwrap();
        let payload = vec![0xab_u8; 2500];
        writer.write_all(&payload).unwrap();
        stop.abandon();

        let mut got = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let read = watch.read(&mut reader, &mut buf).unwrap();
            if read == 0 {
                break;
            }
            got.extend_from_slice(&buf[..read]);
            assert!(
                got.len() <= payload.len(),
                "a stopped pipe must end once the buffered bytes are gone"
            );
        }
        assert_eq!(got, payload);
        let _held = writer;
    }

    /// A probe that reports each scripted state once, then `Idle`.
    fn scripted(states: &[ReadState]) -> impl FnMut() -> io::Result<ReadState> {
        let mut states = states.to_vec().into_iter();
        move || Ok(states.next().unwrap_or(ReadState::Idle))
    }

    #[test]
    fn wait_readable_returns_once_bytes_arrive() {
        let abandoned = AtomicBool::new(false);
        let probe = scripted(&[ReadState::Idle, ReadState::Idle, ReadState::Buffered]);
        assert!(wait_readable(probe, &abandoned, false).unwrap());
    }

    #[test]
    fn wait_readable_reports_a_closed_pipe_as_ready() {
        let abandoned = AtomicBool::new(false);
        assert!(wait_readable(scripted(&[ReadState::Closed]), &abandoned, false).unwrap());
    }

    #[test]
    fn wait_readable_stops_on_an_idle_pipe_when_abandoned() {
        // A descendant holding the write end keeps the probe `Idle` forever.
        let abandoned = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        for drain in [false, true] {
            abandoned.store(false, Ordering::Release);
            let raiser = {
                let abandoned = Arc::clone(&abandoned);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(50));
                    abandoned.store(true, Ordering::Release);
                })
            };
            let ready = wait_readable(|| Ok(ReadState::Idle), &abandoned, drain).unwrap();
            assert!(!ready, "an idle pipe must not report ready (drain={drain})");
            raiser.join().unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "abandonment must end the wait within a poll ceiling"
        );
    }

    #[test]
    fn wait_readable_drains_bytes_buffered_at_abandonment_only_on_request() {
        let abandoned = AtomicBool::new(true);
        let buffered = || scripted(&[ReadState::Buffered, ReadState::Buffered]);
        assert!(wait_readable(buffered(), &abandoned, true).unwrap());
        assert!(!wait_readable(buffered(), &abandoned, false).unwrap());
    }

    #[test]
    fn wait_readable_surfaces_probe_errors() {
        let abandoned = AtomicBool::new(false);
        let error =
            wait_readable(|| Err(io::Error::other("probe failed")), &abandoned, true).unwrap_err();
        assert_eq!(error.to_string(), "probe failed");
    }

    /// `wait_readable` driven by a real pipe on Unix, so the state machine
    /// meets actual idle, buffered and closed pipes. Windows swaps only the
    /// probe, for `PeekNamedPipe`.
    #[cfg(unix)]
    fn poll_probe(pipe: &std::io::PipeReader) -> io::Result<ReadState> {
        use std::os::fd::{AsFd, AsRawFd};
        let mut fd = libc::pollfd {
            fd: pipe.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `fd` is one valid pollfd, exclusively borrowed for the call.
        if unsafe { libc::poll(&raw mut fd, 1, 0) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(if fd.revents & libc::POLLIN != 0 {
            ReadState::Buffered
        } else if fd.revents & libc::POLLHUP != 0 {
            ReadState::Closed
        } else {
            ReadState::Idle
        })
    }

    #[cfg(unix)]
    #[test]
    fn wait_readable_follows_a_real_pipe() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let abandoned = AtomicBool::new(false);
        assert_eq!(poll_probe(&reader).unwrap(), ReadState::Idle);

        writer.write_all(b"x").unwrap();
        assert!(wait_readable(|| poll_probe(&reader), &abandoned, false).unwrap());

        drop(writer);
        let (empty, held) = std::io::pipe().unwrap();
        drop(held);
        assert_eq!(poll_probe(&empty).unwrap(), ReadState::Closed);
        assert!(wait_readable(|| poll_probe(&empty), &abandoned, false).unwrap());
    }

    /// Not executed in CI: only a Windows host runs it.
    #[cfg(windows)]
    #[test]
    fn peek_read_state_reports_idle_buffered_and_closed_pipes() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        assert_eq!(peek_read_state(&reader).unwrap(), ReadState::Idle);
        writer.write_all(b"x").unwrap();
        assert_eq!(peek_read_state(&reader).unwrap(), ReadState::Buffered);
        drop(writer);
        assert_eq!(peek_read_state(&reader).unwrap(), ReadState::Buffered);
        let mut reader = reader;
        let mut byte = [0_u8; 1];
        std::io::Read::read_exact(&mut reader, &mut byte).unwrap();
        assert_eq!(peek_read_state(&reader).unwrap(), ReadState::Closed);
    }

    /// Not executed in CI: only a Windows host runs it.
    #[cfg(windows)]
    #[test]
    fn watch_read_ends_on_an_idle_pipe_once_abandoned() {
        let (mut reader, _held_writer) = std::io::pipe().unwrap();
        let (mut stop, mut watch) = child_pipe_stop().unwrap();
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0_u8; 16];
            watch.read(&mut reader, &mut buf)
        });
        std::thread::sleep(Duration::from_millis(50));
        stop.abandon();
        assert_eq!(reader_thread.join().unwrap().unwrap(), 0);
    }
}
