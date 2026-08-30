use super::{ParseOffset, RegisteredGlobalDb, TranscriptBatch};
use tracedecay_sessions::runtime::{
    SessionMessageRecord, SessionRecord, SessionStoreAccess, TranscriptPersistenceError,
};

pub(super) use tracedecay_sessions::runtime::store_access::{
    require_expected_offset, set_parse_offset,
};

impl RegisteredGlobalDb {
    pub async fn upsert_session(&self, session: &SessionRecord) -> bool {
        SessionStoreAccess::new(self).upsert_session(session).await
    }

    pub async fn get_session(&self, provider: &str, session_id: &str) -> Option<SessionRecord> {
        SessionStoreAccess::new(self)
            .get_session(provider, session_id)
            .await
    }

    pub async fn get_session_result(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Result<Option<SessionRecord>, TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .get_session_result(provider, session_id)
            .await
    }

    pub async fn upsert_transcript_batch(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
    ) -> bool {
        SessionStoreAccess::new(self)
            .upsert_transcript_batch(session, messages, parse_offset_path, parse_offset)
            .await
    }

    pub async fn persist_transcript_batch_result(
        &self,
        session: &SessionRecord,
        messages: &[SessionMessageRecord],
        parse_offset_path: &str,
        expected_offset: ParseOffset,
        parse_offset: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .persist_transcript_batch_result(
                session,
                messages,
                parse_offset_path,
                expected_offset,
                parse_offset,
            )
            .await
    }

    pub async fn persist_transcript_offset_result(
        &self,
        parse_offset_path: &str,
        expected_offset: ParseOffset,
        parse_offset: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .persist_transcript_offset_result(parse_offset_path, expected_offset, parse_offset)
            .await
    }

    pub async fn upsert_transcript_projection_batches(
        &self,
        batches: &[TranscriptBatch],
        parse_offset_path: &str,
        parse_offset: ParseOffset,
    ) -> Result<(), String> {
        SessionStoreAccess::new(self)
            .upsert_transcript_projection_batches(batches, parse_offset_path, parse_offset)
            .await
    }

    pub async fn get_parse_offset(&self, path: &str) -> Option<ParseOffset> {
        SessionStoreAccess::new(self).get_parse_offset(path).await
    }

    pub async fn get_parse_offset_result(
        &self,
        path: &str,
    ) -> Result<Option<ParseOffset>, TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .get_parse_offset_result(path)
            .await
    }

    pub async fn set_parse_offset(&self, path: &str, offset: ParseOffset) -> Result<(), String> {
        SessionStoreAccess::new(self)
            .set_parse_offset(path, offset)
            .await
    }

    pub async fn advance_parse_offset_result(
        &self,
        path: &str,
        offset: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .advance_parse_offset_result(path, offset)
            .await
    }

    pub async fn replace_parse_offset_result(
        &self,
        path: &str,
        expected: ParseOffset,
        next: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .replace_parse_offset_result(path, expected, next)
            .await
    }

    pub async fn replace_parse_offset_pair_result(
        &self,
        first: (&str, ParseOffset, ParseOffset),
        second: (&str, ParseOffset, ParseOffset),
    ) -> Result<(), TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .replace_parse_offset_pair_result(first, second)
            .await
    }
}
