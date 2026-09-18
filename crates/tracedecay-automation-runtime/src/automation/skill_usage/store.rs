//! Per-skill usage files. Independent skills must not share a write target.
//!
//! The legacy `skill_usage.json` map is read once and split. After that, each
//! skill owns `skill_usage/<hex(skill id)>.json`. Import-dedupe keys that
//! already name a skill live on that skill's record. The leftover legacy key
//! set is drained to a read-only file and never written by a new event.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{SkillUsageLedger, SkillUsageRecord, config_error, skill_usage_ledger_path};
use tracedecay_domain::errors::{Result, TraceDecayError};

const SKILL_USAGE_DIR: &str = "skill_usage";
const LEGACY_IMPORTS_FILE: &str = "legacy-imported-events.json";
const MIGRATE_LOCK_FILE: &str = ".migrate.lock";

pub(super) fn skill_usage_dir(profile_root: &Path) -> PathBuf {
    profile_root.join("agent_managed").join(SKILL_USAGE_DIR)
}

pub(super) fn skill_usage_record_path(profile_root: &Path, skill_id: &str) -> PathBuf {
    skill_usage_dir(profile_root).join(record_file_name(skill_id))
}

pub(super) async fn load_ledger(profile_root: &Path) -> Result<SkillUsageLedger> {
    let root = profile_root.to_path_buf();
    tokio::task::spawn_blocking(move || load_ledger_sync(&root))
        .await
        .map_err(|error| config_error(format!("skill usage load task failed: {error}")))?
}

pub(super) async fn update_record(
    profile_root: &Path,
    skill_id: &str,
    seed_timestamp: i64,
    mutate: impl FnOnce(&mut SkillUsageRecord) + Send + 'static,
) -> Result<SkillUsageRecord> {
    let root = profile_root.to_path_buf();
    let skill_id = skill_id.to_string();
    tokio::task::spawn_blocking(move || {
        migrate_legacy(&root)?;
        with_skill_lock(
            &root,
            &skill_id,
            |record| {
                mutate(record);
                record.skill_id.clone_from(&skill_id);
                Ok(true)
            },
            seed_timestamp,
        )
    })
    .await
    .map_err(|error| config_error(format!("skill usage update task failed: {error}")))?
}

pub(super) async fn record_imported_event(
    profile_root: &Path,
    skill_id: &str,
    seed_timestamp: i64,
    import_key: String,
    mutate: impl FnOnce(&mut SkillUsageRecord) + Send + 'static,
) -> Result<Option<SkillUsageRecord>> {
    let root = profile_root.to_path_buf();
    let skill_id = skill_id.to_string();
    tokio::task::spawn_blocking(move || {
        migrate_legacy(&root)?;
        let legacy = read_legacy_imports(&root)?;
        let mut applied = false;
        let record = with_skill_lock(
            &root,
            &skill_id,
            |record| {
                if record.imported_analytics_events.contains(&import_key)
                    || legacy.contains(&import_key)
                {
                    return Ok(false);
                }
                record.imported_analytics_events.insert(import_key.clone());
                mutate(record);
                applied = true;
                Ok(true)
            },
            seed_timestamp,
        )?;
        Ok(applied.then_some(record))
    })
    .await
    .map_err(|error| config_error(format!("skill usage import task failed: {error}")))?
}

fn load_ledger_sync(profile_root: &Path) -> Result<SkillUsageLedger> {
    migrate_legacy(profile_root)?;
    let mut ledger = SkillUsageLedger::default();
    let directory = skill_usage_dir(profile_root);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ledger.imported_analytics_events = read_legacy_imports(profile_root)?;
            return Ok(ledger);
        }
        Err(error) => {
            return Err(config_error(format!(
                "failed to read skill usage directory '{}': {error}",
                directory.display()
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            config_error(format!(
                "failed to read skill usage directory '{}': {error}",
                directory.display()
            ))
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_skill_record_file(name) {
            continue;
        }
        let record = read_record(&entry.path())?;
        ledger
            .imported_analytics_events
            .extend(record.imported_analytics_events.iter().cloned());
        ledger.records.insert(record.skill_id.clone(), record);
    }
    ledger
        .imported_analytics_events
        .extend(read_legacy_imports(profile_root)?);
    Ok(ledger)
}

fn with_skill_lock(
    profile_root: &Path,
    skill_id: &str,
    mutate: impl FnOnce(&mut SkillUsageRecord) -> Result<bool>,
    seed_timestamp: i64,
) -> Result<SkillUsageRecord> {
    let directory = skill_usage_dir(profile_root);
    fs::create_dir_all(&directory).map_err(|error| {
        config_error(format!(
            "failed to create skill usage directory '{}': {error}",
            directory.display()
        ))
    })?;
    let lock_path = directory.join(format!("{}.lock", record_stem(skill_id)));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            config_error(format!(
                "failed to open skill usage lock '{}': {error}",
                lock_path.display()
            ))
        })?;
    lock.lock().map_err(|error| {
        config_error(format!(
            "failed to lock skill usage record '{skill_id}': {error}"
        ))
    })?;
    let path = skill_usage_record_path(profile_root, skill_id);
    let existing = read_record_if_present(&path)?;
    let mut record = existing
        .clone()
        .unwrap_or_else(|| SkillUsageRecord::new(skill_id.to_string(), seed_timestamp));
    record.skill_id = skill_id.to_string();
    if mutate(&mut record)? {
        write_json(&path, &record)?;
    }
    let _ = lock.unlock();
    Ok(if path.exists() {
        read_record(&path)?
    } else {
        record
    })
}

