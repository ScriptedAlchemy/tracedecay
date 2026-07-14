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
            .is_some_and(|lease| {
                if lease.token != self.token {
                    return false;
                }
                lease.refs = lease.refs.saturating_sub(1);
                lease.refs == 0
            });
        if should_remove {
            if let Some(lease) = leases.remove(&self.identity.database_key) {
                unlock_held(lease.held);
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
        let compatible = matches!(
            (&existing.held, role),
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
        existing.refs += 1;
        return Ok(existing.token.clone());
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
        ProcessLease {
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
        HeldLocks::Daemon { access, writer, .. } => {
            let _ = fs2::FileExt::unlock(&writer);
            let _ = fs2::FileExt::unlock(&access);
        }
        HeldLocks::Maintenance { access, writer, .. } => {
            let _ = fs2::FileExt::unlock(&writer);
            let _ = fs2::FileExt::unlock(&access);
        }
    }
}

pub(crate) fn probe_writer_owner(db_path: &Path) -> Result<WriterOwnership> {
    let identity = DatabaseIdentity::for_path(db_path)?;
    {
        let leases = PROCESS_LEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lease) = leases.get(&identity.database_key) {
            let owner = match &lease.held {
                HeldLocks::Daemon { owner, .. } | HeldLocks::Maintenance { owner, .. } => owner,
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
