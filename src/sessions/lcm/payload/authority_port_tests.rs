//! The storage-root-bound LCM store drives its authorities through the narrow
//! ports only, so it can be exercised with no database and no payload files.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::sessions::SessionMessageRecord;
use crate::sessions::lcm::{LcmError, LcmPayloadExpansion};

use super::{LcmPayloadAuthorityPort, LcmPayloadExpandRequest, LcmRawMessagePort, LcmStore};

#[derive(Debug, PartialEq, Eq)]
struct RecordedExpansion {
    storage_root: PathBuf,
    provider: String,
    session_id: String,
    payload_ref: String,
    offset: usize,
    limit: usize,
}

#[derive(Default)]
struct RecordingAuthority {
    ingested: RefCell<Vec<(PathBuf, String)>>,
    expansions: RefCell<Vec<RecordedExpansion>>,
}

impl LcmRawMessagePort for RecordingAuthority {
    async fn ingest_raw_message(
        &self,
        storage_root: &Path,
        message: &SessionMessageRecord,
    ) -> Result<(), LcmError> {
        self.ingested
            .borrow_mut()
            .push((storage_root.to_path_buf(), message.message_id.clone()));
        Ok(())
    }
}

impl LcmPayloadAuthorityPort for RecordingAuthority {
    async fn expand_payload(
        &self,
        storage_root: &Path,
        request: LcmPayloadExpandRequest<'_>,
    ) -> Result<LcmPayloadExpansion, LcmError> {
        self.expansions.borrow_mut().push(RecordedExpansion {
            storage_root: storage_root.to_path_buf(),
            provider: request.provider.to_string(),
            session_id: request.session_id.to_string(),
            payload_ref: request.payload_ref.to_string(),
            offset: request.offset,
            limit: request.limit,
        });
        Err(LcmError::PayloadNotFound)
    }
}

fn message(message_id: &str) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: "cursor".to_string(),
        message_id: message_id.to_string(),
        session_id: "session-1".to_string(),
        role: "assistant".to_string(),
        timestamp: None,
        ordinal: 1,
        text: "message body".to_string(),
        kind: None,
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: None,
    }
}

#[tokio::test]
async fn store_routes_raw_ingest_through_the_bound_storage_root() {
    let authority = RecordingAuthority::default();
    let storage_root = PathBuf::from("/profile/.tracedecay");
    let store = LcmStore::new(&authority, storage_root.clone());

    store
        .ingest_raw_message(&message("message-1"))
        .await
        .expect("recording authority admits the message");

    assert_eq!(
        *authority.ingested.borrow(),
        vec![(storage_root, "message-1".to_string())]
    );
}

#[tokio::test]
async fn store_forwards_payload_expansion_arguments_verbatim() {
    let authority = RecordingAuthority::default();
    let storage_root = PathBuf::from("/profile/.tracedecay");
    let store = LcmStore::new(&authority, storage_root.clone());

    let outcome = store
        .lcm_expand_payload("cursor", "session-1", "payload_abc.payload", 12, 34)
        .await;

    assert_eq!(
        outcome.expect_err("the authority owns the payload miss"),
        LcmError::PayloadNotFound
    );
    assert_eq!(
        *authority.expansions.borrow(),
        vec![RecordedExpansion {
            storage_root,
            provider: "cursor".to_string(),
            session_id: "session-1".to_string(),
            payload_ref: "payload_abc.payload".to_string(),
            offset: 12,
            limit: 34,
        }]
    );
}