fn migrate_legacy(profile_root: &Path) -> Result<()> {
    let legacy_path = skill_usage_ledger_path(profile_root);
    if !legacy_path.exists() {
        return Ok(());
    }
    let directory = skill_usage_dir(profile_root);
    fs::create_dir_all(&directory).map_err(|error| {
        config_error(format!(
            "failed to create skill usage directory '{}': {error}",
            directory.display()
        ))
    })?;
    let lock_path = directory.join(MIGRATE_LOCK_FILE);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            config_error(format!(
                "failed to open skill usage migration lock '{}': {error}",
                lock_path.display()
            ))
        })?;
    lock.lock()
        .map_err(|error| config_error(format!("failed to lock skill usage migration: {error}")))?;
    let result = migrate_legacy_locked(profile_root, &legacy_path);
    let _ = lock.unlock();
    result
}

fn migrate_legacy_locked(profile_root: &Path, legacy_path: &Path) -> Result<()> {
    if !legacy_path.exists() {
        return Ok(());
    }
    let ledger = read_legacy_ledger(legacy_path)?;
    for (skill_id, record) in ledger.records {
        let path = skill_usage_record_path(profile_root, &skill_id);
        if path.exists() {
            continue;
        }
        write_json(&path, &record)?;
    }
    let imports_path = skill_usage_dir(profile_root).join(LEGACY_IMPORTS_FILE);
    if !imports_path.exists() && !ledger.imported_analytics_events.is_empty() {
        write_json(&imports_path, &ledger.imported_analytics_events)?;
    }
    match fs::remove_file(legacy_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(config_error(format!(
            "failed to retire skill usage ledger '{}': {error}",
            legacy_path.display()
        ))),
    }
}

fn read_legacy_ledger(path: &Path) -> Result<SkillUsageLedger> {
    let bytes = fs::read(path).map_err(|error| {
        config_error(format!(
            "failed to read skill usage ledger '{}': {error}",
            path.display()
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        config_error(format!(
            "failed to parse skill usage ledger '{}': {error}",
            path.display()
        ))
    })
}

fn read_legacy_imports(profile_root: &Path) -> Result<BTreeSet<String>> {
    let path = skill_usage_dir(profile_root).join(LEGACY_IMPORTS_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => {
            return Err(config_error(format!(
                "failed to read legacy skill usage imports '{}': {error}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| {
        config_error(format!(
            "failed to parse legacy skill usage imports '{}': {error}",
            path.display()
        ))
    })
}

fn read_record_if_present(path: &Path) -> Result<Option<SkillUsageRecord>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            config_error(format!(
                "failed to parse skill usage record '{}': {error}",
                path.display()
            ))
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(config_error(format!(
            "failed to read skill usage record '{}': {error}",
            path.display()
        ))),
    }
}

fn read_record(path: &Path) -> Result<SkillUsageRecord> {
    read_record_if_present(path)?.ok_or_else(|| {
        config_error(format!(
            "skill usage record disappeared '{}'",
            path.display()
        ))
    })
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            config_error(format!(
                "failed to create skill usage directory '{}': {error}",
                parent.display()
            ))
        })?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(TraceDecayError::from)?;
    let temporary = path.with_extension("json.tmp");
    let mut file = File::create(&temporary).map_err(|error| {
        config_error(format!(
            "failed to write skill usage record '{}': {error}",
            temporary.display()
        ))
    })?;
    file.write_all(&bytes).map_err(|error| {
        config_error(format!(
            "failed to write skill usage record '{}': {error}",
            temporary.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        config_error(format!(
            "failed to sync skill usage record '{}': {error}",
            temporary.display()
        ))
    })?;
    fs::rename(&temporary, path).map_err(|error| {
        config_error(format!(
            "failed to publish skill usage record '{}': {error}",
            path.display()
        ))
    })
}

fn record_file_name(skill_id: &str) -> String {
    format!("{}.json", record_stem(skill_id))
}

fn record_stem(skill_id: &str) -> String {
    hex::encode(skill_id.as_bytes())
}

fn is_skill_record_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    !stem.is_empty() && stem.len() % 2 == 0 && stem.bytes().all(|byte| byte.is_ascii_hexdigit())
}
