use std::time::Duration;

use tokio::io::{AsyncWriteExt, duplex};
use tokio::time::Instant;
use tracedecay_lsp::{
    AsyncContentLengthError, AsyncContentLengthReader, ContentLengthCodec, ContentLengthCodecError,
    FramePoll, MAX_LSP_FRAME_BYTES, write_content_length_frame_until,
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
    let mut reader = AsyncContentLengthReader::new(reader);

    assert_eq!(
        reader
            .read_frame_until(Instant::now() + Duration::from_secs(1))
            .await
            .expect("read opaque frame"),
        FramePoll::Frame(payload.to_vec())
    );
    write_tail.await.expect("tail writer");
}

#[tokio::test(start_paused = true)]
async fn async_reader_distinguishes_idle_and_partial_deadlines() {
    let (mut writer, reader) = duplex(64);
    let mut reader = AsyncContentLengthReader::new(reader);

    assert_eq!(
        reader
            .read_frame_until(Instant::now() + Duration::from_millis(10))
            .await
            .expect("idle deadline"),
        FramePoll::Pending
    );

    writer
        .write_all(b"Content-Length: 2\r\n\r\n{")
        .await
        .expect("write partial frame");
    assert!(matches!(
        reader
            .read_frame_until(Instant::now() + Duration::from_millis(10))
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
        let mut reader = AsyncContentLengthReader::new(reader);
        match reader
            .read_frame_until(Instant::now() + Duration::from_secs(1))
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
    let (mut writer, mut reader) = duplex(256);
    write_content_length_frame_until(
        &mut writer,
        payload,
        Instant::now() + Duration::from_secs(1),
    )
    .await
    .expect("write framed payload");
    let mut decoder = AsyncContentLengthReader::new(&mut reader);
    assert_eq!(
        decoder
            .read_frame_until(Instant::now() + Duration::from_secs(1))
            .await
            .expect("decode written frame"),
        FramePoll::Frame(payload.to_vec())
    );
}
