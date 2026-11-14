use super::{
    Connection, Error, IntoParams, ReadConnection, ReadSnapshot, Result, Rows, Transaction,
    WriteStatement,
};

#[allow(async_fn_in_trait)]
pub trait QueryExecutor {
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams;
}

impl QueryExecutor for Connection {
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Connection::query(self, sql, params).await
    }
}

impl QueryExecutor for ReadConnection {
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        ReadConnection::query(self, sql, params).await
    }
}

impl QueryExecutor for Transaction {
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        Transaction::query(self, sql, params).await
    }
}

impl QueryExecutor for ReadSnapshot {
    async fn query<P>(&self, sql: &str, params: P) -> Result<Rows>
    where
        P: IntoParams,
    {
        ReadSnapshot::query(self, sql, params).await
    }
}

#[allow(async_fn_in_trait)]
pub trait Executor: QueryExecutor {
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams;
    /// Executes owned parameterized writes in input order.
    ///
    /// There is deliberately no default: an adapter over a batching executor
    /// must forward to it, and a silent per-statement fallback would turn a
    /// bounded page into one writer admission per statement. Implementations
    /// whose backend cannot batch call [`execute_statements_serially`].
    async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>>;

    async fn execute_batch(&self, sql: &str) -> Result<()>;
}

impl Executor for Connection {
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        Connection::execute(self, sql, params).await
    }

    async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>> {
        Connection::execute_statements(self, statements).await
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        Connection::execute_batch(self, sql).await
    }
}

impl Executor for Transaction {
    async fn execute<P>(&self, sql: &str, params: P) -> Result<u64>
    where
        P: IntoParams,
    {
        Transaction::execute(self, sql, params).await
    }

    async fn execute_statements(&self, statements: Vec<WriteStatement>) -> Result<Vec<u64>> {
        Transaction::execute_statements(self, statements).await
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        Transaction::execute_batch(self, sql).await
    }
}

/// Executes `statements` in input order with one statement per call.
///
/// Only for executors whose backend has no batched submission. An executor
/// that wraps a batching implementation must forward to it instead, because
/// this helper admits the writer once per statement.
pub async fn execute_statements_serially<E>(
    executor: &E,
    statements: Vec<WriteStatement>,
) -> Result<Vec<u64>>
where
    E: Executor + ?Sized,
{
    let mut results = Vec::with_capacity(statements.len());
    for (index, statement) in statements.into_iter().enumerate() {
        let (sql, params) = statement.into_parts();
        results.push(
            executor
                .execute(&sql, params)
                .await
                .map_err(|error| Error::statement_batch(index, error))?,
        );
    }
    Ok(results)
}
