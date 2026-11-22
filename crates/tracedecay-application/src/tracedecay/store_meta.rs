//! Named project-store metadata counters.

use std::path::PathBuf;

use tracedecay_contracts::RequestId;
use tracedecay_domain::canonical_sha256;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::{BoundedMetadataValue, Database};

const RESET_EPOCH_KEY: &str = "token_savings_reset_epoch";

/// The canonical reset interval observed before executing one accounted call.
///
/// Keep this value with the completion through persistence retries. Its store
/// binding prevents a retained response from being credited to another owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenSavingsAttribution {
    request_id: RequestId,
    epoch: u64,
    database_path: PathBuf,
    file_identity: u64,
}

/// A committed response receipt, including whether this submission added a delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenSavingsReceipt {
    pub lifetime_total: u64,
    pub newly_credited: bool,
}

fn response_receipt_key(request_id: &RequestId) -> Result<String> {
    let digest = canonical_sha256(&("tracedecay.token-savings.response.v1", request_id.as_str()))
        .map_err(|error| TraceDecayError::Database {
        operation: "derive token savings response receipt".to_owned(),
        message: error.to_string(),
    })?;
    Ok(format!("token_savings_receipt.v1.{}", digest.as_str()))
}

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
    let transaction = db
        .begin_write_transaction("reset token savings interval")
        .await?;
    let epoch = parse_counter(
        RESET_EPOCH_KEY,
        db.get_metadata_unguarded(&transaction, RESET_EPOCH_KEY)
            .await?,
    )?;
    let next_epoch = checked_counter_add(RESET_EPOCH_KEY, epoch, 1)?;
    db.set_metadata_unguarded(&transaction, RESET_EPOCH_KEY, &next_epoch.to_string())
        .await?;
    db.set_metadata_unguarded(&transaction, "local_counter", "0")
        .await?;
    transaction.commit().await
}

/// Capture the reset interval through a bounded read, without taking the
/// accounting writer lane. The caller keeps this read inside its admitted
/// request's deadline/cancellation boundary, before starting tool execution.
#[tracing::instrument(
    name = "daemon.store_meta.prepare_token_savings",
    level = "trace",
    skip_all
)]
pub async fn prepare_token_savings(
    db: &Database,
    request_id: &RequestId,
) -> Result<TokenSavingsAttribution> {
    let epoch = match db.get_metadata_bounded(RESET_EPOCH_KEY, 20).await? {
        BoundedMetadataValue::Missing => 0,
        BoundedMetadataValue::Value { value, .. } => parse_counter(RESET_EPOCH_KEY, Some(value))?,
        BoundedMetadataValue::Oversized { encoded_bytes } => {
            return Err(TraceDecayError::Database {
                operation: "prepare token savings".to_owned(),
                message: format!(
                    "persisted reset epoch exceeds u64 encoding: {encoded_bytes} bytes"
                ),
            });
        }
    };
    Ok(TokenSavingsAttribution {
        request_id: request_id.clone(),
        epoch,
        database_path: db.canonical_database_path().to_path_buf(),
        file_identity: db.opened_file_identity(),
    })
}

