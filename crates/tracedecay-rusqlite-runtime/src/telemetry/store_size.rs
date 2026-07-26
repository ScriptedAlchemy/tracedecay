use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tracedecay_application::{
    RequestAdmission, RequestContext, ResolvedScope,
    storage::{
        StorageByteSizeV1, StorageTelemetryFuture, StorageTelemetryReadV1, StoreKeyV1,
        StoreSizeSampleV1, StoreSizeTelemetryPort, TableGrowthSampleV1, TableGrowthTelemetryReadV1,
        TableNameV1,
    },
};
use tracedecay_domain::UtcMicros;
use tracedecay_store::UnavailableReasonV1;

use crate::migration_sql::MigrationSqlHandle;

#[derive(Clone, Copy)]
struct TableWatermark {
    bytes: StorageByteSizeV1,
    observed_at: UtcMicros,
}

/// SQLite-backed application telemetry over the runtime's retained health
/// reader. The adapter is bound to one exact request scope and one store.
#[derive(Clone)]
pub struct SqliteStoreSizeTelemetryPort {
    handle: MigrationSqlHandle,
    store: StoreKeyV1,
    scope: ResolvedScope,
    reader_wait: Duration,
    table_watermarks: Arc<Mutex<Option<BTreeMap<TableNameV1, TableWatermark>>>>,
}

impl SqliteStoreSizeTelemetryPort {
    #[must_use]
    pub fn new(
        handle: MigrationSqlHandle,
        store: StoreKeyV1,
        scope: ResolvedScope,
        reader_wait: Duration,
    ) -> Self {
        Self {
            handle: handle.read_only_clone(),
            store,
            scope,
            reader_wait,
            table_watermarks: Arc::new(Mutex::new(None)),
        }
    }

    fn admits(&self, context: &RequestContext, store: &StoreKeyV1) -> bool {
        context.validate().is_ok()
            && context.scope() == &self.scope
            && store == &self.store
            && context.admission_at(now_micros()) == RequestAdmission::Admitted
    }
}

impl StoreSizeTelemetryPort for SqliteStoreSizeTelemetryPort {
    fn store_size<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, StorageTelemetryReadV1> {
        Box::pin(async move {
            if !self.admits(context, store) {
                return StorageTelemetryReadV1::Denied {
                    store: store.clone(),
                };
            }
            let result = self
                .handle
                .store_size_telemetry(self.reader_wait, || interruption(context));
            let Ok(sample) = result else {
                return StorageTelemetryReadV1::Unknown {
                    store: store.clone(),
                };
            };
            let sample = StoreSizeSampleV1 {
                store: store.clone(),
                page_size_bytes: sample.page_size_bytes,
                page_count: sample.page_count,
                freelist_pages: sample.freelist_pages,
                observed_at: now_micros(),
            };
            if sample.validate().is_err() {
                return StorageTelemetryReadV1::Unknown {
                    store: store.clone(),
                };
            }
            StorageTelemetryReadV1::Observed { sample }
        })
    }

    fn table_growth<'a>(
        &'a self,
        context: &'a RequestContext,
        store: &'a StoreKeyV1,
    ) -> StorageTelemetryFuture<'a, TableGrowthTelemetryReadV1> {
        Box::pin(async move {
            if !self.admits(context, store) {
                return TableGrowthTelemetryReadV1::Denied {
                    store: store.clone(),
                };
            }
            let Ok(current) = self
                .handle
                .table_size_telemetry(self.reader_wait, || interruption(context))
            else {
                return TableGrowthTelemetryReadV1::Unknown {
                    store: store.clone(),
                };
            };
            let observed_at = now_micros();
            let mut current_tables = BTreeMap::new();
            for sample in current {
                let Ok(table) = TableNameV1::new(sample.table_name) else {
                    return TableGrowthTelemetryReadV1::Unknown {
                        store: store.clone(),
                    };
                };
                current_tables.insert(table, StorageByteSizeV1(sample.bytes));
            }
            let mut watermarks = self
                .table_watermarks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(previous_watermarks) = watermarks.as_ref() else {
                let tables_observed = u64::try_from(current_tables.len()).unwrap_or(u64::MAX);
                *watermarks = Some(
                    current_tables
                        .into_iter()
                        .map(|(table, bytes)| (table, TableWatermark { bytes, observed_at }))
                        .collect(),
                );
                return TableGrowthTelemetryReadV1::BaselineEstablished {
                    store: store.clone(),
                    observed_at,
                    tables_observed,
                };
            };

            let tables: BTreeSet<_> = previous_watermarks
                .keys()
                .chain(current_tables.keys())
                .cloned()
                .collect();
            let growth = tables
                .into_iter()
                .filter_map(|table| {
                    let previous = previous_watermarks.get(&table)?;
                    let current_bytes = current_tables
                        .get(&table)
                        .copied()
                        .unwrap_or(StorageByteSizeV1::ZERO);
                    let sample = TableGrowthSampleV1 {
                        store: store.clone(),
                        table,
                        previous_bytes: previous.bytes,
                        current_bytes,
                        previous_observed_at: previous.observed_at,
                        current_observed_at: observed_at,
                    };
                    sample.validate().ok().map(|_| sample)
                })
                .collect();
            *watermarks = Some(
                current_tables
                    .into_iter()
                    .map(|(table, bytes)| (table, TableWatermark { bytes, observed_at }))
                    .collect(),
            );
            TableGrowthTelemetryReadV1::Observed {
                store: store.clone(),
                samples: growth,
            }
        })
    }
}

fn interruption(context: &RequestContext) -> Option<UnavailableReasonV1> {
    match context.admission_at(now_micros()) {
        RequestAdmission::Admitted => None,
        RequestAdmission::Cancelled => Some(UnavailableReasonV1::Cancelled),
        RequestAdmission::TimedOut => Some(UnavailableReasonV1::DeadlineExceeded),
    }
}

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}
