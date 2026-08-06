use super::{Error, Result};

// Visible outside the crate: it appears in the signature of the
// fixture-facing `IntoParams` chain used by integration suites.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Integer(_) => "integer",
            Self::Real(_) => "real",
            Self::Text(_) => "text",
            Self::Blob(_) => "blob",
        }
    }
}

impl From<tracedecay_rusqlite_runtime::exact_sql::ExactSqlValue> for Value {
    fn from(value: tracedecay_rusqlite_runtime::exact_sql::ExactSqlValue) -> Self {
        use tracedecay_rusqlite_runtime::exact_sql::ExactSqlValue;

        match value {
            ExactSqlValue::Null => Self::Null,
            ExactSqlValue::Integer(value) => Self::Integer(value),
            ExactSqlValue::Real(value) => Self::Real(value),
            ExactSqlValue::Text(value) => Self::Text(value),
            ExactSqlValue::Blob(value) => Self::Blob(value),
        }
    }
}

impl From<Value> for tracedecay_rusqlite_runtime::exact_sql::ExactSqlValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Integer(value) => Self::Integer(value),
            Value::Real(value) => Self::Real(value),
            Value::Text(value) => Self::Text(value),
            Value::Blob(value) => Self::Blob(value),
        }
    }
}

pub trait FromValue: Sized {
    const EXPECTED: &'static str;

    fn from_value(value: &Value, column: i32) -> Result<Self>;
}

impl FromValue for Value {
    const EXPECTED: &'static str = "value";

    fn from_value(value: &Value, _: i32) -> Result<Self> {
        Ok(value.clone())
    }
}

impl FromValue for String {
    const EXPECTED: &'static str = "text";

    fn from_value(value: &Value, column: i32) -> Result<Self> {
        match value {
            Value::Text(value) => Ok(value.clone()),
            value => Err(type_mismatch(column, Self::EXPECTED, value)),
        }
    }
}

impl FromValue for i64 {
    const EXPECTED: &'static str = "integer";

    fn from_value(value: &Value, column: i32) -> Result<Self> {
        match value {
            Value::Integer(value) => Ok(*value),
            value => Err(type_mismatch(column, Self::EXPECTED, value)),
        }
    }
}

impl FromValue for f64 {
    const EXPECTED: &'static str = "real";

    fn from_value(value: &Value, column: i32) -> Result<Self> {
        match value {
            Value::Real(value) => Ok(*value),
            value => Err(type_mismatch(column, Self::EXPECTED, value)),
        }
    }
}

impl FromValue for Vec<u8> {
    const EXPECTED: &'static str = "blob";

    fn from_value(value: &Value, column: i32) -> Result<Self> {
        match value {
            Value::Blob(value) => Ok(value.clone()),
            value => Err(type_mismatch(column, Self::EXPECTED, value)),
        }
    }
}

impl FromValue for u32 {
    const EXPECTED: &'static str = "non-negative 32-bit integer";

    fn from_value(value: &Value, column: i32) -> Result<Self> {
        let value = i64::from_value(value, column)?;
        value.try_into().map_err(|_| Error::IntegerOutOfRange {
            column,
            target: "u32",
            value,
        })
    }
}

impl FromValue for u64 {
    const EXPECTED: &'static str = "non-negative 64-bit integer";

    fn from_value(value: &Value, column: i32) -> Result<Self> {
        let value = i64::from_value(value, column)?;
        value.try_into().map_err(|_| Error::IntegerOutOfRange {
            column,
            target: "u64",
            value,
        })
    }
}

impl<T: FromValue> FromValue for Option<T> {
    const EXPECTED: &'static str = T::EXPECTED;

    fn from_value(value: &Value, column: i32) -> Result<Self> {
        match value {
            Value::Null => Ok(None),
            value => T::from_value(value, column).map(Some),
        }
    }
}

fn type_mismatch(column: i32, expected: &'static str, value: &Value) -> Error {
    Error::TypeMismatch {
        column,
        expected,
        actual: value.kind(),
    }
}
