use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::daemon::DaemonHandshake;
use crate::errors::{Result, TraceDecayError};
use tracedecay_usecases::provider_usage::{ProviderUsageCostSummaryV1, ProviderUsageCoverageV1};

const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
// A dropped TUI leaves its detached worker alive briefly; gate replacements too.
static REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

struct RefreshLease;

impl Drop for RefreshLease {
    fn drop(&mut self) {
        REFRESH_IN_FLIGHT.store(false, Ordering::Release);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct CostSnapshot {
    pub(super) today_cost: f64,
    pub(super) week_cost: f64,
    pub(super) tokens_saved: u64,
    pub(super) efficiency_pct: f64,
    pub(super) top_model: Option<String>,
    pub(super) top_model_cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CostCacheState {
    Fresh,
    Stale(String),
    Unavailable(String),
}

type RefreshResult = std::result::Result<Option<CostSnapshot>, String>;

pub(super) struct CostCache {
    pub(super) snapshot: Option<CostSnapshot>,
    pub(super) state: CostCacheState,
    last_refresh: Instant,
    refresh: Option<Receiver<RefreshResult>>,
}

impl CostCache {
    pub(super) fn new() -> Self {
        Self {
            snapshot: None,
            state: CostCacheState::Unavailable("not loaded".to_string()),
            last_refresh: Instant::now()
                .checked_sub(Duration::from_secs(999))
                .unwrap_or_else(Instant::now),
            refresh: None,
        }
    }

    pub(super) fn is_stale(&self) -> bool {
        self.refresh.is_none() && self.last_refresh.elapsed() > REFRESH_INTERVAL
    }

    pub(super) fn begin_refresh(&mut self) {
        self.begin_refresh_with(fetch_cost_snapshot_blocking);
    }

    fn begin_refresh_with<F>(&mut self, fetch: F)
    where
        F: FnOnce() -> RefreshResult + Send + 'static,
    {
        if self.refresh.is_some()
            || REFRESH_IN_FLIGHT
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let guarded_fetch = move || {
            let _lease = RefreshLease;
            fetch()
        };
        match spawn_refresh_worker(guarded_fetch) {
            Ok(refresh) => self.refresh = Some(refresh),
            Err(error) => {
                REFRESH_IN_FLIGHT.store(false, Ordering::Release);
                self.apply_refresh(Err(format!("could not start cost refresh worker: {error}")));
            }
        }
    }

    pub(super) fn poll_refresh(&mut self) {
        let result = match self.refresh.as_ref().map(Receiver::try_recv) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => Some(Err(
                "cost refresh worker stopped without a result".to_string(),
            )),
            Some(Err(TryRecvError::Empty)) | None => None,
        };
        if let Some(result) = result {
            self.refresh = None;
            self.apply_refresh(result);
        }
    }

    fn apply_refresh(&mut self, result: RefreshResult) {
        match result {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.state = CostCacheState::Fresh;
            }
            Err(error) if self.snapshot.is_some() => {
                self.state = CostCacheState::Stale(error);
            }
            Err(error) => {
                self.state = CostCacheState::Unavailable(error);
            }
        }
        self.last_refresh = Instant::now();
    }
}

#[derive(serde::Deserialize)]
struct CostAdminPayload {
    summary: CostSummaryPayload,
    today: TodayCostPayload,
}

#[derive(serde::Deserialize)]
struct CostSummaryPayload {
    provider_usage: ProviderUsageCostSummaryV1,
    tokens_saved: u64,
    efficiency_ratio: Option<f64>,
}

#[derive(serde::Deserialize)]
struct TodayCostPayload {
    provider_usage: ProviderUsageCostSummaryV1,
}

fn map_cost_payloads(
    week: serde_json::Value,
    today: serde_json::Value,
) -> std::result::Result<Option<CostSnapshot>, String> {
    let week = serde_json::from_value::<CostAdminPayload>(week)
        .map_err(|error| format!("invalid daemon 7d cost response: {error}"))?;
    let today = serde_json::from_value::<CostAdminPayload>(today)
        .map_err(|error| format!("invalid daemon today cost response: {error}"))?;
    let week_usage = week.summary.provider_usage;
    let today_usage = today.today.provider_usage;
    if week_usage.coverage == ProviderUsageCoverageV1::Unavailable
        || today_usage.coverage == ProviderUsageCoverageV1::Unavailable
    {
        return Ok(None);
    }
    if week_usage.coverage != ProviderUsageCoverageV1::Complete
        || today_usage.coverage != ProviderUsageCoverageV1::Complete
    {
        return Err("canonical provider usage is incomplete".to_string());
    }
    let week_cost = week_usage
        .total_cost_usd
        .ok_or_else(|| "7d provider usage is not fully priced".to_string())?;
    let today_cost = today_usage
        .total_cost_usd
        .ok_or_else(|| "today provider usage is not fully priced".to_string())?;
    let efficiency_ratio = week
        .summary
        .efficiency_ratio
        .ok_or_else(|| "provider usage efficiency is unavailable".to_string())?;
    if !today_cost.is_finite()
        || today_cost < 0.0
        || !week_cost.is_finite()
        || week_cost < 0.0
        || !efficiency_ratio.is_finite()
        || !(0.0..=1.0).contains(&efficiency_ratio)
    {
        return Err("daemon cost response contains invalid numeric values".to_string());
    }
    if week_usage
        .by_model
        .iter()
        .chain(&today_usage.by_model)
        .filter_map(|model| model.cost_usd)
        .any(|cost| !cost.is_finite() || cost < 0.0)
    {
        return Err("daemon cost response contains an invalid model cost".to_string());
    }
    let top_model = today_usage
        .by_model
        .iter()
        .filter_map(|model| {
            model
                .cost_usd
                .map(|cost| (format!("{}/{}", model.provider, model.model), cost))
        })
        .max_by(|(_, left), (_, right)| left.total_cmp(right));
    let (top_model, top_model_cost) = match top_model {
        Some((model, cost)) => (Some(model), Some(cost)),
        None => (None, None),
    };
    Ok(Some(CostSnapshot {
        today_cost,
        week_cost,
        tokens_saved: week.summary.tokens_saved,
        efficiency_pct: efficiency_ratio * 100.0,
        top_model,
        top_model_cost,
    }))
}

fn global_cost_handshake() -> Result<DaemonHandshake> {
    let cwd = std::env::current_dir()?;
    let project_root = crate::config::discover_project_root(&cwd);
    DaemonHandshake::for_current_client(project_root, None, false, false)
}

async fn call_cost_summary(handshake: &DaemonHandshake, range: &str) -> Result<serde_json::Value> {
    let result = crate::daemon::call_default_tool(
        handshake,
        "tracedecay_admin_cli",
        serde_json::json!({ "action": "cost_summary", "range": range }),
    )
    .await?;
    crate::daemon::tool_json_payload(&result, "tracedecay_admin_cli")
}

async fn fetch_cost_snapshot() -> Result<Option<CostSnapshot>> {
    let handshake = global_cost_handshake()?;
    let fetch = async {
        let (week, today) = tokio::try_join!(
            call_cost_summary(&handshake, "7d"),
            call_cost_summary(&handshake, "today")
        )?;
        map_cost_payloads(week, today).map_err(|message| TraceDecayError::Config { message })
    };
    tokio::time::timeout(FETCH_TIMEOUT, fetch)
        .await
        .map_err(|_| TraceDecayError::Config {
            message: "daemon cost refresh timed out after 5 seconds".to_string(),
        })?
}

fn fetch_cost_snapshot_blocking() -> RefreshResult {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start cost refresh runtime: {error}"))?;
    runtime
        .block_on(fetch_cost_snapshot())
        .map_err(|error| error.to_string())
}

fn spawn_refresh_worker<F>(fetch: F) -> std::io::Result<Receiver<RefreshResult>>
where
    F: FnOnce() -> RefreshResult + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("tracedecay-monitor-cost".to_string())
        .spawn(move || {
            let _ = sender.send(fetch());
        })?;
    Ok(receiver)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn payload(
        total_cost: f64,
        coverage: &str,
        model: &str,
        model_cost: f64,
        today_cost: f64,
    ) -> serde_json::Value {
        serde_json::json!({
            "summary": {
                "provider_usage": {
                    "coverage": coverage,
                    "pricing_revision": "fixture",
                    "usage_events": 1,
                    "unpriced_events": 0,
                    "total_cost_usd": total_cost,
                    "total_input_tokens": 600,
                    "total_output_tokens": 300,
                    "total_cache_read_tokens": 0,
                    "total_cache_write_tokens": 0,
                    "by_model": [{
                        "provider": "codex",
                        "model": model,
                        "usage_events": 1,
                        "total_tokens": 900,
                        "cost_usd": model_cost
                    }]
                },
                "tokens_saved": 1200,
                "efficiency_ratio": 0.6
            },
            "today": {
                "provider_usage": {
                    "coverage": coverage,
                    "pricing_revision": "fixture",
                    "usage_events": 1,
                    "unpriced_events": 0,
                    "total_cost_usd": today_cost,
                    "total_input_tokens": 600,
                    "total_output_tokens": 300,
                    "total_cache_read_tokens": 0,
                    "total_cache_write_tokens": 0,
                    "by_model": [{
                        "provider": "codex",
                        "model": model,
                        "usage_events": 1,
                        "total_tokens": 900,
                        "cost_usd": model_cost
                    }]
                }
            }
        })
    }

