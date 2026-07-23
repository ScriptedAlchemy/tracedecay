use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use tracedecay_store::{StoreOperationIdV1, StoreRuntimeBindingV1};

use super::{
    CorruptionDiagnosis, CorruptionEvidence, CorruptionObservation, CorruptionProbe,
    MaintenanceAuthorization, QuarantineReceipt, RepairDriver, RepairFault, RepairReceipt,
};
use crate::maintenance::SqliteFtsIndex;

pub struct SqliteCorruptionProbe<'a> {
    connection: &'a Connection,
    binding: StoreRuntimeBindingV1,
    evidence_id: StoreOperationIdV1,
    fts_indexes: &'a [SqliteFtsIndex],
}

impl<'a> SqliteCorruptionProbe<'a> {
    pub fn new(
        connection: &'a Connection,
        binding: StoreRuntimeBindingV1,
        evidence_id: StoreOperationIdV1,
        fts_indexes: &'a [SqliteFtsIndex],
    ) -> Self {
        Self {
            connection,
            binding,
            evidence_id,
            fts_indexes,
        }
    }
}

impl CorruptionProbe for SqliteCorruptionProbe<'_> {
    fn evidence(&self) -> Result<CorruptionEvidence, RepairFault> {
        let observations = match integrity_messages(self.connection) {
            Ok(messages) if messages.len() == 1 && messages[0].eq_ignore_ascii_case("ok") => self
                .fts_indexes
                .iter()
                .filter_map(|index| {
                    let table = quoted_identifier(index.table());
                    self.connection
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |_| Ok(()))
                        .err()
                        .map(|_| CorruptionObservation::DerivedFts)
                })
                .collect(),
            Ok(_) => vec![CorruptionObservation::Authoritative],
            Err(_) => vec![CorruptionObservation::Unclassified],
        };
        Ok(CorruptionEvidence {
            binding: self.binding.clone(),
            evidence_id: self.evidence_id.clone(),
            observations,
        })
    }
}

pub trait QuarantineStore {
    fn lookup(
        &self,
        _diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<QuarantineReceipt>, RepairFault>;

    fn preserve(
        &mut self,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<QuarantineReceipt, RepairFault>;
}

pub struct SqliteRepairDriver<'a, Q> {
    connection: &'a mut Connection,
    fts_indexes: &'a [SqliteFtsIndex],
    quarantine: Q,
}

impl<'a, Q> SqliteRepairDriver<'a, Q> {
    pub fn new(
        connection: &'a mut Connection,
        fts_indexes: &'a [SqliteFtsIndex],
        quarantine: Q,
    ) -> Self {
        Self {
            connection,
            fts_indexes,
            quarantine,
        }
    }
}

impl<Q: QuarantineStore> RepairDriver for SqliteRepairDriver<'_, Q> {
    fn lookup_repair_receipt(
        &self,
        _diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<RepairReceipt>, RepairFault> {
        let receipt_id_json = json(receipt_id)?;
        let row = self
            .connection
            .query_row(
                "SELECT evidence_id, binding FROM tracedecay_repair_receipts
                 WHERE receipt_id = ?1",
                [&receipt_id_json],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional();
        let row = match row {
            Ok(row) => row,
            Err(error) if is_missing_table(&error) => return Ok(None),
            Err(error) => return Err(sqlite_fault("lookup_repair_receipt", error)),
        };
        row.map(|(evidence_id, binding)| {
            Ok(RepairReceipt {
                receipt_id: receipt_id.clone(),
                evidence_id: serde_json::from_str(&evidence_id)
                    .map_err(|error| serialization_fault("decode_evidence_id", error))?,
                binding: serde_json::from_str(&binding)
                    .map_err(|error| serialization_fault("decode_binding", error))?,
            })
        })
        .transpose()
    }

    fn lookup_quarantine_receipt(
        &self,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<QuarantineReceipt>, RepairFault> {
        self.quarantine.lookup(diagnosis, receipt_id)
    }

    fn rebuild_derived_fts(
        &mut self,
        _authorization: &dyn MaintenanceAuthorization,
        _diagnosis: &CorruptionDiagnosis,
        receipt: &RepairReceipt,
    ) -> Result<(), RepairFault> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| sqlite_fault("begin_fts_rebuild", error))?;
        for index in self.fts_indexes {
            let table = quoted_identifier(index.table());
            transaction
                .execute(
                    &format!("INSERT INTO {table}({table}) VALUES ('rebuild')"),
                    [],
                )
                .map_err(|error| sqlite_fault("rebuild_derived_fts", error))?;
        }
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS tracedecay_repair_receipts (
                    receipt_id TEXT PRIMARY KEY NOT NULL,
                    evidence_id TEXT NOT NULL,
                    binding TEXT NOT NULL
                ) STRICT;",
            )
            .map_err(|error| sqlite_fault("create_repair_receipts", error))?;
        transaction
            .execute(
                "INSERT INTO tracedecay_repair_receipts
                 (receipt_id, evidence_id, binding) VALUES (?1, ?2, ?3)",
                params![
                    json(&receipt.receipt_id)?,
                    json(&receipt.evidence_id)?,
                    json(&receipt.binding)?,
                ],
            )
            .map_err(|error| sqlite_fault("record_repair_receipt", error))?;
        transaction
            .commit()
            .map_err(|error| sqlite_fault("commit_fts_rebuild", error))
    }

