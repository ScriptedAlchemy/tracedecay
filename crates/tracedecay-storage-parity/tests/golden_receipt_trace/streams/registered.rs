//! Streams over registered stores: the production registry publishes each
//! shard, installs its real schema, and routes submits to its writer.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tracedecay_domain::{BrainId, UserProfileId};
use tracedecay_runtime_core::db::{
    Database, DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
    TestRuntimeProfileIdentityV1, engine::Params,
};
use tracedecay_store::{RepositoryWritePayloadV1, StoreRuntimeBindingV1};

use super::super::{
    fixtures::{BRAIN, PROFILE},
    harness::{Probe, Trace, submit_request},
};

pub struct Registered {
    pub database: Database,
    pub binding: StoreRuntimeBindingV1,
    pub trace: Trace,
    path: PathBuf,
    _authority: DatabaseAuthority,
    _directory: TempDir,
}

impl Registered {
    pub async fn publish(file: &str, scope: TestDatabaseRuntimeScope) -> Self {
        let directory = tempfile::tempdir().expect("registered store directory");
        let path = directory.path().join(file);
        let authority =
            DatabaseAuthority::acquire_test(&path, "golden receipt trace").expect("test authority");
        let identity = TestRuntimeProfileIdentityV1::new(
            BrainId::new(BRAIN).unwrap(),
            UserProfileId::new(PROFILE).unwrap(),
        );
        let (database, _) = Database::publish_registered_test_runtime_for_profile_identity(
            &path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
            identity,
            scope,
        )
        .await
        .unwrap_or_else(|error| panic!("publish registered {file}: {error}"));
        let binding = database.runtime_client().binding().clone();
        Self {
            trace: Trace::new(directory.path()),
            database,
            binding,
            path,
            _authority: authority,
            _directory: directory,
        }
    }

    pub async fn submit(
        &mut self,
        step: &str,
        key: &str,
        digest_byte: char,
        payload: RepositoryWritePayloadV1,
    ) {
        let request = submit_request(&self.binding, step, key, digest_byte, payload);
        let probe = Probe::for_request(&request);
        let result = self
            .database
            .runtime_client()
            .dispatch_submit(request, probe)
            .await;
        self.trace.record(step, result);
    }

    /// Fixture rows no payload variant writes, inserted through the writer.
    pub async fn seed(&mut self, step: &str, sql: &str, params: Params) {
        let result = async {
            let transaction = self.database.begin_write_transaction(step).await?;
            let changed = transaction
                .execute(sql, params)
                .await
                .map_err(|error| tracedecay_domain::errors::TraceDecayError::Database {
                    message: error.to_string(),
                    operation: step.to_owned(),
                })?;
            transaction.commit().await?;
            Ok::<_, tracedecay_domain::errors::TraceDecayError>(changed)
        }
        .await;
        self.trace.note(step, result);
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn finish(self) -> String {
        let Self {
            database,
            trace,
            path,
            _authority,
            _directory,
            ..
        } = self;
        drop(database);
        trace.finish(&path)
    }
}
