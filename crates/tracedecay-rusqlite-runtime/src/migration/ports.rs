use super::model::*;

/// One closed family transform. Implementations own all SQL and must be deterministic.
pub(crate) trait FamilyTransform {
    type Error: std::error::Error + Send + Sync + 'static;

    fn family(&self) -> StoreFamily;
    fn apply(
        &mut self,
        staging: &StagingHandle,
        plan: &FamilyTransformPlan,
    ) -> Result<(), Self::Error>;
}

/// Copy-only migration authority. No method accepts or returns a filesystem path.
pub(crate) trait ConsolidatedMigrationPort {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Verifies provenance and release acceptance outside this engine.
    fn verify_release_freeze(&mut self, proof: &ReleaseFreezeProof) -> Result<bool, Self::Error>;
    fn preflight(&mut self, request: &MigrationRequest) -> Result<PreflightReport, Self::Error>;
    fn lookup_publication(
        &mut self,
        migration: &MigrationId,
    ) -> Result<Option<PublicationReceipt>, Self::Error>;
    fn create_isolated_backup(
        &mut self,
        request: &MigrationRequest,
        source: &LastReleasedSchemaManifest,
    ) -> Result<BackupReceipt, Self::Error>;
    fn find_staging(
        &mut self,
        migration: &MigrationId,
    ) -> Result<Option<StagingHandle>, Self::Error>;
    fn create_isolated_staging(
        &mut self,
        request: &MigrationRequest,
        target: &FinalSchemaManifest,
        backup: &BackupReceipt,
    ) -> Result<StagingHandle, Self::Error>;
    fn load_destination_checkpoint(
        &mut self,
        staging: &StagingHandle,
    ) -> Result<DestinationCheckpoint, Self::Error>;

    /// The transform and returned checkpoint must commit in one destination transaction.
    fn transform_and_checkpoint<T: FamilyTransform<Error = Self::Error>>(
        &mut self,
        staging: &StagingHandle,
        transform: &mut T,
        plan: &FamilyTransformPlan,
        completed: &[StoreFamily],
    ) -> Result<DestinationCheckpoint, Self::Error>;

    fn verify_staging(
        &mut self,
        staging: &StagingHandle,
        source: &LastReleasedSchemaManifest,
        target: &FinalSchemaManifest,
    ) -> Result<VerificationReport, Self::Error>;

    /// Verification evidence and checkpoint must commit together in the destination.
    fn checkpoint_verification(
        &mut self,
        staging: &StagingHandle,
        backup: &BackupReceipt,
        report: &VerificationReport,
    ) -> Result<DestinationCheckpoint, Self::Error>;

    /// Atomically publishes all family members and its durable receipt. An uncertain
    /// acknowledgement must be resolved by `lookup_publication`, never by rollback.
    fn publish_atomically(
        &mut self,
        staging: StagingHandle,
        request: &MigrationRequest,
        target: &FinalSchemaManifest,
        checkpoint: &DestinationCheckpoint,
    ) -> Result<PublicationReceipt, Self::Error>;
}
