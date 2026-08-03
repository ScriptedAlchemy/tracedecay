use sha2::{Digest, Sha256};

pub(super) trait CanonicalSink {
    fn write(&mut self, chunk: &str);
}

/// How much canonical text accumulates before it reaches the wrapped sink.
///
/// Canonical writing emits many one-byte chunks (`"`, `:`, `,`); handing each
/// of those to `Sha256` pays block-buffer bookkeeping per call, so the hashing
/// path batches them here first.
pub(super) const SINK_BUFFER_CAPACITY: usize = 64 * 1024;

/// A [`CanonicalSink`] that batches small writes before forwarding them.
pub(super) struct BufferedSink<S: CanonicalSink> {
    inner: S,
    buffer: String,
}

impl<S: CanonicalSink> BufferedSink<S> {
    pub(super) fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: String::with_capacity(SINK_BUFFER_CAPACITY),
        }
    }

    fn flush(&mut self) {
        if !self.buffer.is_empty() {
            self.inner.write(&self.buffer);
            self.buffer.clear();
        }
    }

    /// Flush every buffered byte and return the wrapped sink.
    pub(super) fn finish(mut self) -> S {
        self.flush();
        self.inner
    }
}

impl<S: CanonicalSink> CanonicalSink for BufferedSink<S> {
    fn write(&mut self, chunk: &str) {
        if self.buffer.len() + chunk.len() > SINK_BUFFER_CAPACITY {
            self.flush();
            if chunk.len() >= SINK_BUFFER_CAPACITY {
                self.inner.write(chunk);
                return;
            }
        }
        self.buffer.push_str(chunk);
    }
}

impl CanonicalSink for String {
    fn write(&mut self, chunk: &str) {
        self.push_str(chunk);
    }
}

impl CanonicalSink for Vec<u8> {
    fn write(&mut self, chunk: &str) {
        self.extend_from_slice(chunk.as_bytes());
    }
}

impl CanonicalSink for Sha256 {
    fn write(&mut self, chunk: &str) {
        Digest::update(self, chunk.as_bytes());
    }
}

/// The JSON escape `serde_json`'s compact formatter emits for each control
/// byte. Mirroring the table here lets canonical writing stream escapes
/// straight into the sink instead of allocating a `String` per string value.
static CONTROL_ESCAPES: [&str; 32] = [
    "\\u0000", "\\u0001", "\\u0002", "\\u0003", "\\u0004", "\\u0005", "\\u0006", "\\u0007", "\\b",
    "\\t", "\\n", "\\u000b", "\\f", "\\r", "\\u000e", "\\u000f", "\\u0010", "\\u0011", "\\u0012",
    "\\u0013", "\\u0014", "\\u0015", "\\u0016", "\\u0017", "\\u0018", "\\u0019", "\\u001a",
    "\\u001b", "\\u001c", "\\u001d", "\\u001e", "\\u001f",
];

/// Write one JSON string literal (quotes included) directly into the sink.
///
/// Byte-for-byte equivalent to `serde_json::to_string(value)` for a string:
/// only `"`, `\`, and the C0 control bytes are escaped, non-ASCII is passed
/// through as UTF-8, and `\u00xx` escapes use lowercase hex.
pub(super) fn write_json_string(value: &str, output: &mut impl CanonicalSink) {
    output.write("\"");
    let mut run_start = 0usize;
    for (index, byte) in value.bytes().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            0x00..=0x1f => CONTROL_ESCAPES[byte as usize],
            _ => continue,
        };
        if run_start < index {
            // Every escaped byte is ASCII, so both ends are char boundaries.
            output.write(&value[run_start..index]);
        }
        output.write(escape);
        run_start = index + 1;
    }
    if run_start < value.len() {
        output.write(&value[run_start..]);
    }
    output.write("\"");
}

/// Write a JSON number without allocating for the common integral cases.
///
/// `serde_json::Number`'s `Display` renders `u64`/`i64` payloads as plain
/// decimal, so the stack-formatted digits are identical; anything else (float
/// payloads) falls back to the owned rendering.
pub(super) fn write_json_number(number: &serde_json::Number, output: &mut impl CanonicalSink) {
    if let Some(value) = number.as_u64() {
        write_u64(value, output);
    } else if let Some(value) = number.as_i64().filter(|value| *value < 0) {
        output.write("-");
        write_u64(value.unsigned_abs(), output);
    } else {
        output.write(&number.to_string());
    }
}

pub(super) fn write_u64(value: u64, output: &mut impl CanonicalSink) {
    let mut buffer = [0u8; 20];
    let mut index = buffer.len();
    let mut remaining = value;
    loop {
        index -= 1;
        buffer[index] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    // Each byte is an ASCII digit by construction. Encode one digit at a time
    // through the stack buffer so this path has no fallible conversion or
    // allocation fallback.
    let mut encoded = [0u8; 4];
    for digit in &buffer[index..] {
        output.write(char::from(*digit).encode_utf8(&mut encoded));
    }
}

pub(super) fn write_i64(value: i64, output: &mut impl CanonicalSink) {
    if value < 0 {
        output.write("-");
        write_u64(value.unsigned_abs(), output);
    } else {
        write_u64(value.unsigned_abs(), output);
    }
}

/// Render an `f64` exactly as `to_value` would: non-finite floats become
/// `null`, finite floats take `serde_json::Number`'s own rendering.
pub(super) fn write_f64(value: f64, output: &mut impl CanonicalSink) {
    match serde_json::Number::from_f64(value) {
        Some(number) => write_json_number(&number, output),
        None => output.write("null"),
    }
}
