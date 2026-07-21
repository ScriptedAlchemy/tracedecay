//! Transport-only stdio bridge for a daemon-owned LSP session.
//!
//! The bridge preserves opaque LSP JSON-RPC payload bytes while moving them
//! between host stdio and one typed daemon session. It intentionally opens no
//! project/profile database, selects no root, starts no analyzer, merges no
//! diagnostics, and makes no capability or routing decisions.

use std::collections::VecDeque;

/// Plan 35's hard JSON-RPC payload limit.
pub const MAX_LSP_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// An opaque LSP JSON-RPC payload. Framing adapters remove and restore the
/// `Content-Length` envelope; the bridge never parses the JSON body.
pub type LspFrame = Vec<u8>;

/// Result of a non-blocking receive attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FramePoll {
    Frame(LspFrame),
    Pending,
    Closed,
}

/// Result of a non-blocking send attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameSend {
    Sent,
    Backpressured,
    Closed,
}

/// The host-side stdio framing adapter.
///
/// Implementations own strict `Content-Length` parsing and stdout framing.
/// Both methods must be non-blocking so one quiet direction cannot starve the
/// other direction.
pub trait StdioFrameTransport {
    type Error;

    fn poll_frame(&mut self) -> Result<FramePoll, Self::Error>;
    fn try_send_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error>;
}

/// The daemon-side typed session transport supplied by the daemon client
/// layer. It is not a raw daemon-socket tunnel.
pub trait DaemonLspSessionTransport {
    type Error;

    fn try_send_client_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error>;
    fn poll_daemon_frame(&mut self) -> Result<FramePoll, Self::Error>;
}

/// Direction associated with a bridge close or frame-limit failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeDirection {
    ClientToDaemon,
    DaemonToClient,
}

/// A transport or protocol-boundary failure from the bridge.
#[derive(Debug, Eq, PartialEq)]
pub enum StdioLspBridgeError<StdioError, DaemonError> {
    Stdio(StdioError),
    Daemon(DaemonError),
    FrameTooLarge {
        direction: BridgeDirection,
        size: usize,
        limit: usize,
    },
}

/// Counts frames forwarded by one bounded, fair bridge pump.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BridgePumpOutcome {
    pub client_to_daemon: usize,
    pub daemon_to_client: usize,
    pub backpressured: bool,
    pub closed: bool,
}

/// A framing bridge between one host process and one daemon LSP session.
///
/// At most one frame per direction is retained while its receiver is
/// backpressured. This prevents loss without allowing an unbounded bridge-local
/// queue. Session admission, cancellation, and JSON-RPC routing remain daemon
/// duties.
pub struct StdioLspBridge<Stdio, Daemon> {
    stdio: Stdio,
    daemon: Daemon,
    client_pending: VecDeque<LspFrame>,
    daemon_pending: VecDeque<LspFrame>,
    closed: bool,
}

