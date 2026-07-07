use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use url::{Host, Url};

use crate::errors::{Result, TraceDecayError};

pub(crate) fn validate_url(raw: &str) -> Result<()> {
    let url = parse_url(raw)?;
    let host = url.host().ok_or_else(|| TraceDecayError::Config {
        message: "job webhook delivery url must include a host".to_string(),
    })?;
    validate_host(&host)
}

pub(crate) fn post_json_url(raw_url: &str, payload: &Value, timeout: Duration) -> Result<u16> {
    let url = parse_url(raw_url)?;
    let endpoint = resolve_endpoint(&url)?;
    post_json(&endpoint, payload, timeout)
}

fn parse_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|e| TraceDecayError::Config {
        message: format!("invalid job webhook delivery url: {e}"),
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return job_error("job webhook delivery url must use http:// or https://");
    }
    if url.host().is_none() {
        return job_error("job webhook delivery url must include a host");
    }
    Ok(url)
}

fn validate_host(host: &Host<&str>) -> Result<()> {
    match host {
        Host::Domain(domain) => {
            let normalized = domain.trim_end_matches('.').to_ascii_lowercase();
            if is_localhost_like_domain(&normalized) {
                return job_error("job webhook delivery url must not target localhost");
            }
        }
        Host::Ipv4(ip) => validate_ip(IpAddr::V4(*ip))?,
        Host::Ipv6(ip) => validate_ip(IpAddr::V6(*ip))?,
    }
    Ok(())
}

fn is_localhost_like_domain(domain: &str) -> bool {
    matches!(domain, "localhost" | "localhost.localdomain")
        || domain.ends_with(".localhost")
        || domain.ends_with(".localhost.localdomain")
}

fn validate_ip(ip: IpAddr) -> Result<()> {
    if is_disallowed_ip(ip) {
        return job_error("job webhook delivery url must not target private or local networks");
    }
    Ok(())
}

fn is_disallowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_disallowed_ipv4(ip),
        IpAddr::V6(ip) => is_disallowed_ipv6(ip),
    }
}

fn is_disallowed_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, d] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && matches!(b, 18 | 19))
        || (224..=255).contains(&a)
        || (a == 0 && (b, c, d) != (0, 0, 0))
}

fn is_disallowed_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = embedded_ipv4(ip) {
        return is_disallowed_ipv4(v4);
    }
    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ((segments[0] & 0xfe00) == 0xfc00)
        || ((segments[0] & 0xffc0) == 0xfe80)
        || ((segments[0] & 0xff00) == 0xff00)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
}

fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let segments = ip.segments();
    if segments[0] == 0x2002 {
        let [a, b] = segments[1].to_be_bytes();
        let [c, d] = segments[2].to_be_bytes();
        return Some(Ipv4Addr::new(a, b, c, d));
    }
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        let [a, b] = (!segments[6]).to_be_bytes();
        let [c, d] = (!segments[7]).to_be_bytes();
        return Some(Ipv4Addr::new(a, b, c, d));
    }
    if segments[..6].iter().all(|s| *s == 0) && !(segments[6] == 0 && segments[7] == 0) {
        let [a, b] = segments[6].to_be_bytes();
        let [c, d] = segments[7].to_be_bytes();
        return Some(Ipv4Addr::new(a, b, c, d));
    }
    None
}

#[derive(Debug, Clone)]
struct WebhookEndpoint {
    url: Url,
    connect_addr: SocketAddr,
}

impl WebhookEndpoint {
    #[cfg(test)]
    fn new_for_test(url: Url, connect_addr: SocketAddr) -> Self {
        Self { url, connect_addr }
    }
}

fn resolve_endpoint(url: &Url) -> Result<WebhookEndpoint> {
    let host = url.host().ok_or_else(|| TraceDecayError::Config {
        message: "job webhook delivery url must include a host".to_string(),
    })?;
    validate_host(&host)?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| TraceDecayError::Config {
            message: "job webhook delivery url must include a port for unknown schemes".to_string(),
        })?;
    let connect_addr = match host {
        Host::Ipv4(ip) => SocketAddr::new(IpAddr::V4(ip), port),
        Host::Ipv6(ip) => SocketAddr::new(IpAddr::V6(ip), port),
        Host::Domain(host) => {
            let addrs: Vec<SocketAddr> = (host, port)
                .to_socket_addrs()
                .map_err(|e| TraceDecayError::Config {
                    message: format!("failed to resolve webhook host '{host}': {e}"),
                })?
                .collect();
            if addrs.is_empty() {
                return Err(TraceDecayError::Config {
                    message: format!("webhook host '{host}' resolved no addresses"),
                });
            }
            for addr in &addrs {
                validate_ip(addr.ip())?;
            }
            addrs[0]
        }
    };
    validate_ip(connect_addr.ip())?;
    Ok(WebhookEndpoint {
        url: url.clone(),
        connect_addr,
    })
}