/// Credits one completed response at the project store's canonical writer.
///
/// Lifetime savings and the resettable counter commit together. Reads use the
/// same transaction as the writes, so concurrent server instances cannot
/// replace each other's totals. A reset atomically advances the interval and
/// clears its counter. An older admitted call still credits lifetime savings,
/// but its delayed completion cannot enter the new reset interval.
///
/// A fixed-size receipt in the same metadata owner commits with the counters.
/// Retrying the same response never adds another delta, including after reset
/// or reopen. Receipts use exact primary-key reads; no startup census or second
/// ledger is needed. A caller must retain its original request identity for a
/// retry, and cannot reuse it with a different delta or reset interval.
#[tracing::instrument(
    name = "daemon.store_meta.record_token_savings",
    level = "trace",
    skip_all
)]
pub async fn record_token_savings(
    db: &Database,
    attribution: &TokenSavingsAttribution,
    delta: u64,
) -> Result<TokenSavingsReceipt> {
    if attribution.database_path.as_path() != db.canonical_database_path()
        || attribution.file_identity != db.opened_file_identity()
    {
        return Err(TraceDecayError::Database {
            operation: "record token savings".to_owned(),
            message: "response attribution belongs to a different project store".to_owned(),
        });
    }
    let receipt_key = response_receipt_key(&attribution.request_id)?;
    let expected_receipt = format!("{}:{delta}", attribution.epoch);
    let transaction = db.begin_write_transaction("record token savings").await?;
    let lifetime = parse_counter(
        "tokens_saved",
        db.get_metadata_unguarded(&transaction, "tokens_saved")
            .await?,
    )?;
    let epoch = parse_counter(
        RESET_EPOCH_KEY,
        db.get_metadata_unguarded(&transaction, RESET_EPOCH_KEY)
            .await?,
    )?;
    if attribution.epoch > epoch {
        return Err(TraceDecayError::Database {
            operation: "record token savings".to_owned(),
            message: "response reset interval is newer than the canonical store".to_owned(),
        });
    }
    if let Some(receipt) = db
        .get_metadata_unguarded(&transaction, &receipt_key)
        .await?
    {
        if receipt != expected_receipt {
            return Err(TraceDecayError::Database {
                operation: "record token savings".to_owned(),
                message: "response identity was already credited with a different token delta or reset interval"
                    .to_owned(),
            });
        }
        transaction.commit().await?;
        return Ok(TokenSavingsReceipt {
            lifetime_total: lifetime,
            newly_credited: false,
        });
    }
    let lifetime = checked_counter_add("tokens_saved", lifetime, delta)?;
    let local = if attribution.epoch == epoch {
        let current = parse_counter(
            "local_counter",
            db.get_metadata_unguarded(&transaction, "local_counter")
                .await?,
        )?;
        Some(checked_counter_add("local_counter", current, delta)?)
    } else {
        None
    };
    db.set_metadata_unguarded(&transaction, "tokens_saved", &lifetime.to_string())
        .await?;
    if let Some(local) = local {
        db.set_metadata_unguarded(&transaction, "local_counter", &local.to_string())
            .await?;
    }
    db.set_metadata_unguarded(&transaction, &receipt_key, &expected_receipt)
        .await?;
    transaction.commit().await?;
    Ok(TokenSavingsReceipt {
        lifetime_total: lifetime,
        newly_credited: true,
    })
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
    use tracedecay_contracts::RequestId;
    use tracedecay_domain::errors::TraceDecayError;
    use tracedecay_runtime_core::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};

    use super::{
        RESET_EPOCH_KEY, TokenSavingsAttribution, TokenSavingsReceipt, get_local_counter,
        get_tokens_saved, prepare_token_savings, record_token_savings, reset_local_counter,
        response_receipt_key,
    };

    fn request(name: &str) -> RequestId {
        RequestId::new(format!("request.accounting.{name}")).unwrap()
    }

    async fn attribution(db: &Database, name: &str) -> TokenSavingsAttribution {
        prepare_token_savings(db, &request(name)).await.unwrap()
    }

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
        let first_id = attribution(&db, "first").await;
        let second_id = attribution(&db, "second").await;
        let (first, second) = tokio::join!(
            record_token_savings(&db, &first_id, 7),
            record_token_savings(&db, &second_id, 11),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(first.newly_credited && second.newly_credited);
        let mut totals = [first.lifetime_total, second.lifetime_total];
        totals.sort();
        assert!(totals == [7, 18] || totals == [11, 18], "{totals:?}");
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 18);
        assert_eq!(get_local_counter(&db).await.unwrap(), 18);
    }

    #[tokio::test]
    async fn reset_preserves_lifetime_and_starts_a_new_committed_interval() {
        let (_directory, db) = database().await;
        let first = attribution(&db, "first").await;
        assert_eq!(
            record_token_savings(&db, &first, 7)
                .await
                .unwrap()
                .lifetime_total,
            7
        );
        reset_local_counter(&db).await.unwrap();
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 7);
        assert_eq!(get_local_counter(&db).await.unwrap(), 0);
        assert_eq!(
            record_token_savings(&db, &first, 7).await.unwrap(),
            TokenSavingsReceipt {
                lifetime_total: 7,
                newly_credited: false
            }
        );
        assert_eq!(get_local_counter(&db).await.unwrap(), 0);
        assert_eq!(
            record_token_savings(&db, &attribution(&db, "second").await, 11)
                .await
                .unwrap()
                .lifetime_total,
            18
        );
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 18);
        assert_eq!(get_local_counter(&db).await.unwrap(), 11);
    }

    #[tokio::test]
    async fn pre_reset_completions_credit_lifetime_without_entering_the_new_interval() {
        let (_directory, db) = database().await;
        let old = attribution(&db, "old-admission").await;
        reset_local_counter(&db).await.unwrap();
        let current = attribution(&db, "new-admission").await;
        assert!(current.epoch > old.epoch);
        record_token_savings(&db, &current, 11).await.unwrap();
        let committed = record_token_savings(&db, &old, 7).await.unwrap();
        assert_eq!(committed.lifetime_total, 18);
        assert!(committed.newly_credited);
        assert_eq!(get_local_counter(&db).await.unwrap(), 11);
        assert!(
            !record_token_savings(&db, &old, 7)
                .await
                .unwrap()
                .newly_credited
        );
        assert_eq!(get_local_counter(&db).await.unwrap(), 11);

        let changed_epoch = prepare_token_savings(&db, &old.request_id).await.unwrap();
        assert!(record_token_savings(&db, &changed_epoch, 7).await.is_err());
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 18);
        assert_eq!(get_local_counter(&db).await.unwrap(), 11);
    }

    #[tokio::test]
    async fn reset_failure_rolls_back_its_epoch_and_counter_together() {
        let (_directory, db) = database().await;
        let admitted = attribution(&db, "before-reset").await;
        record_token_savings(&db, &admitted, 7).await.unwrap();
        db.execute_write(
            "inject reset counter failure",
            "CREATE TRIGGER reject_reset_counter BEFORE INSERT ON metadata
             WHEN NEW.key = 'local_counter' AND NEW.value = '0'
             BEGIN SELECT RAISE(ABORT, 'reset counter write failed'); END",
            (),
        )
        .await
        .unwrap();
        assert!(reset_local_counter(&db).await.is_err());
        let still_current = attribution(&db, "after-failed-reset").await;
        assert_eq!(still_current.epoch, admitted.epoch);
        assert_eq!(get_local_counter(&db).await.unwrap(), 7);
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 7);
        db.execute_write(
            "remove reset fault",
            "DROP TRIGGER reject_reset_counter",
            (),
        )
        .await
        .unwrap();
        reset_local_counter(&db).await.unwrap();
        assert_eq!(
            attribution(&db, "after-reset").await.epoch,
            admitted.epoch + 1
        );
        assert_eq!(get_local_counter(&db).await.unwrap(), 0);
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 7);

        db.set_metadata(RESET_EPOCH_KEY, &u64::MAX.to_string())
            .await
            .unwrap();
        assert!(reset_local_counter(&db).await.is_err());
        assert_eq!(
            db.get_metadata(RESET_EPOCH_KEY).await.unwrap(),
            Some(u64::MAX.to_string())
        );
        assert_eq!(get_local_counter(&db).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn attribution_read_bypasses_the_writer_and_refuses_wrong_store_or_future_epoch() {
        let (_directory, db) = database().await;
        let held = db
            .begin_write_transaction("hold accounting writer")
            .await
            .unwrap();
        let admitted = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            prepare_token_savings(&db, &request("admitted")),
        )
        .await
        .expect("the point read must not take the held writer")
        .unwrap();
        held.rollback().await.unwrap();

        let (_other_directory, other) = database().await;
        assert!(record_token_savings(&other, &admitted, 7).await.is_err());
        let mut future = admitted.clone();
        future.epoch += 1;
        assert!(record_token_savings(&db, &future, 7).await.is_err());
        let mut replacement = admitted.clone();
        replacement.file_identity = replacement.file_identity.wrapping_add(1);
        assert!(record_token_savings(&db, &replacement, 7).await.is_err());
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 0);
        assert_eq!(get_tokens_saved(&other).await.unwrap(), 0);
        db.set_metadata(RESET_EPOCH_KEY, &"1".repeat(1000))
            .await
            .unwrap();
        assert!(
            prepare_token_savings(&db, &request("oversized"))
                .await
                .is_err()
        );
        db.set_metadata(RESET_EPOCH_KEY, "invalid").await.unwrap();
        assert!(
            prepare_token_savings(&db, &request("invalid"))
                .await
                .is_err()
        );
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
                record_token_savings(&db, &attribution(&db, "refused").await, 1).await,
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
        record_token_savings(&db, &attribution(&db, "first").await, 7)
            .await
            .unwrap();
        db.execute_write(
            "inject local counter write failure",
            "CREATE TRIGGER reject_local_counter BEFORE INSERT ON metadata
             WHEN NEW.key = 'local_counter'
             BEGIN SELECT RAISE(ABORT, 'local counter write failed'); END",
            (),
        )
        .await
        .unwrap();

        assert!(
            record_token_savings(&db, &attribution(&db, "retry").await, 11)
                .await
                .is_err()
        );
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 7);
        assert_eq!(get_local_counter(&db).await.unwrap(), 7);

        db.execute_write("remove failure", "DROP TRIGGER reject_local_counter", ())
            .await
            .unwrap();
        assert_eq!(
            record_token_savings(&db, &attribution(&db, "retry").await, 11)
                .await
                .unwrap(),
            TokenSavingsReceipt {
                lifetime_total: 18,
                newly_credited: true
            }
        );
        assert!(
            !record_token_savings(&db, &attribution(&db, "retry").await, 11)
                .await
                .unwrap()
                .newly_credited
        );
        assert_eq!(get_local_counter(&db).await.unwrap(), 18);
    }

    #[tokio::test]
    async fn concurrent_response_replays_credit_once_and_reject_changed_delta() {
        let (_directory, db) = database().await;
        let id = attribution(&db, "replayed").await;
        let (first, replay) = tokio::join!(
            record_token_savings(&db, &id, 7),
            record_token_savings(&db, &id, 7),
        );
        let first = first.unwrap();
        let replay = replay.unwrap();
        assert_ne!(first.newly_credited, replay.newly_credited);
        assert_eq!(first.lifetime_total, 7);
        assert_eq!(replay.lifetime_total, 7);
        assert!(record_token_savings(&db, &id, 11).await.is_err());
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 7);
        assert_eq!(get_local_counter(&db).await.unwrap(), 7);
    }

    #[tokio::test]
    async fn zero_delta_receipts_refuse_positive_reuse_and_survive_replay() {
        let (_directory, db) = database().await;
        let id = attribution(&db, "zero").await;
        assert!(
            record_token_savings(&db, &id, 0)
                .await
                .unwrap()
                .newly_credited
        );
        assert!(
            !record_token_savings(&db, &id, 0)
                .await
                .unwrap()
                .newly_credited
        );
        assert!(record_token_savings(&db, &id, 7).await.is_err());
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 0);
        assert_eq!(get_local_counter(&db).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn failed_response_receipt_write_rolls_back_both_counters_and_allows_retry() {
        let (_directory, db) = database().await;
        let id = attribution(&db, "receipt-failure").await;
        record_token_savings(&db, &attribution(&db, "first").await, 7)
            .await
            .unwrap();
        db.execute_write(
            "inject response receipt failure",
            "CREATE TRIGGER reject_savings_receipt BEFORE INSERT ON metadata
             WHEN NEW.key LIKE 'token_savings_receipt.v1.%'
             BEGIN SELECT RAISE(ABORT, 'response receipt write failed'); END",
            (),
        )
        .await
        .unwrap();
        assert!(record_token_savings(&db, &id, 11).await.is_err());
        assert_eq!(get_tokens_saved(&db).await.unwrap(), 7);
        assert_eq!(get_local_counter(&db).await.unwrap(), 7);
        assert!(
            db.get_metadata(&response_receipt_key(&id.request_id).unwrap())
                .await
                .unwrap()
                .is_none()
        );
        db.execute_write("remove failure", "DROP TRIGGER reject_savings_receipt", ())
            .await
            .unwrap();
        let committed = record_token_savings(&db, &id, 11).await.unwrap();
        assert!(committed.newly_credited);
        assert_eq!(committed.lifetime_total, 18);
        assert!(
            !record_token_savings(&db, &id, 11)
                .await
                .unwrap()
                .newly_credited
        );
        assert_eq!(get_local_counter(&db).await.unwrap(), 18);
    }
}
