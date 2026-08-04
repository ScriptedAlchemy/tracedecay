//! Pre-open, identity-first resolution of local daemon store locators.
//!
//! A [`StoreRuntimeKey`] has already selected the logical shard before this
//! module is called. Paths in this module are therefore only evidence for the
//! physical locator: they never mint, select, or alter a shard identity. In
//! particular, this resolver does not consult the current directory, branch
//! display names, remote URLs, or arbitrary project labels.
//!
//! Resolution deliberately stops before database ownership begins. It may read
//! the lightweight project enrollment marker and inspect filesystem metadata,
//! but it never creates an artifact, opens a database, reads database bytes,
//! migrates, repairs, or otherwise touches a live store.
//!
//! Dead-code allowance lives on the parent `store_runtime` module until daemon
//! construction wires this resolver.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use sha2::{Digest, Sha256};
use tracedecay_store::{
    BrainId, LocatorDigest, ProjectId, StoreShardIdV1, StoreShardScopeV1, UserProfileId,
    VerifiedStoreLocatorV1,
};

use super::registry::{
    ResolvedStoreLocator, StoreRuntimeKey, StoreRuntimeOpenMode, StoreRuntimeRegistryFailure,
    StoreRuntimeRegistryFuture, StoreRuntimeResolver,
};
use crate::storage;

const PROFILE_DATABASE_FILENAME: &str = "global.db";
const LOCATOR_DIGEST_DOMAIN: &[u8] = b"tracedecay.store-runtime.local-locator.v1\0";

/// Explicit local authority for one typed profile.
///
/// `profile_root` is a locator supplied by the daemon's profile authority. It
/// is not an identity source: resolution rejects it unless both typed IDs match
/// the requested shard before looking at the filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProfileStoreAuthorityV1 {
    brain_id: BrainId,
    profile_id: UserProfileId,
    profile_root: PathBuf,
}

impl LocalProfileStoreAuthorityV1 {
    pub fn new(brain_id: BrainId, profile_id: UserProfileId, profile_root: PathBuf) -> Self {
        Self {
            brain_id,
            profile_id,
            profile_root,
        }
    }

    pub fn brain_id(&self) -> &BrainId {
        &self.brain_id
    }

    pub fn profile_id(&self) -> &UserProfileId {
        &self.profile_id
    }

    pub fn profile_root(&self) -> &Path {
        &self.profile_root
    }
}

/// A trusted, typed project enrollment authority and its observed aliases.
///
/// The roots are only places where the resolver may verify the project's
/// enrollment marker. The typed `project_id` selects this record first; an
/// alias never selects a record or changes the requested shard identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProjectEnrollmentAuthorityV1 {
    project_id: ProjectId,
    enrollment_roots: Vec<PathBuf>,
}

impl LocalProjectEnrollmentAuthorityV1 {
    pub fn new(project_id: ProjectId, enrollment_roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut enrollment_roots = enrollment_roots.into_iter().collect::<Vec<_>>();
        enrollment_roots.sort();
        enrollment_roots.dedup();
        Self {
            project_id,
            enrollment_roots,
        }
    }

    pub fn enrollment_roots(&self) -> &[PathBuf] {
        &self.enrollment_roots
    }
}

/// Exact daemon authority for one already-created code-shard database.
///
/// The typed shard selects this record. `database_path` is locator evidence
/// only and can never select or manufacture a project, repository, worktree,
/// or snapshot identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalCodeStoreAuthorityV1 {
    shard_id: StoreShardIdV1,
    database_path: PathBuf,
}

impl LocalCodeStoreAuthorityV1 {
    pub fn new(
        shard_id: StoreShardIdV1,
        database_path: PathBuf,
    ) -> Result<Self, LocalStoreRuntimeResolverConfigurationErrorV1> {
        if !matches!(&shard_id.scope, StoreShardScopeV1::Code { .. }) {
            return Err(
                LocalStoreRuntimeResolverConfigurationErrorV1::CodeAuthorityIsNotCodeShard {
                    shard_id: Box::new(shard_id),
                },
            );
        }
        Ok(Self {
            shard_id,
            database_path,
        })
    }
}

/// In-memory configuration failures for a local resolver.
///
/// This is deliberately separate from resolution unavailability: a duplicate
/// typed authority is a daemon wiring bug, not a reason to choose an arbitrary
/// path at runtime.
///
/// Shard identities are boxed so `Result<_, Self>` stays under Clippy's
/// `result_large_err` threshold; the boxed value remains the exact typed
/// authority identity used for fail-closed equality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalStoreRuntimeResolverConfigurationErrorV1 {
    DuplicateProjectAuthority { project_id: ProjectId },
    DuplicateCodeAuthority { shard_id: Box<StoreShardIdV1> },
    CodeAuthorityIsNotCodeShard { shard_id: Box<StoreShardIdV1> },
    CodeAuthorityRetirementMismatch { shard_id: Box<StoreShardIdV1> },
}

/// Whether an exact code-store authority was added by this registration.
///
/// Callers use this to roll back only their own newly added authority when a
/// subsequent runtime open fails. An identical authority may already be owned
/// by a retained or concurrently opening runtime and must not be retired by an
/// idempotent registrant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalCodeStoreAuthorityRegistrationOutcomeV1 {
    Registered,
    AlreadyRegistered,
}

/// Infrastructure-only resolver for the local profile-sharded layout.
///
/// The resolver supports only physical mappings that the current storage layout
/// can prove exactly: the profile authority database, a project database, and a
/// project sessions database. Worktree/snapshot code shards need a separately
/// typed graph-scope authority and are returned as unavailable rather than
/// guessed from a branch name or path.
#[derive(Clone, Debug)]
pub struct LocalStoreRuntimeResolverV1 {
    profile_authority: LocalProfileStoreAuthorityV1,
    project_authorities: Arc<RwLock<BTreeMap<ProjectId, LocalProjectEnrollmentAuthorityV1>>>,
    code_authorities: Arc<RwLock<BTreeMap<StoreShardIdV1, LocalCodeStoreAuthorityV1>>>,
}

impl LocalStoreRuntimeResolverV1 {
    pub fn new(profile_authority: LocalProfileStoreAuthorityV1) -> Self {
        Self {
            profile_authority,
            project_authorities: Arc::new(RwLock::new(BTreeMap::new())),
            code_authorities: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Adds exactly one typed project authority.
    ///
    /// A project can carry many aliases in its one
    /// [`LocalProjectEnrollmentAuthorityV1`], but two separately configured
    /// authorities for the same typed project are refused rather than merged.
    #[cfg(test)]
    pub(crate) fn with_project_authority(
        self,
        authority: LocalProjectEnrollmentAuthorityV1,
    ) -> Result<Self, LocalStoreRuntimeResolverConfigurationErrorV1> {
        self.register_project_authority(authority)?;
        Ok(self)
    }

    pub fn register_project_authority(
        &self,
        authority: LocalProjectEnrollmentAuthorityV1,
    ) -> Result<(), LocalStoreRuntimeResolverConfigurationErrorV1> {
        let project_id = authority.project_id.clone();
        let mut project_authorities = self.project_authorities_write();
        if project_authorities
            .get(&project_id)
            .is_some_and(|existing| existing == &authority)
        {
            return Ok(());
        }
        if project_authorities.contains_key(&project_id) {
            return Err(
                LocalStoreRuntimeResolverConfigurationErrorV1::DuplicateProjectAuthority {
                    project_id,
                },
            );
        }
        project_authorities.insert(project_id, authority);
        Ok(())
    }

    pub fn register_code_authority(
        &self,
        authority: LocalCodeStoreAuthorityV1,
    ) -> Result<
        LocalCodeStoreAuthorityRegistrationOutcomeV1,
        LocalStoreRuntimeResolverConfigurationErrorV1,
    > {
        let shard_id = authority.shard_id.clone();
        let mut code_authorities = self.code_authorities_write();
        if code_authorities
            .get(&shard_id)
            .is_some_and(|existing| existing == &authority)
        {
            return Ok(LocalCodeStoreAuthorityRegistrationOutcomeV1::AlreadyRegistered);
        }
        if code_authorities.contains_key(&shard_id) {
            return Err(
                LocalStoreRuntimeResolverConfigurationErrorV1::DuplicateCodeAuthority {
                    shard_id: Box::new(shard_id),
                },
            );
        }
        code_authorities.insert(shard_id, authority);
        Ok(LocalCodeStoreAuthorityRegistrationOutcomeV1::Registered)
    }

    pub fn retire_code_authority(
        &self,
        shard_id: &StoreShardIdV1,
        database_path: &Path,
    ) -> Result<(), LocalStoreRuntimeResolverConfigurationErrorV1> {
        let expected =
            LocalCodeStoreAuthorityV1::new(shard_id.clone(), database_path.to_path_buf())?;
        let mut code_authorities = self.code_authorities_write();
        match code_authorities.get(shard_id) {
            None => Ok(()),
            Some(existing) if existing == &expected => {
                code_authorities.remove(shard_id);
                Ok(())
            }
            Some(_) => Err(
                LocalStoreRuntimeResolverConfigurationErrorV1::CodeAuthorityRetirementMismatch {
                    shard_id: Box::new(shard_id.clone()),
                },
            ),
        }
    }

    fn project_authorities_read(
        &self,
    ) -> RwLockReadGuard<'_, BTreeMap<ProjectId, LocalProjectEnrollmentAuthorityV1>> {
        self.project_authorities
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn project_authorities_write(
        &self,
    ) -> RwLockWriteGuard<'_, BTreeMap<ProjectId, LocalProjectEnrollmentAuthorityV1>> {
        self.project_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn code_authorities_read(
        &self,
    ) -> RwLockReadGuard<'_, BTreeMap<StoreShardIdV1, LocalCodeStoreAuthorityV1>> {
        self.code_authorities
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn code_authorities_write(
        &self,
    ) -> RwLockWriteGuard<'_, BTreeMap<StoreShardIdV1, LocalCodeStoreAuthorityV1>> {
        self.code_authorities
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Resolves a canonical key without opening or reading its database.
    ///
    /// Callers that need the typed unavailable result should use this method.
    /// [`StoreRuntimeResolver`] adapts it to the registry's current generic
    /// infrastructure-failure channel without ever falling back to a locator.
    pub fn resolve_key(&self, key: &StoreRuntimeKey) -> LocalStoreLocatorResolutionV1 {
        self.resolve_key_with_filesystem_safety(key, &local_filesystem_safety)
    }

    fn resolve_key_with_filesystem_safety(
        &self,
        key: &StoreRuntimeKey,
        filesystem_safety: &dyn Fn(&Path) -> FilesystemSafety,
    ) -> LocalStoreLocatorResolutionV1 {
        let result = self.resolve_key_inner(key, filesystem_safety);
        match result {
            Ok(locator) => LocalStoreLocatorResolutionV1::Resolved(locator),
            Err(reason) => {
                LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                    shard_id: key.shard_id().clone(),
                    reason,
                })
            }
        }
    }