    #[test]
    fn daemon_cost_response_uses_today_model_and_week_totals() {
        let snapshot = map_cost_payloads(
            payload(4.5, "complete", "week-leader", 3.25, 1.5),
            payload(1.5, "complete", "today-leader", 1.25, 1.5),
        )
        .unwrap()
        .unwrap();
        assert!((snapshot.today_cost - 1.5).abs() < f64::EPSILON);
        assert!((snapshot.week_cost - 4.5).abs() < f64::EPSILON);
        assert_eq!(snapshot.tokens_saved, 1200);
        assert!((snapshot.efficiency_pct - 60.0).abs() < f64::EPSILON);
        assert_eq!(snapshot.top_model.as_deref(), Some("codex/today-leader"));
        assert_eq!(snapshot.top_model_cost, Some(1.25));
    }

    #[test]
    fn daemon_cost_response_preserves_absent_top_model() {
        let week = payload(0.0, "complete", "unused", 0.0, 0.0);
        let mut today = payload(0.0, "complete", "unused", 0.0, 0.0);
        today["today"]["provider_usage"]["by_model"] = serde_json::json!([]);

        let snapshot = map_cost_payloads(week, today).unwrap().unwrap();

        assert_eq!(snapshot.top_model, None);
        assert_eq!(snapshot.top_model_cost, None);
    }

