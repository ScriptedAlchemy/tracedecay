use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rusqlite::Savepoint;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay_domain::UtcMicros;
use tracedecay_graph_db::{
    GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig, GraphGenerationManifest,
    GraphIdempotencyKey, NeverCancelled, VerifiedGraphSnapshot,
};
use tracedecay_rusqlite_runtime::{
    ExistingWriterLocator, PersistentWriter, StorageOperationExecutor,
    exact_sql::{ExactSqlError, ExactSqlHandle, ExactSqlWriteAuthority, ExactSqlWriteIntent},
    reader::{ExistingReaderLocator, ReaderPool, ReaderQueryExecutor},
    repository::{GRAPH_PUBLICATION_SCHEMA_V1, GraphPublicationExactSqlStorage},
};
use tracedecay_store::{
    AdmissionConfigV1, BrainId, GraphProjectionIdentityV1, GraphPublicationInputDigestV1,
    GraphPublicationOperationContextV1, GraphPublicationStoreV1, ProjectId,
    RetainedGraphStoreLeaseV1, RuntimeCancellationIdV1, RuntimeCancellationIdentityV1,
    RuntimeDeadlineIdV1, RuntimeDeadlineV1, RuntimeInterruptionV1, RuntimeRequestControlV1,
    RuntimeRequestProbeV1, StoreAuthorityEpochV1, StoreIncarnationV1, StoreRuntimeBindingV1,
    StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1, canonical_store_locator_digest,
};

#[derive(Debug)]
struct BenchmarkGraphLease {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: PathBuf,
}

impl RetainedGraphStoreLeaseV1 for BenchmarkGraphLease {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }
}

struct NoWrites;

impl StorageOperationExecutor for NoWrites {
    fn execute(
        &mut self,
        _savepoint: &Savepoint<'_>,
        _payload: &tracedecay_store::RepositoryWritePayloadV1,
    ) -> rusqlite::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
struct NoReads;

impl ReaderQueryExecutor for NoReads {
    fn execute_read(
        &mut self,
        _snapshot: &rusqlite::Transaction<'_>,
        _request: &tracedecay_store::RuntimeReadRequestV1,
    ) -> Result<tracedecay_store::RuntimeReadOutcomeV1, tracedecay_store::StorageRuntimeErrorV1>
    {
        unreachable!("Git benchmark publication authority uses exact SQL only")
    }
}

struct AlwaysAuthorized;

impl ExactSqlWriteAuthority for AlwaysAuthorized {
    fn verify(&self, _intent: ExactSqlWriteIntent) -> Result<(), ExactSqlError> {
        Ok(())
    }
}

struct BenchmarkProbe {
    cancellation: RuntimeCancellationIdentityV1,
    deadline: RuntimeDeadlineV1,
}

impl RuntimeRequestProbeV1 for BenchmarkProbe {
    fn cancellation_identity(&self) -> &RuntimeCancellationIdentityV1 {
        &self.cancellation
    }

    fn deadline_identity(&self) -> &RuntimeDeadlineV1 {
        &self.deadline
    }

    fn interruption(&self) -> Option<RuntimeInterruptionV1> {
        None
    }