    fn resolve_key_inner(
        &self,
        key: &StoreRuntimeKey,
        filesystem_safety: &dyn Fn(&Path) -> FilesystemSafety,
    ) -> LocalStoreLocatorResult<VerifiedLocalStoreLocatorV1> {
        let shard_id = key.shard_id();
        if shard_id.brain_id != *self.profile_authority.brain_id()
            || shard_id.profile_id != *self.profile_authority.profile_id()
        {
            return Err(LocalStoreLocatorUnavailableReasonV1::ProfileAuthorityMismatch);
        }

        let canonical_profile_root =
            canonical_existing_directory(self.profile_authority.profile_root())?;
        // The profile root is the authority boundary. Reject a remote or
        // unverified root even if a later child path looks locally mounted.
        require_local_filesystem(&canonical_profile_root, filesystem_safety)?;

        match &shard_id.scope {
            StoreShardScopeV1::Profile => {
                let locator_path = canonical_or_prospective_regular_file(
                    &canonical_profile_root.join(PROFILE_DATABASE_FILENAME),
                    &canonical_profile_root,
                )?;
                verified_locator(
                    key,
                    LocalStoreLocatorKindV1::ProfileAuthority,
                    canonical_profile_root.clone(),
                    canonical_profile_root,
                    locator_path,
                    filesystem_safety,
                )
            }
            StoreShardScopeV1::ProfileMemory => {
                let locator_path = canonical_or_prospective_regular_file(
                    &canonical_profile_root.join(crate::memory::user::USER_MEMORY_DB_FILENAME),
                    &canonical_profile_root,
                )?;
                verified_locator(
                    key,
                    LocalStoreLocatorKindV1::ProfileMemory,
                    canonical_profile_root.clone(),
                    canonical_profile_root,
                    locator_path,
                    filesystem_safety,
                )
            }
            StoreShardScopeV1::ProfileSessions => {
                let locator_path = canonical_or_prospective_regular_file(
                    &canonical_profile_root
                        .join(crate::store_runtime::profile_paths::USER_SESSIONS_DB_FILENAME),
                    &canonical_profile_root,
                )?;
                verified_locator(
                    key,
                    LocalStoreLocatorKindV1::ProfileSessions,
                    canonical_profile_root.clone(),
                    canonical_profile_root,
                    locator_path,
                    filesystem_safety,
                )
            }
            StoreShardScopeV1::Project { project_id } => self.resolve_project_locator(
                key,
                project_id,
                LocalStoreLocatorKindV1::Project,
                &canonical_profile_root,
                filesystem_safety,
            ),
            StoreShardScopeV1::ProjectSessions { project_id } => self.resolve_project_locator(
                key,
                project_id,
                LocalStoreLocatorKindV1::ProjectSessions,
                &canonical_profile_root,
                filesystem_safety,
            ),
            StoreShardScopeV1::Code { .. } => {
                self.resolve_code_locator(key, &canonical_profile_root, filesystem_safety)
            }
        }
    }

    fn resolve_code_locator(
        &self,
        key: &StoreRuntimeKey,
        canonical_profile_root: &Path,
        filesystem_safety: &dyn Fn(&Path) -> FilesystemSafety,
    ) -> LocalStoreLocatorResult<VerifiedLocalStoreLocatorV1> {
        let authority = self
            .code_authorities_read()
            .get(key.shard_id())
            .cloned()
            .ok_or(LocalStoreLocatorUnavailableReasonV1::MissingCodeStoreAuthority)?;
        // A never-indexed worktree has no graph database until its first index
        // creates one under the daemon's authority. Resolve a prospective
        // locator exactly like the project and session locators do, so the
        // `Initialize` open mode can create the file. `canonical_or_prospective_*`
        // still reject symlinked or non-regular existing paths and confine the
        // locator to the profile root; the mode-aware `resolve()` remains the
        // authoritative existence gate, failing closed for a non-`Initialize`
        // open of a missing store. Requiring pre-existence here instead blocked
        // that legitimate first-touch initialization.
        let canonical_path = canonical_or_prospective_regular_file(
            &authority.database_path,
            canonical_profile_root,
        )?;
        let canonical_store_root = canonical_path
            .parent()
            .ok_or(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)?
            .to_path_buf();
        verified_locator(
            key,
            LocalStoreLocatorKindV1::Code,
            canonical_profile_root.to_path_buf(),
            canonical_store_root,
            canonical_path,
            filesystem_safety,
        )
    }

    fn resolve_project_locator(
        &self,
        key: &StoreRuntimeKey,
        project_id: &ProjectId,
        kind: LocalStoreLocatorKindV1,
        canonical_profile_root: &Path,
        filesystem_safety: &dyn Fn(&Path) -> FilesystemSafety,
    ) -> LocalStoreLocatorResult<VerifiedLocalStoreLocatorV1> {
        let authority = self
            .project_authorities_read()
            .get(project_id)
            .cloned()
            .ok_or(LocalStoreLocatorUnavailableReasonV1::MissingProjectEnrollmentAuthority)?;
        if authority.enrollment_roots().is_empty() {
            return Err(LocalStoreLocatorUnavailableReasonV1::MissingProjectEnrollmentAuthority);
        }

        let mut deferred_unavailable = None;
        for enrollment_root in authority.enrollment_roots() {
            match Self::resolve_project_locator_at_root(
                key,
                project_id,
                kind,
                canonical_profile_root,
                enrollment_root,
                filesystem_safety,
            ) {
                Ok(locator) => return Ok(locator),
                Err(reason) if reason.allows_alias_fallback() => {
                    deferred_unavailable = Some(reason);
                }
                // A present but invalid/mismatched enrollment is evidence of a
                // bad authority mapping, not a stale alias to silently ignore.
                Err(reason) => return Err(reason),
            }
        }

        Err(deferred_unavailable
            .unwrap_or(LocalStoreLocatorUnavailableReasonV1::MissingProjectEnrollmentAuthority))
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_project_locator_at_root(
        key: &StoreRuntimeKey,
        project_id: &ProjectId,
        kind: LocalStoreLocatorKindV1,
        canonical_profile_root: &Path,
        enrollment_root: &Path,
        filesystem_safety: &dyn Fn(&Path) -> FilesystemSafety,
    ) -> LocalStoreLocatorResult<VerifiedLocalStoreLocatorV1> {
        let canonical_project_root =
            canonical_existing_directory(enrollment_root).map_err(|reason| {
                if reason == LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable {
                    LocalStoreLocatorUnavailableReasonV1::ProjectEnrollmentRootUnavailable
                } else {
                    reason
                }
            })?;
        require_local_filesystem(&canonical_project_root, filesystem_safety)?;

        // This is lightweight enrollment metadata, not a store read. The typed
        // project ID is compared before any layout-derived path is accepted.
        let marker = read_matching_enrollment(&canonical_project_root, project_id)?;
        let layout = storage::profile_sharded_layout(
            &canonical_project_root,
            canonical_profile_root,
            &marker,
        )
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::InvalidEnrollment)?;
        let expected_store_root =
            storage::profile_sharded_data_root(canonical_profile_root, project_id.as_str());
        if layout.data_root != expected_store_root {
            return Err(LocalStoreLocatorUnavailableReasonV1::InvalidEnrollment);
        }
        let canonical_store_root =
            canonical_or_prospective_directory(&layout.data_root, canonical_profile_root)?;
        let locator_path = match kind {
            LocalStoreLocatorKindV1::Project => layout.graph_db_path,
            LocalStoreLocatorKindV1::ProjectSessions => layout.sessions_db_path,
            LocalStoreLocatorKindV1::ProfileAuthority
            | LocalStoreLocatorKindV1::ProfileMemory
            | LocalStoreLocatorKindV1::ProfileSessions
            | LocalStoreLocatorKindV1::Code => {
                return Err(LocalStoreLocatorUnavailableReasonV1::UnsupportedShardScope);
            }
        };
        let locator_path =
            canonical_or_prospective_regular_file(&locator_path, &canonical_store_root)?;
        verified_locator(
            key,
            kind,
            canonical_profile_root.to_path_buf(),
            canonical_store_root,
            locator_path,
            filesystem_safety,
        )
    }
}

