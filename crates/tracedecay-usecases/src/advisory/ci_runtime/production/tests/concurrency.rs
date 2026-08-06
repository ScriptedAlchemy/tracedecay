use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::discovery::*;
use super::discovery_tests::{config, context, scope};
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Copy)]
enum PageKind {
    Runs,
    Jobs,
    Checks,
}

struct DelayedPagedDiscoveryClient {
    record: GitHubCiProviderRecordV1,
    page_count: u32,
    active: AtomicUsize,
    peak: AtomicUsize,
    requested: Mutex<Vec<(u8, u32)>>,
    permits: Arc<tokio::sync::Semaphore>,
}

impl DelayedPagedDiscoveryClient {
    fn page(&self, kind: PageKind, page: u32) -> Vec<u8> {
        discovery_page(&self.record, self.page_count, kind, page)
    }

    fn read<'a>(
        &'a self,
        kind: PageKind,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.requested.lock().unwrap().push((kind as u8, page));
        let body = self.page(kind, page);
        Box::pin(async move {
            let permit = self.permits.acquire().await.unwrap();
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            drop(permit);
            GitHubCiTransportOutcomeV1::Response(body)
        })
    }
}

fn discovery_page(
    source: &GitHubCiProviderRecordV1,
    page_count: u32,
    kind: PageKind,
    page: u32,
) -> Vec<u8> {
    let total = if page_count == 1 {
        1
    } else {
        u64::from(page_count) * 100
    };
    let count = if page_count == 1 { 1 } else { 100 };
    let offset = u64::from(page.saturating_sub(1)) * 100;
    match kind {
        PageKind::Runs => {
            let records = (0..count)
                .map(|index| {
                    let mut record = source.workflow_run.clone();
                    record.id += offset + index;
                    if page > 1 || index > 0 {
                        record.head_branch = "other".to_owned();
                    }
                    record
                })
                .collect::<Vec<_>>();
            serde_json::to_vec(&serde_json::json!({
                "total_count": total,
                "workflow_runs": records,
            }))
            .unwrap()
        }
        PageKind::Jobs => {
            let records = (0..count)
                .map(|index| {
                    let mut record = source.workflow_job.clone();
                    record.id += offset + index;
                    if page > 1 || index > 0 {
                        record.run_id += 1;
                    }
                    record
                })
                .collect::<Vec<_>>();
            serde_json::to_vec(&serde_json::json!({
                "total_count": total,
                "jobs": records,
            }))
            .unwrap()
        }
        PageKind::Checks => {
            let records = (0..count)
                .map(|index| {
                    let mut record = source.check_run.clone();
                    record.id += offset + index;
                    if page > 1 || index > 0 {
                        record.check_suite.id += 1;
                    }
                    record
                })
                .collect::<Vec<_>>();
            serde_json::to_vec(&serde_json::json!({
                "total_count": total,
                "check_runs": records,
            }))
            .unwrap()
        }
    }
}

impl ProductionCiDiscoveryReadPortV1 for DelayedPagedDiscoveryClient {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        _context: &'a RequestContext,
        _head_sha: &'a str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read(PageKind::Runs, page)
    }

    fn read_workflow_jobs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read(PageKind::Jobs, page)
    }

    fn read_check_runs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _check_suite_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read(PageKind::Checks, page)
    }
}

#[tokio::test]
async fn ci_discovery_uses_four_bounded_slots_and_preserves_result() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let client = DelayedPagedDiscoveryClient {
        record: fixture.ci_provider_record.clone(),
        page_count: 4,
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        requested: Mutex::new(Vec::new()),
        permits: Arc::new(tokio::sync::Semaphore::new(4)),
    };

    let outcome = discover_production_ci_failure_request_with_v1(
        &context(&scope, UtcMicros(i64::MAX)),
        &config(&fixture),
        &scope,
        &client,
    )
    .await;

    assert_eq!(
        outcome.request().map(|request| &request.run),
        Some(&fixture.ci.run)
    );
    assert_eq!(client.peak.load(Ordering::SeqCst), 4);
}

struct OutOfOrderFailureClient {
    record: GitHubCiProviderRecordV1,
}

