use std::cmp;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tracedecay_domain::SignedCursorKeyRefV1;

use tracedecay_temporal_query::ports::{PageKey, PageRequest, TemporalPortError};

use super::super::sql::TemporalSqlRow;
use super::rows::*;
use super::{CANDIDATE_OPERATION, MIN_CURSOR_CAPACITY, RECORD_OPERATION};

pub(super) fn decode_cursor<T: DeserializeOwned>(
    key: &PageKey,
    operation: &'static str,
) -> Result<T, TemporalPortError> {
    serde_json::from_str(key.as_str()).map_err(|error| read_error(operation, error))
}

#[derive(Deserialize)]
pub(super) struct FrozenWatermarksWire {
    pub(super) active_generation: u64,
    pub(super) cursor_key: Option<SignedCursorKeyRefV1>,
    pub(super) projection_frontier: u64,
    pub(super) source_frontier: u64,
    pub(super) summary_frontier: u64,
}

pub(super) fn encode_cursor(
    cursor: &impl Serialize,
    cap: usize,
    operation: &'static str,
) -> Result<PageKey, TemporalPortError> {
    let encoded = serde_json::to_string(cursor).map_err(|error| read_error(operation, error))?;
    if encoded.len() > cap {
        return Err(TemporalPortError::BudgetExceeded {
            resource: "continuation key bytes",
        });
    }
    Ok(PageKey::new(encoded))
}

impl PageBounds {
    pub(super) fn from_request(request: &PageRequest) -> Result<Self, TemporalPortError> {
        if request
            .keyset()
            .is_some_and(|key| key.as_str().len() > request.max_key_bytes())
        {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "continuation key bytes",
            });
        }
        if request.max_key_bytes() < MIN_CURSOR_CAPACITY {
            return Err(TemporalPortError::BudgetExceeded {
                resource: "continuation key capacity",
            });
        }
        Ok(Self {
            items: cmp::min(request.remaining_items(), request.page_item_limit()),
            bytes: cmp::min(
                request.remaining_total_bytes(),
                request.page_total_byte_limit(),
            ),
        })
    }
}

impl CandidateCursor {
    pub(super) fn decode(key: Option<&PageKey>) -> Result<Self, TemporalPortError> {
        key.map_or(
            Ok(Self {
                clause: 0,
                knowledge_at: i64::MAX,
                session_id: String::new(),
                stable_id: String::new(),
            }),
            |key| decode_cursor(key, CANDIDATE_OPERATION),
        )
    }

    pub(super) fn encode(&self, cap: usize) -> Result<PageKey, TemporalPortError> {
        encode_cursor(self, cap, CANDIDATE_OPERATION)
    }
}

impl RecordCursor {
    pub(super) fn decode(key: Option<&PageKey>) -> Result<Self, TemporalPortError> {
        key.map_or(
            Ok(Self {
                candidate: 0,
                kind: 0,
                session_id: String::new(),
                stable_id: String::new(),
            }),
            |key| decode_cursor(key, RECORD_OPERATION),
        )
    }

    pub(super) fn encode(&self, cap: usize) -> Result<PageKey, TemporalPortError> {
        encode_cursor(self, cap, RECORD_OPERATION)
    }

    pub(super) fn from_row(row: &TemporalSqlRow) -> Result<Self, TemporalPortError> {
        let candidate: i64 = row
            .get(0)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
        Ok(Self {
            candidate: usize::try_from(candidate)
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
            kind: row
                .get(1)
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
            session_id: row
                .get(15)
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
            stable_id: row
                .get(2)
                .map_err(|error| read_error(RECORD_OPERATION, error))?,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct PageBounds {
    pub(super) items: usize,
    pub(super) bytes: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct CandidateCursor {
    pub(super) clause: usize,
    pub(super) knowledge_at: i64,
    #[serde(default)]
    pub(super) session_id: String,
    pub(super) stable_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct RecordCursor {
    pub(super) candidate: usize,
    pub(super) kind: i64,
    #[serde(default)]
    pub(super) session_id: String,
    pub(super) stable_id: String,
}
