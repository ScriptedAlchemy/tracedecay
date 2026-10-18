use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncWriteExt, duplex};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracedecay_daemon_protocol::MAX_LSP_FRAME_BYTES;
use tracedecay_lsp::{AsyncContentLengthError, ContentLengthCodec, ContentLengthCodecError};

#[tokio::test]
async fn async_reader_preserves_opaque_partial_crlf_frame() {
    let payload = b"\0not-json\r\n\xff";
    let encoded = ContentLengthCodec::encode(payload).expect("encode opaque payload");
    let (mut writer, reader) = duplex(encoded.len());
    let split = encoded.len() / 2;
    writer
        .write_all(&encoded[..split])
        .await
        .expect("write first fragment");
    let write_tail = tokio::spawn(async move {
        writer
            .write_all(&encoded[split..])
            .await
            .expect("write second fragment");
    });
    let mut reader = FramedRead::new(reader, ContentLengthCodec::new());

    assert_eq!(
        reader
            .next()
            .await
            .expect("frame before EOF")
            .expect("read opaque frame"),
        payload.to_vec()
    );
    write_tail.await.expect("tail writer");
}

#[tokio::test]
async fn async_reader_rejects_malformed_and_oversize_frames() {
    for (wire, expected) in [
        (
            b"Content-Length: 2\n\n{}".to_vec(),
            ContentLengthCodecError::MalformedHeader,
        ),
        (
            format!("Content-Length: {}\r\n\r\n", MAX_LSP_FRAME_BYTES + 1).into_bytes(),
            ContentLengthCodecError::FrameTooLarge {
                size: MAX_LSP_FRAME_BYTES + 1,
                limit: MAX_LSP_FRAME_BYTES,
            },
        ),
    ] {
        let (mut writer, reader) = duplex(wire.len());
        writer
            .write_all(&wire)
            .await
            .expect("write malformed frame");
        let mut reader = FramedRead::new(reader, ContentLengthCodec::new());
        match reader.next().await {
            Some(Err(AsyncContentLengthError::Codec(actual))) => assert_eq!(actual, expected),
            outcome => panic!("expected codec error, got {outcome:?}"),
        }
    }
}

#[tokio::test]
async fn async_writer_uses_strict_codec() {
    let payload = br#"{"jsonrpc":"2.0","method":"initialized"}"#;
    let (writer, reader) = duplex(256);
    let mut writer = FramedWrite::new(writer, ContentLengthCodec::new());
    writer
        .send(payload.as_slice())
        .await
        .expect("write framed payload");
    let mut decoder = FramedRead::new(reader, ContentLengthCodec::new());
    assert_eq!(
        decoder
            .next()
            .await
            .expect("frame before EOF")
            .expect("decode written frame"),
        payload.to_vec()
    );
}
