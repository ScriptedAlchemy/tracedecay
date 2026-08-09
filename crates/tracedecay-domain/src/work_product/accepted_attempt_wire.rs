use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use super::{AcceptedAttemptWireV1, TaskEvidenceLinkId, WorkAttemptIdentityV1};

pub fn serialize<S>(
    attempts: &BTreeMap<WorkAttemptIdentityV1, TaskEvidenceLinkId>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    attempts
        .iter()
        .map(|(identity, link_id)| AcceptedAttemptWireV1 {
            identity: identity.clone(),
            link_id: link_id.clone(),
        })
        .collect::<Vec<_>>()
        .serialize(serializer)
}

pub fn deserialize<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<WorkAttemptIdentityV1, TaskEvidenceLinkId>, D::Error>
where
    D: Deserializer<'de>,
{
    let entries = Vec::<AcceptedAttemptWireV1>::deserialize(deserializer)?;
    let mut attempts = BTreeMap::new();
    for entry in entries {
        if attempts.insert(entry.identity, entry.link_id).is_some() {
            return Err(de::Error::custom("duplicate accepted attempt identity"));
        }
    }
    Ok(attempts)
}
