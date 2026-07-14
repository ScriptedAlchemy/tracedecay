use super::*;

pub(super) fn exact_scoped_runtime_role(
    profile_root: &Path,
    intent: &str,
) -> Result<Option<DatabaseAuthorityRole>> {
    let maintenance = MAINTENANCE_SCOPES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let daemon = DAEMON_SCOPES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match (
        maintenance.contains_key(profile_root),
        daemon.contains_key(profile_root),
    ) {
        (true, true) => Err(access_error(
            intent,
            profile_root,
            "daemon and maintenance database scopes overlap",
        )),
        (true, false) => Ok(Some(DatabaseAuthorityRole::Maintenance)),
        (false, true) => Ok(Some(DatabaseAuthorityRole::Daemon)),
        (false, false) => Ok(None),
    }
}

pub(super) fn scoped_runtime_role(
    identity: &DatabaseIdentity,
    intent: &str,
) -> Result<Option<DatabaseAuthorityRole>> {
    if !identity.allows_ambient_profile_scope {
        return Ok(None);
    }
    let maintenance = MAINTENANCE_SCOPES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let daemon = DAEMON_SCOPES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    fallback_scoped_runtime_role(maintenance.len(), daemon.len())
        .map_err(|message| access_error(intent, &identity.profile_root, message))
}

fn fallback_scoped_runtime_role(
    maintenance_count: usize,
    daemon_count: usize,
) -> std::result::Result<Option<DatabaseAuthorityRole>, &'static str> {
    match (maintenance_count, daemon_count) {
        (1, 0) => Ok(Some(DatabaseAuthorityRole::Maintenance)),
        (0, 1) => Ok(Some(DatabaseAuthorityRole::Daemon)),
        (0, 0) => Ok(None),
        _ => Err("database path is ambiguous across active profile authorities"),
    }
}

pub(crate) fn enter_daemon_database_scope(
    profile_root: &Path,
    election_epoch: u64,
    election_token: &str,
) -> Result<DaemonDatabaseScope> {
    if election_token.is_empty() {
        return Err(access_error(
            "enter daemon database scope",
            Path::new("<daemon>"),
            "daemon election token is empty",
        ));
    }
    let profile_root = canonical_profile_root(profile_root)?;
    let token = format!("{election_epoch}:{election_token}");
    let mut scopes = DAEMON_SCOPES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match scopes.get_mut(&profile_root) {
        Some(existing) if existing.token == token => existing.refs += 1,
        Some(_) => {
            return Err(access_error(
                "enter daemon database scope",
                &profile_root,
                "a different daemon election already owns database scope",
            ));
        }
        None => {
            scopes.insert(
                profile_root.clone(),
                DaemonScopeState {
                    token: token.clone(),
                    refs: 1,
                },
            );
        }
    }
    Ok(DaemonDatabaseScope {
        profile_root,
        token,
    })
}

#[doc(hidden)]
pub fn enter_maintenance_database_scope<'lease>(
    lifecycle: &'lease crate::lifecycle_lease::LifecycleLease,
    profile_root: &Path,
    intent: &str,
) -> Result<MaintenanceDatabaseScope<'lease>> {
    if !lifecycle.is_exclusive() {
        return Err(access_error(
            intent,
            Path::new("<maintenance>"),
            "database maintenance requires an exclusive lifecycle lease",
        ));
    }
    if !lifecycle.guards_profile(profile_root) {
        return Err(access_error(
            intent,
            profile_root,
            "exclusive lifecycle lease belongs to a different profile",
        ));
    }
    let profile_root = canonical_profile_root(profile_root)?;
    let lifecycle_token = lifecycle.token().ok_or_else(|| {
        access_error(
            intent,
            Path::new("<maintenance>"),
            "exclusive lifecycle lease has no owner token",
        )
    })?;
    let token = lifecycle_token.to_string();
    let mut scopes = MAINTENANCE_SCOPES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match scopes.get_mut(&profile_root) {
        Some(existing) if existing.token == token => existing.refs += 1,
        Some(_) => {
            return Err(access_error(
                intent,
                &profile_root,
                "a different maintenance operation already owns database scope",
            ));
        }
        None => {
            scopes.insert(
                profile_root.clone(),
                MaintenanceScopeState {
                    token: token.clone(),
                    refs: 1,
                },
            );
        }
    }
    Ok(MaintenanceDatabaseScope {
        profile_root,
        token,
        _lifecycle: std::marker::PhantomData,
    })
}

