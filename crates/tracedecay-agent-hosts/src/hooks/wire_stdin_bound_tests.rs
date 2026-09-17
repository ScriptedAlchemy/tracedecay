use std::io::{self, ErrorKind, Read};

use super::{
    HookStdinAdmission, HookStdinRead, classify_hook_stdin, hook_stdin_exit_code,
    read_stdin_bounded_from,
};
use tracedecay_framing::MAX_WIRE_MESSAGE_BYTES;

struct ChunkedHostileReader {
    remaining: usize,
    chunk: Vec<u8>,
}

impl ChunkedHostileReader {
    fn new(total: usize, chunk_byte: u8, chunk_len: usize) -> Self {
        Self {
            remaining: total,
            chunk: vec![chunk_byte; chunk_len.max(1)],
        }
    }
}

impl Read for ChunkedHostileReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let n = buf.len().min(self.chunk.len()).min(self.remaining);
        buf[..n].copy_from_slice(&self.chunk[..n]);
        self.remaining -= n;
        Ok(n)
    }
}

#[test]
fn hook_stdin_streams_hostile_input_and_returns_oversized_without_payload() {
    let mut hostile = ChunkedHostileReader::new(MAX_WIRE_MESSAGE_BYTES + 512 * 1024, b'h', 4096);
    let outcome = read_stdin_bounded_from(&mut hostile).unwrap();
    assert!(matches!(outcome, HookStdinRead::Oversized));
    assert!(hostile.remaining < MAX_WIRE_MESSAGE_BYTES + 512 * 1024);
}

#[test]
fn hook_stdin_admission_is_one_policy() {
    let event = classify_hook_stdin(Ok(HookStdinRead::Event("{\"stop\":true}".to_owned())));
    assert!(matches!(event, HookStdinAdmission::Event(_)));
    assert_eq!(hook_stdin_exit_code(&event), None);

    let oversized = classify_hook_stdin(Ok(HookStdinRead::Oversized));
    assert_eq!(hook_stdin_exit_code(&oversized), Some(0));

    let failed = classify_hook_stdin(Err(io::Error::new(ErrorKind::BrokenPipe, "closed")));
    assert_eq!(hook_stdin_exit_code(&failed), Some(1));
}

#[test]
fn hook_stdin_accepts_exact_wire_cap() {
    let body = vec![b'a'; MAX_WIRE_MESSAGE_BYTES];
    let outcome = read_stdin_bounded_from(&mut body.as_slice()).unwrap();
    match outcome {
        HookStdinRead::Event(event) => assert_eq!(event.len(), MAX_WIRE_MESSAGE_BYTES),
        HookStdinRead::Oversized => panic!("exact cap must be accepted"),
    }
}
