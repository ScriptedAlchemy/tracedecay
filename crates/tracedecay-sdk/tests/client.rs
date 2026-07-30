use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use serde_json::json;
use tracedecay_sdk::api::HttpApplicationOperation;
use tracedecay_sdk::client::{
    CancellationStatus, Client, ConnectionMode, PageOptions, RequestOptions, StreamOptions,
    StreamResume,
};

fn request(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        request.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    let length = request
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")?
                .parse()
                .ok()
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    request.push_str(&String::from_utf8(body).unwrap());
    request
}

fn serve(responses: Vec<String>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(request(&mut stream));
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (format!("http://{address}"), task)
}

fn json_response(status: &str, value: serde_json::Value) -> String {
    let body = value.to_string();
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn local_and_remote_clients_preserve_auth_origin_and_paging() {
    let success = json_response(
        "200 OK",
        json!({
            "kind": "success",
            "value": {
                "binding_id": "binding.http.health_read.v1",
                "contract": {"schema_id": "schema.health.result", "schema_revision": 1},
                "request_id": "request.sdk",
                "scope": {},
                "outcome": {"outcome": "evidence", "value": {
                    "temporal": {}, "authority": {}, "evidence_authorities": [],
                    "coverage": {}, "omissions": [], "scores": [], "contributions": [],
                    "page": {"sort_contract_id": "sort.health", "sort_revision": 1,
                        "total": 1, "returned": 1, "cursor": null, "expires_at": null},
                    "execution": {"started_at": 1, "ended_at": 2,
                        "effective_deadline": {"expires_at": 3}, "cancellation": null,
                        "budget": {"units_consumed": 1, "bytes_consumed": 1,
                            "elapsed_micros": 1}, "termination": "completed"},
                    "payload": {"status": "ok"}
                }}
            }
        }),
    );
    let (base_url, server) = serve(vec![success.clone(), success]);
    let local = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();
    let remote = Client::builder(ConnectionMode::remote(
        &base_url,
        "project.sdk",
        "sdk-token",
    ))
    .origin("https://client.example")
    .build()
    .unwrap();
    let options = RequestOptions {
        page: Some(PageOptions {
            size: Some(25),
            cursor: Some("cursor.next".into()),
        }),
        ..RequestOptions::default()
    };

    let local_result = local
        .call(
            HttpApplicationOperation::HealthRead,
            &json!({}),
            options.clone(),
        )
        .unwrap();
    let remote_result = remote
        .call(HttpApplicationOperation::HealthRead, &json!({}), options)
        .unwrap();

    assert_eq!(local_result.payload(), Some(&json!({"status": "ok"})));
    assert_eq!(remote_result.payload(), Some(&json!({"status": "ok"})));
    let requests = server.join().unwrap();
    assert!(requests[0].contains("authorization: Bearer sdk-token"));
    assert!(requests[0].contains(&format!("origin: {base_url}")));
    assert!(requests[0].contains("page_size=25&cursor=cursor.next"));
    assert!(requests[1].contains("origin: https://client.example"));
}

#[test]
fn cancellation_and_stream_resume_use_lifecycle_routes() {
    let cancellation = json_response("202 Accepted", json!({"status": "requested"}));
    let event_body = concat!(
        "event: open\n",
        "data: {\"event\":\"open\",\"data\":{\"correlation_id\":\"request.operation\",",
        "\"frontier\":{\"next_sequence\":7,\"retained_from_sequence\":7,",
        "\"resume_token\":\"resume.next\"}}}\n\n",
        "event: completed\n",
        "id: 7\n",
        "data: {\"event\":\"completed\",\"data\":{\"sequence\":7,\"terminal\":{",
        "\"termination\":\"completed\",\"receipt\":{\"started_at\":1,\"ended_at\":2,",
        "\"effective_deadline\":{\"expires_at\":3},\"cancellation\":null,",
        "\"budget\":{\"units_consumed\":1,\"bytes_consumed\":1,\"elapsed_micros\":1},",
        "\"termination\":\"completed\"}}}}\n\n"
    );
    let event_response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{event_body}",
        event_body.len()
    );
    let (base_url, server) = serve(vec![cancellation, event_response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();

    assert_eq!(
        client
            .cancel_operation("request.operation", None)
            .unwrap()
            .status,
        CancellationStatus::Requested
    );
    let events = client
        .stream_operation(
            "request.operation",
            StreamOptions {
                resume: Some(StreamResume {
                    token: "resume.old".into(),
                    next_sequence: 7,
                }),
                max_reconnects: 0,
                ..StreamOptions::default()
            },
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(events.len(), 2);
    assert!(events.last().unwrap().terminal());
    let requests = server.join().unwrap();
    assert!(requests[0].contains("/application/operations/request.operation/cancel"));
    assert!(requests[1].contains(
        "/application/operations/request.operation/events?next_sequence=7&resume_token=resume.old"
    ));
}
