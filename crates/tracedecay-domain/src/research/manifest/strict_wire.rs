use std::fmt;

use serde::Deserialize;
use serde::de::{MapAccess, SeqAccess, Visitor};

/// A JSON value whose object keys are checked before `serde_json::Value`
/// normalizes maps and discards duplicate keys.
pub(super) struct CheckedJsonValue(pub(super) serde_json::Value);

impl<'de> Deserialize<'de> for CheckedJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ValueVisitor;

        impl<'de> Visitor<'de> for ValueVisitor {
            type Value = CheckedJsonValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
                Ok(CheckedJsonValue(value.into()))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(CheckedJsonValue(value.into()))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(CheckedJsonValue(value.into()))
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                serde_json::Number::from_f64(value)
                    .map(serde_json::Value::Number)
                    .map(CheckedJsonValue)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(CheckedJsonValue(value.into()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(CheckedJsonValue(value.into()))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(CheckedJsonValue(serde_json::Value::Null))
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                CheckedJsonValue::deserialize(deserializer)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(CheckedJsonValue(serde_json::Value::Null))
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<CheckedJsonValue>()? {
                    values.push(value.0);
                }
                Ok(CheckedJsonValue(values.into()))
            }

            fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = serde_json::Map::new();
                while let Some(key) = entries.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!("duplicate field `{key}`")));
                    }
                    let value = entries.next_value::<CheckedJsonValue>()?;
                    values.insert(key, value.0);
                }
                Ok(CheckedJsonValue(values.into()))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_keys_recursively() {
        let error = serde_json::from_str::<CheckedJsonValue>(
            r#"{"dynamic":{"nested":[{"key":1,"key":2}]}}"#,
        )
        .err()
        .expect("duplicate nested key must fail");

        assert!(error.to_string().contains("duplicate field `key`"));
    }
}
