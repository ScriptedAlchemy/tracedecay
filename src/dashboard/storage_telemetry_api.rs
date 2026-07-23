//! `GET /api/storage/telemetry` — per-store size, free-page ratio, and typed
//! budget/growth dimensions (plan 38 §7 read models over the PR14 envelope).
//!
//! The size samples are **real**: they are read directly from the store
//! connections the dashboard already holds, via the cheap `PRAGMA page_count`
//! / `PRAGMA freelist_count` / `PRAGMA page_size` header reads that back
//! [`StoreSizeSampleV1`]. This is the one V2 read model with a live source the
//! dashboard can observe within its own territory, so it renders `ready`.
//!
//! The two dimensions whose producers are not wired server-side are rendered
//! typed-absent rather than fabricated:
//! - **budget**: no owner-configured [`StoreSizeBudgetV1`] source is wired, so a
//!   budget evaluation cannot be produced — the dimension is `unsupported`, not
//!   an invented "within budget".
//! - **growth**: no persisted per-table growth watermark history is wired, so an
//!   empty growth list would be a lie ("zero growth"); the dimension is `absent`.

use axum::Json;
use axum::extract::State;
use libsql::Connection;
use serde::Serialize;
use tracedecay_application::storage::identity::StoreKeyV1;
use tracedecay_application::storage::telemetry::{
    StorageTelemetryReadV1, StoreSizeSampleV1, TableGrowthSampleV1,
};
use tracedecay_domain::UtcMicros;

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardEnvelopeV1, DashboardLegalActionKindV1, DashboardLegalActionRefV1,
    now_micros, scope_from_state,
};

/// One store's telemetry entry.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct StoreTelemetryEntryV1 {
    /// Stable store key (the store's file name), or the raw file name when it is
    /// not a valid [`StoreKeyV1`].
    pub store: String,
    /// The dashboard's role label for the store (`graph` / `memory` / `lcm` /
    /// `savings`).
    pub role: String,
    /// Display path of the store file.
    pub path: String,
    /// The typed telemetry read: `observed` with a sample, or `unknown` when the
    /// pragma read failed. Never silently healthy.
    pub read: StorageTelemetryReadV1,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub free_page_ratio: Option<f64>,
    pub budget: StoreBudgetDimensionV1,
    pub growth: StoreGrowthDimensionV1,
}

/// The budget-evaluation dimension. Kept forward-compatible: a wired budget
/// source would emit [`Self::Evaluated`]; today the source is unwired.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum StoreBudgetDimensionV1 {
    /// No owner-configured `StoreSizeBudgetV1` source is wired server-side.
    Unsupported { reason: String },
}

/// The per-table growth dimension. An empty growth list is never "zero growth":
/// with no persisted watermark history the dimension is explicitly `absent`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub(crate) enum StoreGrowthDimensionV1 {
    /// No persisted per-table growth watermark history is wired server-side.
    Absent { reason: String },
    /// Observed growth samples (unreachable until a watermark store is wired).
    #[allow(dead_code)]
    Observed { samples: Vec<TableGrowthSampleV1> },
}

/// Telemetry payload: one entry per store the dashboard holds a connection to.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct StorageTelemetryPayloadV1 {
    pub stores: Vec<StoreTelemetryEntryV1>,
    /// Why the budget dimension is unsupported, stated once for the whole read.
    pub budget_note: String,
    /// Why the growth dimension is absent, stated once for the whole read.
    pub growth_note: String,
}

const BUDGET_UNSUPPORTED_REASON: &str =
    "no owner-configured StoreSizeBudgetV1 source is wired server-side yet";
const GROWTH_ABSENT_REASON: &str =
    "no persisted per-table growth watermark history is wired server-side yet";

