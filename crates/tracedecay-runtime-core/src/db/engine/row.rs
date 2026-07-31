use std::collections::VecDeque;

use super::value::FromValue;
use super::{Error, Result, Value};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    values: Vec<Value>,
}

impl Row {
    pub(crate) fn from_values(values: Vec<Value>) -> Self {
        Self { values }
    }

    pub(crate) fn get<T: FromValue>(&self, column: i32) -> Result<T> {
        let index = usize::try_from(column).map_err(|_| Error::InvalidColumn(column))?;
        let value = self.values.get(index).ok_or(Error::InvalidColumn(column))?;
        T::from_value(value, column)
    }
}

#[derive(Debug)]
pub(crate) struct Rows {
    columns: Vec<String>,
    rows: VecDeque<Row>,
}

impl Rows {
    pub(crate) fn from_rows(rows: Vec<Row>) -> Self {
        Self::from_parts(Vec::new(), rows)
    }

    pub(crate) fn from_parts(columns: Vec<String>, rows: Vec<Row>) -> Self {
        Self {
            columns,
            rows: rows.into(),
        }
    }

    pub(crate) fn column_count(&self) -> i32 {
        i32::try_from(self.columns.len()).unwrap_or(i32::MAX)
    }

    pub(crate) fn column_name(&self, index: i32) -> Option<&str> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.columns.get(index))
            .map(String::as_str)
    }

    pub(crate) async fn next(&mut self) -> Result<Option<Row>> {
        Ok(self.rows.pop_front())
    }
}
