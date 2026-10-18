use super::{ParseOffset, RegisteredGlobalDb};
use tracedecay_sessions::runtime::{SessionRecord, SessionStoreAccess, TranscriptPersistenceError};

pub(super) use tracedecay_sessions::runtime::store_access::{
    require_expected_offset, set_parse_offset,
};

impl RegisteredGlobalDb {
    #[tracing::instrument(
        name = "global_db.transcript.upsert_session",
        level = "trace",
        skip_all
    )]
    pub async fn upsert_session(&self, session: &SessionRecord) -> bool {
        SessionStoreAccess::new(self).upsert_session(session).await
    }

    #[tracing::instrument(
        name = "global_db.transcript.upsert_sessions",
        level = "trace",
        skip_all
    )]
    pub async fn upsert_sessions(&self, sessions: &[SessionRecord]) -> bool {
        SessionStoreAccess::new(self)
            .upsert_sessions(sessions)
            .await
    }

    #[tracing::instrument(name = "global_db.transcript.get_session", level = "trace", skip_all)]
    pub async fn get_session(
        &self,
        provider: &str,
        session_id: &str,
    ) -> Result<Option<SessionRecord>, TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .get_session(provider, session_id)
            .await
    }

    #[tracing::instrument(
        name = "global_db.transcript.persist_offset",
        level = "trace",
        skip_all
    )]
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

    #[tracing::instrument(
        name = "global_db.transcript.get_parse_offset",
        level = "trace",
        skip_all
    )]
    pub async fn get_parse_offset(
        &self,
        path: &str,
    ) -> Result<Option<ParseOffset>, TranscriptPersistenceError> {
        SessionStoreAccess::new(self).get_parse_offset(path).await
    }

    #[tracing::instrument(
        name = "global_db.transcript.set_parse_offset",
        level = "trace",
        skip_all
    )]
    pub async fn set_parse_offset(&self, path: &str, offset: ParseOffset) -> Result<(), String> {
        SessionStoreAccess::new(self)
            .set_parse_offset(path, offset)
            .await
    }

    #[tracing::instrument(
        name = "global_db.transcript.advance_parse_offset",
        level = "trace",
        skip_all
    )]
    pub async fn advance_parse_offset_result(
        &self,
        path: &str,
        offset: ParseOffset,
    ) -> Result<(), TranscriptPersistenceError> {
        SessionStoreAccess::new(self)
            .advance_parse_offset_result(path, offset)
            .await
    }

    #[tracing::instrument(
        name = "global_db.transcript.replace_parse_offset",
        level = "trace",
        skip_all
    )]
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

    #[tracing::instrument(
        name = "global_db.transcript.replace_parse_offset_pair",
        level = "trace",
        skip_all
    )]
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
