use napi_derive::napi;

#[napi]
pub async fn ping(value: u32) -> u32 {
    value + 1
}