impl ProductionCiDiscoveryReadPortV1 for OutOfOrderFailureClient {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        _context: &'a RequestContext,
        _head_sha: &'a str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        Box::pin(async move {
            match page {
                2 => {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                    GitHubCiTransportOutcomeV1::RateLimited(
                        tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1 {
                            limit: 5_000,
                            remaining: 0,
                            reset_at: UtcMicros(42),
                        },
                    )
                }
                3 => GitHubCiTransportOutcomeV1::Unavailable,
                _ => GitHubCiTransportOutcomeV1::Response(discovery_page(
                    &self.record,
                    4,
                    PageKind::Runs,
                    page,
                )),
            }
        })
    }

    fn read_workflow_jobs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _run_id: u64,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }

    fn read_check_runs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _check_suite_id: u64,
        _page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
    }
}

#[tokio::test]
async fn concurrent_page_failures_reduce_by_page_key_not_completion_order() {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let owner_scope = scope(&fixture);
    let outcome = collect_workflow_runs(
        &context(&owner_scope, UtcMicros(i64::MAX)),
        &config(&fixture),
        &owner_scope,
        &OutOfOrderFailureClient {
            record: fixture.ci_provider_record,
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(
        outcome,
        ProductionCiFailureDiscoveryOutcomeV1::RateLimited(_)
    ));
}

struct DelayedLoopbackDiscoveryClient {
    address: std::net::SocketAddr,
    record: GitHubCiProviderRecordV1,
    page_count: u32,
    permits: Arc<tokio::sync::Semaphore>,
}

impl DelayedLoopbackDiscoveryClient {
    fn read<'a>(
        &'a self,
        kind: PageKind,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        let address = self.address;
        let body = discovery_page(&self.record, self.page_count, kind, page);
        Box::pin(async move {
            let Ok(_permit) = self.permits.acquire().await else {
                return GitHubCiTransportOutcomeV1::Unavailable;
            };
            let Ok(mut stream) = tokio::net::TcpStream::connect(address).await else {
                return GitHubCiTransportOutcomeV1::Unavailable;
            };
            let path = format!("/{}?page={page}", kind as u8);
            if stream
                .write_all(
                    format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .is_err()
            {
                return GitHubCiTransportOutcomeV1::Unavailable;
            }
            let mut response = Vec::new();
            if stream.read_to_end(&mut response).await.is_err()
                || !response.starts_with(b"HTTP/1.1 200")
            {
                return GitHubCiTransportOutcomeV1::Unavailable;
            }
            GitHubCiTransportOutcomeV1::Response(body)
        })
    }
}

impl ProductionCiDiscoveryReadPortV1 for DelayedLoopbackDiscoveryClient {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        _context: &'a RequestContext,
        _head_sha: &'a str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read(PageKind::Runs, page)
    }

    fn read_workflow_jobs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read(PageKind::Jobs, page)
    }

    fn read_check_runs<'a>(
        &'a self,
        _context: &'a RequestContext,
        _check_suite_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read(PageKind::Checks, page)
    }
}

async fn delayed_loopback(
    requests: usize,
    delay: Duration,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut handlers = Vec::with_capacity(requests);
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().await.unwrap();
            handlers.push(tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.unwrap();
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                tokio::time::sleep(delay).await;
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await
                    .unwrap();
            }));
        }
        for handler in handlers {
            handler.await.unwrap();
        }
    });
    (address, server)
}

async fn measure_loopback(page_count: u32, samples: usize) -> Duration {
    let fixture =
        crate::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1().unwrap();
    let scope = scope(&fixture);
    let requests_per_sample = usize::try_from(page_count).unwrap() * 6;
    let (address, server) =
        delayed_loopback(requests_per_sample * samples, Duration::from_millis(5)).await;
    let client = DelayedLoopbackDiscoveryClient {
        address,
        record: fixture.ci_provider_record.clone(),
        page_count,
        permits: Arc::new(tokio::sync::Semaphore::new(4)),
    };
    let mut elapsed = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let outcome = discover_production_ci_failure_request_with_v1(
            &context(&scope, UtcMicros(i64::MAX)),
            &config(&fixture),
            &scope,
            &client,
        )
        .await;
        assert_eq!(
            outcome.request().map(|request| &request.run),
            Some(&fixture.ci.run)
        );
        elapsed.push(started.elapsed());
    }
    server.await.unwrap();
    elapsed.sort_unstable();
    elapsed[elapsed.len() / 2]
}

#[tokio::test]
#[ignore = "30-sample delayed-loopback benchmark"]
async fn report_ci_discovery_loopback_p50() {
    let one_page = measure_loopback(1, 30).await;
    let twenty_pages = measure_loopback(20, 30).await;
    eprintln!(
        "ci-discovery-loopback samples=30 one-page-p50-us={} twenty-page-p50-us={}",
        one_page.as_micros(),
        twenty_pages.as_micros()
    );
}
