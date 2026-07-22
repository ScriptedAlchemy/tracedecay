use std::io::{self, Read, Write};

pub(crate) use tracedecay_sqlite_parity_protocol::MAX_REQUEST_BYTES;

use crate::service::handle_request_bytes;

/// Reads one bounded JSON request and writes one versioned JSON response.
pub fn serve(reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    let mut bytes = Vec::new();
    reader.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    let response = handle_request_bytes(&bytes);
    serde_json::to_writer(&mut writer, &response)?;
    writer.write_all(b"\n")
}
