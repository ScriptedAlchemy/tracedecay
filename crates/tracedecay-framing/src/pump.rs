//! Dedicated-thread pumps that join blocking I/O to async callers.
//!
//! A std thread owns the blocking reader or writer and meets the async side on
//! a bounded `async_channel`, whose futures wake through ordinary `Waker`s. Any
//! executor can await them, so stdio needs neither Tokio's blocking pool nor a
//! native-runtime `Cx`.

use std::io::{self, BufReader, Read, Write};

use crate::BoundedLineReader;

/// Items a dedicated thread reads from a blocking source.
///
/// The channel holds one item, so the thread runs at most one item ahead of the
/// consumer. A thread parked in a blocking read cannot be cancelled: dropping
/// the pump ends it only after that read returns. Nothing joins the thread, so
/// process exit never waits on it.
pub struct ReadPump<T> {
    items: async_channel::Receiver<T>,
}

impl<T: Send + 'static> ReadPump<T> {
    /// Runs `next` on a thread named `name` until it returns `None` or the pump
    /// is dropped.
    pub fn spawn(
        name: &str,
        mut next: impl FnMut() -> Option<T> + Send + 'static,
    ) -> io::Result<Self> {
        let (sender, items) = async_channel::bounded(1);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Some(item) = next() {
                    if sender.send_blocking(item).is_err() {
                        return;
                    }
                }
            })?;
        Ok(Self { items })
    }
}

impl<T> ReadPump<T> {
    /// The next item, or `None` once the source has ended. Dropping the wait
    /// leaves the item queued for the next call.
    pub async fn recv(&self) -> Option<T> {
        self.items.recv().await.ok()
    }
}

impl ReadPump<io::Result<String>> {
    /// MCP JSON-RPC lines from `source`, framed by
    /// [`BoundedLineReader::read_mcp_line_blocking`]. An oversized frame arrives
    /// as its typed error and the next frame follows; EOF ends the pump.
    pub fn mcp_lines(name: &str, source: impl Read + Send + 'static) -> io::Result<Self> {
        let mut lines = BoundedLineReader::new(BufReader::new(source));
        Self::spawn(name, move || lines.read_mcp_line_blocking().transpose())
    }
}

type WriteRequest = (Vec<u8>, async_channel::Sender<io::Result<()>>);

/// A blocking writer owned by a dedicated thread.
pub struct WritePump {
    requests: async_channel::Sender<WriteRequest>,
}

impl WritePump {
    /// Hands `sink` to a thread named `name`, which ends when the pump drops.
    pub fn spawn(name: &str, mut sink: impl Write + Send + 'static) -> io::Result<Self> {
        let (requests, queue) = async_channel::bounded::<WriteRequest>(1);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Ok((bytes, done)) = queue.recv_blocking() {
                    let _ = done.send_blocking(sink.write_all(&bytes).and_then(|()| sink.flush()));
                }
            })?;
        Ok(Self { requests })
    }

    /// Writes and flushes `bytes`, resolving with the sink's own error once
    /// both have finished. Dropping the wait after hand-off does not cancel
    /// the write.
    pub async fn write_flushed(&self, bytes: Vec<u8>) -> io::Result<()> {
        let exited = || io::Error::new(io::ErrorKind::BrokenPipe, "writer thread exited");
        let (done, outcome) = async_channel::bounded(1);
        self.requests
            .send((bytes, done))
            .await
            .map_err(|_| exited())?;
        outcome.recv().await.map_err(|_| exited())?
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    use super::*;

    /// Polls with a no-op waker, so the pumps are proven to need no runtime.
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = std::pin::pin!(future);
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
                return output;
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn mcp_lines_frame_reject_oversized_and_end_at_eof() {
        let mut input = b"first\r\n".to_vec();
        input.extend(std::iter::repeat_n(
            b'z',
            crate::MAX_MCP_JSONRPC_FRAME_BYTES + 1,
        ));
        input.extend_from_slice(b"\nlast");
        let lines = ReadPump::mcp_lines("test-lines", io::Cursor::new(input)).unwrap();

        assert_eq!(block_on(lines.recv()).unwrap().unwrap(), "first");
        let oversized = block_on(lines.recv()).unwrap().unwrap_err();
        assert!(crate::is_wire_oversized_io_error(&oversized));
        assert_eq!(block_on(lines.recv()).unwrap().unwrap(), "last");
        assert!(block_on(lines.recv()).is_none());
        assert!(block_on(lines.recv()).is_none());
    }

    /// Keeps written bytes apart from flushed bytes.
    struct Staged {
        pending: Vec<u8>,
        flushed: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for Staged {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.pending.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed.lock().unwrap().append(&mut self.pending);
            Ok(())
        }
    }

    #[test]
    fn write_flushed_resolves_after_the_sink_flushed() {
        let flushed = Arc::new(Mutex::new(Vec::new()));
        let sink = Staged {
            pending: Vec::new(),
            flushed: Arc::clone(&flushed),
        };
        let pump = WritePump::spawn("test-writer", sink).unwrap();

        block_on(pump.write_flushed(b"one\n".to_vec())).unwrap();
        assert_eq!(*flushed.lock().unwrap(), b"one\n");
        block_on(pump.write_flushed(b"two\n".to_vec())).unwrap();
        assert_eq!(*flushed.lock().unwrap(), b"one\ntwo\n");
    }

    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "stdout closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_flushed_surfaces_the_sink_error() {
        let pump = WritePump::spawn("test-closed", Closed).unwrap();
        let error = block_on(pump.write_flushed(b"lost".to_vec())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(error.to_string(), "stdout closed");
    }
}
