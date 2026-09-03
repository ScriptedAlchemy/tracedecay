use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{DaemonInvocationClient, DaemonLspSessionClient, InvocationCancellationPolicy};
use crate::client_identity::DaemonClientIdentity;
use crate::connection::DaemonConnection;
use crate::contract::{
    CanonicalQualificationBlob, DaemonInvocationOutcome, DaemonInvocationPayload,
    DaemonInvocationProblem, DaemonInvocationRequest, DaemonInvocationResponse,
    DaemonLspSessionAccess, WorkApplicationInvocationV1,
    parse_daemon_invocation_cancellation_request, parse_daemon_invocation_delivery_ack_request,
};
use crate::handshake::DaemonHandshake;
use crate::lsp_wire::{FrameSend, LspSessionAccess, LspSessionCredential, LspSessionId};
use tracedecay_application::{
    CancellationContext, CancellationSignal, Deadline, WorkGraphReadRequestV1,
    WorkProductSelectionScopeV1,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn deadline_after(duration: Duration) -> Deadline {
    let now = now_micros();
    let delta = i64::try_from(duration.as_micros()).unwrap_or(i64::MAX);
    Deadline::new(UtcMicros(now.0.saturating_add(delta))).expect("deadline")
}

fn invocation_request(request_id: &str, deadline: Deadline) -> DaemonInvocationRequest {
    let observed_at = now_micros();
    DaemonInvocationRequest::feedback(
        request_id,
        ApplicationSurfaceOperation::FeedbackList,
        "feedback.remote-controlled-settlement".to_owned(),
        observed_at,
        deadline,
        CancellationContext::active(format!("cancel.{request_id}")).expect("request cancellation"),
    )
}

fn work_invocation_request(request_id: &str) -> DaemonInvocationRequest {
    let observed_at = now_micros();
    DaemonInvocationRequest::work_application(
        request_id,
        WorkApplicationInvocationV1::Views(WorkGraphReadRequestV1::current(
            WorkProductSelectionScopeV1::ProfileOwnedNoGit,
            observed_at,
        )),
        observed_at,
        deadline_after(Duration::from_secs(5)),
        CancellationContext::active(format!("cancel.{request_id}")).expect("request cancellation"),
    )
}

fn invocation_client(
    endpoint: crate::transport::DaemonEndpoint,
    instance_id: &str,
) -> DaemonInvocationClient {
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    DaemonInvocationClient::for_connection_for_test(
        DaemonConnection::unauthenticated_for_test(endpoint),
        DaemonHandshake {
            project_path: Some(profile_root.clone()),
            scope_prefix: None,
            timings: false,
            allow_init: false,
            allow_initialize_root_routing: false,
            client_identity: DaemonClientIdentity {
                global_db_path: profile_root.join("global.db"),
                profile_root,
            },
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            client_instance_id: instance_id.to_owned(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: crate::handshake::MovedStoreAdoption::Never,
        },
    )
}

fn client_activity(client: &DaemonInvocationClient) -> (usize, usize) {
    (
        client
            .activity
            .queued
            .load(std::sync::atomic::Ordering::Acquire),
        client
            .activity
            .in_flight
            .load(std::sync::atomic::Ordering::Acquire),
    )
}

async fn write_unavailable_response(
    writer: &mut tokio::io::WriteHalf<crate::transport::BrokerStream>,
    request_id: &str,
) {
    let response =
        DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    writer
        .write_all(
            serde_json::to_string(&response)
                .expect("response JSON")
                .as_bytes(),
        )
        .await
        .expect("write invocation response");
    writer
        .write_all(b"\n")
        .await
        .expect("write response newline");
    writer.flush().await.expect("flush invocation response");
}

async fn measure_parallel_invocation_workload(
    delayed_response: Duration,
) -> (Duration, Duration, usize, usize) {
    const DELAYED_ID: &str = "request.pool.delayed";
    const SHORT_CALLS: usize = 32;
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind pooled invocation listener");
    let accepts = Arc::new(AtomicUsize::new(0));
    let server_accepts = Arc::clone(&accepts);
    let (delayed_admitted, admitted) = tokio::sync::oneshot::channel();
    let delayed_admitted = Arc::new(tokio::sync::Mutex::new(Some(delayed_admitted)));
    let server = tokio::spawn(async move {
        loop {
            let stream = listener.accept().await.expect("accept pooled invocation");
            server_accepts.fetch_add(1, Ordering::SeqCst);
            let delayed_admitted = Arc::clone(&delayed_admitted);
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut lines = BufReader::new(reader).lines();
                lines
                    .next_line()
                    .await
                    .expect("read invocation handshake")
                    .expect("invocation handshake");
                while let Some(line) = lines.next_line().await.expect("read invocation") {
                    let request: DaemonInvocationRequest =
                        serde_json::from_str(&line).expect("typed invocation");
                    if request.request_id == DELAYED_ID {
                        if let Some(sender) = delayed_admitted.lock().await.take() {
                            sender.send(()).expect("report delayed admission");
                        }
                        tokio::time::sleep(delayed_response).await;
                    }
                    write_unavailable_response(&mut writer, &request.request_id).await;
                }
            });
        }
    });
    let client = invocation_client(endpoint, "client.pool.parallel");
    let delayed_client = client.clone();
    let delayed = tokio::spawn(async move {
        delayed_client
            .invoke(invocation_request(
                DELAYED_ID,
                deadline_after(delayed_response + Duration::from_secs(3)),
            ))
            .await
    });
    admitted.await.expect("delayed request admitted");

    let monitor_done = Arc::new(AtomicBool::new(false));
    let max_queued = Arc::new(AtomicUsize::new(0));
    let monitor_client = client.clone();
    let monitor_done_task = Arc::clone(&monitor_done);
    let max_queued_task = Arc::clone(&max_queued);
    let monitor = tokio::spawn(async move {
        while !monitor_done_task.load(Ordering::Acquire) {
            max_queued_task.fetch_max(
                monitor_client.activity.queued.load(Ordering::Acquire),
                Ordering::AcqRel,
            );
            tokio::task::yield_now().await;
        }
    });
    let mut short_calls = tokio::task::JoinSet::new();
    for ordinal in 0..SHORT_CALLS {
        let short_client = client.clone();
        short_calls.spawn(async move {
            let started = std::time::Instant::now();
            short_client
                .invoke(invocation_request(
                    &format!("request.pool.short.{ordinal}"),
                    deadline_after(Duration::from_secs(3)),
                ))
                .await
                .expect("short invocation");
            started.elapsed()
        });
    }
    let mut latencies = Vec::with_capacity(SHORT_CALLS);
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(result) = short_calls.join_next().await {
            latencies.push(result.expect("short invocation task"));
        }
    })
    .await
    .expect("short invocations were blocked behind the delayed request");
    monitor_done.store(true, Ordering::Release);
    monitor.await.expect("queue monitor");
    delayed
        .await
        .expect("delayed invocation task")
        .expect("delayed invocation response");
    latencies.sort_unstable();
    let p50 = latencies[SHORT_CALLS / 2];
    let p95 = latencies[(SHORT_CALLS * 95 / 100).min(SHORT_CALLS - 1)];
    let max_queue_depth = max_queued.load(Ordering::Acquire);
    let accepted_connections = accepts.load(Ordering::SeqCst);
    server.abort();
    (p50, p95, max_queue_depth, accepted_connections)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delayed_invocation_does_not_block_short_calls_and_pool_stays_bounded() {
    let (p50, p95, max_queue_depth, accepted_connections) =
        measure_parallel_invocation_workload(Duration::from_secs(2)).await;
    println!("short-call p50={p50:?} p95={p95:?} max_queue_depth={max_queue_depth}");
    assert!(
        accepted_connections <= 8,
        "the client pool exceeded its eight-connection admission bound"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "30-second connection-pool latency measurement"]
async fn thirty_second_call_does_not_block_short_call_latency() {
    let (p50, p95, max_queue_depth, accepted_connections) =
        measure_parallel_invocation_workload(Duration::from_secs(30)).await;
    println!(
        "30-second workload short-call p50={p50:?} p95={p95:?} \
         max_queue_depth={max_queue_depth} accepted_connections={accepted_connections}"
    );
    assert!(accepted_connections <= 8);
}

#[tokio::test(start_paused = true)]
async fn delayed_response_opens_no_periodic_probe_connections() {
    const REQUEST_ID: &str = "request.no-probe-connections";
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind delayed response listener");
    let accepts = Arc::new(AtomicUsize::new(0));
    let server_accepts = Arc::clone(&accepts);
    let server = tokio::spawn(async move {
        loop {
            let stream = listener.accept().await.expect("accept delayed invocation");
            server_accepts.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut lines = BufReader::new(reader).lines();
                let Some(_handshake) = lines.next_line().await.expect("read handshake") else {
                    return;
                };
                let Some(line) = lines.next_line().await.expect("read invocation") else {
                    return;
                };
                let request: DaemonInvocationRequest =
                    serde_json::from_str(&line).expect("typed invocation");
                tokio::time::sleep(Duration::from_secs(12)).await;
                write_unavailable_response(&mut writer, &request.request_id).await;
            });
        }
    });
    let client = invocation_client(endpoint, "client.no-probe-connections");

    client
        .invoke(invocation_request(
            REQUEST_ID,
            deadline_after(Duration::from_secs(20)),
        ))
        .await
        .expect("delayed invocation response");

    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "response liveness polling must not create probe connections"
    );
    server.abort();
}

