#[allow(unused_imports)]
#[path = "../../../src/lsp_bridge.rs"]
mod root_facade;

use root_facade::{ContentLengthCodec, FramePoll, FrameSend, LspFrame, MAX_LSP_FRAME_BYTES};

#[test]
fn root_facade_reexports_the_store_free_frame_contract() {
    let payload: LspFrame = br#"{"jsonrpc":"2.0","method":"initialized"}"#.to_vec();
    let encoded = ContentLengthCodec::encode(&payload).unwrap();
    let mut codec = ContentLengthCodec::new();
    codec.push(&encoded);

    assert_eq!(codec.next_frame(), Ok(Some(payload)));
    assert_eq!(FramePoll::Pending, FramePoll::Pending);
    assert_eq!(FrameSend::Sent, FrameSend::Sent);
    assert_eq!(MAX_LSP_FRAME_BYTES, 4 * 1024 * 1024);
}
