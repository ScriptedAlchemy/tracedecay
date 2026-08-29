pub use tracedecay_session_temporal_store::{
    SessionRefreshRecoveryV1, SessionRefreshRestartStateV1,
};

pub type GlobalDbSessionTemporalStore<'a> =
    tracedecay_session_temporal_store::GlobalDbSessionTemporalStore<
        'a,
        tracedecay_global_db::RegisteredGlobalDb,
    >;