#[tokio::test]
async fn work_delivery_ack_uses_the_response_connection() {
    const REQUEST_ID: &str = "request.work.same-connection";
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind Work delivery listener");
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept Work invocation");
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read Work handshake")
            .expect("Work handshake");
        let request_line = lines
            .next_line()
            .await
            .expect("read Work invocation")
            .expect("Work invocation");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed Work invocation");
        assert_eq!(request.request_id, REQUEST_ID);
        write_unavailable_response(&mut writer, REQUEST_ID).await;

        let ack_line = lines
            .next_line()
            .await
            .expect("read Work delivery ACK")
            .expect("Work delivery ACK");
        let ack = parse_daemon_invocation_delivery_ack_request(&ack_line)
            .expect("typed Work delivery ACK");
        assert_eq!(ack.target_request_id(), REQUEST_ID);
        let response = crate::contract::DaemonInvocationDeliveryAckResponse::accepted(REQUEST_ID);
        writer
            .write_all(
                serde_json::to_string(&response)
                    .expect("ACK response JSON")
                    .as_bytes(),
            )
            .await
            .expect("write ACK response");
        writer.write_all(b"\n").await.expect("ACK response newline");
        writer.flush().await.expect("flush ACK response");
    });
    let client = invocation_client(endpoint, "client.work.same-connection");

    let result = client
        .invoke_with_delivery(work_invocation_request(REQUEST_ID))
        .await
        .expect("Work invocation response");
    let (_response, delivery) = result.into_parts();
    delivery
        .expect("Work delivery authority")
        .acknowledge(
            tracedecay_domain::DeliverySettlementOutcomeV1::Delivered,
            None,
        )
        .await
        .expect("Work delivery ACK");
    server.await.expect("server task");
}