    fn quarantine_authoritative(
        &mut self,
        _authorization: &dyn MaintenanceAuthorization,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<QuarantineReceipt, RepairFault> {
        self.quarantine.preserve(diagnosis, receipt_id)
    }
}

pub struct FilesystemQuarantineStore {
    root: PathBuf,
    source_database: PathBuf,
}

impl FilesystemQuarantineStore {
    pub fn new(root: PathBuf, source_database: PathBuf) -> Result<Self, RepairFault> {
        if !root.is_absolute() || !source_database.is_absolute() {
            return Err(RepairFault::new(
                "quarantine_path_not_absolute",
                "quarantine capability requires absolute paths",
            ));
        }
        if !source_database.is_file() {
            return Err(RepairFault::new(
                "quarantine_source_missing",
                "quarantine source is not a regular file",
            ));
        }
        fs::create_dir_all(&root).map_err(|error| io_fault("create_quarantine_root", error))?;
        set_private_directory(&root)?;
        Ok(Self {
            root,
            source_database,
        })
    }
}

impl QuarantineStore for FilesystemQuarantineStore {
    fn lookup(
        &self,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<Option<QuarantineReceipt>, RepairFault> {
        let token = receipt_token(diagnosis, receipt_id)?;
        let path = self.root.join(&token).join("receipt.json");
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| serialization_fault("decode_quarantine_receipt", error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(io_fault("read_quarantine_receipt", error)),
        }
    }

    fn preserve(
        &mut self,
        diagnosis: &CorruptionDiagnosis,
        receipt_id: &StoreOperationIdV1,
    ) -> Result<QuarantineReceipt, RepairFault> {
        if let Some(receipt) = self.lookup(diagnosis, receipt_id)? {
            return Ok(receipt);
        }
        let token = receipt_token(diagnosis, receipt_id)?;
        let staging = self.root.join(format!(".{token}.staging"));
        let published = self.root.join(&token);
        fs::create_dir(&staging).map_err(|error| io_fault("create_quarantine_staging", error))?;
        set_private_directory(&staging)?;
        let outcome = (|| {
            copy_and_sync(&self.source_database, &staging.join("database.sqlite3"))?;
            for (suffix, name) in [
                ("-wal", "database.sqlite3-wal"),
                ("-shm", "database.sqlite3-shm"),
            ] {
                let companion = companion_path(&self.source_database, suffix);
                if companion.is_file() {
                    copy_and_sync(&companion, &staging.join(name))?;
                }
            }
            let receipt = QuarantineReceipt {
                receipt_id: receipt_id.clone(),
                evidence_id: diagnosis.evidence_id.clone(),
                binding: diagnosis.binding.clone(),
                evidence_reference: format!("quarantine:{token}"),
            };
            write_new_and_sync(
                &staging.join("receipt.json"),
                &serde_json::to_vec(&receipt)
                    .map_err(|error| serialization_fault("encode_quarantine_receipt", error))?,
            )?;
            sync_directory(&staging)?;
            fs::rename(&staging, &published)
                .map_err(|error| io_fault("publish_quarantine", error))?;
            sync_directory(&self.root)?;
            Ok(receipt)
        })();
        if outcome.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        outcome
    }
}

fn integrity_messages(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare("PRAGMA quick_check")?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect()
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{identifier}\"")
}

fn is_missing_table(error: &rusqlite::Error) -> bool {
    error
        .to_string()
        .contains("no such table: tracedecay_repair_receipts")
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, RepairFault> {
    serde_json::to_string(value).map_err(|error| serialization_fault("encode_receipt", error))
}

fn receipt_token(
    diagnosis: &CorruptionDiagnosis,
    receipt_id: &StoreOperationIdV1,
) -> Result<String, RepairFault> {
    let bytes = serde_json::to_vec(&(&diagnosis.binding, &diagnosis.evidence_id, receipt_id))
        .map_err(|error| serialization_fault("encode_quarantine_identity", error))?;
    Ok(hex(&Sha256::digest(bytes)))
}

fn copy_and_sync(source: &Path, destination: &Path) -> Result<(), RepairFault> {
    fs::copy(source, destination).map_err(|error| io_fault("copy_quarantine_artifact", error))?;
    File::open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|error| io_fault("sync_quarantine_artifact", error))
}

fn write_new_and_sync(path: &Path, bytes: &[u8]) -> Result<(), RepairFault> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| io_fault("create_quarantine_receipt", error))?;
    io::Write::write_all(&mut file, bytes)
        .map_err(|error| io_fault("write_quarantine_receipt", error))?;
    file.sync_all()
        .map_err(|error| io_fault("sync_quarantine_receipt", error))
}