impl Drop for DaemonDatabaseScope {
    fn drop(&mut self) {
        let mut scopes = DAEMON_SCOPES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_clear = scopes.get_mut(&self.profile_root).is_some_and(|existing| {
            if existing.token != self.token {
                return false;
            }
            existing.refs = existing.refs.saturating_sub(1);
            existing.refs == 0
        });
        if should_clear {
            scopes.remove(&self.profile_root);
        }
    }
}

impl Drop for MaintenanceDatabaseScope<'_> {
    fn drop(&mut self) {
        let mut scopes = MAINTENANCE_SCOPES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_clear = scopes.get_mut(&self.profile_root).is_some_and(|existing| {
            if existing.token != self.token {
                return false;
            }
            existing.refs = existing.refs.saturating_sub(1);
            existing.refs == 0
        });
        if should_clear {
            scopes.remove(&self.profile_root);
        }
    }
}

impl Drop for AuthorityInner {
    fn drop(&mut self) {
        let mut leases = PROCESS_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_remove = leases
            .get_mut(&self.identity.database_key)
            .is_some_and(|lease| match lease {
                ProcessLease::Authority {
                    token,
                    refs,
                    held: _,
                } if token == &self.token => {
                    *refs = refs.saturating_sub(1);
                    *refs == 0
                }
                ProcessLease::Authority { .. } | ProcessLease::Deletion { .. } => false,
            });
        if should_remove {
            if let Some(ProcessLease::Authority { held, .. }) =
                leases.remove(&self.identity.database_key)
            {
                unlock_held(held);
            }
        }
    }
}

pub(super) fn acquire_process_lease(
    identity: &DatabaseIdentity,
    role: DatabaseAuthorityRole,
    intent: &str,
) -> Result<String> {
    let mut leases = PROCESS_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = leases.get_mut(&identity.database_key) {
        let ProcessLease::Authority { token, refs, held } = existing else {
            return Err(access_error(
                intent,
                &identity.database_path,
                "this process already holds an incompatible database deletion fence",
            ));
        };
        let compatible = matches!(
            (&*held, role),
            (
                HeldLocks::Daemon { .. },
                DatabaseAuthorityRole::Daemon | DatabaseAuthorityRole::Test
            ) | (
                HeldLocks::Maintenance { .. },
                DatabaseAuthorityRole::Maintenance
            )
        );
        if !compatible {
            return Err(access_error(
                intent,
                &identity.database_path,
                "this process already holds an incompatible database authority",
            ));
        }
        reject_deletion_tombstone(identity, intent)?;
        *refs += 1;
        return Ok(token.clone());
    }

    let token = authority_token();
    let held = match role {
        DatabaseAuthorityRole::Daemon | DatabaseAuthorityRole::Test => {
            acquire_daemon_locks(identity, &token, intent)?
        }
        DatabaseAuthorityRole::Maintenance => acquire_maintenance_locks(identity, &token, intent)?,
    };
    leases.insert(
        identity.database_key.clone(),
        ProcessLease::Authority {
            token: token.clone(),
            refs: 1,
            held,
        },
    );
    Ok(token)
}

