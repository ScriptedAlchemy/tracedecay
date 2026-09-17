use tracedecay_rusqlite_runtime::exact_sql::ExactSqlStatement;

use super::{IntoParams, Result, Value};

/// One owned parameterized write ready for batched executor submission.
///
/// Construction performs the same request validation as scalar execution, so
/// a malformed statement is attributed before any batch reaches the writer.
pub struct WriteStatement {
    exact: ExactSqlStatement,
}

impl WriteStatement {
    pub fn new<P>(sql: impl Into<String>, params: P) -> Result<Self>
    where
        P: IntoParams,
    {
        Ok(Self {
            exact: ExactSqlStatement::new(
                sql.into(),
                params.into_params()?.into_iter().map(Into::into).collect(),
            )?,
        })
    }

    pub(crate) fn into_exact(self) -> ExactSqlStatement {
        self.exact
    }

    pub(crate) fn into_parts(self) -> (String, Vec<Value>) {
        (
            self.exact.sql,
            self.exact.params.into_iter().map(Value::from).collect(),
        )
    }
}