fn post_json(endpoint: &WebhookEndpoint, payload: &Value, timeout: Duration) -> Result<u16> {
    let body = serde_json::to_vec(payload).map_err(|e| TraceDecayError::Config {
        message: format!("failed to encode webhook payload: {e}"),
    })?;
    match endpoint.url.scheme() {
        "http" => post_json_over_http(endpoint, &body, timeout),
        "https" => post_json_over_https(endpoint, &body, timeout),
        _ => job_error("job webhook delivery url must use http:// or https://"),
    }
}

fn post_json_over_http(endpoint: &WebhookEndpoint, body: &[u8], timeout: Duration) -> Result<u16> {
    let mut stream = connect_tcp(endpoint, timeout)?;
    write_request(&mut stream, endpoint, body)?;
    read_status(&mut stream)
}

fn post_json_over_https(endpoint: &WebhookEndpoint, body: &[u8], timeout: Duration) -> Result<u16> {
    let stream = connect_tcp(endpoint, timeout)?;
    let root_store = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let host_label = host_label(&endpoint.url)?;
    let server_name = rustls::pki_types::ServerName::try_from(host_label).map_err(|_| {
        TraceDecayError::Config {
            message: "invalid webhook host for TLS verification".to_string(),
        }
    })?;
    let conn = rustls::ClientConnection::new(Arc::new(config), server_name).map_err(|e| {
        TraceDecayError::Config {
            message: format!("failed to create webhook TLS connection: {e}"),
        }
    })?;
    let mut stream = rustls::StreamOwned::new(conn, stream);
    write_request(&mut stream, endpoint, body)?;
    read_status(&mut stream)
}

fn connect_tcp(endpoint: &WebhookEndpoint, timeout: Duration) -> Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&endpoint.connect_addr, timeout).map_err(|e| {
        TraceDecayError::Config {
            message: format!(
                "failed to connect webhook endpoint '{}': {e}",
                endpoint.connect_addr
            ),
        }
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to configure webhook socket timeout: {e}"),
        })?;
    Ok(stream)
}

fn write_request<W: Write>(writer: &mut W, endpoint: &WebhookEndpoint, body: &[u8]) -> Result<()> {
    let path = request_target(&endpoint.url);
    let host = host_header(&endpoint.url)?;
    write!(
        writer,
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: tracedecay-automation-jobs\r\nContent-Type: application/json\r\nAccept: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .and_then(|()| writer.write_all(body))
    .and_then(|()| writer.flush())
    .map_err(|e| TraceDecayError::Config {
        message: format!("failed to write webhook request: {e}"),
    })
}

fn read_status<R: Read>(reader: &mut R) -> Result<u16> {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 512];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") || bytes.len() > 8192 {
                    break;
                }
            }
            // A server that sends its full response and then closes abruptly can
            // surface the close as a read error before we observe the end of the
            // headers — e.g. a Windows peer resets the connection (os error
            // 10053) right after `write_all`. If a complete status line already
            // arrived the delivery succeeded, so parse what we have rather than
            // discarding it; only propagate the error when no status was read.
            Err(e) => {
                if parse_status_line(&bytes).is_some() {
                    break;
                }
                return Err(TraceDecayError::Config {
                    message: format!("failed to read webhook response: {e}"),
                });
            }
        }
    }
    parse_status_line(&bytes).ok_or_else(|| TraceDecayError::Config {
        message: "webhook response did not include an HTTP status".to_string(),
    })
}

/// Parses the numeric HTTP status from a (possibly partial) response head.
/// Returns `None` until a terminated status line has arrived so a truncated
/// first line is never mistaken for a status code.
fn parse_status_line(bytes: &[u8]) -> Option<u16> {
    if !bytes.contains(&b'\n') {
        return None;
    }
    let header = String::from_utf8_lossy(bytes);
    header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
}

fn request_target(url: &Url) -> String {
    let mut target = url.path().to_string();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    target
}

fn host_label(url: &Url) -> Result<String> {
    url.host_str()
        .map(|host| host.trim_matches(['[', ']']).to_string())
        .ok_or_else(|| TraceDecayError::Config {
            message: "job webhook delivery url must include a host".to_string(),
        })
}

