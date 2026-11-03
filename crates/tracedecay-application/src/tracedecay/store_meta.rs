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

#[tracing::instrument(
    name = "daemon.store_meta.read_tokens_saved",
    level = "trace",
    skip_all
)]
pub async fn get_tokens_saved(db: &Database) -> Result<u64> {
    parse_counter("tokens_saved", db.get_metadata("tokens_saved").await?)
}

#[tracing::instrument(
    name = "daemon.store_meta.read_local_counter",
    level = "trace",
    skip_all
)]
pub async fn get_local_counter(db: &Database) -> Result<u64> {
    parse_counter("local_counter", db.get_metadata("local_counter").await?)
}

#[tracing::instrument(
    name = "daemon.store_meta.reset_local_counter",
    level = "trace",
    skip_all
)]
pub async fn reset_local_counter(db: &Database) -> Result<()> {
    db.set_metadata("local_counter", "0").await
}

/// Credits one completed response at the project store's canonical writer.
///
/// Lifetime savings and the resettable counter commit together. Reads use the
/// same transaction as the writes, so concurrent server instances cannot
/// replace each other's totals. A reset takes this same writer lane: deltas
/// committed before it are cleared from the local counter, and deltas committed
/// afterwards belong to the next counting interval.
#[tracing::instrument(
    name = "daemon.store_meta.record_token_savings",
    level = "trace",
    skip_all
)]
pub async fn record_token_savings(db: &Database, delta: u64) -> Result<u64> {
    let transaction = db.begin_write_transaction("record token savings").await?;
    let lifetime = parse_counter(
        "tokens_saved",
        db.get_metadata_unguarded(&transaction, "tokens_saved")
            .await?,
    )?;
    let local = parse_counter(
        "local_counter",
        db.get_metadata_unguarded(&transaction, "local_counter")
            .await?,
    )?;
    let lifetime = checked_counter_add("tokens_saved", lifetime, delta)?;
    let local = checked_counter_add("local_counter", local, delta)?;
    db.set_metadata_unguarded(&transaction, "tokens_saved", &lifetime.to_string())
        .await?;
    db.set_metadata_unguarded(&transaction, "local_counter", &local.to_string())
        .await?;
    transaction.commit().await?;
    Ok(lifetime)
}

fn checked_counter_add(key: &'static str, current: u64, delta: u64) -> Result<u64> {
    current
        .checked_add(delta)
        .ok_or_else(|| TraceDecayError::Database {
            operation: "record token savings".to_owned(),
            message: format!("{key} overflowed u64"),
        })
}

#[cfg(test)]
mod tests {
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    use super::{get_local_counter, get_tokens_saved, record_token_savings, reset_local_counter};

    async fn database() -> (tempfile::TempDir, Database) {
        crate::register_test_schema_installer();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token-accounting.db");
        let authority = DatabaseAuthority::acquire_test(&path, "token-accounting").unwrap();
        let (database, _) =
            Database::publish_test_runtime(&path, &authority, TestDatabaseRuntimeMode::Initialize)
                .await
                .unwrap();
        (directory, database)
    }

    #[tokio::test]
    async fn concurrent_deltas_commit_both_counters_without_losing_updates() {
        let (_directory, db) = database().await;
        let (first, second) =
            tokio::join!(record_token_savings(&db, 7), record_token_savings(&db, 11),);
        let mut totals = [first.unwrap(), second.unwrap()];
        totals.sort();
        assert!(totals == [7, 18] || totals == [11, 18], "{totals:?}");
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 18);
        assert_eq!(get_local_counter(&db).await.unwrap(), 18);
    }

    #[tokio::test]
    async fn reset_preserves_lifetime_and_starts_a_new_committed_interval() {
        let (_directory, db) = database().await;
        assert_eq!(record_token_savings(&db, 7).await.unwrap(), 7);
        reset_local_counter(&db).await.unwrap();
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 7);
        assert_eq!(get_local_counter(&db).await.unwrap(), 0);
        assert_eq!(record_token_savings(&db, 11).await.unwrap(), 18);
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 18);
        assert_eq!(get_local_counter(&db).await.unwrap(), 11);
    }

    #[tokio::test]
    async fn invalid_or_overflowed_counters_cannot_partially_credit_savings() {
        let (_directory, db) = database().await;
        for (lifetime, local) in [
            (u64::MAX.to_string(), "4".to_owned()),
            ("7".to_owned(), u64::MAX.to_string()),
            ("7".to_owned(), "invalid".to_owned()),
        ] {
            db.set_metadata("tokens_saved", &lifetime).await.unwrap();
            db.set_metadata("local_counter", &local).await.unwrap();
            assert!(matches!(
                record_token_savings(&db, 1).await,
                Err(TraceDecayError::Database { .. })
            ));
            assert_eq!(
                db.get_metadata("tokens_saved").await.unwrap(),
                Some(lifetime)
            );
            assert_eq!(db.get_metadata("local_counter").await.unwrap(), Some(local));
        }
    }

    #[tokio::test]
    async fn failed_local_counter_write_rolls_back_the_lifetime_credit() {
        let (_directory, db) = database().await;
        record_token_savings(&db, 7).await.unwrap();
        db.execute_write(
            "inject local counter write failure",
            "CREATE TRIGGER reject_local_counter BEFORE INSERT ON metadata
             WHEN NEW.key = 'local_counter'
             BEGIN SELECT RAISE(ABORT, 'local counter write failed'); END",
            (),
        )
        .await
        .unwrap();

        assert!(record_token_savings(&db, 11).await.is_err());
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 7);
        assert_eq!(get_local_counter(&db).await.unwrap(), 7);

        db.execute_write("remove failure", "DROP TRIGGER reject_local_counter", ())
            .await
            .unwrap();
        assert_eq!(record_token_savings(&db, 11).await.unwrap(), 18);
        assert_eq!(get_local_counter(&db).await.unwrap(), 18);
    }
}
