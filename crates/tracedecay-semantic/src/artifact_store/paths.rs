//! Path, lock, and inventory-file helpers for ModelArtifactStore.
//! Split out of artifact_store.rs to keep the facade under the 1000-line
//! hygiene ceiling; pure structural move, no behavior change.

use super::*;

impl ModelArtifactStore {
    pub fn open(
        root: impl Into<PathBuf>,
        retention: RetentionPolicyV1,
    ) -> Result<Self, ArtifactImportErrorV1> {
        let root = root.into();
        let root_dir = open_root_from_trusted_parent(&root)?;
        let staging_dir = open_or_create_component_dir(&root_dir, "staging")?;
        let artifacts_dir = open_or_create_component_dir(&root_dir, "artifacts")?;
        let receipts_dir = open_or_create_component_dir(&root_dir, "receipts")?;
        let store = Self {
            root,
            root_dir,
            staging_dir,
            artifacts_dir,
            receipts_dir,
            retention,
            operation_lock: Arc::new(Mutex::new(())),
        };
        {
            let _lock = store.acquire_lock()?;
            store.recover_locked()?;
        }
        Ok(store)
    }

    #[cfg(test)]
    pub(super) fn inventory_path(&self) -> PathBuf {
        self.root.join("inventory.json")
    }

    #[cfg(test)]
    pub(super) fn recovery_path(&self) -> PathBuf {
        self.root.join(".artifact-store-recovery.json")
    }

    pub(super) fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    pub(super) fn artifacts_root(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    pub(super) fn receipts_root(&self) -> PathBuf {
        self.root.join("receipts")
    }

    pub(super) fn staging_dir_for(&self, staging_id: &str) -> Result<PathBuf, ArtifactImportErrorV1> {
        if !is_valid_staging_id(staging_id) {
            return Err(ArtifactImportErrorV1::UnsafeStagingHandle);
        }
        Ok(self.staging_root().join(staging_id))
    }

    pub(super) fn artifact_dir(&self, digest: &Sha256DigestHex) -> PathBuf {
        self.artifacts_root().join(digest.as_str())
    }

    pub fn installed_directory(&self, digest: &Sha256DigestHex) -> PathBuf {
        self.artifact_dir(digest)
    }

    #[cfg(test)]
    pub(super) fn artifact_path(&self, digest: &Sha256DigestHex) -> PathBuf {
        self.member_path(digest, ArtifactMemberRoleV1::Model)
    }

    #[cfg(test)]
    pub(super) fn member_path(&self, digest: &Sha256DigestHex, role: ArtifactMemberRoleV1) -> PathBuf {
        self.artifact_dir(digest).join(member_file_name(role))
    }

    pub(super) fn acquire_lock(&self) -> Result<ArtifactStoreLock<'_>, ArtifactImportErrorV1> {
        let memory = self
            .operation_lock
            .lock()
            .map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        let file = open_cap_file(
            &self.root_dir,
            ".artifact-store.lock",
            true,
            true,
            true,
            false,
            false,
        )?
        .into_std();
        file.lock_exclusive()
            .map_err(|_| ArtifactImportErrorV1::StoreBusy)?;
        Ok(ArtifactStoreLock {
            _memory: memory,
            _file: file,
        })
    }

    /// Load the inventory (absent file = empty inventory).
    pub fn inventory(&self) -> Result<ArtifactInventoryV1, ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        self.load_inventory_locked()
    }

    #[cfg(test)]
    pub(super) fn save_inventory(&self, inventory: &ArtifactInventoryV1) -> Result<(), ArtifactImportErrorV1> {
        let _lock = self.acquire_lock()?;
        self.recover_locked()?;
        self.save_inventory_locked(inventory)?;
        Ok(())
    }

    pub(super) fn load_inventory_locked(&self) -> Result<ArtifactInventoryV1, ArtifactImportErrorV1> {
        let Some(bytes) = read_optional_cap_file(&self.root_dir, "inventory.json")? else {
            return Ok(ArtifactInventoryV1::default());
        };
        serde_json::from_slice(&bytes).map_err(|_| ArtifactImportErrorV1::StorageFailure)
    }

    pub(super) fn save_inventory_locked(
        &self,
        inventory: &ArtifactInventoryV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        let bytes =
            serde_json::to_vec(inventory).map_err(|_| ArtifactImportErrorV1::StorageFailure)?;
        atomic_write_cap_file(&self.root_dir, &self.root, "inventory.json", &bytes)
    }

    /// Verify the canonical manifest before any bytes are staged.
    pub fn verify_manifest(
        &self,
        manifest: &ModelArtifactManifestV1,
    ) -> Result<(), ArtifactImportErrorV1> {
        manifest
            .validate()
            .map_err(|_| ArtifactImportErrorV1::ManifestRejected)
    }
}
