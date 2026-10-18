//! Shared JSON and text-column decode for durable SQL rows.
//!
//! Git-index, native integration, and the fact store used to repeat the same
//! `serde_json` and `row.get` mappings. The column message names the field so
//! a missing cell is the same failure in every caller.

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::engine::Row;

pub fn encode_stored_json<T: Serialize + ?Sized>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

pub fn decode_stored_json<T: DeserializeOwned>(value: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(value)
}

pub fn text_column(row: &Row, column: i32, field: &'static str) -> Result<String, String> {
    row.get::<String>(column)
        .map_err(|error| format!("read {field}: {error}"))
}

pub fn optional_text_column(
    row: &Row,
    column: i32,
    field: &'static str,
) -> Result<Option<String>, String> {
    row.get::<Option<String>>(column)
        .map_err(|error| format!("read {field}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::super::engine::{Row, Value};
    use super::{optional_text_column, text_column};

    #[test]
    fn text_column_names_the_field_and_keeps_null_optional() {
        let row = Row::from_values(vec![Value::Text("kept".to_owned()), Value::Null]);
        assert_eq!(text_column(&row, 0, "preview").expect("text"), "kept");
        assert_eq!(
            text_column(&row, 2, "preview").expect_err("missing"),
            "read preview: invalid column index 2"
        );
        assert_eq!(
            optional_text_column(&row, 1, "receipt").expect("null"),
            None
        );
    }
}
