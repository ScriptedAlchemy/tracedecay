//! Live MCP `functions_timing` must honor `HOTPATH_FUNCTIONS_LIMIT` when the
//! shipped guard applies it before the profiler starts.
//!
//! The builder limit is unlimited. Without `with_functions_display_limit`,
//! hotpath 0.24's worker keeps that unlimited snapshot and the tool returns
//! every measured function. The env is set before the guard starts, which is
//! the process boundary the shipped binary has.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use tracedecay_hotpath_guard::with_functions_display_limit;

#[hotpath::measure]
fn slow_long() {
    std::thread::sleep(Duration::from_millis(200));
}

#[hotpath::measure]
fn slow_mid() {
    std::thread::sleep(Duration::from_millis(20));
}

#[hotpath::measure]
fn slow_short() {
    std::thread::sleep(Duration::from_millis(5));
}

#[hotpath::measure]
fn slow_tiny() {
    std::thread::sleep(Duration::from_millis(1));
}

#[test]
fn live_mcp_functions_timing_honors_functions_limit() {
    let port = free_port();
    let report_path = std::env::temp_dir().join(format!(
        "hotpath-functions-limit-{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&report_path);

    unsafe {
        std::env::set_var("HOTPATH_EXCLUDE_WRAPPER", "1");
        std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        std::env::set_var("HOTPATH_MCP_PORT", port.to_string());
        std::env::set_var("HOTPATH_FUNCTIONS_LIMIT", "2");
        std::env::set_var("HOTPATH_OUTPUT_FORMAT", "json");
        std::env::set_var("HOTPATH_OUTPUT_PATH", &report_path);
        std::env::remove_var("HOTPATH_LIMIT");
        std::env::remove_var("HOTPATH_REPORT");
        std::env::remove_var("HOTPATH_MCP_AUTH_TOKEN");
    }

    let guard = with_functions_display_limit(
        hotpath::HotpathGuardBuilder::new("functions-limit-live")
            .functions_limit(0)
            .format(hotpath::Format::Json)
            .output_path(&report_path),
    )
    .build();

    slow_long();
    slow_mid();
    slow_short();
    slow_tiny();

    let session = initialize(&port);
    let names = wait_for_functions(&port, &session, |got| !got.is_empty());
    assert_live_limit(&names);

    drop(guard);

    let report = std::fs::read_to_string(&report_path).unwrap_or_else(|error| {
        panic!("exit report missing at {}: {error}", report_path.display())
    });
    let report: serde_json::Value = serde_json::from_str(&report)
        .unwrap_or_else(|error| panic!("exit report is not JSON: {error}\n{report}"));
    let exit_names = names_from_list(
        report
            .get("functions_timing")
            .unwrap_or_else(|| panic!("exit report has no functions_timing: {report}")),
    );
    assert_live_limit(&exit_names);
    let _ = std::fs::remove_file(&report_path);
}

fn assert_live_limit(names: &[String]) {
    assert_eq!(
        names.len(),
        2,
        "HOTPATH_FUNCTIONS_LIMIT=2 must keep the two slowest functions, got {names:?}"
    );
    assert!(
        names.iter().any(|name| name.contains("slow_long")),
        "missing slow_long in {names:?}"
    );
    assert!(
        names.iter().any(|name| name.contains("slow_mid")),
        "missing slow_mid in {names:?}"
    );
    assert!(
        names
            .iter()
            .all(|name| !name.contains("slow_short") && !name.contains("slow_tiny")),
        "faster functions leaked past the limit: {names:?}"
    );
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("ephemeral local addr")
        .port()
}

fn initialize(port: &u16) -> String {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"functions-limit-live","version":"0"}}}"#;
    let mut last = String::new();
    for _ in 0..50 {
        match post(port, None, body) {
            Ok(response) if response.status == 200 => {
                let session = response.header("mcp-session-id").unwrap_or_else(|| {
                    panic!(
                        "initialize response missing mcp-session-id: status {} body {}",
                        response.status, response.body
                    )
                });
                let notified = post(
                    port,
                    Some(session.as_str()),
                    r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                )
                .unwrap_or_else(|error| panic!("initialized notification failed: {error}"));
                assert!(
                    notified.status == 202 || notified.status == 200,
                    "initialized notification status {}: {}",
                    notified.status,
                    notified.body
                );
                return session;
            }
            Ok(response) => {
                last = format!("status {} body {}", response.status, response.body);
            }
            Err(error) => last = error,
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("MCP server did not accept initialize on port {port}: {last}");
}

fn wait_for_functions(port: &u16, session: &str, ready: impl Fn(&[String]) -> bool) -> Vec<String> {
    let mut last = String::new();
    for _ in 0..40 {
        match post(
            port,
            Some(session),
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"functions_timing","arguments":{}}}"#,
        ) {
            Ok(response) if response.status == 200 => {
                let payload = rpc_result(&response.body);
                if payload
                    .pointer("/result/isError")
                    .and_then(|value| value.as_bool())
                    == Some(true)
                {
                    panic!("functions_timing returned an error: {}", response.body);
                }
                let text = payload
                    .pointer("/result/content/0/text")
                    .and_then(|value| value.as_str())
                    .unwrap_or_else(|| {
                        panic!(
                            "functions_timing response has no text content: {}",
                            response.body
                        )
                    });
                let list: serde_json::Value = serde_json::from_str(text).unwrap_or_else(|error| {
                    panic!("functions_timing text is not JSON: {error}\n{text}")
                });
                let names = names_from_list(&list);
                if ready(&names) {
                    return names;
                }
                last = format!("functions_timing not ready: {names:?}");
            }
            Ok(response) => {
                last = format!("status {} body {}", response.status, response.body);
            }
            Err(error) => last = error,
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("functions_timing did not return the expected measurements: {last}");
}

fn names_from_list(list: &serde_json::Value) -> Vec<String> {
    list.get("data")
        .and_then(|data| data.as_array())
        .unwrap_or_else(|| panic!("function list has no data array: {list}"))
        .iter()
        .map(|entry| {
            entry
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or_else(|| panic!("function entry has no name: {entry}"))
                .to_string()
        })
        .collect()
}

fn rpc_result(body: &str) -> serde_json::Value {
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed)
            .unwrap_or_else(|error| panic!("MCP body is not JSON: {error}\n{body}"));
    }
    let mut found = None;
    for line in trimmed.lines() {
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if !data.starts_with('{') {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(data)
            .unwrap_or_else(|error| panic!("MCP event is not JSON: {error}\n{data}"));
        if value.get("result").is_some() || value.get("error").is_some() {
            found = Some(value);
        }
    }
    found.unwrap_or_else(|| panic!("MCP response had no JSON-RPC payload:\n{body}"))
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<String> {
        self.headers.iter().find_map(|(key, value)| {
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_string())
        })
    }
}

