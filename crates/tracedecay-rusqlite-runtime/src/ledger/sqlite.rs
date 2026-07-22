use std::ops::Deref;

use rusqlite::{Savepoint, Statement, Transaction};
use tracedecay_store::{
    DurabilityClassV1, RuntimeTransactionScopeV1, ShardWatermarkV1, StoreCommitReceiptV1,
    StoreIncarnationV1, StoreOperationMetadataV1, StoreRuntimeBindingV1, StoreShardIdV1,
    TransactionalOutboxEntryV1,
};

use super::LedgerError;

pub(crate) trait LedgerTransaction {
    fn execute<P: rusqlite::Params>(&self, sql: &str, parameters: P) -> rusqlite::Result<usize>;
    fn execute_batch(&self, sql: &str) -> rusqlite::Result<()>;
    fn prepare(&self, sql: &str) -> rusqlite::Result<Statement<'_>>;
}

macro_rules! impl_transaction {
    ($type:ty) => {
        impl LedgerTransaction for $type {
            fn execute<P: rusqlite::Params>(
                &self,
                sql: &str,
                parameters: P,
            ) -> rusqlite::Result<usize> {
                self.deref().execute(sql, parameters)
            }

            fn execute_batch(&self, sql: &str) -> rusqlite::Result<()> {
                self.deref().execute_batch(sql)
            }

            fn prepare(&self, sql: &str) -> rusqlite::Result<Statement<'_>> {
                self.deref().prepare(sql)
            }
        }
    };
}

impl_transaction!(Transaction<'_>);
impl_transaction!(Savepoint<'_>);

pub(super) trait CanonicalJson: Sized {
    fn encode(&self) -> serde_json::Result<String>;
    fn decode(raw: &str) -> serde_json::Result<Self>;
}

macro_rules! impl_canonical_json {
    ($($type:ty),+ $(,)?) => {
        $(
            impl CanonicalJson for $type {
                fn encode(&self) -> serde_json::Result<String> {
                    serde_json::to_string(self)
                }

                fn decode(raw: &str) -> serde_json::Result<Self> {
                    serde_json::from_str(raw)
                }
            }
        )+
    };
}

impl_canonical_json!(
    StoreShardIdV1,
    RuntimeTransactionScopeV1,
    DurabilityClassV1,
    ShardWatermarkV1,
    StoreCommitReceiptV1,
    TransactionalOutboxEntryV1,
);

pub(super) fn encode_json<T: CanonicalJson>(
    value: &T,
    field: &'static str,
) -> Result<String, LedgerError> {
    value
        .encode()
        .map_err(|_| LedgerError::Encoding { value: field })
}

pub(super) fn decode_json<T: CanonicalJson>(
    raw: &str,
    table: &'static str,
    field: &'static str,
) -> Result<T, LedgerError> {
    let value = T::decode(raw).map_err(|_| LedgerError::Corrupt { table, field })?;
    if encode_json(&value, field)? != raw {
        return Err(LedgerError::Corrupt { table, field });
    }
    Ok(value)
}

#[derive(Clone)]
pub(super) struct BindingKey {
    pub(super) shard_json: String,
    pub(super) incarnation: StoreIncarnationV1,
    pub(super) incarnation_sql: i64,
}

impl BindingKey {
    pub(super) fn from_binding(binding: &StoreRuntimeBindingV1) -> Result<Self, LedgerError> {
        Self::from_parts(&binding.shard_id, binding.incarnation)
    }

    pub(super) fn from_parts(
        shard_id: &StoreShardIdV1,
        incarnation: StoreIncarnationV1,
    ) -> Result<Self, LedgerError> {
        Ok(Self {
            shard_json: encode_json(shard_id, "shard_json")?,
            incarnation,
            incarnation_sql: sqlite_u64(incarnation.get(), "store incarnation")?,
        })
    }
}

pub(super) struct Submission<'a> {
    pub(super) metadata: &'a StoreOperationMetadataV1,
    pub(super) transaction_scope: &'a RuntimeTransactionScopeV1,
    pub(super) binding_key: BindingKey,
    pub(super) authority_epoch_sql: i64,
    pub(super) transaction_scope_json: String,
    pub(super) durability_json: String,
}

impl<'a> Submission<'a> {
    pub(super) fn new(
        metadata: &'a StoreOperationMetadataV1,
        transaction_scope: &'a RuntimeTransactionScopeV1,
    ) -> Result<Self, LedgerError> {
        metadata.validate().map_err(LedgerError::InvalidRequest)?;
        transaction_scope
            .validate_operation(metadata)
            .map_err(LedgerError::InvalidRequest)?;
        Ok(Self {
            metadata,
            transaction_scope,
            binding_key: BindingKey::from_parts(&metadata.shard_id, metadata.incarnation)?,
            authority_epoch_sql: sqlite_u64(metadata.authority_epoch.get(), "authority epoch")?,
            transaction_scope_json: encode_json(transaction_scope, "transaction_scope_json")?,
            durability_json: encode_json(&metadata.durability, "durability_json")?,
        })
    }

    pub(super) fn binding(&self) -> StoreRuntimeBindingV1 {
        StoreRuntimeBindingV1::new(
            self.metadata.shard_id.clone(),
            self.metadata.incarnation,
            self.metadata.authority_epoch,
        )
    }
}

pub(super) fn sqlite_u64(value: u64, field: &'static str) -> Result<i64, LedgerError> {
    i64::try_from(value).map_err(|_| LedgerError::UnsupportedInteger { field })
}