fn host_header(url: &Url) -> Result<String> {
    let host = url.host_str().ok_or_else(|| TraceDecayError::Config {
        message: "job webhook delivery url must include a host".to_string(),
    })?;
    let host = match url.host().ok_or_else(|| TraceDecayError::Config {
        message: "job webhook delivery url must include a host".to_string(),
    })? {
        Host::Ipv6(_) => format!("[{}]", host.trim_matches(['[', ']'])),
        _ => host.to_string(),
    };
    let include_port = url
        .port()
        .is_some_and(|port| Some(port) != url.port_or_known_default());
    if include_port {
        let port = url.port().ok_or_else(|| TraceDecayError::Config {
            message: "job webhook delivery url must include a port".to_string(),
        })?;
        Ok(format!("{host}:{port}"))
    } else {
        Ok(host)
    }
}

fn job_error<T>(message: &str) -> Result<T> {
    Err(TraceDecayError::Config {
        message: message.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
    use std::sync::mpsc;
    use std::thread;

    /// A reader that yields a canned buffer once, then fails every subsequent
    /// read with `ConnectionAborted` — models a peer that sends its full
    /// response and then resets the socket (Windows os error 10053).
    struct ResetAfterResponse {
        response: Vec<u8>,
        sent: bool,
    }

    impl Read for ResetAfterResponse {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.sent {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    "connection aborted",
                ));
            }
            self.sent = true;
            let n = self.response.len().min(buf.len());
            buf[..n].copy_from_slice(&self.response[..n]);
            Ok(n)
        }
    }

    #[test]
    fn read_status_accepts_a_complete_response_before_a_connection_reset() {
        let mut reader = ResetAfterResponse {
            response: b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n".to_vec(),
            sent: false,
        };
        // The trailing `\r\n\r\n` never arrives before the reset, so the loop
        // hits the read error — but a full status line was already received.
        let status = match read_status(&mut reader) {
            Ok(status) => status,
            Err(err) => panic!("read_status should succeed: {err}"),
        };
        assert_eq!(status, 202);
    }

    #[test]
    fn read_status_propagates_a_reset_with_no_status_line() {
        // `sent: true` makes the very first read fail, so no bytes arrive.
        let mut reader = ResetAfterResponse {
            response: Vec::new(),
            sent: true,
        };
        assert!(read_status(&mut reader).is_err());
    }

    #[test]
    fn ipv6_embedded_ipv4_targets_are_blocked() -> std::result::Result<(), std::net::AddrParseError>
    {
        for s in [
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::ffff:192.168.1.1",
            "::127.0.0.1",
            "2002:a9fe:a9fe::",
        ] {
            let ip: Ipv6Addr = s.parse()?;
            assert!(
                is_disallowed_ipv6(ip),
                "{s} embeds a blocked IPv4 target and must be rejected"
            );
        }
        assert!(!is_disallowed_ipv6("2606:4700:4700::1111".parse()?));
        assert!(!is_disallowed_ipv6("::ffff:93.184.216.34".parse()?));
        Ok(())
    }

    #[test]
    fn webhook_http_post_uses_validated_socket_addr_and_preserves_host()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let Ok((mut stream, peer)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut buf = [0_u8; 512];
            while let Ok(read) = stream.read(&mut buf) {
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = tx.send((peer, String::from_utf8_lossy(&request).to_string()));
            let _ = stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
            let _ = stream.flush();
            // Close gracefully: signal end-of-response with a half-close and let
            // the client read the full response before the socket is dropped. A
            // bare drop can send an RST on some platforms (Windows os error
            // 10053), aborting the client mid-read.
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let mut drain = [0_u8; 64];
            while stream.read(&mut drain).is_ok_and(|n| n > 0) {}
        });

        let url = Url::parse("http://webhook.example.test/hook?token=abc")?;
        let endpoint = WebhookEndpoint::new_for_test(
            url,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), addr.port()),
        );
        let status = post_json(&endpoint, &json!({"ok": true}), Duration::from_secs(10))
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
        let (_peer, request) = rx.recv()?;

        assert_eq!(status, 202);
        assert!(request.starts_with("POST /hook?token=abc HTTP/1.1\r\n"));
        assert!(request.contains("\r\nHost: webhook.example.test\r\n"));
        assert!(request.contains("\r\nContent-Type: application/json\r\n"));
        Ok(())
    }
}
