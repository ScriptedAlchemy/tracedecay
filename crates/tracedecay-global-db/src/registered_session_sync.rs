use tracedecay_domain::errors::TraceDecayError;
use tracedecay_sessions::runtime::SessionStoreAccess;

use crate::RegisteredGlobalDb;

impl RegisteredGlobalDb {
    #[hotpath::skip]
    pub async fn list_session_sync_source_frontiers(
        &self,
    ) -> Result<Vec<(String, String, String)>, TraceDecayError> {
        SessionStoreAccess::new(self)
            .list_session_sync_source_frontiers()
            .await
    }

    #[hotpath::skip]
    pub async fn read_session_sync_journal(
        &self,
        key: &str,
    ) -> Result<Option<String>, TraceDecayError> {
        SessionStoreAccess::new(self)
            .read_session_sync_journal(key)
            .await
    }

    #[hotpath::skip]
    pub async fn list_session_sync_journals(
        &self,
        key_prefix: &str,
    ) -> Result<Vec<(String, String)>, TraceDecayError> {
        SessionStoreAccess::new(self)
            .list_session_sync_journals(key_prefix)
            .await
    }

    #[hotpath::skip]
    pub async fn list_incomplete_session_sync_journal_page(
        &self,
        key_prefix: &str,
        after_key: Option<&str>,
    ) -> Result<Vec<(String, String)>, TraceDecayError> {
        SessionStoreAccess::new(self)
            .list_incomplete_session_sync_journal_page(key_prefix, after_key)
            .await
    }

    #[hotpath::skip]
    pub async fn list_incomplete_session_sync_journal_page_through(
        &self,
        key_prefix: &str,
        after_key: Option<&str>,
        through_key: &str,
    ) -> Result<Vec<(String, String)>, TraceDecayError> {
        SessionStoreAccess::new(self)
            .list_incomplete_session_sync_journal_page_through(key_prefix, after_key, through_key)
            .await
    }

    #[hotpath::skip]
    pub async fn session_sync_journal_high_water(
        &self,
        key_prefix: &str,
    ) -> Result<Option<String>, TraceDecayError> {
        SessionStoreAccess::new(self)
            .session_sync_journal_high_water(key_prefix)
            .await
    }

    #[hotpath::skip]
    pub async fn insert_session_sync_journal(
        &self,
        key: &str,
        value: &str,
    ) -> Result<bool, TraceDecayError> {
        crate::hotpath_observe::record_transaction_rows(1);
        SessionStoreAccess::new(self)
            .insert_session_sync_journal(key, value)
            .await
    }

    #[hotpath::skip]
    pub async fn compare_and_swap_session_sync_journal(
        &self,
        key: &str,
        expected: &str,
        replacement: &str,
    ) -> Result<bool, TraceDecayError> {
        crate::hotpath_observe::record_transaction_rows(1);
        SessionStoreAccess::new(self)
            .compare_and_swap_session_sync_journal(key, expected, replacement)
            .await
    }

    #[hotpath::skip]
    pub async fn compare_and_delete_session_sync_journal(
        &self,
        key: &str,
        expected: &str,
    ) -> Result<bool, TraceDecayError> {
        crate::hotpath_observe::record_transaction_rows(1);
        SessionStoreAccess::new(self)
            .compare_and_delete_session_sync_journal(key, expected)
            .await
    }
}
