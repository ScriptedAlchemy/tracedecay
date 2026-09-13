use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use super::WorkAttemptIdentityV1;

pub fn serialize<S>(
    attempts: &BTreeSet<WorkAttemptIdentityV1>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    attempts.iter().collect::<Vec<_>>().serialize(serializer)
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<BTreeSet<WorkAttemptIdentityV1>, D::Error>
where
    D: Deserializer<'de>,
{
    let entries = Vec::<WorkAttemptIdentityV1>::deserialize(deserializer)?;
    let mut attempts = BTreeSet::new();
    for identity in entries {
        if !attempts.insert(identity) {
            return Err(de::Error::custom("duplicate accepted attempt identity"));
        }
    }
    Ok(attempts)
}