fn acquire_daemon_locks(
    identity: &DatabaseIdentity,
    token: &str,
    intent: &str,
) -> Result<HeldLocks> {
    let access = open_lock_file(&identity.access_lock_path)?;
    fs2::FileExt::try_lock_shared(&access)
        .map_err(|error| lock_acquisition_error("ordinary access", identity, intent, &error))?;

    let writer = match open_lock_file(&identity.writer_lock_path).and_then(|writer| {
        fs2::FileExt::try_lock_exclusive(&writer)
            .map_err(|error| lock_acquisition_error("writer", identity, intent, &error))?;
        Ok(writer)
    }) {
        Ok(writer) => writer,
        Err(error) => {
            let _ = fs2::FileExt::unlock(&access);
            return Err(error);
        }
    };

    if let Err(error) = reject_deletion_tombstone(identity, intent) {
        let _ = fs2::FileExt::unlock(&writer);
        let _ = fs2::FileExt::unlock(&access);
        return Err(error);
    }
    let owner = writer_owner(token, intent);
    if let Err(error) = write_owner(&identity.writer_owner_path, &owner) {
        let _ = fs2::FileExt::unlock(&writer);
        let _ = fs2::FileExt::unlock(&access);
        return Err(error);
    }
    Ok(HeldLocks::Daemon {
        access,
        writer,
        owner,
    })
}

fn acquire_maintenance_locks(
    identity: &DatabaseIdentity,
    token: &str,
    intent: &str,
) -> Result<HeldLocks> {
    let access = open_lock_file(&identity.access_lock_path)?;
    fs2::FileExt::try_lock_exclusive(&access)
        .map_err(|error| lock_acquisition_error("maintenance", identity, intent, &error))?;
    let writer = match open_lock_file(&identity.writer_lock_path).and_then(|writer| {
        fs2::FileExt::try_lock_exclusive(&writer)
            .map_err(|error| lock_acquisition_error("writer", identity, intent, &error))?;
        Ok(writer)
    }) {
        Ok(writer) => writer,
        Err(error) => {
            let _ = fs2::FileExt::unlock(&access);
            return Err(error);
        }
    };
    if let Err(error) = reject_deletion_tombstone(identity, intent) {
        let _ = fs2::FileExt::unlock(&writer);
        let _ = fs2::FileExt::unlock(&access);
        return Err(error);
    }
    let owner = writer_owner(token, intent);
    if let Err(error) = write_owner(&identity.writer_owner_path, &owner) {
        let _ = fs2::FileExt::unlock(&writer);
        let _ = fs2::FileExt::unlock(&access);
        return Err(error);
    }
    Ok(HeldLocks::Maintenance {
        access,
        writer,
        owner,
    })
}

fn unlock_held(held: HeldLocks) {
    match held {
        HeldLocks::Daemon { access, writer, .. }
        | HeldLocks::Maintenance { access, writer, .. } => {
            let _ = fs2::FileExt::unlock(&writer);
            let _ = fs2::FileExt::unlock(&access);
        }
    }
}

impl DatabaseDeletionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Deleting => "deleting",
            Self::Deleted => "deleted",
        }
    }
}

impl DatabaseDeletionStates {
    fn record(&mut self, state: DatabaseDeletionState) {
        match state {
            DatabaseDeletionState::Missing => self.missing += 1,
            DatabaseDeletionState::Deleting => self.deleting += 1,
            DatabaseDeletionState::Deleted => self.deleted += 1,
        }
    }

    pub(crate) fn missing(self) -> usize {
        self.missing
    }

    pub(crate) fn deleting(self) -> usize {
        self.deleting
    }

    pub(crate) fn deleted(self) -> usize {
        self.deleted
    }

    pub(crate) fn has_missing(self) -> bool {
        self.missing != 0
    }

    #[cfg(test)]
    pub(crate) fn has_deleting(self) -> bool {
        self.deleting != 0
    }

    pub(crate) fn has_deleted(self) -> bool {
        self.deleted != 0
    }
}

#[derive(Debug, Eq, PartialEq)]
struct DeletionTombstone {
    state: DatabaseDeletionState,
    transaction_id: String,
}

