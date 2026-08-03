use super::{
    Arc, Connection, Database, DatabaseAccessMode, DatabaseAuthority, DatabaseInner, Path, Result,
    StoreRuntimeHandle, TraceDecayError, database_slot, integrity, registered_attachment_required,
};

impl Database {
    pub fn retained_runtime(&self) -> &StoreRuntimeHandle {
        &self.inner._runtime
    }

    /// Canonical path held by this database's verified runtime locator.
    pub fn canonical_database_path(&self) -> &Path {
        &self.inner.canonical_path
    }

    /// Returns the canonical path bound to this already-open database.
    ///
    /// Primarily exposed for read-only inspection and integration fixtures;
    /// callers must not treat the path as a substitute for write authority.
    #[doc(hidden)]
    pub fn database_path(&self) -> &Path {
        self.canonical_database_path()
    }

    /// Physical `SQLite` identity captured when this retained handle was opened.
    pub fn opened_file_identity(&self) -> u64 {
        self.inner.opened_file_identity
    }

    pub fn filesystem_is_read_only(&self) -> bool {
        std::fs::metadata(self.canonical_database_path())
            .is_ok_and(|metadata| metadata.permissions().readonly())
    }

    /// Clones the originating revocable write capability for actor-time checks.
    pub fn write_authority(&self) -> Result<DatabaseAuthority> {
        if !self.inner.writable {
            return Err(integrity::read_only_upgrade_error(
                self.canonical_database_path(),
                "acquire database write authority",
            ));
        }
        self.inner
            ._authority
            .clone()
            .ok_or_else(|| TraceDecayError::Database {
                message: "writable database facade has no originating authority".to_owned(),
                operation: "acquire database write authority".to_owned(),
            })
    }

    /// Publishes one verified registry runtime as the only physical owner of
    /// this database path.
    ///
    /// The runtime already carries its typed binding, verified locator, and
    /// opened file identity. A read-write facade additionally retains the
    /// originating authority; a read-only facade never requests it. Neither
    /// mode derives identity from a path or extracts the physical attachment.
    pub async fn publish_runtime(
        runtime: StoreRuntimeHandle,
        access: DatabaseAccessMode,
    ) -> Result<Self> {
        let writable = access.is_writable();
        let authority = if writable {
            if !runtime.writer_present() {
                return Err(TraceDecayError::Database {
                    message: "registered runtime has no physical writer".to_owned(),
                    operation: "publish database runtime".to_owned(),
                });
            }
            let authority = runtime
                .database_authority("publish database runtime")
                .map_err(|error| TraceDecayError::Database {
                    message: format!("{error:?}"),
                    operation: "publish database runtime".to_owned(),
                })?;
            authority.require_active_write_scope("publish database runtime")?;
            Some(authority)
        } else {
            None
        };
        let slot = authority
            .as_ref()
            .map(|authority| database_slot(authority.database_identity_key()));
        if let Some(slot) = &slot {
            let mut open = slot.lock().await;
            if let Some(inner) = open.upgrade() {
                return Ok(Self { inner });
            }
            let inner = Arc::new(DatabaseInner::publish(
                runtime,
                true,
                authority,
                Some(Arc::clone(slot)),
            )?);
            *open = Arc::downgrade(&inner);
            return Ok(Self { inner });
        }
        DatabaseInner::publish(runtime, false, None, None)
            .map(Arc::new)
            .map(|inner| Self { inner })
    }

    /// Legacy compatibility lookup.
    ///
    /// Physical creation and schema bootstrap are owned by the registered
    /// runtime. This method can reuse an attachment already published for the
    /// exact authority, but it never opens a path or invents store identity.
    pub async fn initialize(db_path: &Path, authority: &DatabaseAuthority) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "initialize")?;
        authority.require_active_write_scope("initialize")?;
        let slot = database_slot(authority.database_identity_key());
        let open = slot.lock().await;
        if let Some(inner) = open.upgrade() {
            if !inner.writable {
                return Err(integrity::read_only_upgrade_error(db_path, "initialize"));
            }
            return Ok((Self { inner }, false));
        }
        Err(registered_attachment_required("initialize", db_path))
    }

    /// Reuses an already-published writable attachment for `db_path`.
    pub async fn open(db_path: &Path, authority: &DatabaseAuthority) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "open")?;
        authority.require_active_write_scope("open")?;
        let slot = database_slot(authority.database_identity_key());
        let open = slot.lock().await;
        if let Some(inner) = open.upgrade() {
            if !inner.writable {
                return Err(integrity::read_only_upgrade_error(db_path, "open"));
            }
            return Ok((Self { inner }, false));
        }
        Err(registered_attachment_required("open", db_path))
    }

    /// Reuses an already-published attachment for a read-only caller.
    pub async fn open_read_only(
        db_path: &Path,
        authority: &DatabaseAuthority,
    ) -> Result<(Self, bool)> {
        let authority = authority.hold_for(db_path, "open_read_only")?;
        let slot = database_slot(authority.database_identity_key());
        if let Some(inner) = slot.lock().await.upgrade() {
            let read_only = DatabaseInner::publish(inner._runtime.clone(), false, None, None)?;
            return Ok((
                Self {
                    inner: Arc::new(read_only),
                },
                false,
            ));
        }
        Err(registered_attachment_required("open_read_only", db_path))
    }

    /// Returns the canonical runtime facade.
    ///
    /// Mutations must use [`Self::writer_connection`] or an isolated
    /// transaction while holding [`Self::writer`].
    pub fn conn(&self) -> &Connection {
        &self.inner.conn
    }
}