impl StoreRuntimeResolver for LocalStoreRuntimeResolverV1 {
    fn resolve<'a>(
        &'a self,
        key: &'a StoreRuntimeKey,
        mode: StoreRuntimeOpenMode,
        database_authority: Option<&'a crate::db::DatabaseAuthority>,
    ) -> StoreRuntimeRegistryFuture<'a, Result<ResolvedStoreLocator, StoreRuntimeRegistryFailure>>
    {
        Box::pin(async move {
            let locator = match self.resolve_key(key) {
                LocalStoreLocatorResolutionV1::Resolved(locator) => locator.into_registry_locator(),
                LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
                    return Err(unavailable.into_registry_failure());
                }
            };
            if let Some(authority) = database_authority {
                authority
                    .require_active_write_scope("resolve registered SQLite runtime")
                    .map_err(|error| StoreRuntimeRegistryFailure::ResolverFailed {
                        message: error.to_string(),
                    })?;
                if authority.canonical_database_path() != locator.path() {
                    return Err(StoreRuntimeRegistryFailure::ResolverFailed {
                        message: format!(
                            "resolved locator {} does not match originating database authority {}",
                            locator.path().display(),
                            authority.canonical_database_path().display()
                        ),
                    });
                }
            } else if mode == StoreRuntimeOpenMode::Initialize {
                return Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "initialization requires originating database authority".to_owned(),
                });
            }
            match fs::symlink_metadata(locator.path()) {
                Ok(metadata)
                    if !metadata.file_type().is_symlink()
                        && metadata.file_type().is_file()
                        && fs::canonicalize(locator.path()).ok().as_deref()
                            == Some(locator.path()) =>
                {
                    if mode == StoreRuntimeOpenMode::Initialize {
                        Err(StoreRuntimeRegistryFailure::ResolverFailed {
                            message: "initialization requires a prospective database locator"
                                .to_owned(),
                        })
                    } else {
                        Ok(locator)
                    }
                }
                Ok(_) => Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: "resolved database locator is not a canonical regular file".to_owned(),
                }),
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && mode == StoreRuntimeOpenMode::Initialize =>
                {
                    Ok(ResolvedStoreLocator::prospective(
                        locator.verified().clone(),
                        locator.path().to_path_buf(),
                    ))
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Err(StoreRuntimeRegistryFailure::ResolverFailed {
                        message: "resolved database does not exist".to_owned(),
                    })
                }
                Err(error) => Err(StoreRuntimeRegistryFailure::ResolverFailed {
                    message: format!("inspect resolved database locator: {error}"),
                }),
            }
        })
    }
}

/// The exactly mapped database family selected by this resolver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalStoreLocatorKindV1 {
    ProfileAuthority,
    ProfileMemory,
    ProfileSessions,
    Project,
    ProjectSessions,
    Code,
}

/// Locality facts captured while resolving a physical locator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalStoreLocatorMetadataV1 {
    pub kind: LocalStoreLocatorKindV1,
    pub canonical_profile_root: PathBuf,
    pub canonical_store_root: PathBuf,
    pub filesystem_type: String,
}

/// A pre-open verified local locator plus metadata for runtime construction.
///
/// The wrapped [`ResolvedStoreLocator`] contains the exact canonical path and
/// a path-only [`LocatorDigest`]. The digest is deliberately not database
/// content evidence: this resolver never reads database bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedLocalStoreLocatorV1 {
    locator: ResolvedStoreLocator,
    metadata: LocalStoreLocatorMetadataV1,
}

impl VerifiedLocalStoreLocatorV1 {
    pub fn locator(&self) -> &ResolvedStoreLocator {
        &self.locator
    }

    pub fn metadata(&self) -> &LocalStoreLocatorMetadataV1 {
        &self.metadata
    }

    fn into_registry_locator(self) -> ResolvedStoreLocator {
        self.locator
    }
}

/// A resolution result that preserves typed unavailability for local callers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalStoreLocatorResolutionV1 {
    Resolved(VerifiedLocalStoreLocatorV1),
    Unavailable(LocalStoreLocatorUnavailableV1),
}

/// Typed unavailability returned instead of choosing a fallback locator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalStoreLocatorUnavailableV1 {
    pub shard_id: StoreShardIdV1,
    pub reason: LocalStoreLocatorUnavailableReasonV1,
}

impl LocalStoreLocatorUnavailableV1 {
    fn into_registry_failure(self) -> StoreRuntimeRegistryFailure {
        match self.reason {
            LocalStoreLocatorUnavailableReasonV1::UnsupportedShardScope => {
                StoreRuntimeRegistryFailure::UnsupportedShardScope
            }
            LocalStoreLocatorUnavailableReasonV1::NetworkFilesystem { filesystem_type } => {
                StoreRuntimeRegistryFailure::NetworkFilesystemUnavailable { filesystem_type }
            }
            LocalStoreLocatorUnavailableReasonV1::FilesystemLocalityUnverified {
                filesystem_type,
            } => StoreRuntimeRegistryFailure::FilesystemLocalityUnavailable { filesystem_type },
            reason => StoreRuntimeRegistryFailure::ResolverFailed {
                message: format!("local store locator unavailable: {reason}"),
            },
        }
    }
}

/// Reasons a local path cannot be used for a typed shard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalStoreLocatorUnavailableReasonV1 {
    ProfileAuthorityMismatch,
    UnsupportedShardScope,
    MissingProjectEnrollmentAuthority,
    MissingCodeStoreAuthority,
    CodeDatabaseUnavailable,
    ProjectEnrollmentRootUnavailable,
    MissingEnrollment,
    InvalidEnrollment,
    EnrollmentProjectMismatch,
    EnrollmentStorageModeMismatch,
    UnsafeLocatorPath,
    FilesystemMetadataUnavailable,
    NetworkFilesystem { filesystem_type: String },
    FilesystemLocalityUnverified { filesystem_type: String },
    LocatorDigestUnavailable,
}

impl LocalStoreLocatorUnavailableReasonV1 {
    fn allows_alias_fallback(&self) -> bool {
        matches!(
            self,
            Self::ProjectEnrollmentRootUnavailable | Self::MissingEnrollment
        )
    }
}

impl fmt::Display for LocalStoreLocatorUnavailableReasonV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProfileAuthorityMismatch => formatter.write_str("profile authority mismatch"),
            Self::UnsupportedShardScope => formatter.write_str("unsupported shard scope"),
            Self::MissingProjectEnrollmentAuthority => {
                formatter.write_str("missing project enrollment authority")
            }
            Self::MissingCodeStoreAuthority => {
                formatter.write_str("missing exact code-store authority")
            }
            Self::CodeDatabaseUnavailable => {
                formatter.write_str("authorized code database is unavailable")
            }
            Self::ProjectEnrollmentRootUnavailable => {
                formatter.write_str("project enrollment root is unavailable")
            }
            Self::MissingEnrollment => formatter.write_str("project enrollment is missing"),
            Self::InvalidEnrollment => formatter.write_str("project enrollment is invalid"),
            Self::EnrollmentProjectMismatch => {
                formatter.write_str("project enrollment does not match typed project identity")
            }
            Self::EnrollmentStorageModeMismatch => {
                formatter.write_str("project enrollment does not use profile-sharded storage")
            }
            Self::UnsafeLocatorPath => formatter.write_str("locator path is unsafe"),
            Self::FilesystemMetadataUnavailable => {
                formatter.write_str("filesystem metadata is unavailable")
            }
            Self::NetworkFilesystem { filesystem_type } => {
                write!(formatter, "network filesystem '{filesystem_type}'")
            }
            Self::FilesystemLocalityUnverified { filesystem_type } => {
                write!(
                    formatter,
                    "filesystem locality is unverified ('{filesystem_type}')"
                )
            }
            Self::LocatorDigestUnavailable => formatter.write_str("locator digest is unavailable"),
        }
    }
}

