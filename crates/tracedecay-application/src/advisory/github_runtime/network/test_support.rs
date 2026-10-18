use std::io::{Read, Write};
use std::net::TcpStream;

pub(in crate::advisory::github_runtime) fn read_http_request_with_headers(
    stream: &mut TcpStream,
) -> (String, serde_json::Value) {
    // macOS `accept` inherits `O_NONBLOCK` from a non-blocking listener, so
    // the first `read` returns `WouldBlock` instead of the request bytes.
    stream
        .set_nonblocking(false)
        .expect("accepted GraphQL fixture stream must be blocking");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "fixture client closed before request headers");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).unwrap();
        assert!(read > 0, "fixture client closed before request body");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body = if content_length == 0 {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
    };
    (
        String::from_utf8(bytes[..header_end].to_vec()).unwrap(),
        body,
    )
}

pub(super) fn read_http_request(stream: &mut TcpStream) -> serde_json::Value {
    read_http_request_with_headers(stream).1
}

pub(in crate::advisory::github_runtime) fn write_http_json(
    stream: &mut TcpStream,
    value: &serde_json::Value,
) {
    let body = serde_json::to_vec(value).unwrap();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-RateLimit-Limit: 5000\r\nX-RateLimit-Remaining: 4999\r\nX-RateLimit-Reset: 2000000000\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(&body).unwrap();
}

/// Writes one response with an exact status line and extra headers, as a
/// captured provider refusal carries them.
pub(in crate::advisory::github_runtime) fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(&str, &str)],
    value: &serde_json::Value,
) {
    let body = serde_json::to_vec(value).unwrap();
    let mut head = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(&body).unwrap();
}