#[tokio::test]
async fn dropping_unacknowledged_work_delivery_closes_its_connection() {
    const REQUEST_ID: &str = "request.work.dropped-handle";
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind dropped Work delivery listener");
    let (closed, connection_closed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept Work invocation");
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read Work handshake")
            .expect("Work handshake");
        lines
            .next_line()
            .await
            .expect("read Work invocation")
            .expect("Work invocation");
        write_unavailable_response(&mut writer, REQUEST_ID).await;
        assert!(
            lines
                .next_line()
                .await
                .expect("read Work connection close")
                .is_none(),
            "an unacknowledged delivery handle must close instead of returning its connection"
        );
        closed.send(()).expect("report Work connection close");
    });
    let client = invocation_client(endpoint, "client.work.dropped-handle");

    let result = client
        .invoke_with_delivery(work_invocation_request(REQUEST_ID))
        .await
        .expect("Work invocation response");
    let (_response, delivery) = result.into_parts();
    drop(delivery.expect("Work delivery authority"));

    tokio::time::timeout(Duration::from_secs(1), connection_closed)
        .await
        .expect("unacknowledged Work connection stayed open")
        .expect("Work connection close signal");
    server.await.expect("server task");
}

#[tokio::test]
async fn lsp_session_pins_one_connection_through_detach() {
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind LSP session listener");
    let accepts = Arc::new(AtomicUsize::new(0));
    let server_accepts = Arc::clone(&accepts);
    let server = tokio::spawn(async move {
        let stream = listener.accept().await.expect("accept LSP session");
        server_accepts.fetch_add(1, Ordering::SeqCst);
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read LSP handshake")
            .expect("LSP handshake");
        loop {
            let line = lines
                .next_line()
                .await
                .expect("read LSP invocation")
                .expect("LSP invocation");
            let request: DaemonInvocationRequest =
                serde_json::from_str(&line).expect("typed LSP invocation");
            let request_id = request.request_id.clone();
            let (response, detached) = match request.payload {
                DaemonInvocationPayload::LspOpen { .. } => {
                    let access = LspSessionAccess::new(
                        LspSessionId::new("session.pinned").expect("session id"),
                        LspSessionCredential::new(vec![7; 32]).expect("session credential"),
                    );
                    (
                        DaemonInvocationResponse::lsp_opened(
                            request_id,
                            DaemonLspSessionAccess::from_access(&access),
                            60_000,
                            None,
                            None,
                        ),
                        false,
                    )
                }
                DaemonInvocationPayload::LspFrame { .. } => (
                    DaemonInvocationResponse::with_outcome(
                        request_id,
                        DaemonInvocationOutcome::LspFrameAccepted {
                            backpressured: false,
                            closed: false,
                        },
                    ),
                    false,
                ),
                DaemonInvocationPayload::LspDetach { .. } => (
                    DaemonInvocationResponse::with_outcome(
                        request_id,
                        DaemonInvocationOutcome::LspDetached,
                    ),
                    true,
                ),
                payload => panic!("unexpected LSP payload: {payload:?}"),
            };
            writer
                .write_all(
                    serde_json::to_string(&response)
                        .expect("LSP response JSON")
                        .as_bytes(),
                )
                .await
                .expect("write LSP response");
            writer.write_all(b"\n").await.expect("LSP response newline");
            writer.flush().await.expect("flush LSP response");
            if detached {
                break;
            }
        }
    });
    let client = invocation_client(endpoint, "client.lsp.pinned");
    let journey = async {
        let mut session = DaemonLspSessionClient::open(
            client,
            "3.17",
            None,
            Vec::new(),
            deadline_after(Duration::from_secs(2)),
            CancellationSignal::active("cancel.lsp.open").expect("open cancellation"),
        )
        .await
        .expect("open LSP session");
        assert_eq!(
            session
                .try_send_client_frame(
                    r#"{"jsonrpc":"2.0","method":"initialized"}"#,
                    deadline_after(Duration::from_secs(2)),
                    CancellationSignal::active("cancel.lsp.frame").expect("frame cancellation"),
                )
                .await
                .expect("send LSP frame"),
            FrameSend::Sent
        );
        session
            .detach(
                deadline_after(Duration::from_secs(2)),
                CancellationSignal::active("cancel.lsp.detach").expect("detach cancellation"),
            )
            .await
            .expect("detach LSP session");
    };
    tokio::time::timeout(Duration::from_secs(1), journey)
        .await
        .expect("LSP operations opened another connection");
    server.await.expect("server task");
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_hundred_invocations_use_at_most_eight_connections_without_leaks() {
    const INVOCATIONS: usize = 200;
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind bounded pool listener");
    let accepts = Arc::new(AtomicUsize::new(0));
    let server_accepts = Arc::clone(&accepts);
    let (stop, mut stopping) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stopping => break,
                accepted = listener.accept() => {
                    let stream = accepted.expect("accept bounded pool connection");
                    server_accepts.fetch_add(1, Ordering::SeqCst);
                    handlers.spawn(async move {
                        let (reader, mut writer) = stream.into_split();
                        let mut lines = BufReader::new(reader).lines();
                        lines
                            .next_line()
                            .await
                            .expect("read bounded pool handshake")
                            .expect("bounded pool handshake");
                        while let Some(line) = lines.next_line().await.expect("read invocation") {
                            let request: DaemonInvocationRequest =
                                serde_json::from_str(&line).expect("typed invocation");
                            write_unavailable_response(&mut writer, &request.request_id).await;
                        }
                    });
                }
            }
        }
        handlers.abort_all();
        while handlers.join_next().await.is_some() {}
    });
    let client = invocation_client(endpoint, "client.pool.two-hundred");
    let mut invocations = tokio::task::JoinSet::new();
    for ordinal in 0..INVOCATIONS {
        let invocation_client = client.clone();
        invocations.spawn(async move {
            invocation_client
                .invoke(invocation_request(
                    &format!("request.pool.bounded.{ordinal}"),
                    deadline_after(Duration::from_secs(5)),
                ))
                .await
        });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(result) = invocations.join_next().await {
            result
                .expect("bounded invocation task")
                .expect("bounded invocation response");
        }
    })
    .await
    .expect("bounded invocation workload timed out");

    assert!(
        accepts.load(Ordering::SeqCst) <= 8,
        "accepted more than the pool capacity"
    );
    assert_eq!(client_activity(&client), (0, 0));
    assert_eq!(client.pool.permits.available_permits(), 8);
    stop.send(()).expect("stop bounded pool server");
    server.await.expect("bounded pool server task");
}

