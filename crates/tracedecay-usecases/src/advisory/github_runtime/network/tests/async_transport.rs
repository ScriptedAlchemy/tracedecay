use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FULL_COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[tokio::test]
async fn cancellation_stops_the_body_and_releases_its_permit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let accept_deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < accept_deadline,
                        "request was never accepted"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        let _ = read_http_request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 67108864\r\nConnection: close\r\n\r\n")
            .unwrap();
        stream.flush().unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let started = Instant::now();
        let chunk = [b'x'; 64 * 1024];
        let mut delivered = 0_usize;
        let stopped = loop {
            match stream.write_all(&chunk) {
                Ok(()) => {
                    delivered += chunk.len();
                    if delivered >= 64 * 1024 * 1024 {
                        break false;
                    }
                }
                Err(_) => break true,
            }
        };
        (started.elapsed(), delivered, stopped)
    });
    let config = GitHubHttpReadConfigV1 {
        rest_base_uri: format!("http://{address}"),
        graphql_uri: format!("http://{address}/graphql"),
        ..GitHubHttpReadConfigV1::default()
    };
    let transport = test_http_transport(config);
    let client = GitHubCiReadOnlyClientV1::new(
        GitHubCiRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "tracedecay".to_owned(),
        },
        GitHubReadOnlyCredentialV1::anonymous(),
        transport.clone(),
    )
    .unwrap();
    let owner_scope = scope("cancel-live-body");
    let deadline = Deadline::new(UtcMicros(now_micros().0.saturating_add(75_000))).unwrap();
    let expiring = context(&owner_scope).with_deadline(deadline);

    let started = Instant::now();
    let outcome = client
        .read_workflow_runs_for_head(&expiring, FULL_COMMIT, 1)
        .await;
    let outcome_after = started.elapsed();
    let permits_after = transport.available_permits();
    let (stopped_after, delivered, stopped) = server.join().unwrap();

    assert_eq!(outcome, GitHubCiTransportOutcomeV1::Unavailable);
    assert!(stopped, "server delivered the full body after cancellation");
    assert!(
        stopped_after < Duration::from_millis(250),
        "outcome_after={outcome_after:?} stopped_after={stopped_after:?} delivered={delivered} permits_after={permits_after}"
    );
    assert_eq!(permits_after, 4);
}

#[tokio::test]
async fn shared_transport_is_fifo_across_review_and_ci_without_starvation() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (accepted_tx, mut accepted_rx) = tokio::sync::mpsc::unbounded_channel();
    let server_release = Arc::clone(&release);
    let server_active = Arc::clone(&active);
    let server_peak = Arc::clone(&peak);
    let server = tokio::spawn(async move {
        let mut handlers = Vec::new();
        for _ in 0..6 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let release = Arc::clone(&server_release);
            let active = Arc::clone(&server_active);
            let peak = Arc::clone(&server_peak);
            let accepted = accepted_tx.clone();
            handlers.push(tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    bytes.extend_from_slice(&buffer[..read]);
                    if read == 0 || bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let first_line = String::from_utf8_lossy(&bytes)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                accepted.send(first_line).unwrap();
                let permit = release.acquire().await.unwrap();
                permit.forget();
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await
                    .unwrap();
                active.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });
    let config = GitHubHttpReadConfigV1 {
        rest_base_uri: format!("http://{address}"),
        graphql_uri: format!("http://{address}/graphql"),
        ..GitHubHttpReadConfigV1::default()
    };
    let transport = test_http_transport(config);
    let ci = GitHubCiReadOnlyClientV1::new(
        GitHubCiRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "tracedecay".to_owned(),
        },
        GitHubReadOnlyCredentialV1::anonymous(),
        transport.clone(),
    )
    .unwrap();
    let review = GitHubReadOnlyClientV1::new(
        GitHubRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "tracedecay".to_owned(),
            pull_request_number: 421,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        },
        GitHubReadOnlyCredentialV1::anonymous(),
        transport.clone(),
    )
    .unwrap();
    let owner_scope = scope("fifo");
    let request_context = context(&owner_scope);
    let mut initial = Vec::new();
    for page in 1..=4 {
        let client = ci.clone();
        let context = request_context.clone();
        let head = FULL_COMMIT.to_owned();
        initial.push(tokio::spawn(async move {
            client
                .read_workflow_runs_for_head(&context, &head, page)
                .await
        }));
    }
    let mut accepted_initial = 0;
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(2), accepted_rx.recv()).await {
            Ok(Some(_)) => accepted_initial += 1,
            other => panic!("accepted {accepted_initial} initial requests; next={other:?}"),
        }
    }
    let review_client = review.clone();
    let review_context = request_context.clone();
    let review_url = format!("http://{address}/review-waiter");
    let review_waiter = tokio::spawn(async move {
        review_client
            .get(
                &review_context,
                &review_url,
                None,
                GitHubReadPermissionV1::PullRequests,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    let late_client = ci.clone();
    let late_context = request_context.clone();
    let late_head = FULL_COMMIT.to_owned();
    let late_ci = tokio::spawn(async move {
        late_client
            .read_workflow_runs_for_head(&late_context, &late_head, 5)
            .await
    });

    release.add_permits(1);
    let next = tokio::time::timeout(Duration::from_secs(2), accepted_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(next.contains("/review-waiter"));
    release.add_permits(5);
    tokio::time::timeout(Duration::from_secs(2), accepted_rx.recv())
        .await
        .unwrap()
        .unwrap();
    for task in initial {
        task.await.unwrap();
    }
    assert!(matches!(
        review_waiter.await.unwrap(),
        HttpResponseV1::Ok { .. }
    ));
    assert!(matches!(
        late_ci.await.unwrap(),
        GitHubCiTransportOutcomeV1::Response(_)
    ));
    server.await.unwrap();

    assert!(peak.load(Ordering::SeqCst) <= 4);
    assert_eq!(transport.available_permits(), 4);
}
