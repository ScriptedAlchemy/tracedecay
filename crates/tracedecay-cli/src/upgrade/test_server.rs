//! A loopback stand-in for GitHub's release, attestation, and bundle
//! storage endpoints.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// The attestations API path for a lowercase hex SHA-256 digest.
pub(super) fn attestations_path(hex_digest: &str) -> String {
    format!("/repos/ScriptedAlchemy/tracedecay/attestations/sha256:{hex_digest}")
}

pub(super) struct FakeGitHub {
    pub(super) base: String,
    listener: TcpListener,
    requests: Arc<Mutex<Vec<String>>>,
}

impl FakeGitHub {
    /// Binds the port first, so served bodies can point back at `base`.
    pub(super) fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        Self {
            base,
            listener,
            requests: Arc::default(),
        }
    }

    /// Serves each `(path, body)` over plain HTTP/1.1 for as long as the test
    /// process lives, answering any other path with GitHub's 404. Returns the
    /// log of every requested path.
    pub(super) fn serve(self, responses: Vec<(String, Vec<u8>)>) -> Arc<Mutex<Vec<String>>> {
        let requests = Arc::clone(&self.requests);
        std::thread::spawn(move || {
            for stream in self.listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut request = [0u8; 4096];
                let read = stream.read(&mut request).unwrap_or(0);
                let head = String::from_utf8_lossy(&request[..read]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                self.requests.lock().unwrap().push(path.clone());
                let (status, body) = responses.iter().find(|(served, _)| *served == path).map_or(
                    ("404 Not Found", &br#"{"message":"Not Found"}"#[..]),
                    |(_, body)| ("200 OK", body.as_slice()),
                );
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
        });
        requests
    }
}
