use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tracedecay_sdk::client::{
    CancellationStatus, Client, ClientError, ConnectionMode, McpToolTransport,
    OperationRequestOptions, StreamOptions, StreamResume,
};
use tracedecay_sdk::operation::DeadlineBehavior;
use tracedecay_sdk::operations::{
    ApplicationGitStatus, CodeExactOccurrence, MultiRootExecute, MultiRootScopeSetCompareAndSwap,
    MultiRootScopeSetRead, OperationTransport, TypedOperation, WorkRetrieveEvidence,
    WorkflowListDefinitions, WorkflowRegisterDefinition,
};

#[derive(Debug, Default)]
struct RecordingMcpTransport {
    calls: Mutex<Vec<(String, Value)>>,
    response: Mutex<Option<Value>>,
}

impl McpToolTransport for RecordingMcpTransport {
    fn call_tool(&self, tool_name: &str, request: &Value) -> Result<Value, ClientError> {
        self.calls
            .lock()
            .expect("test MCP calls lock")
            .push((tool_name.to_owned(), request.clone()));
        Ok(self
            .response
            .lock()
            .expect("test MCP response lock")
            .clone()
            .unwrap_or(json!({})))
    }
}

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

fn serve_until_stopped(
    responses: Vec<String>,
) -> (String, mpsc::Sender<()>, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = mpsc::channel();
    let task = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in responses {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if !matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                            return requests;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("test HTTP listener failed: {error}"),
                }
            };
            requests.push(request(&mut stream));
            stream.write_all(response.as_bytes()).unwrap();
        }
        requests
    });
    (format!("http://{address}"), stop, task)
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

