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

pub(super) fn fallback_scoped_runtime_role(
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

pub fn enter_daemon_database_scope(
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
    let (profile_root, token) =
        register_maintenance_database_scope(lifecycle, profile_root, intent)?;
    Ok(MaintenanceDatabaseScope {
        profile_root,
        token,
        _lifecycle: std::marker::PhantomData,
    })
}

#[cfg(not(test))]
pub fn enter_owned_maintenance_database_scope(
    lifecycle: crate::lifecycle_lease::LifecycleLease,
    profile_root: &Path,
    intent: &str,
) -> Result<OwnedMaintenanceDatabaseScope> {
    let (profile_root, token) =
        register_maintenance_database_scope(&lifecycle, profile_root, intent)?;
    Ok(OwnedMaintenanceDatabaseScope {
        profile_root,
        token,
        _lifecycle: lifecycle,
    })
}

fn register_maintenance_database_scope(
    lifecycle: &crate::lifecycle_lease::LifecycleLease,
    profile_root: &Path,
    intent: &str,
) -> Result<(PathBuf, String)> {
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
    Ok((profile_root, token))
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
        release_maintenance_database_scope(&self.profile_root, &self.token);
    }
}

impl Drop for OwnedMaintenanceDatabaseScope {
    fn drop(&mut self) {
        release_maintenance_database_scope(&self.profile_root, &self.token);
    }
}

fn release_maintenance_database_scope(profile_root: &Path, token: &str) {
    let mut scopes = MAINTENANCE_SCOPES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let should_clear = scopes.get_mut(profile_root).is_some_and(|existing| {
        if existing.token != token {
            return false;
        }
        existing.refs = existing.refs.saturating_sub(1);
        existing.refs == 0
    });
    if should_clear {
        scopes.remove(profile_root);
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
            leases.remove(&self.identity.database_key);
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
            (existing.role, role),
            (
                DatabaseAuthorityRole::Daemon | DatabaseAuthorityRole::Test,
                DatabaseAuthorityRole::Daemon | DatabaseAuthorityRole::Test
            ) | (
                DatabaseAuthorityRole::Maintenance,
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
    leases.insert(
        identity.database_key.clone(),
        ProcessLease {
            token: token.clone(),
            refs: 1,
            role,
            owner: writer_owner(&token, intent),
        },
    );
    Ok(token)
}

pub fn probe_writer_owner(db_path: &Path) -> Result<WriterOwnership> {
    let identity = DatabaseIdentity::for_path(db_path)?;
    Ok(PROCESS_LEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&identity.database_key)
        .map(|lease| WriterOwnership::Active(lease.owner.clone()))
        .unwrap_or(WriterOwnership::Idle))
}