#[tokio::test]
async fn transport_failure_purges_other_idle_connections_before_reconnect() {
    const FIRST_WARM_ID: &str = "request.pool.restart.warm-first";
    const SECOND_WARM_ID: &str = "request.pool.restart.warm-second";
    const FAILED_ID: &str = "request.pool.restart.failed";
    const RECOVERED_ID: &str = "request.pool.restart.recovered";
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind restart pool listener");
    let accepts = Arc::new(AtomicUsize::new(0));
    let server_accepts = Arc::clone(&accepts);
    let (close_warm_connections, close_warm) = tokio::sync::oneshot::channel();
    let (warm_connections_closed, warm_closed) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let first_stream = listener
            .accept()
            .await
            .expect("accept first warm connection");
        server_accepts.fetch_add(1, Ordering::SeqCst);
        let (first_reader, mut first_writer) = first_stream.into_split();
        let mut first_lines = BufReader::new(first_reader).lines();
        first_lines
            .next_line()
            .await
            .expect("read first warm handshake")
            .expect("first warm handshake");
        let first_request = first_lines
            .next_line()
            .await
            .expect("read first warm invocation")
            .expect("first warm invocation");

        let second_stream = listener
            .accept()
            .await
            .expect("accept second warm connection");
        server_accepts.fetch_add(1, Ordering::SeqCst);
        let (second_reader, mut second_writer) = second_stream.into_split();
        let mut second_lines = BufReader::new(second_reader).lines();
        second_lines
            .next_line()
            .await
            .expect("read second warm handshake")
            .expect("second warm handshake");
        let second_request = second_lines
            .next_line()
            .await
            .expect("read second warm invocation")
            .expect("second warm invocation");

        let first_request: DaemonInvocationRequest =
            serde_json::from_str(&first_request).expect("typed first warm invocation");
        let second_request: DaemonInvocationRequest =
            serde_json::from_str(&second_request).expect("typed second warm invocation");
        write_unavailable_response(&mut first_writer, &first_request.request_id).await;
        write_unavailable_response(&mut second_writer, &second_request.request_id).await;

        close_warm.await.expect("close warm pool connections");
        drop(first_lines);
        drop(first_writer);
        drop(second_lines);
        drop(second_writer);
        warm_connections_closed
            .send(())
            .expect("report warm connections closed");

        let recovered_stream = listener
            .accept()
            .await
            .expect("accept recovered connection");
        server_accepts.fetch_add(1, Ordering::SeqCst);
        let (recovered_reader, mut recovered_writer) = recovered_stream.into_split();
        let mut recovered_lines = BufReader::new(recovered_reader).lines();
        recovered_lines
            .next_line()
            .await
            .expect("read recovered handshake")
            .expect("recovered handshake");
        let recovered_request = recovered_lines
            .next_line()
            .await
            .expect("read recovered invocation")
            .expect("recovered invocation");
        let recovered_request: DaemonInvocationRequest =
            serde_json::from_str(&recovered_request).expect("typed recovered invocation");
        assert_eq!(recovered_request.request_id, RECOVERED_ID);
        write_unavailable_response(&mut recovered_writer, RECOVERED_ID).await;
    });
    let client = invocation_client(endpoint, "client.pool.restart");

    let (first_warm, second_warm) = tokio::join!(
        client.invoke(invocation_request(
            FIRST_WARM_ID,
            deadline_after(Duration::from_secs(5)),
        )),
        client.invoke(invocation_request(
            SECOND_WARM_ID,
            deadline_after(Duration::from_secs(5)),
        )),
    );
    first_warm.expect("first warm invocation");
    second_warm.expect("second warm invocation");
    assert_eq!(
        client
            .pool
            .idle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        2,
        "concurrent warmup must leave two idle pooled connections"
    );

    close_warm_connections
        .send(())
        .expect("request warm connection close");
    warm_closed.await.expect("warm connections closed");

    client
        .invoke(invocation_request(
            FAILED_ID,
            deadline_after(Duration::from_secs(5)),
        ))
        .await
        .expect_err("first invocation after restart must observe transport failure");
    client
        .invoke(invocation_request(
            RECOVERED_ID,
            deadline_after(Duration::from_secs(5)),
        ))
        .await
        .expect("second invocation after restart must reconnect");

    server.await.expect("restart pool server task");
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        3,
        "recovery must accept exactly one fresh connection"
    );
}

