//! Crate-private ExactSql value constructors and error-agnostic row accessors.

use super::ExactSqlValue;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExactSqlColumnError {
    pub index: usize,
    pub expected: &'static str,
}

impl ExactSqlColumnError {
    const fn new(index: usize, expected: &'static str) -> Self {
        Self { index, expected }
    }

    pub(crate) fn missing(self, values: &[ExactSqlValue]) -> bool {
        self.index >= values.len()
    }
}

pub(crate) fn text(value: impl Into<String>) -> ExactSqlValue {
    ExactSqlValue::Text(value.into())
}

pub(crate) fn optional_text(value: Option<impl Into<String>>) -> ExactSqlValue {
    value.map_or(ExactSqlValue::Null, |value| {
        ExactSqlValue::Text(value.into())
    })
}

pub(crate) fn text_at(values: &[ExactSqlValue], index: usize) -> Result<&str, ExactSqlColumnError> {
    match values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(value),
        _ => Err(ExactSqlColumnError::new(index, "text")),
    }
}

pub(crate) fn optional_text_at(
    values: &[ExactSqlValue],
    index: usize,
) -> Result<Option<&str>, ExactSqlColumnError> {
    match values.get(index) {
        Some(ExactSqlValue::Text(value)) => Ok(Some(value)),
        Some(ExactSqlValue::Null) => Ok(None),
        _ => Err(ExactSqlColumnError::new(index, "optional text")),
    }
}

pub(crate) fn integer_at(
    values: &[ExactSqlValue],
    index: usize,
) -> Result<i64, ExactSqlColumnError> {
    match values.get(index) {
        Some(ExactSqlValue::Integer(value)) => Ok(*value),
        _ => Err(ExactSqlColumnError::new(index, "integer")),
    }
}

pub(crate) fn text_column(values: &[ExactSqlValue], index: usize) -> Option<&str> {
    text_at(values, index).ok()
}

pub(crate) fn integer_column(values: &[ExactSqlValue], index: usize) -> Option<i64> {
    integer_at(values, index).ok()
}

pub(crate) fn take_text(
    values: &mut [ExactSqlValue],
    index: usize,
) -> Result<String, ExactSqlColumnError> {
    match values.get_mut(index) {
        Some(ExactSqlValue::Text(value)) => Ok(std::mem::take(value)),
        _ => Err(ExactSqlColumnError::new(index, "text")),
    }
}

pub(crate) fn take_optional_text(
    values: &mut [ExactSqlValue],
    index: usize,
) -> Result<Option<String>, ExactSqlColumnError> {
    match values.get_mut(index) {
        Some(ExactSqlValue::Text(value)) => Ok(Some(std::mem::take(value))),
        Some(ExactSqlValue::Null) => Ok(None),
        _ => Err(ExactSqlColumnError::new(index, "optional text")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_accessors_cover_present_null_and_wrong_type() {
        let present = [
            ExactSqlValue::Text("ok".to_owned()),
            ExactSqlValue::Integer(7),
        ];
        assert_eq!(text_at(&present, 0), Ok("ok"));
        assert_eq!(optional_text_at(&present, 0), Ok(Some("ok")));
        assert_eq!(integer_at(&present, 1), Ok(7));

        let null = [ExactSqlValue::Null];
        assert_eq!(optional_text_at(&null, 0), Ok(None));
        assert_eq!(text_at(&null, 0), Err(ExactSqlColumnError::new(0, "text")));
        assert_eq!(
            integer_at(&null, 0),
            Err(ExactSqlColumnError::new(0, "integer"))
        );

        let wrong = [ExactSqlValue::Integer(1)];
        assert_eq!(text_at(&wrong, 0), Err(ExactSqlColumnError::new(0, "text")));
        assert_eq!(
            optional_text_at(&wrong, 0),
            Err(ExactSqlColumnError::new(0, "optional text"))
        );
        assert!(!text_at(&wrong, 0).expect_err("wrong type").missing(&wrong));
        assert!(text_at(&[], 0).expect_err("missing").missing(&[]));
    }
}