fn post(port: &u16, session: Option<&str>, body: &str) -> Result<HttpResponse, String> {
    let address = SocketAddr::from(([127, 0, 0, 1], *port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(200))
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;

    let mut request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2024-11-05\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(session) = session {
        request.push_str(&format!("mcp-session-id: {session}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .map_err(|error| error.to_string())?;

    let mut raw = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => raw.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    parse_http(&raw)
}

fn parse_http(raw: &[u8]) -> Result<HttpResponse, String> {
    let text = String::from_utf8_lossy(raw);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return Err(format!("incomplete HTTP response: {text}"));
    };
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line: {status_line}"))?;
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect::<Vec<_>>();
    let chunked = headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
    });
    let body = if chunked {
        decode_chunks(body)?
    } else {
        body.to_string()
    };
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn decode_chunks(body: &str) -> Result<String, String> {
    let mut rest = body;
    let mut out = String::new();
    loop {
        let Some((size_line, after)) = rest.split_once("\r\n") else {
            return Err(format!("truncated chunk size: {body}"));
        };
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|error| format!("bad chunk size {size_line}: {error}"))?;
        if size == 0 {
            return Ok(out);
        }
        if after.len() < size {
            return Err(format!("truncated chunk of {size} bytes"));
        }
        out.push_str(&after[..size]);
        rest = after.get(size + 2..).unwrap_or("");
    }
}
