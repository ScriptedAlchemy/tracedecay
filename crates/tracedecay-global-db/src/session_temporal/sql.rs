use tracedecay_runtime_core::db::{DatabaseEngineReadSnapshot, engine};

#[derive(Clone, Copy)]
pub(super) enum TemporalSqlRead<'a> {
    #[cfg(test)]
    EngineConnection(&'a engine::Connection),
    Registered(&'a DatabaseEngineReadSnapshot),
}

impl<'a> TemporalSqlRead<'a> {
    #[cfg(test)]
    pub(super) const fn engine_connection(read: &'a engine::Connection) -> Self {
        Self::EngineConnection(read)
    }

    pub(super) const fn registered(read: &'a DatabaseEngineReadSnapshot) -> Self {
        Self::Registered(read)
    }

    pub(super) async fn query<P>(&self, sql: &str, params: P) -> engine::Result<TemporalSqlRows>
    where
        P: engine::IntoParams,
    {
        match self {
            #[cfg(test)]
            Self::EngineConnection(read) => read.query(sql, params).await,
            Self::Registered(read) => read.query(sql, params).await,
        }
    }
}

impl engine::QueryExecutor for TemporalSqlRead<'_> {
    async fn query<P>(&self, sql: &str, params: P) -> engine::Result<engine::Rows>
    where
        P: engine::IntoParams,
    {
        TemporalSqlRead::query(self, sql, params).await
    }
}

pub(super) type TemporalSqlRows = engine::Rows;
pub(super) type TemporalSqlRow = engine::Row;
