use super::{HookStdinRead, read_stdin_bounded_from};
use crate::application::host_admission::MAX_WIRE_MESSAGE_BYTES;
use std::io::{self, Read};

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
fn hook_stdin_accepts_exact_wire_cap() {
    let body = vec![b'a'; MAX_WIRE_MESSAGE_BYTES];
    let outcome = read_stdin_bounded_from(&mut body.as_slice()).unwrap();
    match outcome {
        HookStdinRead::Event(event) => assert_eq!(event.len(), MAX_WIRE_MESSAGE_BYTES),
        HookStdinRead::Oversized => panic!("exact cap must be accepted"),
    }
}
