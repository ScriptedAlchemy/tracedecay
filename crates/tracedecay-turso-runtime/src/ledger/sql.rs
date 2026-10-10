use super::LedgerError;
use crate::{Connection, ExecutionGuard, Value};
use std::cell::RefCell;
use tracedecay_store::{
    DurabilityClassV1, OperationPriorityV1, RuntimeTransactionScopeV1, ShardWatermarkV1,
    StoreCommitReceiptV1, StoreIncarnationV1, StoreOperationMetadataV1, StoreRuntimeBindingV1,
    StoreShardIdV1, TransactionalOutboxEntryV1,
};
/// SQL capability borrowed from the writer-owned request savepoint. Domain
/// mutations and bookkeeping therefore use the same transaction and guard.
pub struct RequestSql<'a> {
    connection: RefCell<&'a mut Connection>,
    guard: &'a ExecutionGuard,
}

impl<'a> RequestSql<'a> {
    pub(crate) fn new(connection: &'a mut Connection, guard: &'a ExecutionGuard) -> Self {
        Self {
            connection: RefCell::new(connection),
            guard,
        }
    }

    pub fn execute(&self, sql: &str, parameters: &[Value]) -> crate::Result<u64> {
        self.connection
            .borrow_mut()
            .execute(sql, parameters, self.guard)
    }

    pub fn query(&self, sql: &str, parameters: &[Value]) -> crate::Result<crate::Rows> {
        self.connection
            .borrow_mut()
            .query(sql, parameters, self.guard)
    }
}

pub(crate) trait LedgerTransaction {
    fn execute(&self, sql: &str, parameters: Vec<Value>) -> crate::Result<usize>;
    fn execute_batch(&self, sql: &str) -> crate::Result<()>;
    fn query(&self, sql: &str, parameters: Vec<Value>) -> crate::Result<crate::Rows>;
}

impl LedgerTransaction for RequestSql<'_> {
    fn execute(&self, sql: &str, parameters: Vec<Value>) -> crate::Result<usize> {
        let changed = self.execute(sql, &parameters)?;
        usize::try_from(changed)
            .map_err(|_| crate::Error::InvalidOperation("native row count exceeds usize".into()))
    }
    fn execute_batch(&self, sql: &str) -> crate::Result<()> {
        self.connection.borrow_mut().execute_batch(sql, self.guard)
    }
    fn query(&self, sql: &str, parameters: Vec<Value>) -> crate::Result<crate::Rows> {
        self.query(sql, &parameters)
    }
}

pub(crate) struct Row(pub(crate) Vec<Value>);
pub(crate) trait Column: Sized {
    fn decode(value: &Value) -> crate::Result<Self>;
}
impl Column for i64 {
    fn decode(value: &Value) -> crate::Result<Self> {
        match value {
            Value::Integer(value) => Ok(*value),
            _ => Err(crate::Error::InvalidOperation(
                "ledger integer column has wrong type".into(),
            )),
        }
    }
}
impl Column for String {
    fn decode(value: &Value) -> crate::Result<Self> {
        match value {
            Value::Text(value) => Ok(value.clone()),
            _ => Err(crate::Error::InvalidOperation(
                "ledger text column has wrong type".into(),
            )),
        }
    }
}
impl<T: Column> Column for Option<T> {
    fn decode(value: &Value) -> crate::Result<Self> {
        match value {
            Value::Null => Ok(None),
            value => T::decode(value).map(Some),
        }
    }
}
impl Row {
    pub(crate) fn get<I: TryInto<usize>, T: Column>(&self, index: I) -> crate::Result<T> {
        let index = index
            .try_into()
            .map_err(|_| crate::Error::InvalidOperation("invalid ledger column".into()))?;
        let value = self
            .0
            .get(index)
            .ok_or_else(|| crate::Error::InvalidOperation("missing ledger column".into()))?;
        T::decode(value)
    }
}

pub(crate) trait Parameter {
    fn value(&self) -> Value;
}
impl Parameter for i64 {
    fn value(&self) -> Value {
        Value::Integer(*self)
    }
}
impl Parameter for str {
    fn value(&self) -> Value {
        Value::Text(self.to_owned())
    }
}
impl Parameter for String {
    fn value(&self) -> Value {
        Value::Text(self.clone())
    }
}
impl<T: Parameter + ?Sized> Parameter for &T {
    fn value(&self) -> Value {
        (**self).value()
    }
}
impl<T: Parameter> Parameter for Option<T> {
    fn value(&self) -> Value {
        self.as_ref().map_or(Value::Null, Parameter::value)
    }
}
pub(crate) fn parameter<T: Parameter + ?Sized>(value: &T) -> Value {
    value.value()
}
macro_rules! params {
    ($($value:expr),* $(,)?) => { vec![$($crate::ledger::parameter(&$value)),*] };
}
pub(crate) use params;

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
    OperationPriorityV1,
    ShardWatermarkV1,
    StoreCommitReceiptV1,
    TransactionalOutboxEntryV1,
);

pub(super) fn encode_json<T: CanonicalJson>(
    value: &T,
    field: &'static str,
) -> Result<String, LedgerError> {
    let encoded = value
        .encode()
        .map_err(|_| LedgerError::Encoding { value: field })?;
    Ok(encoded)
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
            incarnation_sql: sql_u64(incarnation.get(), "store incarnation")?,
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
            authority_epoch_sql: sql_u64(metadata.authority_epoch.get(), "authority epoch")?,
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

pub(super) fn sql_u64(value: u64, field: &'static str) -> Result<i64, LedgerError> {
    i64::try_from(value).map_err(|_| LedgerError::UnsupportedInteger { field })
}
