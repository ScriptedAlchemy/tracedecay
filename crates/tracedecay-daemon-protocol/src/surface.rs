//! Wire DTOs that the daemon invocation contract carries but that cannot live
//! in `tracedecay-contracts` without inverting the crate DAG.

use serde::{Deserialize, Serialize};
use tracedecay_contracts::git::GitReadRequestV1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitReadSurfaceRequest {
    pub request: GitReadRequestV1,
    pub max_entries: u32,
    pub max_bytes: u64,
}