type LocalStoreLocatorResult<T> = Result<T, LocalStoreLocatorUnavailableReasonV1>;

fn read_matching_enrollment(
    project_root: &Path,
    expected_project_id: &ProjectId,
) -> LocalStoreLocatorResult<storage::EnrollmentMarker> {
    let marker_path = storage::enrollment_marker_path(project_root);
    validate_absolute_no_symlink_components(&marker_path)?;
    match fs::symlink_metadata(&marker_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(LocalStoreLocatorUnavailableReasonV1::MissingEnrollment);
        }
        Err(_) => return Err(LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable),
    }

    let marker = storage::read_enrollment_marker(project_root)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::InvalidEnrollment)?
        .ok_or(LocalStoreLocatorUnavailableReasonV1::MissingEnrollment)?;
    if marker.storage_mode != storage::StorageMode::ProfileSharded {
        return Err(LocalStoreLocatorUnavailableReasonV1::EnrollmentStorageModeMismatch);
    }
    if marker.project_id != expected_project_id.as_str() {
        return Err(LocalStoreLocatorUnavailableReasonV1::EnrollmentProjectMismatch);
    }
    Ok(marker)
}

fn canonical_existing_directory(path: &Path) -> LocalStoreLocatorResult<PathBuf> {
    validate_absolute_no_symlink_components(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
    }
    fs::canonicalize(path)
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable)
}

fn canonical_or_prospective_directory(
    path: &Path,
    canonical_parent: &Path,
) -> LocalStoreLocatorResult<PathBuf> {
    if !path.starts_with(canonical_parent) {
        return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
    }
    validate_absolute_no_symlink_components(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() => {
            Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)
        }
        Ok(_) => {
            let canonical = fs::canonicalize(path)
                .map_err(|_| LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable)?;
            canonical
                .starts_with(canonical_parent)
                .then_some(canonical)
                .ok_or(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(_) => Err(LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable),
    }
}

fn canonical_or_prospective_regular_file(
    path: &Path,
    canonical_parent: &Path,
) -> LocalStoreLocatorResult<PathBuf> {
    if !path.starts_with(canonical_parent) {
        return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
    }
    validate_absolute_no_symlink_components(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)
        }
        Ok(_) => {
            let canonical = fs::canonicalize(path)
                .map_err(|_| LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable)?;
            canonical
                .starts_with(canonical_parent)
                .then_some(canonical)
                .ok_or(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(_) => Err(LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable),
    }
}

fn validate_absolute_no_symlink_components(path: &Path) -> LocalStoreLocatorResult<()> {
    if !path.is_absolute() {
        return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
    }

    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => current.push(component.as_os_str()),
            Component::CurDir | Component::ParentDir => {
                return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
            }
        }
        // A Windows drive prefix by itself is not an absolute path and can be
        // resolved relative to that drive's CWD. Wait for its root component.
        if !current.is_absolute() {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath);
            }
            Ok(_) => {}
            // The remaining tail cannot exist while this ancestor is absent.
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(_) => {
                return Err(LocalStoreLocatorUnavailableReasonV1::FilesystemMetadataUnavailable);
            }
        }
    }
    Ok(())
}