fn list_definitions_success() -> serde_json::Value {
    json!({
        "kind": "success",
        "value": {
            "binding_id": "binding.http.workflow.list_definitions",
            "contract": {"schema_id": "schema.workflow.list_definitions.result", "schema_revision": 1},
            "request_id": "request.sdk",
            "scope": {},
            "outcome": {"outcome": "evidence", "value": {
                "temporal": {}, "authority": {}, "evidence_authorities": [],
                "coverage": {}, "omissions": [], "scores": [], "contributions": [],
                "page": {"sort_contract_id": "sort.workflow", "sort_revision": 1,
                    "total": 1, "returned": 1, "cursor": null, "expires_at": null},
                "execution": {"started_at": 1, "ended_at": 2,
                    "effective_deadline": {"expires_at": 3}, "cancellation": null,
                    "budget": {"units_consumed": 1, "bytes_consumed": 1,
                        "elapsed_micros": 1}, "termination": "completed"},
                "payload": {"not": "a workflow definition list"}
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
                "binding_id": "binding.http.workflow.list_definitions",
                "contract": {"schema_id": "schema.workflow.list_definitions.result", "schema_revision": 1},
                "request_id": "request.sdk",
                "scope": {},
                "outcome": {"outcome": "evidence", "value": {
                    "temporal": {}, "authority": {}, "evidence_authorities": [],
                    "coverage": {}, "omissions": [], "scores": [], "contributions": [],
                    "page": {"sort_contract_id": "sort.workflow", "sort_revision": 1,
                        "total": 1, "returned": 1, "cursor": null, "expires_at": null},
                    "execution": {"started_at": 1, "ended_at": 2,
                        "effective_deadline": {"expires_at": 3}, "cancellation": null,
                        "budget": {"units_consumed": 1, "bytes_consumed": 1,
                            "elapsed_micros": 1}, "termination": "completed"},
                    "payload": [{
                        "definition_id": "workflow.sdk",
                        "definition_version": 1,
                        "pinned_catalog_digest": "sha256:catalog",
                        "pinned_configuration_digest": "sha256:configuration",
                        "pinned_policy_digest": "sha256:policy",
                        "project_id": "project.sdk",
                        "steps": []
                    }]
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

    let request = serde_json::from_value(json!({})).unwrap();
    let local_result = local
        .execute_with_options::<WorkflowListDefinitions>(
            &request,
            OperationRequestOptions {
                deadline_micros: Some(1_800_000_000_000_001),
            },
        )
        .unwrap();
    let remote_result = remote
        .execute_with_options::<WorkflowListDefinitions>(
            &request,
            OperationRequestOptions {
                deadline_micros: Some(1_800_000_000_000_002),
            },
        )
        .unwrap();

    assert_eq!(
        local_result.envelope["outcome"]["value"]["execution"]["effective_deadline"]["expires_at"],
        3
    );
    assert_eq!(
        serde_json::to_value(&local_result.result).unwrap()[0]["definition_id"],
        "workflow.sdk"
    );
    assert_eq!(
        serde_json::to_value(&remote_result.result).unwrap()[0]["definition_id"],
        "workflow.sdk"
    );
    let requests = server.join().unwrap();
    assert!(requests[0].contains("authorization: Bearer sdk-token"));
    assert!(requests[0].contains(&format!("origin: {base_url}")));
    assert!(requests[0].contains("x-tracedecay-deadline-micros: 1800000000000001"));
    assert!(requests[0].contains("/application/workflow/list-definitions"));
    assert!(!requests[0].contains("/application/workflow/list-definitions?"));
    assert!(requests[1].contains("origin: https://client.example"));
    assert!(requests[1].contains("x-tracedecay-deadline-micros: 1800000000000002"));
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
fn typed_workflow_descriptors_retain_canonical_contract_identity() {
    fn assert_typed_contract<Operation: TypedOperation>() {}

    assert_typed_contract::<WorkflowRegisterDefinition>();
    assert_eq!(
        WorkflowRegisterDefinition::OPERATION_ID,
        "operation.workflow.register_definition"
    );
    assert_eq!(
        WorkflowRegisterDefinition::TRANSPORT,
        tracedecay_sdk::operations::OperationTransport::Http {
            route: "/application/workflow/register-definition"
        }
    );
    assert_eq!(
        WorkflowRegisterDefinition::BINDING_ID,
        "binding.http.workflow.register_definition"
    );
    assert_eq!(WorkflowRegisterDefinition::MAXIMUM_DEADLINE_MILLIS, 30_000);
    assert_eq!(
        WorkflowRegisterDefinition::DEADLINE_BEHAVIOR,
        DeadlineBehavior::ReturnEffectReceipt
    );
}

#[test]
fn invalid_typed_deadline_is_rejected_before_transport() {
    let client = Client::builder(ConnectionMode::local(
        "http://127.0.0.1:1",
        "project.sdk",
        "sdk-token",
    ))
    .build()
    .unwrap();
    let request = serde_json::from_value(json!({})).unwrap();

    let error = client
        .execute_with_options::<WorkflowListDefinitions>(
            &request,
            OperationRequestOptions {
                deadline_micros: Some(0),
            },
        )
        .unwrap_err();

    assert!(matches!(error, ClientError::InvalidRequest(_)));
}

#[test]
fn typed_result_rejects_malformed_payloads() {
    let response = json_response("200 OK", list_definitions_success());
    let (base_url, server) = serve(vec![response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();

    let request =
        serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!({}))
            .unwrap();
    let error = client
        .execute::<WorkflowListDefinitions>(&request)
        .unwrap_err();

    assert!(matches!(error, ClientError::Protocol { .. }));
    let requests = server.join().unwrap();
    assert!(
        requests[0]
            .contains("POST /projects/project.sdk/application/workflow/list-definitions HTTP/1.1")
    );
    assert!(!requests[0].contains("/application/workflow/list-definitions?"));
}

#[test]
fn callable_code_uses_the_mounted_http_route_without_an_mcp_transport() {
    assert_eq!(
        CodeExactOccurrence::TRANSPORT,
        tracedecay_sdk::operations::OperationTransport::Http {
            route: "/application/code/code_exact_occurrence"
        },
        "canonical SDK generation must select the mounted HTTP executable"
    );
    let response = json_response("200 OK", json!({}));
    let (base_url, server) = serve(vec![response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();
    let request =
        serde_json::from_value::<<CodeExactOccurrence as TypedOperation>::Request>(json!({
            "literal": "sdk_executable_binding_registry",
            "kind": null,
            "scope": {
                "generation": "generation.sdk",
                "path_prefix": "crates/tracedecay-application"
            },
            "meta": {
                "temporal": {"kind": "current"},
                "page": {"page_size": 10, "cursor": null},
                "projection": "evidence",
                "order": "relevance"
            }
        }))
        .expect("canonical callable-code request");

    let error = client
        .execute::<CodeExactOccurrence>(&request)
        .expect_err("malformed fixture response must fail closed");

    assert!(matches!(error, ClientError::Protocol { .. }));
    let requests = server.join().unwrap();
    assert!(
        requests[0]
            .contains("POST /projects/project.sdk/application/code/code_exact_occurrence HTTP/1.1")
    );
    assert!(requests[0].contains("sdk_executable_binding_registry"));
}

#[test]
fn multi_root_operations_reach_their_exact_project_application_routes() {
    let response = json_response("200 OK", json!({}));
    let (base_url, stop_server, server) =
        serve_until_stopped(vec![response.clone(), response.clone(), response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();

    let read = serde_json::from_value::<<MultiRootScopeSetRead as TypedOperation>::Request>(
        json!({"scope_set_id": "scope-set.sdk"}),
    )
    .expect("canonical scope-set read request");
    let compare_and_swap = serde_json::from_value::<
        <MultiRootScopeSetCompareAndSwap as TypedOperation>::Request,
    >(json!({
        "scope_set_id": "scope-set.sdk",
        "expected_revision": null,
        "roots": [{"project_id": "project.sdk-root", "root": "/project/sdk-root"}]
    }))
    .expect("canonical scope-set compare-and-swap request");
    let execute = serde_json::from_value::<<MultiRootExecute as TypedOperation>::Request>(json!({
        "scope_set_id": "scope-set.sdk",
        "scope_set_revision": 1,
        "scope_set_digest": format!("sha256:{}", "a".repeat(64)),
        "operation": {"kind": "query", "request": {}},
        "page": 0,
        "continuation": null
    }))
    .expect("canonical multi-root execute request");

    for result in [
        client.execute::<MultiRootScopeSetRead>(&read).map(|_| ()),
        client
            .execute::<MultiRootScopeSetCompareAndSwap>(&compare_and_swap)
            .map(|_| ()),
        client.execute::<MultiRootExecute>(&execute).map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(ClientError::Protocol { .. })),
            "the malformed fixture response must fail only after HTTP admission"
        );
    }

    let _ = stop_server.send(());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3, "every typed operation must issue HTTP");
    assert!(
        requests[0].starts_with(
            "POST /projects/project.sdk/application/multi-root/scope-set/read HTTP/1.1"
        )
    );
    assert!(requests[1].starts_with(
        "POST /projects/project.sdk/application/multi-root/scope-set/compare-and-swap HTTP/1.1"
    ));
    assert!(
        requests[2]
            .starts_with("POST /projects/project.sdk/application/multi-root/execute HTTP/1.1")
    );
}

#[derive(Clone, Copy, Debug)]
struct NonSdkHttpOperation;

impl TypedOperation for NonSdkHttpOperation {
    type Request = <MultiRootScopeSetRead as TypedOperation>::Request;
    type Result = <MultiRootScopeSetRead as TypedOperation>::Result;

    const OPERATION_ID: &'static str = "operation.non_sdk.arbitrary";
    const TRANSPORT: OperationTransport = OperationTransport::Http {
        route: "/application/not-a-canonical-sdk-route",
    };
    const BINDING_ID: &'static str = "binding.http.non_sdk.arbitrary";
    const EFFECT: tracedecay_tool_catalog::EffectClass = MultiRootScopeSetRead::EFFECT;
    const IDEMPOTENCY: tracedecay_tool_catalog::IdempotencyContract =
        MultiRootScopeSetRead::IDEMPOTENCY;
    const CANCELLABLE: bool = MultiRootScopeSetRead::CANCELLABLE;
    const CANCELLATION_POINTS: &'static [tracedecay_tool_catalog::CancellationPoint] =
        MultiRootScopeSetRead::CANCELLATION_POINTS;
    const MAXIMUM_DEADLINE_MILLIS: u64 = MultiRootScopeSetRead::MAXIMUM_DEADLINE_MILLIS;
    const DEADLINE_BEHAVIOR: tracedecay_tool_catalog::DeadlineBehavior =
        MultiRootScopeSetRead::DEADLINE_BEHAVIOR;
    const RECONCILIATION: tracedecay_tool_catalog::ReconciliationContract =
        MultiRootScopeSetRead::RECONCILIATION;
    const RECEIPT: tracedecay_tool_catalog::ReceiptContract = MultiRootScopeSetRead::RECEIPT;
    const TERMINAL_STATES: &'static [tracedecay_tool_catalog::TerminalState] =
        MultiRootScopeSetRead::TERMINAL_STATES;
    const RESULT_SCHEMA_ID: &'static str = MultiRootScopeSetRead::RESULT_SCHEMA_ID;
    const RESULT_SCHEMA_REVISION: u32 = MultiRootScopeSetRead::RESULT_SCHEMA_REVISION;
}

#[test]
fn client_denies_an_arbitrary_route_absent_from_the_canonical_sdk_registry() {
    let client = Client::builder(ConnectionMode::local(
        "http://127.0.0.1:1",
        "project.sdk",
        "sdk-token",
    ))
    .build()
    .unwrap();
    let request = serde_json::from_value::<<NonSdkHttpOperation as TypedOperation>::Request>(
        json!({"scope_set_id": "scope-set.sdk"}),
    )
    .unwrap();

    let error = client.execute::<NonSdkHttpOperation>(&request).unwrap_err();

    assert!(matches!(error, ClientError::InvalidConfiguration(message)
            if message.contains("canonical SDK registry")));
}

#[test]
fn typed_result_rejects_a_terminal_outside_the_operation_contract() {
    let mut body = list_definitions_success();
    body["value"]["outcome"]["value"]["execution"]["termination"] = json!("effect_unknown");
    let response = json_response("200 OK", body);
    let (base_url, server) = serve(vec![response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();
    let request =
        serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!({}))
            .unwrap();

    let error = client
        .execute::<WorkflowListDefinitions>(&request)
        .unwrap_err();

    assert!(
        matches!(
            error,
            ClientError::Protocol { message, .. }
                if message.contains("outside the operation.workflow.list_definitions contract")
        ),
        "the generated operation terminal contract must reject illegal daemon outcomes"
    );
    server.join().unwrap();
}

#[test]
fn typed_result_rejects_a_cancellation_stage_outside_the_operation_contract() {
    let mut body = list_definitions_success();
    body["value"]["outcome"]["value"]["execution"]["termination"] = json!("cancelled");
    body["value"]["outcome"]["value"]["execution"]["cancellation"] = json!({
        "stage": "effect_in_flight",
        "observed_at": 2
    });
    let response = json_response("200 OK", body);
    let (base_url, server) = serve(vec![response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .unwrap();
    let request =
        serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!({}))
            .unwrap();

    let error = client
        .execute::<WorkflowListDefinitions>(&request)
        .unwrap_err();

    assert!(
        matches!(
            error,
            ClientError::Protocol { message, .. }
                if message.contains("cancellation stage effect_in_flight outside")
        ),
        "the generated operation cancellation contract must reject illegal daemon evidence"
    );
    server.join().unwrap();
}

#[test]
fn malformed_success_and_problem_fields_are_protocol_errors() {
    let mut missing_scope = list_definitions_success();
    missing_scope["value"]
        .as_object_mut()
        .unwrap()
        .remove("scope");
    let mut bad_outcome = list_definitions_success();
    bad_outcome["value"]["outcome"]["outcome"] = json!("future");
    let mut missing_receipt = list_definitions_success();
    missing_receipt["value"]["outcome"]["value"]["execution"]
        .as_object_mut()
        .unwrap()
        .remove("budget");
    let mut bad_contract = list_definitions_success();
    bad_contract["value"]["contract"]["schema_revision"] = json!("1");
    let mut missing_identity = list_definitions_success();
    missing_identity["value"]["request_id"] = Value::Null;
    let mut problem = json!({
        "kind": "problem",
        "value": {
            "binding_id": "binding.http.workflow.list_definitions",
            "contract": {"schema_id": "schema.application.problem", "schema_revision": 1},
            "request_id": "request.sdk",
            "problem": {
                "revision": 1, "kind": "unavailable", "code": "sdk.unavailable",
                "message": "unavailable", "diagnostic": null,
                "owning_layer": "application", "terminality": "terminal",
                "retryable": true, "retry": "after_delay",
                "retry_scope": "same_operation", "retry_after_millis": 1,
                "cancellation_stage": null, "unavailable_classification": "authority",
                "execution_failure_classification": null, "request_id": "request.sdk",
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
    let request =
        serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!({}))
            .unwrap();
    for _ in 0..6 {
        assert!(matches!(
            client.execute::<WorkflowListDefinitions>(&request),
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

#[test]
fn mounted_git_reads_use_the_generated_http_route() {
    let response = json_response("200 OK", json!({}));
    let (base_url, server) = serve(vec![response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .expect("client configuration");
    let request =
        serde_json::from_value::<<ApplicationGitStatus as TypedOperation>::Request>(json!({}))
            .unwrap();

    let error = client
        .execute::<ApplicationGitStatus>(&request)
        .expect_err("malformed HTTP result must fail closed");

    assert!(matches!(error, ClientError::Protocol { .. }));
    let requests = server.join().unwrap();
    assert!(requests[0].contains("POST /projects/project.sdk/application/git/status HTTP/1.1"));
}

#[test]
fn work_evidence_uses_the_generated_http_route_and_typed_request() {
    let response = json_response("200 OK", json!({}));
    let (base_url, server) = serve(vec![response]);
    let client = Client::builder(ConnectionMode::local(&base_url, "project.sdk", "sdk-token"))
        .build()
        .expect("client configuration");
    let request =
        serde_json::from_value::<<WorkRetrieveEvidence as TypedOperation>::Request>(json!({
            "selection": {"selection": "profile_owned_no_git"},
            "task_id": "task.sdk.evidence",
            "verified_version": {
                "graph_version": 4,
                "event_sequence": 4,
                "source_watermark": {},
                "recovered_graph_digest": concat!(
                    "sha256:",
                    "11111111111111111111111111111111",
                    "11111111111111111111111111111111"
                )
            },
            "temporal": {"kind": "forensic"},
            "page_size": 8,
            "expansion": {
                "kind": "task_session",
                "attempt": {
                    "task_id": "task.sdk.evidence",
                    "run_id": "run.sdk.evidence",
                    "attempt_id": "attempt.sdk.evidence"
                }
            },
            "continuation": null,
            "observed_at": 100
        }))
        .expect("typed Work evidence request");

    let error = client
        .execute::<WorkRetrieveEvidence>(&request)
        .expect_err("malformed HTTP result must fail closed");

    assert!(matches!(error, ClientError::Protocol { .. }));
    let requests = server.join().unwrap();
    assert!(
        requests[0]
            .contains("POST /projects/project.sdk/application/work/retrieve-evidence HTTP/1.1")
    );
    assert!(requests[0].contains("\"kind\":\"task_session\""));
    assert!(requests[0].contains("\"attempt_id\":\"attempt.sdk.evidence\""));
}

#[test]
fn http_operations_refuse_the_mcp_execution_path_with_a_typed_error() {
    let mcp = Arc::new(RecordingMcpTransport::default());
    let client = Client::builder(ConnectionMode::local(
        "http://127.0.0.1:43123",
        "project.sdk",
        "sdk-token",
    ))
    .mcp_transport(mcp.clone())
    .build()
    .expect("client configuration");

    let error = client
        .execute_mcp::<WorkflowListDefinitions>(
            &serde_json::from_value::<<WorkflowListDefinitions as TypedOperation>::Request>(json!(
                {}
            ))
            .unwrap(),
        )
        .expect_err("HTTP-bound operations must not reach the MCP bridge");
    assert!(matches!(
        error,
        ClientError::UnsupportedTransport { operation_id, .. }
            if operation_id == WorkflowListDefinitions::OPERATION_ID
    ));
    assert!(mcp.calls.lock().expect("test MCP calls lock").is_empty());
}
