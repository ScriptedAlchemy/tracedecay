use std::sync::Arc;

use tracedecay_rusqlite_runtime::exact_sql::ExactSqlReadSnapshot;

use super::{IntoParams, Result, Rows, connection::statement};

pub struct ReadSnapshot {
    /// One snapshot holds one pooled reader worker for its whole lifetime, and
    /// the runtime snapshot already serializes the statements issued against
    /// that worker. A snapshot handed to several concurrent readers therefore
    /// funnels all of them through it, so a single slow statement stalls the
    /// rest. The measured query path remains the stable boundary.
    runtime: Runtime,
}

enum Runtime {
    Sqlite(Arc<ExactSqlReadSnapshot>),
    Native(Arc<super::native::NativeSnapshot>),
}

impl ReadSnapshot {
    pub(super) fn from_native(runtime: Arc<super::native::NativeSnapshot>) -> Self {
        Self {
            runtime: Runtime::Native(runtime),
        }
    }

    pub fn backend_kind(&self) -> super::BackendKind {
        match &self.runtime {
            Runtime::Sqlite(_) => super::BackendKind::Sqlite,
            Runtime::Native(_) => super::BackendKind::NativeTurso,
        }
    }

    pub(super) fn from_runtime(runtime: ExactSqlReadSnapshot) -> Self {
        Self {
            runtime: Runtime::Sqlite(Arc::new(runtime)),
        }
    }

    pub async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        let runtime = match &self.runtime {
            Runtime::Sqlite(runtime) => Arc::clone(runtime),
            Runtime::Native(runtime) => {
                return runtime.query(sql.to_owned(), params.into_params()?).await;
            }
        };
        let statement = statement(sql, params)?;
        let rows = tokio::task::spawn_blocking(move || {
            runtime.query(statement).map_err(super::Error::from)
        })
        .await
        .map_err(join_error)??;
        Ok(Rows::from_exact(rows))
    }
}

fn join_error(error: tokio::task::JoinError) -> super::Error {
    super::Error::Runtime(format!("exact SQL read snapshot task failed: {error}"))
}
