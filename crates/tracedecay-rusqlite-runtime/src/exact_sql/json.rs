use serde::{Serialize, de::DeserializeOwned};

pub(crate) fn encode<T, E>(
    value: &T,
    map_error: impl FnOnce(serde_json::Error) -> E,
) -> Result<String, E>
where
    T: Serialize + ?Sized,
{
    serde_json::to_string(value).map_err(map_error)
}

pub(crate) fn decode<T, E>(
    payload: &str,
    map_error: impl FnOnce(serde_json::Error) -> E,
) -> Result<T, E>
where
    T: DeserializeOwned,
{
    serde_json::from_str(payload).map_err(map_error)
}