fn verified_locator(
    key: &StoreRuntimeKey,
    kind: LocalStoreLocatorKindV1,
    canonical_profile_root: PathBuf,
    canonical_store_root: PathBuf,
    canonical_path: PathBuf,
    filesystem_safety: &dyn Fn(&Path) -> FilesystemSafety,
) -> LocalStoreLocatorResult<VerifiedLocalStoreLocatorV1> {
    let filesystem = require_local_filesystem(&canonical_path, filesystem_safety)?;
    let locator_digest = canonical_locator_digest(&canonical_path)?;
    let verified =
        VerifiedStoreLocatorV1::new(key.shard_id().clone(), key.incarnation(), locator_digest);
    Ok(VerifiedLocalStoreLocatorV1 {
        locator: ResolvedStoreLocator::new(verified, canonical_path),
        metadata: LocalStoreLocatorMetadataV1 {
            kind,
            canonical_profile_root,
            canonical_store_root,
            filesystem_type: filesystem.filesystem_type,
        },
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalFilesystemMetadata {
    filesystem_type: String,
}

fn require_local_filesystem(
    path: &Path,
    filesystem_safety: &dyn Fn(&Path) -> FilesystemSafety,
) -> LocalStoreLocatorResult<LocalFilesystemMetadata> {
    match filesystem_safety(path) {
        FilesystemSafety::Local { filesystem_type } => {
            Ok(LocalFilesystemMetadata { filesystem_type })
        }
        FilesystemSafety::Network { filesystem_type } => {
            Err(LocalStoreLocatorUnavailableReasonV1::NetworkFilesystem { filesystem_type })
        }
        FilesystemSafety::NotDetectable { filesystem_type } => Err(
            LocalStoreLocatorUnavailableReasonV1::FilesystemLocalityUnverified { filesystem_type },
        ),
    }
}

fn canonical_locator_digest(path: &Path) -> LocalStoreLocatorResult<LocatorDigest> {
    let path = path
        .to_str()
        .ok_or(LocalStoreLocatorUnavailableReasonV1::LocatorDigestUnavailable)?;
    let mut hasher = Sha256::new();
    hasher.update(LOCATOR_DIGEST_DOMAIN);
    hasher.update((path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    LocatorDigest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(|_| LocalStoreLocatorUnavailableReasonV1::LocatorDigestUnavailable)
}

pub fn canonical_store_locator_digest(path: &Path) -> Result<LocatorDigest, String> {
    canonical_locator_digest(path).map_err(|reason| format!("{reason:?}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FilesystemSafety {
    Local { filesystem_type: String },
    Network { filesystem_type: String },
    NotDetectable { filesystem_type: String },
}

const UNKNOWN_FILESYSTEM_TYPE: &str = "unknown";

fn undetectable_filesystem() -> FilesystemSafety {
    FilesystemSafety::NotDetectable {
        filesystem_type: UNKNOWN_FILESYSTEM_TYPE.to_owned(),
    }
}

/// Platform-reported evidence about the volume backing one path.
///
/// `locally_attached` carries the operating system's own answer to "is this
/// storage attached to this machine": macOS reports `MNT_LOCAL` in `statfs`,
/// and Windows distinguishes `DRIVE_REMOTE` from the locally attached drive
/// types. Linux exposes no equivalent per-mount flag, so it leaves this `None`
/// and classification falls back to the mount's filesystem-type name alone.
struct FilesystemEvidence {
    filesystem_type: String,
    locally_attached: Option<bool>,
}

/// Decides locality from platform evidence, fail-closed on anything ambiguous.
///
/// Every platform funnels through here so the three outcomes stay identical
/// across targets, and so the decision itself is testable on any host.
fn classify_filesystem(evidence: FilesystemEvidence) -> FilesystemSafety {
    let FilesystemEvidence {
        filesystem_type,
        locally_attached,
    } = evidence;
    let filesystem_type = if filesystem_type.is_empty() {
        UNKNOWN_FILESYSTEM_TYPE.to_owned()
    } else {
        filesystem_type
    };

    if locally_attached == Some(false) || is_network_filesystem(&filesystem_type) {
        return FilesystemSafety::Network { filesystem_type };
    }
    // A positive kernel locality flag is stronger evidence than any name table,
    // but an unnamed filesystem is still undetectable: refuse rather than mount
    // a volume we cannot describe in the locator's metadata.
    if locally_attached == Some(true) && filesystem_type != UNKNOWN_FILESYSTEM_TYPE {
        return FilesystemSafety::Local { filesystem_type };
    }
    if is_known_local_filesystem(&filesystem_type) {
        return FilesystemSafety::Local { filesystem_type };
    }
    FilesystemSafety::NotDetectable { filesystem_type }
}

/// Returns the deepest ancestor of `path` that exists, `path` itself included.
///
/// A locator legitimately names a store file or directory that resolution has
/// not created yet. Linux answers for such a path from the mount table without
/// touching it; the query-by-path interfaces on other platforms need a real
/// object, and the prospective path's locality is that of the nearest directory
/// that will contain it.
#[cfg(any(target_os = "macos", windows))]
fn nearest_existing_ancestor(path: &Path) -> Option<&Path> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        if fs::symlink_metadata(current).is_ok() {
            return Some(current);
        }
        candidate = current.parent();
    }
    None
}

#[cfg(target_os = "linux")]
fn local_filesystem_safety(path: &Path) -> FilesystemSafety {
    let Ok(mountinfo) = fs::read_to_string("/proc/self/mountinfo") else {
        return undetectable_filesystem();
    };
    filesystem_safety_from_linux_mountinfo(path, &mountinfo)
}

/// Classifies a macOS path from `statfs(2)`.
///
/// `statfs` answers for the exact path rather than for an enumerated mount
/// table, so no longest-prefix matching is needed. `MNT_LOCAL` is the kernel's
/// own record that the mount is served by hardware attached to this machine.
#[cfg(target_os = "macos")]
fn local_filesystem_safety(path: &Path) -> FilesystemSafety {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::os::unix::ffi::OsStrExt;

    let Some(existing) = nearest_existing_ancestor(path) else {
        return undetectable_filesystem();
    };
    let Ok(path) = CString::new(existing.as_os_str().as_bytes()) else {
        return undetectable_filesystem();
    };
    let mut buffer = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is a live NUL-terminated C string for the whole call, and
    // `buffer` is a live, correctly sized, correctly aligned `statfs` slot that
    // the kernel fully initializes when it reports success.
    let status = unsafe { libc::statfs(path.as_ptr(), buffer.as_mut_ptr()) };
    if status != 0 {
        return undetectable_filesystem();
    }
    // SAFETY: `statfs` returned success, so it initialized `buffer`.
    let buffer = unsafe { buffer.assume_init() };

    classify_filesystem(FilesystemEvidence {
        filesystem_type: nul_terminated_c_string(&buffer.f_fstypename),
        locally_attached: Some((buffer.f_flags & (libc::MNT_LOCAL as u32)) != 0),
    })
}

#[cfg(target_os = "macos")]
fn nul_terminated_c_string(field: &[libc::c_char]) -> String {
    let bytes = field
        .iter()
        .take_while(|unit| **unit != 0)
        .map(|unit| *unit as u8)
        .collect::<Vec<u8>>();
    String::from_utf8(bytes)
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Classifies a Windows path from its volume mount point.
///
/// `GetVolumePathNameW` maps the path onto the volume that serves it, including
/// volumes mounted into a directory and UNC shares. `GetDriveTypeW` then
/// separates `DRIVE_REMOTE` from locally attached storage, and
/// `GetVolumeInformationW` names the filesystem for the locator's metadata.
#[cfg(windows)]
fn local_filesystem_safety(path: &Path) -> FilesystemSafety {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
    };

    // `MAX_PATH` plus the terminating NUL, the buffer size both
    // `GetVolumePathNameW` and `GetVolumeInformationW` document.
    const WIDE_BUFFER_CAPACITY: usize = 261;

    let Some(existing) = nearest_existing_ancestor(path) else {
        return undetectable_filesystem();
    };
    let mut requested = existing.as_os_str().encode_wide().collect::<Vec<u16>>();
    if requested.contains(&0) {
        return undetectable_filesystem();
    }
    requested.push(0);

    let mut volume_root = [0u16; WIDE_BUFFER_CAPACITY];
    // SAFETY: `requested` is NUL-terminated and outlives the call, and the
    // declared capacity matches the `volume_root` buffer being written.
    let resolved = unsafe {
        GetVolumePathNameW(
            requested.as_ptr(),
            volume_root.as_mut_ptr(),
            volume_root.len() as u32,
        )
    };
    if resolved == 0 {
        return undetectable_filesystem();
    }

    // SAFETY: the successful call above NUL-terminated `volume_root`.
    let drive_type = unsafe { GetDriveTypeW(volume_root.as_ptr()) };
    let locally_attached = match drive_type {
        DRIVE_REMOTE => Some(false),
        DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_RAMDISK | DRIVE_CDROM => Some(true),
        // `DRIVE_UNKNOWN` and `DRIVE_NO_ROOT_DIR` carry no locality evidence,
        // so fall back to the filesystem name the way Linux does.
        _ => None,
    };

    let mut filesystem_name = [0u16; WIDE_BUFFER_CAPACITY];
    // SAFETY: `volume_root` is NUL-terminated, the null out-pointers are the
    // documented way to skip the fields we do not read, and the declared
    // capacity matches the `filesystem_name` buffer being written.
    let described = unsafe {
        GetVolumeInformationW(
            volume_root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem_name.as_mut_ptr(),
            filesystem_name.len() as u32,
        )
    };
    let filesystem_type = if described == 0 {
        String::new()
    } else {
        nul_terminated_wide_string(&filesystem_name)
    };

    classify_filesystem(FilesystemEvidence {
        filesystem_type,
        locally_attached,
    })
}

#[cfg(windows)]
fn nul_terminated_wide_string(field: &[u16]) -> String {
    let length = field
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(field.len());
    String::from_utf16_lossy(&field[..length]).to_ascii_lowercase()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn local_filesystem_safety(_path: &Path) -> FilesystemSafety {
    // No mount classifier is implemented for these targets. Returning
    // unavailable is safer than assuming an arbitrary path is local.
    undetectable_filesystem()
}

#[cfg(target_os = "linux")]
fn filesystem_safety_from_linux_mountinfo(path: &Path, mountinfo: &str) -> FilesystemSafety {
    let mut best_match = None::<(usize, String)>;
    for line in mountinfo.lines() {
        let Some((before_separator, after_separator)) = line.split_once(" - ") else {
            continue;
        };
        let fields = before_separator.split_whitespace().collect::<Vec<_>>();
        let Some(filesystem_type) = after_separator.split_whitespace().next() else {
            continue;
        };
        let Some(mount_point) = fields
            .get(4)
            .and_then(|value| unescape_mountinfo_path(value))
        else {
            continue;
        };
        if !mount_point.is_absolute() || !path.starts_with(&mount_point) {
            continue;
        }
        let depth = mount_point.components().count();
        if best_match
            .as_ref()
            .is_none_or(|(best_depth, _)| depth > *best_depth)
        {
            best_match = Some((depth, filesystem_type.to_ascii_lowercase()));
        }
    }

    let Some((_, filesystem_type)) = best_match else {
        return undetectable_filesystem();
    };
    classify_filesystem(FilesystemEvidence {
        filesystem_type,
        locally_attached: None,
    })
}

#[cfg(target_os = "linux")]
fn unescape_mountinfo_path(value: &str) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        let digits = bytes.get(index + 1..index + 4)?;
        if !digits.iter().all(u8::is_ascii_digit) || digits.iter().any(|digit| *digit > b'7') {
            return None;
        }
        let value = (digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + (digits[2] - b'0');
        output.push(value);
        index += 4;
    }
    String::from_utf8(output).ok().map(PathBuf::from)
}

/// Lowercase filesystem-type names that always denote remote storage.
///
/// Names come from Linux `mountinfo`, macOS `f_fstypename`, and the Windows
/// volume filesystem name, so the table spans all three vocabularies.
fn is_network_filesystem(filesystem_type: &str) -> bool {
    matches!(
        filesystem_type,
        "9p" | "afpfs"
            | "afs"
            | "ceph"
            | "cifs"
            | "davfs"
            | "ftp"
            | "fuse.sshfs"
            | "gfs"
            | "gfs2"
            | "glusterfs"
            | "lustre"
            | "ncp"
            | "ncpfs"
            | "nfs"
            | "nfs4"
            | "smb"
            | "smb2"
            | "smb3"
            | "smbfs"
            | "webdav"
    )
}

/// Lowercase filesystem-type names known to be backed by attached storage.
fn is_known_local_filesystem(filesystem_type: &str) -> bool {
    matches!(
        filesystem_type,
        "apfs"
            | "btrfs"
            | "bcachefs"
            | "cdfs"
            | "erofs"
            | "exfat"
            | "ext2"
            | "ext3"
            | "ext4"
            | "f2fs"
            | "fat"
            | "fat32"
            | "hfs"
            | "hfsplus"
            | "iso9660"
            | "msdos"
            | "ntfs"
            | "ntfs3"
            | "ramfs"
            | "refs"
            | "squashfs"
            | "tmpfs"
            | "vfat"
            | "xfs"
            | "zfs"
    )
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::fs;

    use tempfile::TempDir;
    use tracedecay_store::{BrainId, ProjectId, StoreIncarnationV1, StoreShardIdV1, UserProfileId};

    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: Debug,
    {
        T::try_from(value.to_owned()).expect("canonical fixture identity")
    }

    fn incarnation() -> StoreIncarnationV1 {
        StoreIncarnationV1::new(1).expect("non-zero fixture incarnation")
    }

    struct Fixture {
        _temporary: TempDir,
        root: PathBuf,
        profile_root: PathBuf,
        first_alias: PathBuf,
        second_alias: PathBuf,
        project_id: ProjectId,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().expect("temporary fixture root");
            // macOS commonly exposes /var through a system symlink. The real
            // resolver requires its authority roots to already be canonical.
            let root = temporary
                .path()
                .canonicalize()
                .expect("canonical fixture root");
            let profile_root = root.join("profile");
            let first_alias = root.join("project-a");
            let second_alias = root.join("project-b");
            fs::create_dir_all(&profile_root).expect("profile root");
            fs::create_dir_all(&first_alias).expect("first alias");
            fs::create_dir_all(&second_alias).expect("second alias");
            let project_id = id::<ProjectId>("project.canonical");
            for project_root in [&first_alias, &second_alias] {
                storage::write_enrollment_marker(
                    project_root,
                    &storage::EnrollmentMarker {
                        project_id: project_id.as_str().to_owned(),
                        storage_mode: storage::StorageMode::ProfileSharded,
                    },
                )
                .expect("write enrollment marker");
            }
            Self {
                _temporary: temporary,
                root,
                profile_root,
                first_alias,
                second_alias,
                project_id,
            }
        }

        fn shard(&self) -> StoreShardIdV1 {
            StoreShardIdV1::project(
                id::<BrainId>("brain.local-resolver"),
                id::<UserProfileId>("profile.local-resolver"),
                self.project_id.clone(),
            )
        }

        fn profile_authority(&self) -> LocalProfileStoreAuthorityV1 {
            LocalProfileStoreAuthorityV1::new(
                id::<BrainId>("brain.local-resolver"),
                id::<UserProfileId>("profile.local-resolver"),
                self.profile_root.clone(),
            )
        }

        fn resolver_for(
            &self,
            roots: impl IntoIterator<Item = PathBuf>,
        ) -> LocalStoreRuntimeResolverV1 {
            LocalStoreRuntimeResolverV1::new(self.profile_authority())
                .with_project_authority(LocalProjectEnrollmentAuthorityV1::new(
                    self.project_id.clone(),
                    roots,
                ))
                .expect("one authority per typed project")
        }
    }

    fn resolve_as_local(
        resolver: &LocalStoreRuntimeResolverV1,
        key: &StoreRuntimeKey,
    ) -> LocalStoreLocatorResolutionV1 {
        resolver.resolve_key_with_filesystem_safety(key, &|_| FilesystemSafety::Local {
            filesystem_type: "fixture-local".to_owned(),
        })
    }

    fn resolved(value: LocalStoreLocatorResolutionV1) -> VerifiedLocalStoreLocatorV1 {
        match value {
            LocalStoreLocatorResolutionV1::Resolved(locator) => locator,
            LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
                panic!("expected resolved locator, got {unavailable:?}")
            }
        }
    }

    #[test]
    fn aliases_converge_by_typed_project_id_without_creating_a_store() {
        let fixture = Fixture::new();
        let key = StoreRuntimeKey::new(fixture.shard(), incarnation());
        let first = resolved(resolve_as_local(
            &fixture.resolver_for([fixture.first_alias.clone()]),
            &key,
        ));
        let second = resolved(resolve_as_local(
            &fixture.resolver_for([fixture.second_alias.clone()]),
            &key,
        ));

        assert_eq!(first.locator().verified().shard_id, key.shard_id().clone());
        assert_eq!(first.locator().path(), second.locator().path());
        assert_eq!(
            first.locator().verified().locator_digest,
            second.locator().verified().locator_digest
        );
        assert_eq!(first.metadata(), second.metadata());
        assert_eq!(
            first.metadata().canonical_store_root,
            fixture
                .profile_root
                .join("projects")
                .join(fixture.project_id.as_str())
        );
        assert!(
            !first.locator().path().exists() && !first.metadata().canonical_store_root.exists(),
            "resolution must not create or require a live database family"
        );
    }

    #[test]
    fn shared_resolver_accepts_a_typed_project_authority_after_construction() {
        let fixture = Fixture::new();
        let resolver = LocalStoreRuntimeResolverV1::new(fixture.profile_authority());
        let key = StoreRuntimeKey::new(fixture.shard(), incarnation());

        assert!(matches!(
            resolve_as_local(&resolver, &key),
            LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                reason: LocalStoreLocatorUnavailableReasonV1::MissingProjectEnrollmentAuthority,
                ..
            })
        ));

        resolver
            .register_project_authority(LocalProjectEnrollmentAuthorityV1::new(
                fixture.project_id.clone(),
                [fixture.first_alias.clone()],
            ))
            .expect("register typed project authority");
        let locator = resolved(resolve_as_local(&resolver, &key));

        assert_eq!(locator.locator().verified().shard_id, fixture.shard());
    }

    #[test]
    fn exact_profile_and_session_layout_mappings_remain_pre_open() {
        let fixture = Fixture::new();
        let resolver = fixture.resolver_for([fixture.first_alias.clone()]);
        let profile_key = StoreRuntimeKey::new(
            StoreShardIdV1::profile(
                id::<BrainId>("brain.local-resolver"),
                id::<UserProfileId>("profile.local-resolver"),
            ),
            incarnation(),
        );
        let profile = resolved(resolve_as_local(&resolver, &profile_key));
        assert_eq!(
            profile.locator().path(),
            fixture.profile_root.join(PROFILE_DATABASE_FILENAME)
        );
        assert_eq!(
            profile.metadata().kind,
            LocalStoreLocatorKindV1::ProfileAuthority
        );

        let profile_memory_key = StoreRuntimeKey::new(
            StoreShardIdV1::profile_memory(
                id::<BrainId>("brain.local-resolver"),
                id::<UserProfileId>("profile.local-resolver"),
            ),
            incarnation(),
        );
        let profile_memory = resolved(resolve_as_local(&resolver, &profile_memory_key));
        assert_eq!(
            profile_memory.locator().path(),
            fixture
                .profile_root
                .join(crate::memory::user::USER_MEMORY_DB_FILENAME)
        );
        assert_eq!(
            profile_memory.metadata().kind,
            LocalStoreLocatorKindV1::ProfileMemory
        );

        let profile_sessions_key = StoreRuntimeKey::new(
            StoreShardIdV1::profile_sessions(
                id::<BrainId>("brain.local-resolver"),
                id::<UserProfileId>("profile.local-resolver"),
            ),
            incarnation(),
        );
        let profile_sessions = resolved(resolve_as_local(&resolver, &profile_sessions_key));
        assert_eq!(
            profile_sessions.locator().path(),
            fixture
                .profile_root
                .join(crate::store_runtime::profile_paths::USER_SESSIONS_DB_FILENAME)
        );
        assert_eq!(
            profile_sessions.metadata().kind,
            LocalStoreLocatorKindV1::ProfileSessions
        );

        let sessions_key = StoreRuntimeKey::new(
            StoreShardIdV1::project_sessions(
                id::<BrainId>("brain.local-resolver"),
                id::<UserProfileId>("profile.local-resolver"),
                fixture.project_id.clone(),
            ),
            incarnation(),
        );
        let sessions = resolved(resolve_as_local(&resolver, &sessions_key));
        assert_eq!(
            sessions.locator().path(),
            fixture
                .profile_root
                .join("projects")
                .join(fixture.project_id.as_str())
                .join(storage::SESSIONS_DB_FILENAME)
        );
        assert_eq!(
            sessions.metadata().kind,
            LocalStoreLocatorKindV1::ProjectSessions
        );
        assert!(
            !profile.locator().path().exists()
                && !profile_memory.locator().path().exists()
                && !profile_sessions.locator().path().exists()
                && !sessions.locator().path().exists(),
            "resolution must not create or read live database files"
        );
    }

    #[test]
    fn project_alias_paths_do_not_influence_the_shard_identity() {
        let fixture = Fixture::new();
        let key = StoreRuntimeKey::new(fixture.shard(), incarnation());
        let locator = resolved(resolve_as_local(
            &fixture.resolver_for([fixture.second_alias.clone()]),
            &key,
        ));

        assert_ne!(
            storage::default_profile_project_id(&fixture.second_alias),
            fixture.project_id.as_str(),
            "fixture protects against accidentally falling back to path-hashed identity"
        );
        assert_eq!(
            locator.locator().verified().shard_id,
            key.shard_id().clone()
        );
        assert_eq!(
            locator.metadata().canonical_store_root,
            fixture
                .profile_root
                .join("projects")
                .join(fixture.project_id.as_str())
        );
    }

    /// Reproduces the intermittent enrollment race: several independent paths
    /// (CLI init, daemon first-touch open, enrollment-root repair) each write
    /// the same marker file, and the resolver may read concurrently. The write
    /// must be atomic — a reader must never observe a present-but-empty or
    /// partially written marker as `InvalidEnrollment`/`MissingEnrollment`.
    #[test]
    fn concurrent_marker_rewrites_never_deny_an_enrolled_project() {
        let fixture = Fixture::new();
        let resolver = fixture.resolver_for([fixture.first_alias.clone()]);
        let key = StoreRuntimeKey::new(fixture.shard(), incarnation());
        let marker = storage::EnrollmentMarker {
            project_id: fixture.project_id.as_str().to_owned(),
            storage_mode: storage::StorageMode::ProfileSharded,
        };

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let project_root = fixture.first_alias.clone();
            let marker = marker.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    storage::write_enrollment_marker(&project_root, &marker)
                        .expect("rewrite enrollment marker");
                }
            })
        };

        let mut denial = None;
        for _ in 0..20_000 {
            match resolve_as_local(&resolver, &key) {
                LocalStoreLocatorResolutionV1::Resolved(_) => {}
                LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
                    denial = Some(unavailable.reason);
                    break;
                }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().expect("marker writer thread");

        assert!(
            denial.is_none(),
            "an enrolled project was denied during a concurrent marker rewrite: {denial:?}"
        );
    }

    #[test]
    fn missing_enrollment_is_typed_unavailable() {
        let temporary = tempfile::tempdir().expect("temporary fixture root");
        let root = temporary
            .path()
            .canonicalize()
            .expect("canonical fixture root");
        let profile_root = root.join("profile");
        let project_root = root.join("project");
        fs::create_dir_all(&profile_root).expect("profile root");
        fs::create_dir_all(&project_root).expect("project root");
        let project_id = id::<ProjectId>("project.missing-enrollment");
        let shard = StoreShardIdV1::project(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            project_id.clone(),
        );
        let resolver = LocalStoreRuntimeResolverV1::new(LocalProfileStoreAuthorityV1::new(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            profile_root,
        ))
        .with_project_authority(LocalProjectEnrollmentAuthorityV1::new(
            project_id,
            [project_root],
        ))
        .expect("one project authority");

        assert!(matches!(
            resolve_as_local(&resolver, &StoreRuntimeKey::new(shard, incarnation())),
            LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                reason: LocalStoreLocatorUnavailableReasonV1::MissingEnrollment,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_profile_root_is_typed_unavailable() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let profile_alias = fixture.root.join("profile-alias");
        symlink(&fixture.profile_root, &profile_alias).expect("profile symlink");
        let resolver = LocalStoreRuntimeResolverV1::new(LocalProfileStoreAuthorityV1::new(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            profile_alias,
        ))
        .with_project_authority(LocalProjectEnrollmentAuthorityV1::new(
            fixture.project_id.clone(),
            [fixture.first_alias.clone()],
        ))
        .expect("one project authority");

        assert!(matches!(
            resolve_as_local(
                &resolver,
                &StoreRuntimeKey::new(fixture.shard(), incarnation())
            ),
            LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                reason: LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath,
                ..
            })
        ));
    }

    #[test]
    fn network_filesystems_are_typed_unavailable() {
        let fixture = Fixture::new();
        let resolver = fixture.resolver_for([fixture.first_alias.clone()]);
        let key = StoreRuntimeKey::new(fixture.shard(), incarnation());

        let resolution =
            resolver.resolve_key_with_filesystem_safety(&key, &|_| FilesystemSafety::Network {
                filesystem_type: "nfs4".to_owned(),
            });
        assert!(matches!(
            resolution,
            LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                reason: LocalStoreLocatorUnavailableReasonV1::NetworkFilesystem {
                    filesystem_type
                },
                ..
            }) if filesystem_type == "nfs4"
        ));
    }

    #[test]
    fn registry_adapter_preserves_safety_critical_unavailability() {
        let shard_id = Fixture::new().shard();
        for (reason, expected) in [
            (
                LocalStoreLocatorUnavailableReasonV1::NetworkFilesystem {
                    filesystem_type: "nfs4".to_owned(),
                },
                StoreRuntimeRegistryFailure::NetworkFilesystemUnavailable {
                    filesystem_type: "nfs4".to_owned(),
                },
            ),
            (
                LocalStoreLocatorUnavailableReasonV1::FilesystemLocalityUnverified {
                    filesystem_type: "overlay".to_owned(),
                },
                StoreRuntimeRegistryFailure::FilesystemLocalityUnavailable {
                    filesystem_type: "overlay".to_owned(),
                },
            ),
            (
                LocalStoreLocatorUnavailableReasonV1::UnsupportedShardScope,
                StoreRuntimeRegistryFailure::UnsupportedShardScope,
            ),
        ] {
            assert_eq!(
                LocalStoreLocatorUnavailableV1 {
                    shard_id: shard_id.clone(),
                    reason,
                }
                .into_registry_failure(),
                expected
            );
        }
    }

    #[test]
    fn unsupported_or_insufficient_typed_inputs_do_not_guess_a_locator() {
        let fixture = Fixture::new();
        let profile_authority = fixture.profile_authority();
        let resolver = LocalStoreRuntimeResolverV1::new(profile_authority);
        let missing_project_authority = StoreRuntimeKey::new(fixture.shard(), incarnation());
        assert!(matches!(
            resolve_as_local(&resolver, &missing_project_authority),
            LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                reason: LocalStoreLocatorUnavailableReasonV1::MissingProjectEnrollmentAuthority,
                ..
            })
        ));

        let code_shard = StoreShardIdV1::code(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            fixture.project_id.clone(),
            id("repository.local-resolver"),
            tracedecay_store::CodeShardScopeV1::Worktree {
                worktree_id: id("worktree.local-resolver"),
            },
        );
        assert!(matches!(
            resolve_as_local(&resolver, &StoreRuntimeKey::new(code_shard, incarnation())),
            LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                reason: LocalStoreLocatorUnavailableReasonV1::MissingCodeStoreAuthority,
                ..
            })
        ));
    }

    #[test]
    fn exact_code_authority_resolves_only_its_typed_code_shard() {
        let fixture = Fixture::new();
        let resolver = LocalStoreRuntimeResolverV1::new(fixture.profile_authority());
        let graph_root = fixture.profile_root.join("code-fixture");
        fs::create_dir_all(&graph_root).expect("graph root");
        let graph_path = graph_root.join("graph.db");
        fs::write(&graph_path, b"fixture").expect("graph fixture");
        let code_shard = StoreShardIdV1::code(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            fixture.project_id.clone(),
            id("repository.local-resolver"),
            tracedecay_store::CodeShardScopeV1::Worktree {
                worktree_id: id("worktree.local-resolver"),
            },
        );
        resolver
            .register_code_authority(
                LocalCodeStoreAuthorityV1::new(code_shard.clone(), graph_path.clone())
                    .expect("typed code authority"),
            )
            .expect("register exact code authority");

        let locator = resolved(resolve_as_local(
            &resolver,
            &StoreRuntimeKey::new(code_shard.clone(), incarnation()),
        ));
        assert_eq!(locator.locator().verified().shard_id, code_shard);
        assert_eq!(
            locator.locator().path(),
            graph_path.canonicalize().expect("canonical graph path")
        );
        assert_eq!(locator.metadata().kind, LocalStoreLocatorKindV1::Code);

        let other_worktree = StoreShardIdV1::code(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            fixture.project_id.clone(),
            id("repository.local-resolver"),
            tracedecay_store::CodeShardScopeV1::Worktree {
                worktree_id: id("worktree.other"),
            },
        );
        assert!(matches!(
            resolve_as_local(
                &resolver,
                &StoreRuntimeKey::new(other_worktree, incarnation())
            ),
            LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                reason: LocalStoreLocatorUnavailableReasonV1::MissingCodeStoreAuthority,
                ..
            })
        ));
    }

    #[test]
    fn branch_code_authority_keeps_its_typed_ref_path_distinct_from_worktree() {
        let fixture = Fixture::new();
        let resolver = LocalStoreRuntimeResolverV1::new(fixture.profile_authority());
        let worktree_id = id::<tracedecay_domain::WorktreeId>("worktree-component");
        let project_id = fixture.project_id.clone();
        let branch_shard = StoreShardIdV1::code(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            project_id.clone(),
            id("repository.local-resolver"),
            tracedecay_store::CodeShardScopeV1::Branch {
                worktree_id: worktree_id.clone(),
                ref_id: id("ref-component"),
            },
        );
        let worktree_shard = StoreShardIdV1::code(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            project_id,
            id("repository.local-resolver"),
            tracedecay_store::CodeShardScopeV1::Worktree { worktree_id },
        );
        let worktree_root = fixture
            .profile_root
            .join("code-fixture")
            .join("worktrees")
            .join("worktree-component");
        let worktree_path = worktree_root.join("graph.db");
        let branch_path = worktree_root
            .join("refs")
            .join("ref-component")
            .join("graph.db");
        fs::create_dir_all(worktree_path.parent().expect("worktree graph parent"))
            .expect("worktree graph root");
        fs::create_dir_all(branch_path.parent().expect("branch graph parent"))
            .expect("branch graph root");
        fs::write(&worktree_path, b"worktree fixture").expect("worktree graph fixture");
        fs::write(&branch_path, b"branch fixture").expect("branch graph fixture");

        for (shard, path) in [
            (worktree_shard.clone(), worktree_path.clone()),
            (branch_shard.clone(), branch_path.clone()),
        ] {
            resolver
                .register_code_authority(
                    LocalCodeStoreAuthorityV1::new(shard, path).expect("typed code authority"),
                )
                .expect("register exact code authority");
        }

        let branch = resolved(resolve_as_local(
            &resolver,
            &StoreRuntimeKey::new(branch_shard.clone(), incarnation()),
        ));
        let worktree = resolved(resolve_as_local(
            &resolver,
            &StoreRuntimeKey::new(worktree_shard.clone(), incarnation()),
        ));

        assert_eq!(branch.locator().verified().shard_id, branch_shard);
        assert_eq!(worktree.locator().verified().shard_id, worktree_shard);
        assert_eq!(
            branch.locator().path(),
            branch_path.canonicalize().expect("canonical branch path")
        );
        assert_eq!(
            worktree.locator().path(),
            worktree_path
                .canonicalize()
                .expect("canonical worktree path")
        );
        assert_ne!(branch.locator().path(), worktree.locator().path());
    }

    #[test]
    fn code_authority_rejects_scope_conflicts_and_paths_outside_profile() {
        let fixture = Fixture::new();
        assert!(matches!(
            LocalCodeStoreAuthorityV1::new(fixture.shard(), fixture.profile_root.join("graph.db")),
            Err(LocalStoreRuntimeResolverConfigurationErrorV1::CodeAuthorityIsNotCodeShard { .. })
        ));

        let code_shard = StoreShardIdV1::code(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            fixture.project_id.clone(),
            id("repository.local-resolver"),
            tracedecay_store::CodeShardScopeV1::Worktree {
                worktree_id: id("worktree.local-resolver"),
            },
        );
        let outside_path = fixture.root.join("outside.db");
        fs::write(&outside_path, b"fixture").expect("outside graph fixture");
        let resolver = LocalStoreRuntimeResolverV1::new(fixture.profile_authority());
        resolver
            .register_code_authority(
                LocalCodeStoreAuthorityV1::new(code_shard.clone(), outside_path)
                    .expect("typed code authority"),
            )
            .expect("register code authority");
        assert!(matches!(
            resolve_as_local(&resolver, &StoreRuntimeKey::new(code_shard, incarnation())),
            LocalStoreLocatorResolutionV1::Unavailable(LocalStoreLocatorUnavailableV1 {
                reason: LocalStoreLocatorUnavailableReasonV1::UnsafeLocatorPath,
                ..
            })
        ));
    }

    #[test]
    fn code_authority_registration_is_idempotent_but_rejects_rebinding() {
        let fixture = Fixture::new();
        let code_shard = StoreShardIdV1::code(
            id::<BrainId>("brain.local-resolver"),
            id::<UserProfileId>("profile.local-resolver"),
            fixture.project_id.clone(),
            id("repository.local-resolver"),
            tracedecay_store::CodeShardScopeV1::Worktree {
                worktree_id: id("worktree.local-resolver"),
            },
        );
        let first_path = fixture.profile_root.join("first.db");
        let second_path = fixture.profile_root.join("second.db");
        let resolver = LocalStoreRuntimeResolverV1::new(fixture.profile_authority());
        let first = LocalCodeStoreAuthorityV1::new(code_shard.clone(), first_path.clone())
            .expect("first authority");

        assert_eq!(
            resolver
                .register_code_authority(first.clone())
                .expect("first registration"),
            LocalCodeStoreAuthorityRegistrationOutcomeV1::Registered
        );
        assert_eq!(
            resolver
                .register_code_authority(first)
                .expect("identical registration is idempotent"),
            LocalCodeStoreAuthorityRegistrationOutcomeV1::AlreadyRegistered
        );
        assert!(matches!(
            resolver.register_code_authority(
                LocalCodeStoreAuthorityV1::new(code_shard.clone(), second_path.clone())
                    .expect("conflicting authority")
            ),
            Err(LocalStoreRuntimeResolverConfigurationErrorV1::DuplicateCodeAuthority { .. })
        ));
        assert!(matches!(
            resolver.retire_code_authority(&code_shard, &second_path),
            Err(
                LocalStoreRuntimeResolverConfigurationErrorV1::CodeAuthorityRetirementMismatch { .. }
            )
        ));
        resolver
            .retire_code_authority(&code_shard, &first_path)
            .expect("retire exact authority");
        resolver
            .register_code_authority(
                LocalCodeStoreAuthorityV1::new(code_shard, second_path)
                    .expect("replacement authority"),
            )
            .expect("retired shard may bind its next physical generation");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_mountinfo_detects_network_filesystems_without_opening_a_store() {
        let mountinfo = concat!(
            "31 24 0:27 / / rw,relatime - ext4 /dev/root rw\n",
            "32 31 0:42 / /network rw,relatime - nfs4 server:/export rw\n"
        );
        assert_eq!(
            filesystem_safety_from_linux_mountinfo(Path::new("/network/projects/proj"), mountinfo),
            FilesystemSafety::Network {
                filesystem_type: "nfs4".to_owned(),
            }
        );
    }

    #[test]
    fn kernel_locality_flag_decides_before_the_filesystem_name_table() {
        // macOS `MNT_LOCAL` and the Windows drive type are direct answers, so a
        // named volume the tables do not list is still local.
        assert_eq!(
            classify_filesystem(FilesystemEvidence {
                filesystem_type: "apfs".to_owned(),
                locally_attached: Some(true),
            }),
            FilesystemSafety::Local {
                filesystem_type: "apfs".to_owned(),
            }
        );
        assert_eq!(
            classify_filesystem(FilesystemEvidence {
                filesystem_type: "some-future-local-fs".to_owned(),
                locally_attached: Some(true),
            }),
            FilesystemSafety::Local {
                filesystem_type: "some-future-local-fs".to_owned(),
            }
        );
    }

    #[test]
    fn classification_refuses_remote_and_undescribable_filesystems() {
        // A cleared locality flag refuses even when the name looks ordinary.
        assert_eq!(
            classify_filesystem(FilesystemEvidence {
                filesystem_type: "ntfs".to_owned(),
                locally_attached: Some(false),
            }),
            FilesystemSafety::Network {
                filesystem_type: "ntfs".to_owned(),
            }
        );
        // A known-remote name refuses even when the platform claims locality.
        assert_eq!(
            classify_filesystem(FilesystemEvidence {
                filesystem_type: "smbfs".to_owned(),
                locally_attached: Some(true),
            }),
            FilesystemSafety::Network {
                filesystem_type: "smbfs".to_owned(),
            }
        );
        // A volume the platform cannot name stays unverified rather than local.
        assert_eq!(
            classify_filesystem(FilesystemEvidence {
                filesystem_type: String::new(),
                locally_attached: Some(true),
            }),
            undetectable_filesystem()
        );
    }

    #[test]
    fn platforms_without_a_locality_flag_fall_back_to_the_name_table() {
        assert_eq!(
            classify_filesystem(FilesystemEvidence {
                filesystem_type: "ext4".to_owned(),
                locally_attached: None,
            }),
            FilesystemSafety::Local {
                filesystem_type: "ext4".to_owned(),
            }
        );
        assert_eq!(
            classify_filesystem(FilesystemEvidence {
                filesystem_type: "unrecognized".to_owned(),
                locally_attached: None,
            }),
            FilesystemSafety::NotDetectable {
                filesystem_type: "unrecognized".to_owned(),
            }
        );
    }

    #[test]
    fn the_running_platform_mounts_an_ordinary_local_store() {
        let fixture = Fixture::new();
        let resolver = fixture.resolver_for([fixture.first_alias.clone()]);
        let key = StoreRuntimeKey::new(fixture.shard(), incarnation());

        // Exercises the real per-platform classifier, not the test double: an
        // ordinary temporary directory must resolve on every supported host.
        match resolver.resolve_key(&key) {
            LocalStoreLocatorResolutionV1::Resolved(_) => {}
            LocalStoreLocatorResolutionV1::Unavailable(unavailable) => {
                panic!("expected a resolved locator on this platform, got {unavailable:?}")
            }
        }
    }
}