impl<Stdio, Daemon> StdioLspBridge<Stdio, Daemon>
where
    Stdio: StdioFrameTransport,
    Daemon: DaemonLspSessionTransport,
{
    pub fn new(stdio: Stdio, daemon: Daemon) -> Self {
        Self {
            stdio,
            daemon,
            client_pending: VecDeque::with_capacity(1),
            daemon_pending: VecDeque::with_capacity(1),
            closed: false,
        }
    }

    /// Polls and forwards at most one frame in each direction.
    pub fn pump_once(
        &mut self,
    ) -> Result<BridgePumpOutcome, StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        if self.closed {
            return Ok(BridgePumpOutcome {
                closed: true,
                ..BridgePumpOutcome::default()
            });
        }

        let mut outcome = BridgePumpOutcome::default();
        self.fill_client_slot()?;
        if self.closed {
            self.discard_pending();
            outcome.closed = true;
            return Ok(outcome);
        }
        self.fill_daemon_slot()?;
        if self.closed {
            self.discard_pending();
            outcome.closed = true;
            return Ok(outcome);
        }
        self.flush_client_slot(&mut outcome)?;
        if self.closed {
            self.discard_pending();
            outcome.closed = true;
            return Ok(outcome);
        }
        self.flush_daemon_slot(&mut outcome)?;
        if self.closed {
            self.discard_pending();
        }
        outcome.closed = self.closed;
        Ok(outcome)
    }

    pub fn into_parts(self) -> (Stdio, Daemon) {
        (self.stdio, self.daemon)
    }

    fn fill_client_slot(&mut self) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        if !self.client_pending.is_empty() {
            return Ok(());
        }
        match self
            .stdio
            .poll_frame()
            .map_err(StdioLspBridgeError::Stdio)?
        {
            FramePoll::Frame(frame) => {
                self.validate_size(BridgeDirection::ClientToDaemon, &frame)?;
                self.client_pending.push_back(frame);
            }
            FramePoll::Pending => {}
            FramePoll::Closed => self.closed = true,
        }
        Ok(())
    }

    fn fill_daemon_slot(&mut self) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        if !self.daemon_pending.is_empty() {
            return Ok(());
        }
        match self
            .daemon
            .poll_daemon_frame()
            .map_err(StdioLspBridgeError::Daemon)?
        {
            FramePoll::Frame(frame) => {
                self.validate_size(BridgeDirection::DaemonToClient, &frame)?;
                self.daemon_pending.push_back(frame);
            }
            FramePoll::Pending => {}
            FramePoll::Closed => self.closed = true,
        }
        Ok(())
    }

    fn flush_client_slot(
        &mut self,
        outcome: &mut BridgePumpOutcome,
    ) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        let Some(frame) = self.client_pending.front() else {
            return Ok(());
        };
        match self
            .daemon
            .try_send_client_frame(frame)
            .map_err(StdioLspBridgeError::Daemon)?
        {
            FrameSend::Sent => {
                self.client_pending.pop_front();
                outcome.client_to_daemon = 1;
            }
            FrameSend::Backpressured => outcome.backpressured = true,
            FrameSend::Closed => self.closed = true,
        }
        Ok(())
    }

    fn flush_daemon_slot(
        &mut self,
        outcome: &mut BridgePumpOutcome,
    ) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        let Some(frame) = self.daemon_pending.front() else {
            return Ok(());
        };
        match self
            .stdio
            .try_send_frame(frame)
            .map_err(StdioLspBridgeError::Stdio)?
        {
            FrameSend::Sent => {
                self.daemon_pending.pop_front();
                outcome.daemon_to_client = 1;
            }
            FrameSend::Backpressured => outcome.backpressured = true,
            FrameSend::Closed => self.closed = true,
        }
        Ok(())
    }

    fn validate_size(
        &mut self,
        direction: BridgeDirection,
        frame: &[u8],
    ) -> Result<(), StdioLspBridgeError<Stdio::Error, Daemon::Error>> {
        if frame.len() <= MAX_LSP_FRAME_BYTES {
            return Ok(());
        }
        self.closed = true;
        Err(StdioLspBridgeError::FrameTooLarge {
            direction,
            size: frame.len(),
            limit: MAX_LSP_FRAME_BYTES,
        })
    }

    fn discard_pending(&mut self) {
        self.client_pending.clear();
        self.daemon_pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Stdio {
        input: VecDeque<FramePoll>,
        output: Vec<LspFrame>,
        block_send: bool,
    }

    impl StdioFrameTransport for Stdio {
        type Error = ();

        fn poll_frame(&mut self) -> Result<FramePoll, Self::Error> {
            Ok(self.input.pop_front().unwrap_or(FramePoll::Pending))
        }

        fn try_send_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error> {
            if self.block_send {
                return Ok(FrameSend::Backpressured);
            }
            self.output.push(frame.to_vec());
            Ok(FrameSend::Sent)
        }
    }

    #[derive(Default)]
    struct Daemon {
        input: VecDeque<FramePoll>,
        output: Vec<LspFrame>,
        block_send: bool,
    }

    impl DaemonLspSessionTransport for Daemon {
        type Error = ();

        fn try_send_client_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error> {
            if self.block_send {
                return Ok(FrameSend::Backpressured);
            }
            self.output.push(frame.to_vec());
            Ok(FrameSend::Sent)
        }

        fn poll_daemon_frame(&mut self) -> Result<FramePoll, Self::Error> {
            Ok(self.input.pop_front().unwrap_or(FramePoll::Pending))
        }
    }

    #[test]
    fn forwards_both_directions_without_inspecting_payloads() {
        let mut stdio = Stdio::default();
        stdio.input.push_back(FramePoll::Frame(vec![0, 1, 2]));
        let mut daemon = Daemon::default();
        daemon.input.push_back(FramePoll::Frame(vec![3, 4, 5]));
        let mut bridge = StdioLspBridge::new(stdio, daemon);

        assert_eq!(
            bridge.pump_once().unwrap(),
            BridgePumpOutcome {
                client_to_daemon: 1,
                daemon_to_client: 1,
                backpressured: false,
                closed: false,
            }
        );
        let (stdio, daemon) = bridge.into_parts();
        assert_eq!(stdio.output, vec![vec![3, 4, 5]]);
        assert_eq!(daemon.output, vec![vec![0, 1, 2]]);
    }

    #[test]
    fn retains_exactly_one_frame_across_backpressure() {
        let mut stdio = Stdio::default();
        stdio.input.push_back(FramePoll::Frame(vec![1]));
        stdio.input.push_back(FramePoll::Frame(vec![2]));
        let daemon = Daemon {
            block_send: true,
            ..Daemon::default()
        };
        let mut bridge = StdioLspBridge::new(stdio, daemon);

        assert!(bridge.pump_once().unwrap().backpressured);
        bridge.daemon.block_send = false;
        assert_eq!(bridge.pump_once().unwrap().client_to_daemon, 1);
        assert_eq!(bridge.daemon.output, vec![vec![1]]);
        assert_eq!(bridge.pump_once().unwrap().client_to_daemon, 1);
        assert_eq!(bridge.daemon.output, vec![vec![1], vec![2]]);
    }

    #[test]
    fn oversized_frame_closes_before_daemon_dispatch() {
        let mut stdio = Stdio::default();
        stdio
            .input
            .push_back(FramePoll::Frame(vec![0; MAX_LSP_FRAME_BYTES + 1]));
        let mut bridge = StdioLspBridge::new(stdio, Daemon::default());

        assert_eq!(
            bridge.pump_once(),
            Err(StdioLspBridgeError::FrameTooLarge {
                direction: BridgeDirection::ClientToDaemon,
                size: MAX_LSP_FRAME_BYTES + 1,
                limit: MAX_LSP_FRAME_BYTES,
            })
        );
        assert!(bridge.daemon.output.is_empty());
        assert!(bridge.pump_once().unwrap().closed);
    }

    #[test]
    fn peer_close_never_sends_a_frame_to_the_closed_transport() {
        let mut stdio = Stdio::default();
        stdio.input.push_back(FramePoll::Closed);
        let mut daemon = Daemon::default();
        daemon.input.push_back(FramePoll::Frame(vec![1]));
        let mut bridge = StdioLspBridge::new(stdio, daemon);

        assert!(bridge.pump_once().unwrap().closed);
        let (stdio, daemon) = bridge.into_parts();
        assert!(stdio.output.is_empty());
        assert!(daemon.output.is_empty());
    }
}
