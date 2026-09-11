//! Named project-store metadata counters.

use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::Database;

fn parse_counter(key: &'static str, value: Option<String>) -> Result<u64> {
    let Some(value) = value else {
        return Ok(0);
    };
    value
        .parse::<u64>()
        .map_err(|error| TraceDecayError::Database {
            operation: format!("read {key}"),
            message: format!("persisted {key} counter is invalid: {error}"),
        })
}

#[hotpath::measure(label = "daemon.store_meta.read_tokens_saved", future = true)]
pub async fn get_tokens_saved(db: &Database) -> Result<u64> {
    parse_counter("tokens_saved", db.get_metadata("tokens_saved").await?)
}

#[hotpath::measure(label = "daemon.store_meta.write_tokens_saved", future = true)]
pub async fn set_tokens_saved(db: &Database, value: u64) -> Result<()> {
    db.set_metadata("tokens_saved", &value.to_string()).await
}

#[hotpath::measure(label = "daemon.store_meta.read_local_counter", future = true)]
pub async fn get_local_counter(db: &Database) -> Result<u64> {
    parse_counter("local_counter", db.get_metadata("local_counter").await?)
}

#[hotpath::measure(label = "daemon.store_meta.reset_local_counter", future = true)]
pub async fn reset_local_counter(db: &Database) -> Result<()> {
    db.set_metadata("local_counter", "0").await
}

#[hotpath::measure(label = "daemon.store_meta.add_local_counter", future = true)]
pub async fn add_local_counter(db: &Database, delta: u64) -> Result<()> {
    let transaction = db.begin_write_transaction("add local counter").await?;
    let current = get_local_counter(db).await?;
    let updated = current
        .checked_add(delta)
        .ok_or_else(|| TraceDecayError::Database {
            operation: "add local counter".to_owned(),
            message: "local_counter overflowed u64".to_owned(),
        })?;
    db.set_metadata_unguarded(&transaction, "local_counter", &updated.to_string())
        .await?;
    transaction.commit().await
}
