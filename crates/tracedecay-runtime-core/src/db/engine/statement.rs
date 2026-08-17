use super::{Connection, IntoParams, Result};

pub struct Statement<'a> {
    connection: &'a Connection,
    sql: String,
}

impl<'a> Statement<'a> {
    pub(super) fn for_connection(connection: &'a Connection, sql: &str) -> Result<Self> {
        validate_sql(sql)?;
        Ok(Self {
            connection,
            sql: sql.to_owned(),
        })
    }

    pub async fn execute<P>(&self, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        self.connection.execute(&self.sql, params).await
    }

    /// Execution owns no `SQLite` cursor or bound parameter state: every call is
    /// submitted as a fresh runtime request. Reset therefore preserves the
    /// statement-reuse contract without touching the writer or reader pool.
    pub fn reset(&self) {}
}

fn validate_sql(sql: &str) -> Result<()> {
    if sql.trim().is_empty() {
        Err(super::Error::InvalidOperation(
            "prepared SQL statement is empty".to_owned(),
        ))
    } else {
        Ok(())
    }
}