/// `GET /api/storage/telemetry`
pub(crate) async fn telemetry(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<StorageTelemetryPayloadV1>> {
    let mut entries: Vec<StoreTelemetryEntryV1> = Vec::new();

    // Graph store: the dashboard owns the connection directly.
    entries.push(sample_entry("graph", &state.graph_db_path, &state.graph_conn).await);
    // Project-memory store.
    entries.push(sample_entry("memory", &state.mem_db_path, state.mem_db.conn()).await);
    // LCM session store, when a read connection is held.
    if let Some(conn) = &state.lcm_conn {
        entries.push(sample_entry("lcm", &state.lcm_db_path, conn).await);
    }
    // Global accounting store, when available.
    if let Some(db) = &state.savings_db {
        entries.push(sample_entry("savings", &state.savings_db_path, db.read_connection()).await);
    }

    let total = entries.len() as u64;
    let observed = entries
        .iter()
        .filter(|entry| matches!(entry.read, StorageTelemetryReadV1::Observed { .. }))
        .count() as u64;

    // Coverage is over the known, enumerated set of dashboard-held stores. A
    // complete claim therefore always carries a real denominator.
    let coverage = if observed == total {
        DashboardCoverageV1::complete(total, "stores")
    } else {
        DashboardCoverageV1::partial(
            total,
            observed,
            "stores",
            vec!["store telemetry read failed (pragma unavailable)".to_string()],
        )
    };

    let payload = StorageTelemetryPayloadV1 {
        stores: entries,
        budget_note: BUDGET_UNSUPPORTED_REASON.to_string(),
        growth_note: GROWTH_ABSENT_REASON.to_string(),
    };

    let envelope = DashboardEnvelopeV1::ready(scope_from_state(&state), coverage, payload)
        .with_legal_actions(vec![DashboardLegalActionRefV1::new(
            DashboardLegalActionKindV1::Refresh,
            "use-case.dashboard.storage.telemetry.refresh",
        )]);
    Json(envelope)
}

/// Sample one store's size from a live connection. A pragma failure produces a
/// typed [`StorageTelemetryReadV1::Unknown`], never a fabricated size.
async fn sample_entry(role: &str, path: &str, conn: &Connection) -> StoreTelemetryEntryV1 {
    let store_name = store_file_name(path);
    let store_key = StoreKeyV1::new(store_name.clone());

    let read = match &store_key {
        Ok(store) => match read_size_sample(conn, store).await {
            Some(sample) => StorageTelemetryReadV1::Observed { sample },
            None => StorageTelemetryReadV1::Unknown {
                store: store.clone(),
            },
        },
        // The store file name is not a valid store key; report the read as
        // unknown against a sanitized fallback key rather than inventing a size.
        Err(_) => StorageTelemetryReadV1::Unknown {
            store: StoreKeyV1::new(sanitize_store_key(&store_name))
                .unwrap_or_else(|_| StoreKeyV1::new("store").expect("static key")),
        },
    };

    let (total_bytes, free_bytes, free_page_ratio) = match &read {
        StorageTelemetryReadV1::Observed { sample } => (
            Some(sample.total_bytes().get()),
            Some(sample.free_bytes().get()),
            Some(sample.free_page_ratio().as_f64()),
        ),
        _ => (None, None, None),
    };

    StoreTelemetryEntryV1 {
        store: store_name,
        role: role.to_string(),
        path: path.to_string(),
        read,
        total_bytes,
        free_bytes,
        free_page_ratio,
        budget: StoreBudgetDimensionV1::Unsupported {
            reason: BUDGET_UNSUPPORTED_REASON.to_string(),
        },
        growth: StoreGrowthDimensionV1::Absent {
            reason: GROWTH_ABSENT_REASON.to_string(),
        },
    }
}

/// Read `PRAGMA page_size` / `page_count` / `freelist_count` into a validated
/// [`StoreSizeSampleV1`]. Returns `None` on any pragma failure or an invalid
/// sample so the caller reports a typed `unknown` read.
async fn read_size_sample(conn: &Connection, store: &StoreKeyV1) -> Option<StoreSizeSampleV1> {
    let page_size = pragma_u64(conn, "page_size").await?;
    let page_count = pragma_u64(conn, "page_count").await?;
    let freelist_pages = pragma_u64(conn, "freelist_count").await?;
    let page_size_bytes = u32::try_from(page_size).ok()?;
    let sample = StoreSizeSampleV1 {
        store: store.clone(),
        page_size_bytes,
        page_count,
        freelist_pages,
        observed_at: UtcMicros(now_micros()),
    };
    sample.validate().ok()?;
    Some(sample)
}

/// Run one `PRAGMA` and read its first column as a `u64`. `None` distinguishes a
/// failed read from a real zero, so the caller never treats a query error as a
/// zero-sized store.
async fn pragma_u64(conn: &Connection, pragma: &str) -> Option<u64> {
    let sql = format!("PRAGMA {pragma}");
    let mut rows = conn.query(&sql, ()).await.ok()?;
    let row = rows.next().await.ok()??;
    let value = row.get::<i64>(0).ok()?;
    Some(value.max(0) as u64)
}

fn store_file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.to_string(), str::to_string)
}

