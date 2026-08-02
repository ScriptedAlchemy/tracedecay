use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use serde_json::{Value, json};
use tracedecay_sdk::client::{
    CancellationStatus, Client, ClientError, ConnectionMode, StreamOptions, StreamResume,
};
use tracedecay_sdk::operations::{
    TypedOperation, WorkCreate, WorkSnapshot, base_operation_capabilities,
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

fn event_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn work_snapshot_success() -> serde_json::Value {
    json!({
        "kind": "success",
        "value": {
            "binding_id": "binding.http.work.snapshot",
            "contract": {"schema_id": "schema.work.snapshot.result", "schema_revision": 1},
            "request_id": "request.sdk",
            "scope": {},
            "outcome": {"outcome": "evidence", "value": {
                "temporal": {}, "authority": {}, "evidence_authorities": [],
                "coverage": {}, "omissions": [], "scores": [], "contributions": [],
                "page": {"sort_contract_id": "sort.work", "sort_revision": 1,
                    "total": 1, "returned": 1, "cursor": null, "expires_at": null},
                "execution": {"started_at": 1, "ended_at": 2,
                    "effective_deadline": {"expires_at": 3}, "cancellation": null,
                    "budget": {"units_consumed": 1, "bytes_consumed": 1,
                        "elapsed_micros": 1}, "termination": "completed"},
                "payload": {"not": "a work snapshot"}
            }}
        }
    })
}

#[test]
fn local_and_remote_clients_preserve_auth_origin_without_query_paging() {
    let success = json_response(
        "200 OK",
        json!({
            "kind": "success",
            "value": {
                "binding_id": "binding.http.work.create",
                "contract": {"schema_id": "schema.work.create.result", "schema_revision": 1},
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
                    "payload": {
                        "accepted_proposal": null,
                        "authority": {
                            "actor_id": "actor.sdk",
                            "policy_digest": "sha256:policy",
                            "project_id": "project.sdk",
                            "repository_id": "repository.sdk",
                            "worktree_id": "worktree.sdk"
                        },
                        "dependencies": [],
                        "execution_admitted": false,
                        "history_len": 1,
                        "runtime_evidence": [],
                        "task_accepted": false,
                        "task_id": "task.sdk",
                        "title": "SDK task",
                        "version": 1
                    }
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

    let request = serde_json::from_value(json!({
        "command_id": "command.sdk",
        "occurred_at": 1,
        "task_id": "task.sdk",
        "title": "SDK task"
    }))
    .unwrap();
    let local_result = local.execute::<WorkCreate>(&request).unwrap();
    let remote_result = remote.execute::<WorkCreate>(&request).unwrap();

    assert_eq!(
        serde_json::to_value(local_result.result).unwrap()["task_id"],
        "task.sdk"
    );
    assert_eq!(
        serde_json::to_value(remote_result.result).unwrap()["task_id"],
        "task.sdk"
    );
    let requests = server.join().unwrap();
    assert!(requests[0].contains("authorization: Bearer sdk-token"));
    assert!(requests[0].contains(&format!("origin: {base_url}")));
    assert!(requests[0].contains("/application/work/create"));
    assert!(!requests[0].contains("/application/work/create?"));
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
        client.cancel_operation("request.operation").unwrap().status,
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

#[test]
fn typed_work_descriptors_close_the_public_operation_surface() {
    fn assert_typed_contract<Operation: TypedOperation>() {}

    assert_typed_contract::<WorkCreate>();
    assert_eq!(WorkCreate::OPERATION_ID, "operation.work.create");
    let capabilities = base_operation_capabilities().collect::<Vec<_>>();
    let exposed_count = tracedecay_sdk::api::HttpApplicationOperation::ALL
        .into_iter()
        .filter(|operation| operation.is_http_exposed())
        .count();
    assert_eq!(
        capabilities.len(),
        exposed_count,
        "every base HTTP application operation must surface as a capability"
    );
    assert!(capabilities.iter().all(|capability| {
        capability.operation.is_http_exposed()
            && !matches!(
                capability.operation,
                tracedecay_sdk::api::HttpApplicationOperation::GitPreview
                    | tracedecay_sdk::api::HttpApplicationOperation::GitApply
            )
            && !matches!(
                capability.route.as_str(),
                "/application/git/preview" | "/application/git/apply"
            )
    }));
    assert!(capabilities.iter().all(|capability| capability.disposition
        == tracedecay_sdk::operation::ExecutableUnavailableDispositionV1::SchemaUnavailable));
}

#[test]
fn typed_work_result_rejects_malformed_payloads() {
    let response = json_response("200 OK", work_snapshot_success());
    let (base_url, server) = serve(vec![response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();

    let request = serde_json::from_value::<<WorkSnapshot as TypedOperation>::Request>(
        json!({"page_size": 1}),
    )
    .unwrap();
    let error = client.execute::<WorkSnapshot>(&request).unwrap_err();

    assert!(matches!(error, ClientError::Protocol { .. }));
    let requests = server.join().unwrap();
    assert!(requests[0].contains("POST /projects/project.sdk/application/work/snapshot HTTP/1.1"));
    assert!(!requests[0].contains("/application/work/snapshot?"));
    assert!(requests[0].contains(r#""page_size":1"#));
}

#[test]
fn malformed_success_and_problem_fields_are_protocol_errors() {
    let mut missing_scope = work_snapshot_success();
    missing_scope["value"]
        .as_object_mut()
        .unwrap()
        .remove("scope");
    let mut bad_outcome = work_snapshot_success();
    bad_outcome["value"]["outcome"]["outcome"] = json!("future");
    let mut missing_receipt = work_snapshot_success();
    missing_receipt["value"]["outcome"]["value"]["execution"]
        .as_object_mut()
        .unwrap()
        .remove("budget");
    let mut bad_contract = work_snapshot_success();
    bad_contract["value"]["contract"]["schema_revision"] = json!("1");
    let mut missing_identity = work_snapshot_success();
    missing_identity["value"]["request_id"] = Value::Null;
    let mut problem = json!({
        "kind": "problem",
        "value": {
            "binding_id": "binding.http.work.snapshot",
            "contract": {"schema_id": "schema.application.problem", "schema_revision": 1},
            "request_id": "request.sdk",
            "problem": {
                "revision": 1, "kind": "unavailable", "code": "sdk.unavailable",
                "message": "unavailable", "diagnostic": null,
                "owning_layer": "application", "terminality": "terminal",
                "retryable": true, "retry": "after_delay",
                "retry_scope": "same_operation", "retry_after_millis": 1,
                "cancellation_stage": null, "request_id": "request.sdk",
                "trace_id": "trace.sdk", "details": [], "legal_actions": ["retry"],
                "coverage": null
            }
        }
    });
    problem["value"]["problem"]
        .as_object_mut()
        .unwrap()
        .remove("retry");
    let responses = [
        missing_scope,
        bad_outcome,
        missing_receipt,
        bad_contract,
        missing_identity,
    ]
    .into_iter()
    .map(|value| json_response("200 OK", value))
    .chain([json_response("503 Service Unavailable", problem)])
    .collect::<Vec<_>>();
    let (base_url, server) = serve(responses);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();
    let request = serde_json::from_value::<<WorkSnapshot as TypedOperation>::Request>(
        json!({"page_size": 1}),
    )
    .unwrap();
    for _ in 0..6 {
        assert!(matches!(
            client.execute::<WorkSnapshot>(&request),
            Err(ClientError::Protocol { .. })
        ));
    }
    assert_eq!(server.join().unwrap().len(), 6);
}

#[test]
fn malformed_sse_events_are_protocol_errors() {
    let open = concat!(
        "event: open\n",
        "data: {\"event\":\"open\",\"data\":{\"correlation_id\":\"request.operation\",",
        "\"frontier\":{\"next_sequence\":0,\"retained_from_sequence\":0,",
        "\"resume_token\":\"resume\"}}}\n\n"
    );
    let cases = [
        format!(
            "{open}event: future\nid: 0\ndata: {{\"event\":\"future\",\"data\":{{\"sequence\":0}}}}\n\n"
        ),
        open.replace("request.operation", "request.other"),
        format!(
            "{open}event: completed\nid: 0\ndata: {{\"event\":\"completed\",\"data\":{{\"sequence\":0,\"terminal\":{{\"termination\":\"completed\"}}}}}}\n\n"
        ),
        format!(
            "{open}event: completed\ndata: {{\"event\":\"completed\",\"data\":{{\"sequence\":0,\"terminal\":{{\"termination\":\"completed\",\"receipt\":{{}}}}}}}}\n\n"
        ),
    ];
    for body in cases {
        let (base_url, server) = serve(vec![event_response(&body)]);
        let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
            .build()
            .unwrap();
        let stream = client
            .stream_operation("request.operation", StreamOptions::default())
            .unwrap();
        assert!(matches!(
            stream.collect::<Result<Vec<_>, _>>(),
            Err(ClientError::Protocol { .. })
        ));
        server.join().unwrap();
    }
}
