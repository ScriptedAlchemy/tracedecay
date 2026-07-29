//! Compatibility façade for the store-free LSP bridge contracts.
//!
//! The implementation lives in `tracedecay-lsp`; retaining this module keeps
//! existing root-crate callers on the same bounded framing and bridge API.

pub use tracedecay_lsp::{
    BridgeDirection, BridgePumpOutcome, ContentLengthCodec, ContentLengthCodecError,
    ContentLengthStdioError, ContentLengthStdioTransport, DaemonLspSessionTransport, FramePoll,
    FrameSend, LspFrame, MAX_LSP_FRAME_BYTES, MAX_LSP_HEADER_BYTES, StdioFrameTransport,
    StdioLspBridge, StdioLspBridgeError,
};