fn companion_path(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

fn sync_directory(path: &Path) -> Result<(), RepairFault> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| io_fault("sync_quarantine_directory", error))
}

fn set_private_directory(path: &Path) -> Result<(), RepairFault> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| io_fault("protect_quarantine_directory", error))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        },
    )
}

fn sqlite_fault(stage: &'static str, error: rusqlite::Error) -> RepairFault {
    RepairFault::new(stage, error.to_string())
}

fn io_fault(stage: &'static str, error: io::Error) -> RepairFault {
    RepairFault::new(stage, error.to_string())
}

fn serialization_fault(stage: &'static str, error: serde_json::Error) -> RepairFault {
    RepairFault::new(stage, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maintenance::FtsIndexId;

    fn binding() -> StoreRuntimeBindingV1 {
        serde_json::from_value(serde_json::json!({
            "shard_id": {
                "brain_id": "brain.repair.sqlite",
                "profile_id": "profile.repair.sqlite",
                "scope": { "kind": "project", "project_id": "project.repair.sqlite" }
            },
            "incarnation": 1,
            "authority_epoch": 1
        }))
        .unwrap()
    }

    fn evidence_id() -> StoreOperationIdV1 {
        StoreOperationIdV1::try_from("evidence.repair.sqlite".to_owned()).unwrap()
    }

    #[test]
    fn read_only_probe_reports_healthy_database() {
        let connection = Connection::open_in_memory().unwrap();
        let diagnosis = SqliteCorruptionProbe::new(&connection, binding(), evidence_id(), &[])
            .evidence()
            .unwrap();
        assert!(diagnosis.observations.is_empty());
    }

    #[test]
    fn read_only_probe_classifies_broken_fts_projection_only() {
        let connection = Connection::open_in_memory().unwrap();
        let index =
            SqliteFtsIndex::new(FtsIndexId::new("fts.missing").unwrap(), "missing_fts").unwrap();
        let diagnosis = SqliteCorruptionProbe::new(&connection, binding(), evidence_id(), &[index])
            .evidence()
            .unwrap();
        assert_eq!(
            diagnosis.observations,
            vec![CorruptionObservation::DerivedFts]
        );
    }
}
