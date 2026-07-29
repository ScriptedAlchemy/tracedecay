use std::collections::VecDeque;

use tracedecay_lsp::{
    BridgePumpOutcome, ContentLengthCodec, ContentLengthCodecError, DaemonLspSessionTransport,
    FramePoll, FrameSend, LspFrame, MAX_LSP_FRAME_BYTES, StdioFrameTransport, StdioLspBridge,
};

#[derive(Default)]
struct Host {
    input: VecDeque<FramePoll>,
}

impl StdioFrameTransport for Host {
    type Error = ();

    fn poll_frame(&mut self) -> Result<FramePoll, Self::Error> {
        Ok(self.input.pop_front().unwrap_or(FramePoll::Pending))
    }

    fn try_send_frame(&mut self, _frame: &[u8]) -> Result<FrameSend, Self::Error> {
        Ok(FrameSend::Sent)
    }
}

#[derive(Default)]
struct Session {
    received: Vec<LspFrame>,
}

impl DaemonLspSessionTransport for Session {
    type Error = ();

    fn try_send_client_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error> {
        self.received.push(frame.to_vec());
        Ok(FrameSend::Sent)
    }

    fn poll_daemon_frame(&mut self) -> Result<FramePoll, Self::Error> {
        Ok(FramePoll::Pending)
    }
}

#[test]
fn bridge_forwards_cancellation_and_shutdown_as_opaque_frames() {
    let cancellation =
        br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":7}}"#.to_vec();
    let shutdown = br#"{"jsonrpc":"2.0","id":8,"method":"shutdown"}"#.to_vec();
    let mut host = Host::default();
    host.input.push_back(FramePoll::Frame(cancellation.clone()));
    host.input.push_back(FramePoll::Frame(shutdown.clone()));
    let mut bridge = StdioLspBridge::new(host, Session::default());

    assert_eq!(
        bridge.pump_once().unwrap(),
        BridgePumpOutcome {
            client_to_daemon: 1,
            ..BridgePumpOutcome::default()
        }
    );
    assert_eq!(
        bridge.pump_once().unwrap(),
        BridgePumpOutcome {
            client_to_daemon: 1,
            ..BridgePumpOutcome::default()
        }
    );

    let (_, session) = bridge.into_parts();
    assert_eq!(session.received, vec![cancellation, shutdown]);
}

#[test]
fn framing_rejects_payloads_above_the_hard_frame_cap() {
    let oversized = vec![0; MAX_LSP_FRAME_BYTES + 1];

    assert_eq!(
        ContentLengthCodec::encode(&oversized),
        Err(ContentLengthCodecError::FrameTooLarge {
            size: MAX_LSP_FRAME_BYTES + 1,
            limit: MAX_LSP_FRAME_BYTES,
        })
    );
}