#[tokio::test]
async fn concurrent_invocations_report_parallel_activity() {
    const FIRST_ID: &str = "request.concurrent-first";
    const SECOND_ID: &str = "request.concurrent-second";
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind invocation listener");
    let (first_read, first_admitted) = tokio::sync::oneshot::channel();
    let (release_first, first_release) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let stream = listener
            .accept()
            .await
            .expect("accept invocation connection");
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();
        lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let first_line = lines
            .next_line()
            .await
            .expect("read first invocation")
            .expect("first invocation");
        let first: DaemonInvocationRequest =
            serde_json::from_str(&first_line).expect("typed first invocation");
        assert_eq!(first.request_id, FIRST_ID);
        first_read.send(()).expect("report first admission");
        first_release.await.expect("release first response");
        write_unavailable_response(&mut writer, FIRST_ID).await;

        let second_stream = listener
            .accept()
            .await
            .expect("accept second invocation connection");
        let (second_reader, mut second_writer) = second_stream.into_split();
        let mut second_lines = BufReader::new(second_reader).lines();
        second_lines
            .next_line()
            .await
            .expect("read second invocation handshake")
            .expect("second invocation handshake");
        let second_line = second_lines
            .next_line()
            .await
            .expect("read second invocation")
            .expect("second invocation");
        let second: DaemonInvocationRequest =
            serde_json::from_str(&second_line).expect("typed second invocation");
        assert_eq!(second.request_id, SECOND_ID);
        write_unavailable_response(&mut second_writer, SECOND_ID).await;
    });
    let client = invocation_client(endpoint, "client.concurrent-activity");
    let first_client = client.clone();
    let first = tokio::spawn(async move {
        first_client
            .invoke(invocation_request(
                FIRST_ID,
                deadline_after(Duration::from_secs(2)),
            ))
            .await
    });
    first_admitted.await.expect("first request admitted");
    assert_eq!(client_activity(&client), (0, 1));

    let second_client = client.clone();
    let second = tokio::spawn(async move {
        second_client
            .invoke(invocation_request(
                SECOND_ID,
                deadline_after(Duration::from_secs(2)),
            ))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while client_activity(&client) != (0, 2) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second invocation reported in flight");

    release_first.send(()).expect("release first response");
    first
        .await
        .expect("first invocation task")
        .expect("first invocation response");
    second
        .await
        .expect("second invocation task")
        .expect("second invocation response");
    server.await.expect("server task");
    assert_eq!(client_activity(&client), (0, 0));
}

async fn controlled_client(
    request_id: &'static str,
) -> (
    DaemonInvocationClient,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind invocation listener");
    let (request_admitted, admitted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let invocation_stream = listener.accept().await.expect("accept invocation");
        let (invocation_reader, mut invocation_writer) = invocation_stream.into_split();
        let mut invocation_lines = BufReader::new(invocation_reader).lines();
        invocation_lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let request_line = invocation_lines
            .next_line()
            .await
            .expect("read invocation request")
            .expect("invocation request");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed invocation request");
        assert_eq!(request.request_id, request_id);
        let _ = request_admitted.send(());

        let control_stream = listener.accept().await.expect("accept cancellation");
        let (control_reader, _control_writer) = control_stream.into_split();
        let mut control_lines = BufReader::new(control_reader).lines();
        control_lines
            .next_line()
            .await
            .expect("read cancellation handshake")
            .expect("cancellation handshake");
        let cancellation_line = control_lines
            .next_line()
            .await
            .expect("read cancellation request")
            .expect("cancellation request");
        let cancellation = parse_daemon_invocation_cancellation_request(&cancellation_line)
            .expect("typed invocation cancellation");
        assert_eq!(cancellation.target_request_id(), request_id);

        let response =
            DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::ResetRequired);
        invocation_writer
            .write_all(
                serde_json::to_string(&response)
                    .expect("response JSON")
                    .as_bytes(),
            )
            .await
            .expect("write authoritative settlement");
        invocation_writer
            .write_all(b"\n")
            .await
            .expect("write response newline");
        invocation_writer
            .flush()
            .await
            .expect("flush authoritative settlement");
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let handshake = DaemonHandshake {
        project_path: Some(profile_root.clone()),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root,
        },
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: format!("client.{request_id}"),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::handshake::MovedStoreAdoption::Never,
    };
    (
        DaemonInvocationClient::for_connection_for_test(
            DaemonConnection::unauthenticated_for_test(endpoint),
            handshake,
        ),
        admitted,
        server,
    )
}