impl DatabaseDeletionFence {
    pub(crate) fn acquire(database_paths: &[PathBuf], intent: &str) -> Result<Self> {
        let identities = canonical_deletion_identities(database_paths, intent)?;
        let identity_hash = deletion_identity_set_hash(&identities);
        let transaction_id = format!("{identity_hash:016x}:{}", authority_token());
        let (entries, state) = acquire_deletion_locks(
            identities,
            &transaction_id,
            intent,
            DeletionFenceAcquireMode::Fresh,
        )?;
        debug_assert_eq!(state.missing(), entries.len());
        Ok(Self {
            transaction_id,
            entries,
        })
    }

    pub(crate) fn reacquire(
        database_paths: &[PathBuf],
        transaction_id: &str,
        intent: &str,
    ) -> Result<(Self, DatabaseDeletionStates)> {
        let identities = canonical_deletion_identities(database_paths, intent)?;
        validate_deletion_transaction_id(
            transaction_id,
            deletion_identity_set_hash(&identities),
            intent,
            Path::new("<database deletion>"),
        )?;
        let (entries, states) = acquire_deletion_locks(
            identities,
            transaction_id,
            intent,
            DeletionFenceAcquireMode::Recovery,
        )?;
        Ok((
            Self {
                transaction_id: transaction_id.to_string(),
                entries,
            },
            states,
        ))
    }

    pub(crate) fn transaction_id(&self) -> &str {
        &self.transaction_id
    }

    pub(crate) fn database_paths(&self) -> impl ExactSizeIterator<Item = &Path> {
        self.entries
            .iter()
            .map(|entry| entry.identity.database_path.as_path())
    }

    #[cfg(test)]
    pub(crate) fn tombstone_states(&self) -> Result<DatabaseDeletionStates> {
        classify_tombstone_states(
            &self.entries,
            &self.transaction_id,
            "inspect database deletion tombstones",
        )
    }

    #[cfg(test)]
    pub(crate) fn tombstone_paths(&self) -> impl ExactSizeIterator<Item = &Path> {
        self.entries
            .iter()
            .map(|entry| entry.identity.deletion_tombstone_path.as_path())
    }

    pub(crate) fn publish_deleting(&self) -> Result<()> {
        let mut missing = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            match read_deletion_tombstone(&entry.identity)? {
                None => missing.push(true),
                Some(tombstone)
                    if tombstone.transaction_id == self.transaction_id
                        && tombstone.state == DatabaseDeletionState::Deleting =>
                {
                    missing.push(false);
                }
                Some(tombstone) => {
                    return Err(tombstone_transition_error(
                        &entry.identity,
                        "publish database deletion tombstone",
                        &self.transaction_id,
                        &tombstone,
                    ));
                }
            }
        }

