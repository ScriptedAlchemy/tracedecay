use std::time::Duration;

use futures_util::SinkExt;
use tokio::io::{AsyncWriteExt, duplex};
use tokio::time::Instant;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracedecay_lsp::{
    AsyncContentLengthError, ContentLengthCodec, ContentLengthCodecError, FramePoll,
    MAX_LSP_FRAME_BYTES, read_content_length_frame_until,
};

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
        read_content_length_frame_until(&mut reader, Instant::now() + Duration::from_secs(1),)
            .await
            .expect("read opaque frame"),
        FramePoll::Frame(payload.to_vec())
    );
    write_tail.await.expect("tail writer");
}

#[tokio::test(start_paused = true)]
async fn async_reader_distinguishes_idle_and_partial_deadlines() {
    let (mut writer, reader) = duplex(64);
    let mut reader = FramedRead::new(reader, ContentLengthCodec::new());

    assert_eq!(
        read_content_length_frame_until(&mut reader, Instant::now() + Duration::from_millis(10),)
            .await
            .expect("idle deadline"),
        FramePoll::Pending
    );

    writer
        .write_all(b"Content-Length: 2\r\n\r\n{")
        .await
        .expect("write partial frame");
    assert!(matches!(
        read_content_length_frame_until(&mut reader, Instant::now() + Duration::from_millis(10),)
            .await,
        Err(AsyncContentLengthError::DeadlineElapsed)
    ));
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
        match read_content_length_frame_until(&mut reader, Instant::now() + Duration::from_secs(1))
            .await
        {
            Err(AsyncContentLengthError::Codec(actual)) => assert_eq!(actual, expected),
            outcome => panic!("expected codec error, got {outcome:?}"),
        }
    }
}

#[tokio::test]
async fn async_writer_uses_strict_codec_and_deadline() {
    let payload = br#"{"jsonrpc":"2.0","method":"initialized"}"#;
    let (writer, reader) = duplex(256);
    let mut writer = FramedWrite::new(writer, ContentLengthCodec::new());
    tokio::time::timeout_at(
        Instant::now() + Duration::from_secs(1),
        writer.send(payload.as_slice()),
    )
    .await
    .expect("write deadline")
    .expect("write framed payload");
    let mut decoder = FramedRead::new(reader, ContentLengthCodec::new());
    assert_eq!(
        read_content_length_frame_until(&mut decoder, Instant::now() + Duration::from_secs(1),)
            .await
            .expect("decode written frame"),
        FramePoll::Frame(payload.to_vec())
    );
}