    fn try_begin_commit(&self) -> bool {
        true
    }
}

fn operation_control(sequence: usize) -> (RuntimeRequestControlV1, BenchmarkProbe) {
    let cancellation = RuntimeCancellationIdentityV1 {
        cancellation_id: RuntimeCancellationIdV1::new(format!("git-benchmark-cancel:{sequence}"))
            .expect("benchmark cancellation identity is valid"),
        generation: 1,
    };
    let deadline = RuntimeDeadlineV1 {
        deadline_id: RuntimeDeadlineIdV1::new(format!("git-benchmark-deadline:{sequence}"))
            .expect("benchmark deadline identity is valid"),
    };
    (
        RuntimeRequestControlV1 {
            requested_at: UtcMicros(sequence as i64),
            deadline: deadline.clone(),
            cancellation: cancellation.clone(),
        },
        BenchmarkProbe {
            cancellation,
            deadline,
        },
    )
}

pub struct PersistentGitGraph {
    _writer: PersistentWriter,
    _readers: ReaderPool<NoReads>,
    registry: GraphDbRegistry,
    binding: StoreRuntimeBindingV1,
    graph_path: PathBuf,
    authority: GraphPublicationExactSqlStorage,
    latest_head: Option<tracedecay_store::GraphVerifiedHeadV1>,
    latest_projection: Option<GraphProjectionIdentityV1>,
    sequence: usize,
    _root: TempDir,
}

impl PersistentGitGraph {
    pub fn new() -> Self {
        let root = TempDir::new().expect("benchmark temporary directory exists");
        let binding = StoreRuntimeBindingV1::new(
            StoreShardIdV1::project(
                BrainId::try_from("brain.git-ancestry-benchmark".to_owned())
                    .expect("benchmark brain identity is valid"),
                UserProfileId::try_from("profile.git-ancestry-benchmark".to_owned())
                    .expect("benchmark profile identity is valid"),
                ProjectId::try_from("project.git-ancestry-benchmark".to_owned())
                    .expect("benchmark project identity is valid"),
            ),
            StoreIncarnationV1::new(1).expect("benchmark incarnation is valid"),
            StoreAuthorityEpochV1::new(1).expect("benchmark authority epoch is valid"),
        );
        let relational_path = root.path().join("publication.sqlite3");
        drop(
            rusqlite::Connection::open(&relational_path)
                .expect("benchmark publication database is created"),
        );
        let relational_path = relational_path
            .canonicalize()
            .expect("benchmark publication database path is canonical");
        let relational_locator = VerifiedStoreLocatorV1::new(
            binding.shard_id.clone(),
            binding.incarnation,
            canonical_store_locator_digest(&relational_path)
                .expect("benchmark relational locator digest is valid"),
        );
        let writer = PersistentWriter::start(
            ExistingWriterLocator::new(
                binding.clone(),
                relational_locator.clone(),
                relational_path.clone(),
            )
            .expect("benchmark writer locator is valid"),
            AdmissionConfigV1::default(),
            NoWrites,
        )
        .expect("benchmark publication writer starts");
        let readers = ReaderPool::start(
            ExistingReaderLocator::new(binding.clone(), relational_locator, relational_path)
                .expect("benchmark reader locator is valid"),
            AdmissionConfigV1::default().readers,
            NoReads,
        )
        .expect("benchmark publication readers start");
        let handle =
            ExactSqlHandle::attach(&writer, &readers).expect("benchmark exact SQL handle attaches");
        handle
            .execute_batch(GRAPH_PUBLICATION_SCHEMA_V1.to_owned())
            .expect("benchmark graph publication schema installs");
        let handle = handle
            .with_write_authority(Arc::new(AlwaysAuthorized))
            .expect("benchmark exact SQL write authority attaches");
        Self {
            registry: GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 })
                .expect("benchmark graph registry config is valid"),
            authority: GraphPublicationExactSqlStorage::from_authorized_handle(handle)
                .expect("benchmark graph publication authority attaches"),
            binding,
            graph_path: root.path().join("git-ancestry.grafeo"),
            _writer: writer,
            _readers: readers,
            latest_head: None,
            latest_projection: None,
            sequence: 0,
            _root: root,
        }
    }

    pub fn publish(&mut self, manifest: GraphGenerationManifest) -> VerifiedGraphSnapshot {
        self.sequence += 1;
        let (append_control, append_probe) = operation_control(self.sequence);
        let append_context =
            GraphPublicationOperationContextV1::new(&append_control, &append_probe)
                .expect("benchmark replay append context is valid");
        let input_digest = GraphPublicationInputDigestV1::new(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(
                serde_json::to_vec(&manifest).expect("benchmark manifest serializes")
            ))
        ))
        .expect("benchmark publication input digest is valid");
        let replay = manifest
            .relational_metadata_replay(
                self.binding.shard_id.clone(),
                GraphIdempotencyKey::new(format!("git-benchmark-publication:{}", self.sequence))
                    .expect("benchmark idempotency identity is valid"),
                input_digest,
                self.latest_head.clone(),
                &|| Ok(()),
            )
            .expect("benchmark replay is valid");
        let key = replay.key.clone();
        self.authority
            .append_replay(&replay, &append_context)
            .expect("benchmark replay persists");

        self.sequence += 1;
        let (publish_control, publish_probe) = operation_control(self.sequence);
        let publish_context =
            GraphPublicationOperationContextV1::new(&publish_control, &publish_probe)
                .expect("benchmark publication context is valid");
        let commit = self
            .registry
            .publish_verified(
                self.registration(),
                &mut self.authority,
                &publish_context,
                &key,
                Some(manifest.into()),
            )
            .expect("benchmark generation publishes and verifies");
        self.latest_head = Some(commit.head);
        self.latest_projection = Some(key.projection);
        commit.snapshot
    }

    pub fn recover_snapshot(&mut self) -> VerifiedGraphSnapshot {
        let registration = self.registration();
        assert!(
            self.registry
                .close(&registration)
                .expect("benchmark graph store closes"),
            "benchmark recovery must close an open graph store",
        );
        self.sequence += 1;
        let (control, probe) = operation_control(self.sequence);
        let context = GraphPublicationOperationContextV1::new(&control, &probe)
            .expect("benchmark recovery context is valid");
        let latest_projection = self
            .latest_projection
            .clone()
            .expect("benchmark has a published projection");
        self.registry
            .recover_verified_snapshot(
                self.registration(),
                &mut self.authority,
                &context,
                &latest_projection,
            )
            .expect("benchmark verified snapshot recovers")
    }

    fn registration(&self) -> GraphDbRegistration {
        let canonical_path = self.graph_path.clone();
        let verified_locator = VerifiedStoreLocatorV1::new(
            self.binding.shard_id.clone(),
            self.binding.incarnation,
            canonical_store_locator_digest(&canonical_path)
                .expect("benchmark graph locator digest is valid"),
        );
        GraphDbRegistration {
            authority_lease: Arc::new(BenchmarkGraphLease {
                binding: self.binding.clone(),
                verified_locator,
                canonical_path,
            }),
            cancellation: Arc::new(NeverCancelled),
            lifecycle_cancellation: Arc::new(NeverCancelled),
            deadline: Instant::now() + Duration::from_secs(3_600),
        }
    }
}