        for (entry, missing) in self.entries.iter().zip(missing) {
            if missing {
                write_deletion_tombstone(
                    &entry.identity,
                    &self.transaction_id,
                    DatabaseDeletionState::Deleting,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn promote_deleted(&self) -> Result<()> {
        let mut needs_promotion = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            match read_deletion_tombstone(&entry.identity)? {
                Some(tombstone) if tombstone.transaction_id == self.transaction_id => {
                    needs_promotion.push(tombstone.state == DatabaseDeletionState::Deleting);
                }
                Some(tombstone) => {
                    return Err(tombstone_transition_error(
                        &entry.identity,
                        "promote database deletion tombstone",
                        &self.transaction_id,
                        &tombstone,
                    ));
                }
                None => {
                    return Err(access_error(
                        "promote database deletion tombstone",
                        &entry.identity.database_path,
                        "database deletion tombstone is missing",
                    ));
                }
            }
        }

        for (entry, needs_promotion) in self.entries.iter().zip(needs_promotion) {
            if needs_promotion {
                write_deletion_tombstone(
                    &entry.identity,
                    &self.transaction_id,
                    DatabaseDeletionState::Deleted,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn rollback_deleting(&self) -> Result<()> {
        let mut present = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            match read_deletion_tombstone(&entry.identity)? {
                None => present.push(false),
                Some(tombstone)
                    if tombstone.transaction_id == self.transaction_id
                        && tombstone.state == DatabaseDeletionState::Deleting =>
                {
                    present.push(true);
                }
                Some(tombstone) => {
                    return Err(tombstone_transition_error(
                        &entry.identity,
                        "rollback database deletion tombstone",
                        &self.transaction_id,
                        &tombstone,
                    ));
                }
            }
        }

        for (entry, present) in self.entries.iter().zip(present) {
            if present {
                remove_record_durably(
                    &entry.identity.deletion_tombstone_path,
                    "database deletion tombstone",
                )?;
            }
        }
        Ok(())
    }
}

impl Drop for DatabaseDeletionFence {
    fn drop(&mut self) {
        {
            let mut leases = PROCESS_LEASES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for entry in self.entries.iter().rev() {
                let _ = fs2::FileExt::unlock(&entry.writer);
                let _ = fs2::FileExt::unlock(&entry.access);
                let owns_process_lease = matches!(
                    leases.get(&entry.identity.database_key),
                    Some(ProcessLease::Deletion { transaction_id, .. })
                        if transaction_id == &self.transaction_id
                );
                if owns_process_lease {
                    leases.remove(&entry.identity.database_key);
                }
            }
        }
        let bootstrap = DELETION_BOOTSTRAP_AUTHORITIES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.transaction_id);
        drop(bootstrap);
    }
}

#[derive(Clone, Copy)]
enum DeletionFenceAcquireMode {
    Fresh,
    Recovery,
}

static DELETION_BOOTSTRAP_AUTHORITIES: LazyLock<Mutex<HashMap<String, Vec<BootstrapAuthority>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn canonical_deletion_identities(
    database_paths: &[PathBuf],
    intent: &str,
) -> Result<Vec<DatabaseIdentity>> {
    if database_paths.is_empty() {
        return Err(access_error(
            intent,
            Path::new("<database deletion>"),
            "database deletion fence requires at least one database path",
        ));
    }
    let mut identities = database_paths
        .iter()
        .map(|path| DatabaseIdentity::for_path(path))
        .collect::<Result<Vec<_>>>()?;
    identities.sort_by_cached_key(deletion_identity_key);
    identities.dedup_by(|left, right| deletion_identity_key(left) == deletion_identity_key(right));
    Ok(identities)
}

fn deletion_identity_key(identity: &DatabaseIdentity) -> PathBuf {
    deletion_bootstrap_key(identity).unwrap_or_else(|| identity.database_key.clone())
}

fn deletion_bootstrap_key(identity: &DatabaseIdentity) -> Option<PathBuf> {
    let parent = identity
        .database_path
        .parent()
        .unwrap_or(&identity.profile_root);
    let file_name = identity.database_path.file_name().unwrap_or_default();
    bootstrap_database_key(parent, file_name)
}

fn deletion_bootstrap_lock_path(identity: &DatabaseIdentity) -> Option<PathBuf> {
    deletion_bootstrap_key(identity).map(|key| {
        let lock_root = identity
            .access_lock_path
            .parent()
            .unwrap_or(&identity.profile_root);
        lock_root.join(format!("{:016x}.bootstrap.lock", stable_path_hash(&key)))
    })
}

fn deletion_identity_set_hash(identities: &[DatabaseIdentity]) -> u64 {
    let keys = identities
        .iter()
        .map(deletion_identity_key)
        .collect::<Vec<_>>();
    stable_path_set_hash(keys.iter().map(PathBuf::as_path))
}

fn acquire_deletion_locks(
    identities: Vec<DatabaseIdentity>,
    transaction_id: &str,
    intent: &str,
    mode: DeletionFenceAcquireMode,
) -> Result<(Vec<DeletionFenceEntry>, DatabaseDeletionStates)> {
    let mut bootstrap_authorities = Vec::new();
    for identity in &identities {
        let mut bootstrap_identity = identity.clone();
        bootstrap_identity.bootstrap_lock_path = deletion_bootstrap_lock_path(identity);
        if let Some(authority) = acquire_bootstrap_authority(&bootstrap_identity, intent)? {
            bootstrap_authorities.push(authority);
        }
    }

    let mut leases = PROCESS_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for identity in &identities {
        if leases.contains_key(&identity.database_key) {
            return Err(access_error(
                intent,
                &identity.database_path,
                "this process already holds an incompatible database authority or deletion fence",
            ));
        }
    }

    let mut entries = Vec::with_capacity(identities.len());
    for identity in identities {
        let access = match open_lock_file(&identity.access_lock_path).and_then(|access| {
            fs2::FileExt::try_lock_exclusive(&access).map_err(|error| {
                lock_acquisition_error("deletion access", &identity, intent, &error)
            })?;
            Ok(access)
        }) {
            Ok(access) => access,
            Err(error) => {
                unlock_deletion_entries(&entries);
                return Err(error);
            }
        };
        let writer = match open_lock_file(&identity.writer_lock_path).and_then(|writer| {
            fs2::FileExt::try_lock_exclusive(&writer)
                .map_err(|error| lock_acquisition_error("writer", &identity, intent, &error))?;
            Ok(writer)
        }) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = fs2::FileExt::unlock(&access);
                unlock_deletion_entries(&entries);
                return Err(error);
            }
        };
        entries.push(DeletionFenceEntry {
            identity,
            access,
            writer,
        });
    }

    let state = match mode {
        DeletionFenceAcquireMode::Fresh => entries
            .iter()
            .try_for_each(|entry| reject_deletion_tombstone(&entry.identity, intent))
            .map(|()| DatabaseDeletionStates {
                missing: entries.len(),
                ..DatabaseDeletionStates::default()
            }),
        DeletionFenceAcquireMode::Recovery => {
            classify_tombstone_states(&entries, transaction_id, intent)
        }
    };
    let state = match state {
        Ok(state) => state,
        Err(error) => {
            unlock_deletion_entries(&entries);
            return Err(error);
        }
    };

    let owner = writer_owner(transaction_id, intent);
    for entry in &entries {
        if let Err(error) = write_owner(&entry.identity.writer_owner_path, &owner) {
            unlock_deletion_entries(&entries);
            return Err(error);
        }
    }
    if !bootstrap_authorities.is_empty() {
        let mut active = DELETION_BOOTSTRAP_AUTHORITIES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match active.entry(transaction_id.to_string()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(bootstrap_authorities);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                drop(active);
                unlock_deletion_entries(&entries);
                return Err(access_error(
                    intent,
                    Path::new("<database deletion>"),
                    "database deletion transaction is already active",
                ));
            }
        }
    }
    for entry in &entries {
        leases.insert(
            entry.identity.database_key.clone(),
            ProcessLease::Deletion {
                transaction_id: transaction_id.to_string(),
                owner: owner.clone(),
            },
        );
    }
    Ok((entries, state))
}

fn unlock_deletion_entries(entries: &[DeletionFenceEntry]) {
    for entry in entries.iter().rev() {
        let _ = fs2::FileExt::unlock(&entry.writer);
        let _ = fs2::FileExt::unlock(&entry.access);
    }
}

fn validate_deletion_transaction_id(
    transaction_id: &str,
    expected_path_hash: u64,
    operation: &str,
    path: &Path,
) -> Result<()> {
    if !valid_deletion_transaction_id(transaction_id) {
        return Err(access_error(
            operation,
            path,
            "database deletion transaction ID is invalid",
        ));
    }
    let Some((path_hash, token)) = transaction_id.split_once(':') else {
        return Err(access_error(
            operation,
            path,
            "database deletion transaction ID is invalid",
        ));
    };
    let parsed_hash = (path_hash.len() == 16 && !token.is_empty())
        .then(|| u64::from_str_radix(path_hash, 16).ok())
        .flatten()
        .ok_or_else(|| {
            access_error(
                operation,
                path,
                "database deletion transaction ID is invalid",
            )
        })?;
    if parsed_hash != expected_path_hash {
        return Err(access_error(
            operation,
            path,
            "database deletion transaction ID does not match the database path set",
        ));
    }
    Ok(())
}

fn valid_deletion_transaction_id(transaction_id: &str) -> bool {
    !transaction_id.is_empty()
        && transaction_id.len() <= 512
        && !transaction_id.chars().any(char::is_control)
}

fn classify_tombstone_states(
    entries: &[DeletionFenceEntry],
    transaction_id: &str,
    operation: &str,
) -> Result<DatabaseDeletionStates> {
    let mut states = DatabaseDeletionStates::default();
    for entry in entries {
        let state = match read_deletion_tombstone(&entry.identity)? {
            None => DatabaseDeletionState::Missing,
            Some(tombstone) if tombstone.transaction_id == transaction_id => tombstone.state,
            Some(tombstone) => {
                return Err(tombstone_transition_error(
                    &entry.identity,
                    operation,
                    transaction_id,
                    &tombstone,
                ));
            }
        };
        states.record(state);
    }
    Ok(states)
}

fn reject_deletion_tombstone(identity: &DatabaseIdentity, intent: &str) -> Result<()> {
    let Some(tombstone) = read_deletion_tombstone(identity)? else {
        return Ok(());
    };
    let message = match tombstone.state {
        DatabaseDeletionState::Missing => {
            return Err(corrupt_tombstone_error(
                identity,
                "record encodes missing state instead of being absent",
            ));
        }
        DatabaseDeletionState::Deleting => format!(
            "database deletion is in progress for transaction {}",
            tombstone.transaction_id
        ),
        DatabaseDeletionState::Deleted => format!(
            "database was deleted by transaction {}",
            tombstone.transaction_id
        ),
    };
    Err(access_error(intent, &identity.database_path, &message))
}

fn read_deletion_tombstone(identity: &DatabaseIdentity) -> Result<Option<DeletionTombstone>> {
    let Some(record) = read_record_strict(
        &identity.deletion_tombstone_path,
        "database deletion tombstone",
    )?
    else {
        return Ok(None);
    };
    parse_deletion_tombstone(identity, &record).map(Some)
}

fn parse_deletion_tombstone(
    identity: &DatabaseIdentity,
    record: &str,
) -> Result<DeletionTombstone> {
    let payload = record
        .strip_suffix('\n')
        .ok_or_else(|| corrupt_tombstone_error(identity, "record is not newline terminated"))?;
    if payload.contains('\r') || payload.contains('\n') {
        return Err(corrupt_tombstone_error(
            identity,
            "record contains multiple lines",
        ));
    }

    let mut fields = HashMap::new();
    for field in payload.split('\t') {
        let (key, value) = field
            .split_once('=')
            .ok_or_else(|| corrupt_tombstone_error(identity, "record field has no value"))?;
        if fields.insert(key, value).is_some() {
            return Err(corrupt_tombstone_error(
                identity,
                "record field is duplicated",
            ));
        }
    }
    if fields.len() != 4 || fields.get("version") != Some(&"1") {
        return Err(corrupt_tombstone_error(
            identity,
            "record version or field set is invalid",
        ));
    }

    let state = match fields.get("state") {
        Some(&"deleting") => DatabaseDeletionState::Deleting,
        Some(&"deleted") => DatabaseDeletionState::Deleted,
        _ => return Err(corrupt_tombstone_error(identity, "record state is invalid")),
    };
    let transaction_id = fields.get("transaction_id").copied().unwrap_or_default();
    if !valid_deletion_transaction_id(transaction_id) {
        return Err(corrupt_tombstone_error(
            identity,
            "record transaction ID is invalid",
        ));
    }
    let database_id = fields.get("database_id").copied().unwrap_or_default();
    if database_id.len() != 16
        || u64::from_str_radix(database_id, 16).ok() != Some(identity.database_id)
    {
        return Err(corrupt_tombstone_error(
            identity,
            "record database identity does not match the canonical database",
        ));
    }

    Ok(DeletionTombstone {
        state,
        transaction_id: transaction_id.to_string(),
    })
}

fn write_deletion_tombstone(
    identity: &DatabaseIdentity,
    transaction_id: &str,
    state: DatabaseDeletionState,
) -> Result<()> {
    let payload = format!(
        "version=1\tstate={}\ttransaction_id={}\tdatabase_id={:016x}\n",
        state.as_str(),
        transaction_id,
        identity.database_id
    );
    write_record_atomically(
        &identity.deletion_tombstone_path,
        payload.as_bytes(),
        "database deletion tombstone",
    )?;
    match read_deletion_tombstone(identity)? {
        Some(tombstone)
            if tombstone.transaction_id == transaction_id && tombstone.state == state =>
        {
            Ok(())
        }
        Some(tombstone) => Err(tombstone_transition_error(
            identity,
            "verify database deletion tombstone",
            transaction_id,
            &tombstone,
        )),
        None => Err(access_error(
            "verify database deletion tombstone",
            &identity.database_path,
            "database deletion tombstone disappeared after publication",
        )),
    }
}

fn corrupt_tombstone_error(identity: &DatabaseIdentity, reason: &str) -> TraceDecayError {
    access_error(
        "read database deletion tombstone",
        &identity.database_path,
        &format!("database deletion tombstone is corrupt: {reason}"),
    )
}

fn tombstone_transition_error(
    identity: &DatabaseIdentity,
    operation: &str,
    expected_transaction_id: &str,
    tombstone: &DeletionTombstone,
) -> TraceDecayError {
    let message = if tombstone.transaction_id == expected_transaction_id {
        format!(
            "database deletion tombstone is already {} and cannot perform this transition",
            tombstone.state.as_str()
        )
    } else {
        format!(
            "database deletion tombstone belongs to transaction {}, not {}",
            tombstone.transaction_id, expected_transaction_id
        )
    };
    access_error(operation, &identity.database_path, &message)
}

pub(crate) fn database_path_is_tombstoned(db_path: &Path) -> Result<bool> {
    let identity = DatabaseIdentity::for_path(db_path)?;
    read_deletion_tombstone(&identity).map(|tombstone| tombstone.is_some())
}

pub(crate) fn probe_writer_owner(db_path: &Path) -> Result<WriterOwnership> {
    let identity = DatabaseIdentity::for_path(db_path)?;
    {
        let leases = PROCESS_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lease) = leases.get(&identity.database_key) {
            let owner = match lease {
                ProcessLease::Authority {
                    held: HeldLocks::Daemon { owner, .. } | HeldLocks::Maintenance { owner, .. },
                    ..
                }
                | ProcessLease::Deletion { owner, .. } => owner,
            };
            return Ok(WriterOwnership::Active(owner.clone()));
        }
    }

    let writer = open_lock_file(&identity.writer_lock_path)?;
    match fs2::FileExt::try_lock_exclusive(&writer) {
        Ok(()) => {
            let _ = fs2::FileExt::unlock(&writer);
            Ok(WriterOwnership::Idle)
        }
        Err(error) if is_lock_contended(&error) => Ok(read_owner(&identity.writer_owner_path)
            .map(WriterOwnership::Active)
            .unwrap_or(WriterOwnership::ActiveUnknown)),
        Err(error) => Err(access_io_error(
            "probe writer",
            &identity.writer_lock_path,
            &error,
        )),
    }
}

fn lock_acquisition_error(
    kind: &str,
    identity: &DatabaseIdentity,
    intent: &str,
    error: &std::io::Error,
) -> TraceDecayError {
    if is_lock_contended(error) {
        access_error(
            intent,
            &identity.database_path,
            &format!("{kind} lease is held by another process"),
        )
    } else {
        access_io_error(
            &format!("acquire {kind} lease for {intent}"),
            &identity.database_path,
            error,
        )
    }
}
