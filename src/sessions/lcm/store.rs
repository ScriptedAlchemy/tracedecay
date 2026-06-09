use libsql::Connection;

use crate::sessions::SessionMessageRecord;

pub(crate) struct LcmStore<'db> {
    conn: &'db Connection,
}

impl<'db> LcmStore<'db> {
    pub(crate) fn new(conn: &'db Connection) -> Self {
        Self { conn }
    }

    pub(crate) async fn ingest_raw_message(&self, message: &SessionMessageRecord) -> bool {
        super::raw::upsert_raw_message(self.conn, message).await
    }
}
