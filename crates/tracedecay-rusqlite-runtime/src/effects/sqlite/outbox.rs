use rusqlite::{Connection, params};
use tracedecay_store::{StoreEffectIdV1, StoreRuntimeBindingV1};

use crate::ledger::{self, LedgerError};

const SELECT_REPLAY_HEADS: &str = r#"
SELECT candidate.effect_id
FROM td_runtime_writer_outbox_v1 AS candidate
WHERE candidate.source_shard_json = ?1
  AND candidate.source_incarnation = ?2
  AND candidate.source_authority_epoch = ?3
  AND json_extract(candidate.entry_json, '$.identity.target_watermark.shard_id') = ?4
  AND json_extract(candidate.entry_json, '$.identity.target_watermark.incarnation') = ?5
  AND json_extract(candidate.entry_json, '$.identity.target_watermark.authority_epoch') = ?6
  AND candidate.state IN ('pending', 'dispatched', 'effect_unknown')
  AND NOT EXISTS (
      SELECT 1
      FROM td_runtime_writer_outbox_v1 AS earlier
      WHERE earlier.source_shard_json = candidate.source_shard_json
        AND earlier.source_incarnation = candidate.source_incarnation
        AND earlier.source_authority_epoch = candidate.source_authority_epoch
        AND earlier.ordering_key = candidate.ordering_key
        AND earlier.state != 'acknowledged'
        AND (
            earlier.source_sequence < candidate.source_sequence
            OR (
                earlier.source_sequence = candidate.source_sequence
                AND earlier.effect_id < candidate.effect_id
            )
        )
  )
ORDER BY candidate.source_sequence, candidate.effect_id
LIMIT ?7
"#;

pub(crate) fn replay_candidates(
    reader: &mut Connection,
    origin_binding: &StoreRuntimeBindingV1,
    target_binding: &StoreRuntimeBindingV1,
    limit: usize,
) -> Result<Vec<StoreEffectIdV1>, LedgerError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let shard_json =
        serde_json::to_string(&origin_binding.shard_id).map_err(|_| LedgerError::Encoding {
            value: "outbox replay shard",
        })?;
    let incarnation = i64::try_from(origin_binding.incarnation.get()).map_err(|_| {
        LedgerError::UnsupportedInteger {
            field: "outbox replay incarnation",
        }
    })?;
    let authority_epoch = i64::try_from(origin_binding.authority_epoch.get()).map_err(|_| {
        LedgerError::UnsupportedInteger {
            field: "outbox replay authority epoch",
        }
    })?;
    let target_shard_json =
        serde_json::to_string(&target_binding.shard_id).map_err(|_| LedgerError::Encoding {
            value: "outbox replay target shard",
        })?;
    let target_incarnation = i64::try_from(target_binding.incarnation.get()).map_err(|_| {
        LedgerError::UnsupportedInteger {
            field: "outbox replay target incarnation",
        }
    })?;
    let target_authority_epoch =
        i64::try_from(target_binding.authority_epoch.get()).map_err(|_| {
            LedgerError::UnsupportedInteger {
                field: "outbox replay target authority epoch",
            }
        })?;
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let transaction = reader.transaction()?;
    let raw_ids = {
        let mut statement = transaction.prepare(SELECT_REPLAY_HEADS)?;
        let rows = statement.query_map(
            params![
                shard_json,
                incarnation,
                authority_epoch,
                target_shard_json,
                target_incarnation,
                target_authority_epoch,
                limit
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut effect_ids = Vec::with_capacity(raw_ids.len());
    for raw_id in raw_ids {
        let effect_id = StoreEffectIdV1::new(raw_id).map_err(|_| LedgerError::Corrupt {
            table: "td_runtime_writer_outbox_v1",
            field: "effect_id",
        })?;
        if ledger::outbox_entry(&transaction, origin_binding, &effect_id)?.is_none() {
            return Err(LedgerError::Corrupt {
                table: "td_runtime_writer_outbox_v1",
                field: "replay candidate",
            });
        }
        effect_ids.push(effect_id);
    }
    transaction.commit()?;
    Ok(effect_ids)
}
