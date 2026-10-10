use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    pub columns: Vec<String>,
    pub values: Vec<Vec<Value>>,
}

impl Value {
    pub(crate) fn to_native(&self) -> Result<turso_core::Value> {
        Ok(match self {
            Self::Null => turso_core::Value::Null,
            Self::Integer(value) => turso_core::Value::from_i64(*value),
            Self::Real(value) => turso_core::Value::from_f64(*value),
            Self::Text(value) => turso_core::Value::build_text(value.clone()),
            Self::Blob(value) => turso_core::Value::from_slice(value)
                .map_err(|error| Error::InvalidOperation(error.to_string()))?,
        })
    }

    pub(crate) fn from_native(value: &turso_core::Value) -> Self {
        match value {
            turso_core::Value::Null => Self::Null,
            turso_core::Value::Numeric(turso_core::Numeric::Integer(value)) => {
                Self::Integer(*value)
            }
            turso_core::Value::Numeric(turso_core::Numeric::Float(value)) => {
                Self::Real(f64::from(*value))
            }
            turso_core::Value::Text(value) => Self::Text(value.as_str().to_owned()),
            turso_core::Value::Blob(value) => Self::Blob(value.to_vec()),
        }
    }
}