/// Reduce an invalid store file name to a bounded, control-free key.
fn sanitize_store_key(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(200)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "store".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::dashboard::read_model::{DashboardDomainStateV1, DashboardFreshnessStateV1};
    use crate::tracedecay::TraceDecay;

    async fn state_for_test() -> (tempfile::TempDir, DashboardState) {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture source");
        let cg = TraceDecay::init(project.path()).await.expect("project init");
        let state = crate::dashboard::build_state(&cg)
            .await
            .expect("dashboard state");
        (project, state)
    }

    #[tokio::test]
    async fn telemetry_reports_real_observed_sizes_for_held_stores() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let (_project, state) = state_for_test().await;
        let Json(envelope) = telemetry(State(state)).await;

        assert_eq!(envelope.schema_revision, 1);
        assert_eq!(envelope.domain_state, DashboardDomainStateV1::Ready);
        assert_eq!(envelope.freshness.state, DashboardFreshnessStateV1::Fresh);
        assert!(
            !envelope.payload.stores.is_empty(),
            "dashboard always holds at least the graph and memory stores"
        );

        // Every held store produced a real observed size sample.
        for entry in &envelope.payload.stores {
            assert!(
                matches!(entry.read, StorageTelemetryReadV1::Observed { .. }),
                "store {} should have an observed size read",
                entry.store
            );
            assert!(entry.total_bytes.unwrap_or(0) > 0, "store {} sized", entry.store);
            // Budget and growth are typed-absent, never fabricated.
            assert!(matches!(
                entry.budget,
                StoreBudgetDimensionV1::Unsupported { .. }
            ));
            assert!(matches!(entry.growth, StoreGrowthDimensionV1::Absent { .. }));
        }

        // Complete coverage carries a real denominator equal to the store count.
        assert!(envelope.coverage.is_complete());
        assert_eq!(
            envelope.coverage.denominator,
            Some(envelope.payload.stores.len() as u64)
        );
    }

    #[tokio::test]
    async fn malformed_pragma_read_is_typed_unknown_not_zero() {
        // A closed/empty connection cannot answer pragmas: the read is `unknown`,
        // and the size fields stay `None` rather than collapsing to zero.
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .expect("memory db");
        let conn = db.connect().expect("conn");
        // Drop the table backing so a bogus pragma path fails deterministically:
        // query a non-existent pragma name.
        let store = StoreKeyV1::new("probe.db").expect("key");
        // `PRAGMA not_a_real_pragma` returns no row -> None.
        assert!(pragma_u64(&conn, "definitely_not_a_pragma").await.is_none());
        // A valid pragma still reads.
        assert!(read_size_sample(&conn, &store).await.is_some());
    }
}
