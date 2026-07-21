use crate::global_db::{GlobalDb, GlobalDbSessionTemporalExecution};
use crate::store::{GlobalDbSessionTemporalStore, GlobalDbTranscriptStore};

/// Borrowed facade for the established `GlobalDb` compatibility ports.
pub(crate) struct GlobalDbRuntime<'db> {
    db: &'db GlobalDb,
}

impl<'db> GlobalDbRuntime<'db> {
    pub(crate) const fn new(db: &'db GlobalDb) -> Self {
        Self { db }
    }

    pub(crate) const fn profile_project(&self) -> &'db GlobalDb {
        self.db
    }

    pub(crate) const fn transcript_store(&self) -> GlobalDbTranscriptStore<'db> {
        GlobalDbTranscriptStore::new(self.db)
    }

    pub(crate) const fn session_store(&self) -> GlobalDbSessionTemporalStore<'db> {
        GlobalDbSessionTemporalStore::new(self.db)
    }

    pub(crate) const fn session_execution(&self) -> GlobalDbSessionTemporalExecution<'db> {
        GlobalDbSessionTemporalExecution::new(self.db)
    }
}

#[cfg(test)]
mod tests {
    use tracedecay_store::{
        SessionRefreshStore, SessionTemporalCapabilityProvider, SessionTemporalProjectionStore,
        TranscriptStore,
    };

    use crate::application::session::SessionTemporalExecutionPort;

    use super::*;

    #[test]
    fn facade_delegates_to_existing_ports() {
        fn assert_transcript_port<T: TranscriptStore>(_: &T) {}
        fn assert_session_port<
            T: SessionTemporalCapabilityProvider
                + SessionTemporalProjectionStore
                + SessionRefreshStore,
        >(
            _: &T,
        ) {
        }
        fn assert_execution_port<T: SessionTemporalExecutionPort>(_: &T) {}

        fn assert_facade<'db>(db: &'db GlobalDb) {
            let facade = GlobalDbRuntime::new(db);
            let _: &'db GlobalDb = facade.profile_project();
            assert_transcript_port(&facade.transcript_store());
            assert_session_port(&facade.session_store());
            assert_execution_port(&facade.session_execution());
        }

        let _ = assert_facade;
    }
}
