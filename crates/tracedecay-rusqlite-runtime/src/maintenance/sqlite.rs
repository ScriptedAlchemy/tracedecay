use std::collections::BTreeMap;

use rusqlite::{Connection, TransactionBehavior};

use super::{
    CompactionMode, DriverMaintenanceError, ExclusiveMaintenancePermit, FtsIndexId,
    MaintenanceDriver, MigrationPlanId, VerifiedMaintenanceArtifact,
};

#[derive(Clone, Debug)]
pub struct SqliteMigration {
    id: MigrationPlanId,
    target_user_version: u32,
    statements: Vec<String>,
}

impl SqliteMigration {
    pub fn new(
        id: MigrationPlanId,
        target_user_version: u32,
        statements: Vec<String>,
    ) -> Option<Self> {
        (!statements.is_empty()).then_some(Self {
            id,
            target_user_version,
            statements,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SqliteFtsIndex {
    id: FtsIndexId,
    table: String,
}

impl SqliteFtsIndex {
    pub fn new(id: FtsIndexId, table: impl Into<String>) -> Option<Self> {
        let table = table.into();
        valid_identifier(&table).then_some(Self { id, table })
    }

    pub(crate) fn table(&self) -> &str {
        &self.table
    }
}

#[derive(Clone, Debug, Default)]
pub struct SqliteMaintenanceCatalog {
    migrations: BTreeMap<String, SqliteMigration>,
    fts_indexes: BTreeMap<String, SqliteFtsIndex>,
}

impl SqliteMaintenanceCatalog {
    pub fn new(
        migrations: impl IntoIterator<Item = SqliteMigration>,
        fts_indexes: impl IntoIterator<Item = SqliteFtsIndex>,
    ) -> Option<Self> {
        let mut catalog = Self::default();
        for migration in migrations {
            if catalog
                .migrations
                .insert(migration.id.as_str().to_owned(), migration)
                .is_some()
            {
                return None;
            }
        }
        for index in fts_indexes {
            if catalog
                .fts_indexes
                .insert(index.id.as_str().to_owned(), index)
                .is_some()
            {
                return None;
            }
        }
        Some(catalog)
    }
}

pub trait MaintenanceArtifactInstaller {
    fn restore(
        &mut self,
        connection: &mut Connection,
        permit: &ExclusiveMaintenancePermit,
        artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError>;

    fn replace_shard(
        &mut self,
        connection: &mut Connection,
        permit: &ExclusiveMaintenancePermit,
        artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError>;
}

pub struct SqliteMaintenanceDriver<I> {
    connection: Connection,
    catalog: SqliteMaintenanceCatalog,
    artifacts: I,
}

impl<I> SqliteMaintenanceDriver<I> {
    /// Takes ownership of the writer's sole open connection after drain.
    pub fn from_writer_connection(
        connection: Connection,
        catalog: SqliteMaintenanceCatalog,
        artifacts: I,
    ) -> Self {
        Self {
            connection,
            catalog,
            artifacts,
        }
    }

    pub fn close(self) -> rusqlite::Result<()> {
        self.connection.close().map_err(|(_, error)| error)
    }
}

impl<I: MaintenanceArtifactInstaller> MaintenanceDriver for SqliteMaintenanceDriver<I> {
    fn migrate(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        plan: &MigrationPlanId,
    ) -> Result<(), DriverMaintenanceError> {
        let migration = self
            .catalog
            .migrations
            .get(plan.as_str())
            .ok_or_else(|| driver_error("unknown_migration_plan", false))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| driver_error("begin_migration", true))?;
        let current: u32 = transaction
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| driver_error("read_schema_version", false))?;
        if current > migration.target_user_version {
            return Err(driver_error("migration_would_downgrade", false));
        }
        if current == migration.target_user_version {
            return transaction
                .commit()
                .map_err(|_| driver_error("commit_migration", true));
        }
        for statement in &migration.statements {
            transaction
                .execute_batch(statement)
                .map_err(|_| driver_error("execute_migration", false))?;
        }
        transaction
            .pragma_update(None, "user_version", migration.target_user_version)
            .map_err(|_| driver_error("write_schema_version", false))?;
        transaction
            .commit()
            .map_err(|_| driver_error("commit_migration", true))
    }

    fn rebuild_fts(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        index: &FtsIndexId,
    ) -> Result<(), DriverMaintenanceError> {
        let index = self
            .catalog
            .fts_indexes
            .get(index.as_str())
            .ok_or_else(|| driver_error("unknown_fts_index", false))?;
        let table = quoted_identifier(index.table());
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| driver_error("begin_fts_rebuild", true))?;
        transaction
            .execute(
                &format!("INSERT INTO {table}({table}) VALUES ('rebuild')"),
                [],
            )
            .map_err(|_| driver_error("rebuild_fts", false))?;
        transaction
            .execute(
                &format!("INSERT INTO {table}({table}) VALUES ('optimize')"),
                [],
            )
            .map_err(|_| driver_error("optimize_fts", false))?;
        transaction
            .commit()
            .map_err(|_| driver_error("commit_fts_rebuild", true))
    }

    fn restore(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError> {
        self.artifacts
            .restore(&mut self.connection, permit, artifact)
    }

    fn compact(
        &mut self,
        _permit: &ExclusiveMaintenancePermit,
        mode: CompactionMode,
    ) -> Result<(), DriverMaintenanceError> {
        match mode {
            CompactionMode::Incremental => self
                .connection
                .execute_batch("PRAGMA incremental_vacuum")
                .map_err(|_| driver_error("incremental_compaction", true)),
            CompactionMode::Full => self
                .connection
                .execute_batch("VACUUM")
                .map_err(|_| driver_error("full_compaction", true)),
        }
    }

    fn replace_shard(
        &mut self,
        permit: &ExclusiveMaintenancePermit,
        artifact: &VerifiedMaintenanceArtifact,
    ) -> Result<(), DriverMaintenanceError> {
        self.artifacts
            .replace_shard(&mut self.connection, permit, artifact)
    }
}

fn valid_identifier(identifier: &str) -> bool {
    let mut bytes = identifier.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{identifier}\"")
}

fn driver_error(code: &'static str, retryable: bool) -> DriverMaintenanceError {
    DriverMaintenanceError { code, retryable }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maintenance::MaintenanceOwnerId;
    use tracedecay_store::StoreRuntimeBindingV1;

    struct NoArtifacts;

    impl MaintenanceArtifactInstaller for NoArtifacts {
        fn restore(
            &mut self,
            _connection: &mut Connection,
            _permit: &ExclusiveMaintenancePermit,
            _artifact: &VerifiedMaintenanceArtifact,
        ) -> Result<(), DriverMaintenanceError> {
            unreachable!()
        }

        fn replace_shard(
            &mut self,
            _connection: &mut Connection,
            _permit: &ExclusiveMaintenancePermit,
            _artifact: &VerifiedMaintenanceArtifact,
        ) -> Result<(), DriverMaintenanceError> {
            unreachable!()
        }
    }

    fn binding() -> StoreRuntimeBindingV1 {
        serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.maintenance.sqlite",
                "profile_id": "profile.maintenance.sqlite",
                "scope": { "kind": "project", "project_id": "project.maintenance.sqlite" }
            },
            "incarnation": 1,
            "authority_epoch": 2
        }))
        .unwrap()
    }

    #[test]
    fn fts_catalog_rejects_non_identifier_table_names() {
        let id = FtsIndexId::new("fts.code").unwrap();
        assert!(SqliteFtsIndex::new(id.clone(), "code_fts").is_some());
        assert!(SqliteFtsIndex::new(id, "code_fts; DROP TABLE facts").is_none());
    }

    #[test]
    fn duplicate_closed_ids_are_rejected() {
        let first = SqliteMigration::new(
            MigrationPlanId::new("migration.v2").unwrap(),
            2,
            vec!["CREATE TABLE one(value INTEGER)".to_owned()],
        )
        .unwrap();
        let second = SqliteMigration::new(
            MigrationPlanId::new("migration.v2").unwrap(),
            3,
            vec!["CREATE TABLE two(value INTEGER)".to_owned()],
        )
        .unwrap();
        assert!(SqliteMaintenanceCatalog::new([first, second], []).is_none());
    }

    #[test]
    fn migration_runs_on_the_handed_off_writer_connection() {
        let plan = MigrationPlanId::new("migration.v2").unwrap();
        let migration = SqliteMigration::new(
            plan.clone(),
            2,
            vec!["CREATE TABLE facts(value INTEGER) STRICT;".to_owned()],
        )
        .unwrap();
        let catalog = SqliteMaintenanceCatalog::new([migration], []).unwrap();
        let mut driver = SqliteMaintenanceDriver::from_writer_connection(
            Connection::open_in_memory().unwrap(),
            catalog,
            NoArtifacts,
        );
        let permit =
            ExclusiveMaintenancePermit::issue(MaintenanceOwnerId::new(1).unwrap(), binding());

        driver.migrate(&permit, &plan).unwrap();

        let version: u32 = driver
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let table: String = driver
            .connection
            .query_row(
                "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'facts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 2);
        assert_eq!(table, "facts");
    }
}