    #[test]
    fn cost_refresh_failure_preserves_snapshot_and_invalid_values_fail() {
        let mut cache = CostCache::new();
        cache.apply_refresh(Err("daemon offline".to_string()));
        assert_eq!(
            cache.state,
            CostCacheState::Unavailable("daemon offline".to_string())
        );
        let snapshot = CostSnapshot {
            today_cost: 1.5,
            week_cost: 4.5,
            tokens_saved: 1200,
            efficiency_pct: 60.0,
            top_model: Some("today-leader".to_string()),
            top_model_cost: Some(1.25),
        };
        cache.apply_refresh(Ok(Some(snapshot.clone())));
        cache.apply_refresh(Err("daemon epoch changed".to_string()));
        assert_eq!(cache.snapshot, Some(snapshot));
        assert!(matches!(cache.state, CostCacheState::Stale(_)));
        assert!(
            map_cost_payloads(
                payload(-1.0, "complete", "a", 1.0, 0.0),
                payload(0.0, "complete", "a", 0.0, 0.0)
            )
            .is_err()
        );
        assert_eq!(
            map_cost_payloads(
                payload(0.0, "unavailable", "a", 0.0, 0.0),
                payload(0.0, "unavailable", "a", 0.0, 0.0)
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn cost_handshake_preserves_discovered_project_identity() {
        let handshake = global_cost_handshake().unwrap();
        assert_eq!(
            handshake.project_path,
            crate::config::discover_project_root(&std::env::current_dir().unwrap())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refresh_worker_is_nonblocking_and_runtime_context_safe() {
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(0);
        let receiver = spawn_refresh_worker(move || {
            release_receiver.recv().unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            assert_eq!(runtime.block_on(async { 42 }), 42);
            Ok(None)
        })
        .unwrap();
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        release_sender.send(()).unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            Ok(None)
        );
    }

    #[test]
    fn refresh_lifecycle_is_single_flight_across_cache_drop() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;

        REFRESH_IN_FLIGHT.store(false, Ordering::Release);
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_sender, started_receiver) = std::sync::mpsc::sync_channel(0);
        let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(0);
        let mut first_cache = CostCache::new();
        let first_calls = Arc::clone(&calls);
        first_cache.begin_refresh_with(move || {
            first_calls.fetch_add(1, Ordering::AcqRel);
            started_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            Ok(None)
        });
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let skipped_calls = Arc::clone(&calls);
        first_cache.begin_refresh_with(move || {
            skipped_calls.fetch_add(1, Ordering::AcqRel);
            Ok(None)
        });
        drop(first_cache);

        let mut replacement = CostCache::new();
        let dropped_cache_calls = Arc::clone(&calls);
        replacement.begin_refresh_with(move || {
            dropped_cache_calls.fetch_add(1, Ordering::AcqRel);
            Ok(None)
        });
        assert!(replacement.refresh.is_none());
        assert!(replacement.snapshot.is_none());
        assert_eq!(calls.load(Ordering::Acquire), 1);

        release_sender.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while REFRESH_IN_FLIGHT.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!REFRESH_IN_FLIGHT.load(Ordering::Acquire));
        assert!(replacement.snapshot.is_none());

        let replacement_calls = Arc::clone(&calls);
        replacement.begin_refresh_with(move || {
            replacement_calls.fetch_add(1, Ordering::AcqRel);
            Ok(Some(CostSnapshot {
                today_cost: 2.0,
                week_cost: 5.0,
                tokens_saved: 10,
                efficiency_pct: 50.0,
                top_model: Some("new".to_string()),
                top_model_cost: Some(2.0),
            }))
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while replacement.refresh.is_some() && Instant::now() < deadline {
            replacement.poll_refresh();
            std::thread::yield_now();
        }
        assert_eq!(calls.load(Ordering::Acquire), 2);
        assert_eq!(
            replacement.snapshot.unwrap().top_model.as_deref(),
            Some("new")
        );
    }
}