async fn reset_then_reconnect_client(
    first_request_id: &'static str,
    second_request_id: &'static str,
) -> (
    DaemonInvocationClient,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind invocation listener");
    let (first_admitted, admitted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let first_stream = listener.accept().await.expect("accept first invocation");
        let (first_reader, _first_writer) = first_stream.into_split();
        let mut first_lines = BufReader::new(first_reader).lines();
        first_lines
            .next_line()
            .await
            .expect("read first handshake")
            .expect("first handshake");
        let first_line = first_lines
            .next_line()
            .await
            .expect("read first invocation")
            .expect("first invocation");
        let first: DaemonInvocationRequest =
            serde_json::from_str(&first_line).expect("typed first invocation");
        assert_eq!(first.request_id, first_request_id);
        let _ = first_admitted.send(());

        let control_stream = listener.accept().await.expect("accept cancellation");
        let (control_reader, _control_writer) = control_stream.into_split();
        let mut control_lines = BufReader::new(control_reader).lines();
        control_lines
            .next_line()
            .await
            .expect("read cancellation handshake")
            .expect("cancellation handshake");
        control_lines
            .next_line()
            .await
            .expect("read cancellation request")
            .expect("cancellation request");

        // The response-grace read polls liveness with handshake-less probe
        // connections; skip them like the real daemon's accept loop does.
        let (mut second_lines, mut second_writer) = loop {
            let second_stream = listener.accept().await.expect("accept second invocation");
            let (second_reader, second_writer) = second_stream.into_split();
            let mut second_lines = BufReader::new(second_reader).lines();
            if let Ok(Some(_handshake)) = second_lines.next_line().await {
                break (second_lines, second_writer);
            }
        };
        let second_line = second_lines
            .next_line()
            .await
            .expect("read second invocation")
            .expect("second invocation");
        let second: DaemonInvocationRequest =
            serde_json::from_str(&second_line).expect("typed second invocation");
        assert_eq!(second.request_id, second_request_id);
        let response = DaemonInvocationResponse::problem(
            second_request_id,
            DaemonInvocationProblem::Unavailable,
        );
        second_writer
            .write_all(
                serde_json::to_string(&response)
                    .expect("response JSON")
                    .as_bytes(),
            )
            .await
            .expect("write second response");
        second_writer
            .write_all(b"\n")
            .await
            .expect("write second response newline");
        second_writer.flush().await.expect("flush second response");
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let handshake = DaemonHandshake {
        project_path: Some(profile_root.clone()),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root,
        },
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: "client.remote-effect-reconnect".to_owned(),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::handshake::MovedStoreAdoption::Never,
    };
    (
        DaemonInvocationClient::for_connection_for_test(
            DaemonConnection::unauthenticated_for_test(endpoint),
            handshake,
        ),
        admitted,
        server,
    )
}

#[derive(Clone, Copy)]
enum UnsettledControl {
    CancellationDelivered,
    CancellationConnectionRejected,
}

async fn unsettled_client(
    request_id: &'static str,
    control: UnsettledControl,
) -> (
    DaemonInvocationClient,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind invocation listener");
    let (request_admitted, admitted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let invocation_stream = listener.accept().await.expect("accept invocation");
        let (invocation_reader, _invocation_writer) = invocation_stream.into_split();
        let mut invocation_lines = BufReader::new(invocation_reader).lines();
        invocation_lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let request_line = invocation_lines
            .next_line()
            .await
            .expect("read invocation request")
            .expect("invocation request");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed invocation request");
        assert_eq!(request.request_id, request_id);

        match control {
            UnsettledControl::CancellationDelivered => {
                let _ = request_admitted.send(());
                let control_stream = listener.accept().await.expect("accept cancellation");
                let (control_reader, _control_writer) = control_stream.into_split();
                let mut control_lines = BufReader::new(control_reader).lines();
                control_lines
                    .next_line()
                    .await
                    .expect("read cancellation handshake")
                    .expect("cancellation handshake");
                let cancellation_line = control_lines
                    .next_line()
                    .await
                    .expect("read cancellation request")
                    .expect("cancellation request");
                let cancellation = parse_daemon_invocation_cancellation_request(&cancellation_line)
                    .expect("typed invocation cancellation");
                assert_eq!(cancellation.target_request_id(), request_id);
            }
            UnsettledControl::CancellationConnectionRejected => {
                drop(listener);
                let _ = request_admitted.send(());
            }
        }
        std::future::pending::<()>().await;
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let handshake = DaemonHandshake {
        project_path: Some(profile_root.clone()),
        scope_prefix: None,
        timings: false,
        allow_init: false,
        allow_initialize_root_routing: false,
        client_identity: DaemonClientIdentity {
            global_db_path: profile_root.join("global.db"),
            profile_root,
        },
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        client_instance_id: format!("client.{request_id}"),
        tool_list_changed_capable: false,
        catalog_version: String::new(),
        moved_store_adoption: crate::handshake::MovedStoreAdoption::Never,
    };
    (
        DaemonInvocationClient::for_connection_for_test(
            DaemonConnection::unauthenticated_for_test(endpoint),
            handshake,
        ),
        admitted,
        server,
    )
}

fn assert_authoritative_settlement(response: DaemonInvocationResponse) {
    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::ResetRequired
        }
    ));
}

#[tokio::test]
async fn remote_effect_cancellation_requests_daemon_cancel_and_awaits_settlement() {
    const REQUEST_ID: &str = "request.remote-effect-cancel";
    let (client, admitted, server) = controlled_client(REQUEST_ID).await;
    let cancellation =
        CancellationSignal::active("cancel.remote-effect-cancel").expect("cancellation signal");
    let cancel_after_admission = cancellation.clone();
    let cancel = tokio::spawn(async move {
        admitted.await.expect("request admission");
        assert!(cancel_after_admission.cancel(now_micros()));
    });
    let deadline = deadline_after(Duration::from_secs(1));

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client.invoke_controlled(
            invocation_request(REQUEST_ID, deadline.clone()),
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative settlement is joined")
    .expect("daemon settlement is returned");

    assert_authoritative_settlement(response);
    cancel.await.expect("cancellation task");
    server.await.expect("server task");
}

#[tokio::test]
async fn remote_effect_deadline_requests_daemon_cancel_and_awaits_settlement() {
    const REQUEST_ID: &str = "request.remote-effect-deadline";
    let (client, admitted, server) = controlled_client(REQUEST_ID).await;
    drop(admitted);
    let deadline = deadline_after(Duration::from_millis(250));

    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client.invoke_controlled(
            invocation_request(REQUEST_ID, deadline.clone()),
            deadline,
            CancellationSignal::active("cancel.remote-effect-deadline")
                .expect("cancellation signal"),
            InvocationCancellationPolicy::AuthoritativeEffect,
        ),
    )
    .await
    .expect("authoritative settlement is joined")
    .expect("daemon settlement is returned");

    assert_authoritative_settlement(response);
    server.await.expect("server task");
}

