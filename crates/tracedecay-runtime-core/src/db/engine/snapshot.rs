use std::sync::{Arc, Mutex};

use tracedecay_rusqlite_runtime::exact_sql::ExactSqlReadSnapshot;

use super::{IntoParams, Result, Rows, Value, connection::statement};

pub struct ReadSnapshot {
    /// Serializes every read issued against one snapshot. A snapshot handed to
    /// several concurrent readers funnels all of them through this one lock,
    /// so a single slow statement stalls the rest. Snapshots are created per
    /// query, so per-instance lock profiling would retain two HDR histograms
    /// for every read. The measured query path remains the stable boundary.
    runtime: Arc<Mutex<ExactSqlReadSnapshot>>,
}

impl ReadSnapshot {
    pub(super) fn from_runtime(runtime: ExactSqlReadSnapshot) -> Self {
        hotpath::gauge!("runtime_core.db.snapshots_active").inc(1.0);
        Self {
            runtime: Arc::new(Mutex::new(runtime)),
        }
    }

    #[hotpath::skip]
    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let runtime = Arc::clone(&self.runtime);
        let statement = statement(sql, params)?;
        let rows = tokio::task::spawn_blocking(move || {
            lock_runtime(&runtime)?
                .query(statement)
                .map_err(super::Error::from)
        })
        .await
        .map_err(join_error)??;
        Ok(Rows::from_parts(
            rows.columns,
            rows.rows
                .into_iter()
                .map(|row| {
                    super::Row::from_values(row.values.into_iter().map(Value::from).collect())
                })
                .collect(),
        ))
    }
}

impl Drop for ReadSnapshot {
    fn drop(&mut self) {
        hotpath::gauge!("runtime_core.db.snapshots_active").dec(1.0);
    }
}

fn lock_runtime<T>(runtime: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    runtime
        .lock()
        .map_err(|_| super::Error::Runtime("exact SQL read snapshot lock poisoned".to_owned()))
}

fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("exact SQL read snapshot task failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::{super::Error, lock_runtime};

    #[test]
    fn poisoned_snapshot_lock_returns_a_typed_error() {
        let runtime = Mutex::new(());
        let _ = std::panic::catch_unwind(|| {
            let _guard = runtime.lock().unwrap();
            panic!("poison snapshot lock");
        });

        let result = lock_runtime(&runtime);
        let Err(Error::Runtime(message)) = result else {
            panic!("poisoned snapshot lock must return a runtime error");
        };
        assert_eq!(message, "exact SQL read snapshot lock poisoned");
    }
}