#[tokio::test(start_paused = true)]
async fn remote_effect_without_authoritative_settlement_returns_reset_required() {
    const REQUEST_ID: &str = "request.remote-effect-no-settlement";
    let (client, admitted, server) =
        unsettled_client(REQUEST_ID, UnsettledControl::CancellationDelivered).await;
    let cancellation =
        CancellationSignal::active("cancel.remote-effect-no-settlement").expect("cancellation");
    let deadline = deadline_after(Duration::from_secs(10));
    let call_cancellation = cancellation.clone();
    let call = tokio::spawn(async move {
        client
            .invoke_controlled(
                invocation_request(REQUEST_ID, deadline.clone()),
                deadline,
                call_cancellation,
                InvocationCancellationPolicy::AuthoritativeEffect,
            )
            .await
    });
    admitted.await.expect("request admission");
    assert!(cancellation.cancel(now_micros()));
    tokio::time::advance(crate::connection::DAEMON_TOOL_RESPONSE_GRACE + Duration::from_secs(1))
        .await;
    let response = call
        .await
        .expect("authoritative invocation task")
        .expect("indeterminate settlement is typed");

    assert_authoritative_settlement(response);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn remote_effect_cancel_delivery_failure_returns_reset_required() {
    const REQUEST_ID: &str = "request.remote-effect-cancel-delivery-failure";
    let (client, admitted, server) =
        unsettled_client(REQUEST_ID, UnsettledControl::CancellationConnectionRejected).await;
    let cancellation =
        CancellationSignal::active("cancel.remote-effect-delivery-failure").expect("cancellation");
    let deadline = deadline_after(Duration::from_secs(10));
    let call_cancellation = cancellation.clone();
    let call = tokio::spawn(async move {
        client
            .invoke_controlled(
                invocation_request(REQUEST_ID, deadline.clone()),
                deadline,
                call_cancellation,
                InvocationCancellationPolicy::AuthoritativeEffect,
            )
            .await
    });
    admitted.await.expect("request admission");
    assert!(cancellation.cancel(now_micros()));
    tokio::time::advance(crate::connection::DAEMON_TOOL_RESPONSE_GRACE + Duration::from_secs(1))
        .await;
    let response = call
        .await
        .expect("authoritative invocation task")
        .expect("indeterminate settlement is typed");

    assert_authoritative_settlement(response);
    server.abort();
}

#[tokio::test(start_paused = true)]
async fn indeterminate_effect_discards_connection_before_next_invocation() {
    const FIRST_ID: &str = "request.remote-effect-reset-state";
    const SECOND_ID: &str = "request.remote-after-effect-reset";
    // The paused clock virtualizes the response grace the first invocation
    // must exhaust before it settles as an indeterminate effect; the
    // choreography itself still runs over real loopback connections.
    let (client, admitted, server) = reset_then_reconnect_client(FIRST_ID, SECOND_ID).await;
    let cancellation =
        CancellationSignal::active("cancel.remote-effect-reset-state").expect("cancellation");
    let cancel_after_admission = cancellation.clone();
    let cancel = tokio::spawn(async move {
        admitted.await.expect("request admission");
        assert!(cancel_after_admission.cancel(now_micros()));
    });
    let deadline = deadline_after(Duration::from_secs(10));
    let first = client
        .invoke_controlled(
            invocation_request(FIRST_ID, deadline.clone()),
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        )
        .await
        .expect("indeterminate effect is typed");
    assert_authoritative_settlement(first);

    let second = client
        .invoke(invocation_request(
            SECOND_ID,
            deadline_after(Duration::from_secs(1)),
        ))
        .await
        .expect("next invocation reconnects");
    assert!(matches!(
        second.outcome,
        DaemonInvocationOutcome::Problem {
            problem: DaemonInvocationProblem::Unavailable
        }
    ));
    cancel.await.expect("cancellation task");
    server.await.expect("server task");
}

#[tokio::test]
async fn semantic_qualification_uses_daemon_owned_profile_deadline_and_canonical_bytes() {
    const DEADLINE_MICROS: i64 = 5_000_000;
    const CANCELLATION_ID: &str = "cancel.semantic-qualification.success";
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind invocation listener");
    let server = tokio::spawn(async move {
        let invocation_stream = listener.accept().await.expect("accept invocation");
        let (invocation_reader, mut invocation_writer) = invocation_stream.into_split();
        let mut invocation_lines = BufReader::new(invocation_reader).lines();
        invocation_lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let request_line = invocation_lines
            .next_line()
            .await
            .expect("read invocation request")
            .expect("invocation request");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed invocation request");
        let request_id = request.request_id.clone();
        match request.payload {
            DaemonInvocationPayload::SemanticQualify {
                evaluated_profile_id,
                observed_at,
                deadline,
                cancellation,
            } => {
                assert_eq!(evaluated_profile_id, "hybrid-conservative");
                assert_eq!(
                    deadline.expires_at.0 - observed_at.0,
                    DEADLINE_MICROS,
                    "the client must carry the exact caller deadline"
                );
                assert_eq!(cancellation.token_id.as_str(), CANCELLATION_ID);
            }
            payload => panic!("expected semantic qualification payload, got {payload:?}"),
        }
        let response = DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::SemanticEvaluatedProfileQualified {
                qualification: CanonicalQualificationBlob::new(vec![0x51, 0x55, 0x41, 0x4c])
                    .expect("bounded canonical qualification"),
            },
        );
        invocation_writer
            .write_all(
                serde_json::to_string(&response)
                    .expect("response JSON")
                    .as_bytes(),
            )
            .await
            .expect("write qualification response");
        invocation_writer
            .write_all(b"\n")
            .await
            .expect("write response newline");
        invocation_writer
            .flush()
            .await
            .expect("flush qualification response");
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let client = DaemonInvocationClient::for_connection_for_test(
        DaemonConnection::unauthenticated_for_test(endpoint),
        DaemonHandshake {
            project_path: Some(profile_root.clone()),
            scope_prefix: None,
            timings: false,
            allow_init: false,
            allow_initialize_root_routing: false,
            client_identity: DaemonClientIdentity {
                global_db_path: profile_root.join("global.db"),
                profile_root,
            },
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            client_instance_id: "client.semantic-qualification-success".to_owned(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: crate::handshake::MovedStoreAdoption::Never,
        },
    );
    let cancellation = CancellationSignal::active(CANCELLATION_ID).expect("cancellation");

    let result = client
        .qualify_semantic_profile_until("hybrid-conservative", DEADLINE_MICROS, cancellation)
        .await
        .expect("canonical qualification bytes");

    assert_eq!(result.qualification_bytes, vec![0x51, 0x55, 0x41, 0x4c]);
    server.await.expect("server task");
}

#[tokio::test]
async fn semantic_qualification_cancellation_controls_the_same_payload_request() {
    const CANCELLATION_ID: &str = "cancel.semantic-qualification.control";
    let (listener, endpoint) =
        crate::transport::BrokerListener::bind(&crate::transport::default_loopback_endpoint())
            .await
            .expect("bind invocation listener");
    let (request_admitted, admitted) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let invocation_stream = listener.accept().await.expect("accept invocation");
        let (invocation_reader, _invocation_writer) = invocation_stream.into_split();
        let mut invocation_lines = BufReader::new(invocation_reader).lines();
        invocation_lines
            .next_line()
            .await
            .expect("read invocation handshake")
            .expect("invocation handshake");
        let request_line = invocation_lines
            .next_line()
            .await
            .expect("read invocation request")
            .expect("invocation request");
        let request: DaemonInvocationRequest =
            serde_json::from_str(&request_line).expect("typed invocation request");
        let request_id = request.request_id.clone();
        match request.payload {
            DaemonInvocationPayload::SemanticQualify { cancellation, .. } => {
                assert_eq!(cancellation.token_id.as_str(), CANCELLATION_ID);
            }
            payload => panic!("expected semantic qualification payload, got {payload:?}"),
        }
        let _ = request_admitted.send(());

        let control_stream = listener.accept().await.expect("accept cancellation");
        let (control_reader, _control_writer) = control_stream.into_split();
        let mut control_lines = BufReader::new(control_reader).lines();
        control_lines
            .next_line()
            .await
            .expect("read cancellation handshake")
            .expect("cancellation handshake");
        let cancellation_line = control_lines
            .next_line()
            .await
            .expect("read cancellation request")
            .expect("cancellation request");
        let control = parse_daemon_invocation_cancellation_request(&cancellation_line)
            .expect("typed invocation cancellation");
        assert_eq!(control.target_request_id(), request_id);
        std::future::pending::<()>().await;
    });
    let profile = tempfile::tempdir().expect("profile");
    let profile_root = profile.path().to_path_buf();
    let client = DaemonInvocationClient::for_connection_for_test(
        DaemonConnection::unauthenticated_for_test(endpoint),
        DaemonHandshake {
            project_path: Some(profile_root.clone()),
            scope_prefix: None,
            timings: false,
            allow_init: false,
            allow_initialize_root_routing: false,
            client_identity: DaemonClientIdentity {
                global_db_path: profile_root.join("global.db"),
                profile_root,
            },
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            client_instance_id: "client.semantic-qualification-cancel".to_owned(),
            tool_list_changed_capable: false,
            catalog_version: String::new(),
            moved_store_adoption: crate::handshake::MovedStoreAdoption::Never,
        },
    );
    let cancellation = CancellationSignal::active(CANCELLATION_ID).expect("cancellation");
    let call_cancellation = cancellation.clone();
    let call_client = client.clone();
    let call = tokio::spawn(async move {
        call_client
            .qualify_semantic_profile_until("hybrid-conservative", 5_000_000, call_cancellation)
            .await
    });
    admitted.await.expect("request admission");
    assert!(cancellation.cancel(now_micros()));

    let error = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .expect("read-only cancellation returns promptly")
        .expect("qualification call task")
        .expect_err("cancelled qualification must not return bytes");
    let (reason, retryable, _) = error
        .project_route_context()
        .expect("typed semantic qualification cancellation");
    assert_eq!(reason, "semantic_qualification_cancelled");
    assert!(!retryable);
    server.abort();
}
