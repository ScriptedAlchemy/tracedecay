//! The rusqlite-shaped surface over the guarded native connection, compiled
//! only under `--config=native`. Rows are materialized by the guarded layer;
//! anything the guarded layer cannot yet do returns `ErrorKind::Unsupported`.

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    fmt,
    marker::PhantomData,
    ops::{BitOr, Deref, DerefMut},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

use super::{Access, Database, ExecutionGuard};
pub use super::{InterruptHandle, TransactionBehavior};
use crate::{
    Capability, CheckpointMode, CheckpointOutcome, ConnectionControl, Engine, ErrorExt, ErrorKind,
    PageStats,
};
use types::{FromSql, FromSqlError, ToSqlOutput, Type, Value, ValueRef};

pub const ENGINE: Engine = Engine::Native;

pub type Result<T, E = Error> = std::result::Result<T, E>;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Mirrors the rusqlite variants owned code constructs or matches. Engine
/// failures carry only their classification; match on [`ErrorExt::kind`].
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    QueryReturnedNoRows,
    InvalidParameterName(String),
    InvalidParameterCount(usize, usize),
    InvalidColumnIndex(usize),
    InvalidColumnName(String),
    InvalidColumnType(usize, String, Type),
    FromSqlConversionFailure(usize, Type, BoxError),
    ToSqlConversionFailure(BoxError),
    IntegralValueOutOfRange(usize, i64),
    InvalidQuery,
    InvalidPath(PathBuf),
    Engine { kind: ErrorKind, message: String },
}

impl Error {
    fn unsupported(operation: &str) -> Self {
        Self::Engine {
            kind: ErrorKind::Unsupported,
            message: format!("{operation} is not implemented on the native engine yet"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueryReturnedNoRows => formatter.write_str("query returned no rows"),
            Self::InvalidParameterName(name) => write!(formatter, "invalid parameter name: {name}"),
            Self::InvalidParameterCount(actual, expected) => write!(
                formatter,
                "wrong number of parameters: {actual} supplied, {expected} expected"
            ),
            Self::InvalidColumnIndex(index) => write!(formatter, "invalid column index: {index}"),
            Self::InvalidColumnName(name) => write!(formatter, "invalid column name: {name}"),
            Self::InvalidColumnType(index, name, kind) => {
                write!(
                    formatter,
                    "invalid column type {kind} at index {index}, name {name}"
                )
            }
            Self::FromSqlConversionFailure(index, kind, error) => write!(
                formatter,
                "conversion error from type {kind} at index {index}: {error}"
            ),
            Self::ToSqlConversionFailure(error) => write!(formatter, "{error}"),
            Self::IntegralValueOutOfRange(index, value) => {
                write!(formatter, "integer {value} out of range at index {index}")
            }
            Self::InvalidQuery => formatter.write_str("query is not read-only"),
            Self::InvalidPath(path) => write!(formatter, "invalid path: {}", path.display()),
            Self::Engine { message, .. } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FromSqlConversionFailure(_, _, error) | Self::ToSqlConversionFailure(error) => {
                Some(error.as_ref())
            }
            _ => None,
        }
    }
}

impl ErrorExt for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::QueryReturnedNoRows => ErrorKind::NoRows,
            Self::Engine { kind, .. } => *kind,
            _ => ErrorKind::Other,
        }
    }
}

impl From<super::Error> for Error {
    fn from(error: super::Error) -> Self {
        use turso_core::LimboError;

        let kind = match &error {
            super::Error::Busy => ErrorKind::Busy,
            super::Error::Cancelled
            | super::Error::DeadlineExceeded
            | super::Error::Authority(_) => ErrorKind::Interrupted,
            super::Error::Denied(_) => ErrorKind::Denied,
            super::Error::Unsupported(_) => ErrorKind::Unsupported,
            super::Error::Engine(engine) => match engine {
                LimboError::Constraint(_)
                | LimboError::ForeignKeyConstraint(_)
                | LimboError::Raise(..) => ErrorKind::Constraint,
                LimboError::Busy | LimboError::BusySnapshot | LimboError::TableLocked => {
                    ErrorKind::Busy
                }
                LimboError::Interrupt => ErrorKind::Interrupted,
                LimboError::ReadOnly => ErrorKind::ReadOnly,
                LimboError::SchemaUpdated | LimboError::SchemaConflict => ErrorKind::Schema,
                // turso's exclusive per-process file lock has no typed refusal;
                // its message is the only mark of another process's ownership.
                LimboError::LockingError(message)
                    if message.contains("locked by another process") =>
                {
                    ErrorKind::OwnedElsewhere
                }
                _ => ErrorKind::Other,
            },
            super::Error::InvalidOperation(_)
            | super::Error::QueryLimitExceeded
            | super::Error::RequestLimitExceeded => ErrorKind::Other,
        };
        Self::Engine {
            kind,
            message: error.to_string(),
        }
    }
}

pub trait OptionalExtension<T> {
    fn optional(self) -> Result<Option<T>>;
}

impl<T> OptionalExtension<T> for Result<T> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub mod types {
    use std::fmt;

    pub use super::super::Value;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Type {
        Null,
        Integer,
        Real,
        Text,
        Blob,
    }

    impl fmt::Display for Type {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(match self {
                Self::Null => "Null",
                Self::Integer => "Integer",
                Self::Real => "Real",
                Self::Text => "Text",
                Self::Blob => "Blob",
            })
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq)]
    pub enum ValueRef<'a> {
        Null,
        Integer(i64),
        Real(f64),
        Text(&'a [u8]),
        Blob(&'a [u8]),
    }

    impl<'a> ValueRef<'a> {
        pub fn data_type(&self) -> Type {
            match self {
                Self::Null => Type::Null,
                Self::Integer(_) => Type::Integer,
                Self::Real(_) => Type::Real,
                Self::Text(_) => Type::Text,
                Self::Blob(_) => Type::Blob,
            }
        }

        pub fn as_i64(&self) -> FromSqlResult<i64> {
            match self {
                Self::Integer(value) => Ok(*value),
                _ => Err(FromSqlError::InvalidType),
            }
        }

        pub fn as_f64(&self) -> FromSqlResult<f64> {
            match self {
                Self::Real(value) => Ok(*value),
                _ => Err(FromSqlError::InvalidType),
            }
        }

        pub fn as_str(&self) -> FromSqlResult<&'a str> {
            match self {
                Self::Text(bytes) => {
                    std::str::from_utf8(bytes).map_err(|error| FromSqlError::Other(Box::new(error)))
                }
                _ => Err(FromSqlError::InvalidType),
            }
        }

        pub fn as_blob(&self) -> FromSqlResult<&'a [u8]> {
            match self {
                Self::Blob(bytes) => Ok(bytes),
                _ => Err(FromSqlError::InvalidType),
            }
        }
    }

    impl<'a> From<&'a Value> for ValueRef<'a> {
        fn from(value: &'a Value) -> Self {
            match value {
                Value::Null => Self::Null,
                Value::Integer(value) => Self::Integer(*value),
                Value::Real(value) => Self::Real(*value),
                Value::Text(value) => Self::Text(value.as_bytes()),
                Value::Blob(value) => Self::Blob(value),
            }
        }
    }

    impl Value {
        pub fn data_type(&self) -> Type {
            ValueRef::from(self).data_type()
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    pub enum ToSqlOutput<'a> {
        Borrowed(ValueRef<'a>),
        Owned(Value),
    }

    #[derive(Debug)]
    #[non_exhaustive]
    pub enum FromSqlError {
        InvalidType,
        OutOfRange(i64),
        Other(Box<dyn std::error::Error + Send + Sync + 'static>),
    }

    impl fmt::Display for FromSqlError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::InvalidType => formatter.write_str("invalid type"),
                Self::OutOfRange(value) => write!(formatter, "value {value} out of range"),
                Self::Other(error) => write!(formatter, "{error}"),
            }
        }
    }

    impl std::error::Error for FromSqlError {}

    pub type FromSqlResult<T> = Result<T, FromSqlError>;

    pub trait FromSql: Sized {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self>;
    }

    macro_rules! from_sql_integer {
        ($($integer:ty),*) => {$(
            impl FromSql for $integer {
                fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                    let value = value.as_i64()?;
                    <$integer>::try_from(value).map_err(|_| FromSqlError::OutOfRange(value))
                }
            }
        )*};
    }
    from_sql_integer!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

    impl FromSql for f64 {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            match value {
                ValueRef::Integer(value) => Ok(value as f64),
                ValueRef::Real(value) => Ok(value),
                _ => Err(FromSqlError::InvalidType),
            }
        }
    }

    impl FromSql for f32 {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            f64::column_result(value).map(|value| value as f32)
        }
    }

    impl FromSql for bool {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            i64::column_result(value).map(|value| value != 0)
        }
    }

    impl FromSql for String {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            value.as_str().map(str::to_owned)
        }
    }

    impl FromSql for Box<str> {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            value.as_str().map(Into::into)
        }
    }

    impl FromSql for Vec<u8> {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            value.as_blob().map(<[u8]>::to_vec)
        }
    }

    impl FromSql for Value {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            Ok(match value {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(value) => Value::Integer(value),
                ValueRef::Real(value) => Value::Real(value),
                ValueRef::Text(_) => Value::Text(value.as_str()?.to_owned()),
                ValueRef::Blob(value) => Value::Blob(value.to_vec()),
            })
        }
    }

    impl<T: FromSql> FromSql for Option<T> {
        fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
            match value {
                ValueRef::Null => Ok(None),
                value => T::column_result(value).map(Some),
            }
        }
    }
}

pub trait ToSql {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>>;
}

macro_rules! to_sql_integer {
    ($($integer:ty),*) => {$(
        impl ToSql for $integer {
            fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::Owned(Value::Integer(i64::from(*self))))
            }
        }
    )*};
}
to_sql_integer!(i8, i16, i32, i64, u8, u16, u32, bool);

macro_rules! to_sql_wide_integer {
    ($($integer:ty),*) => {$(
        impl ToSql for $integer {
            fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
                i64::try_from(*self)
                    .map(|value| ToSqlOutput::Owned(Value::Integer(value)))
                    .map_err(|error| Error::ToSqlConversionFailure(Box::new(error)))
            }
        }
    )*};
}
to_sql_wide_integer!(isize, u64, usize);

impl ToSql for f64 {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(Value::Real(*self)))
    }
}

impl ToSql for f32 {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(Value::Real(f64::from(*self))))
    }
}

impl ToSql for str {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Text(self.as_bytes())))
    }
}

impl ToSql for String {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        self.as_str().to_sql()
    }
}

impl ToSql for Cow<'_, str> {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        self.as_ref().to_sql()
    }
}

impl ToSql for [u8] {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Blob(self)))
    }
}

impl<const N: usize> ToSql for [u8; N] {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        self.as_slice().to_sql()
    }
}

impl ToSql for Vec<u8> {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        self.as_slice().to_sql()
    }
}

impl ToSql for Value {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::from(self)))
    }
}

impl ToSql for ValueRef<'_> {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(*self))
    }
}

impl ToSql for ToSqlOutput<'_> {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Ok(match self {
            Self::Borrowed(value) => ToSqlOutput::Borrowed(*value),
            Self::Owned(value) => ToSqlOutput::Borrowed(ValueRef::from(value)),
        })
    }
}

impl<T: ToSql> ToSql for Option<T> {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        match self {
            Some(value) => value.to_sql(),
            None => Ok(ToSqlOutput::Owned(Value::Null)),
        }
    }
}

impl<T: ToSql + ?Sized> ToSql for &T {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        (**self).to_sql()
    }
}

impl<T: ToSql + ?Sized> ToSql for Box<T> {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        (**self).to_sql()
    }
}

impl<T: ToSql + ?Sized> ToSql for Arc<T> {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        (**self).to_sql()
    }
}

fn bind(value: &(impl ToSql + ?Sized)) -> Result<Value> {
    Ok(match value.to_sql()? {
        ToSqlOutput::Owned(value) => value,
        ToSqlOutput::Borrowed(ValueRef::Text(bytes)) => Value::Text(
            String::from_utf8(bytes.to_vec())
                .map_err(|error| Error::ToSqlConversionFailure(Box::new(error)))?,
        ),
        ToSqlOutput::Borrowed(value) => Value::column_result(value)
            .map_err(|error| Error::ToSqlConversionFailure(Box::new(error)))?,
    })
}

/// Positional parameters. Named parameters are not part of the owned surface.
pub trait Params {
    fn bind_values(self) -> Result<Vec<Value>>;
}

impl Params for () {
    fn bind_values(self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
}

// The only impl for an empty array, so `execute(sql, [])` is unambiguous.
impl Params for [&(dyn ToSql + Send + Sync); 0] {
    fn bind_values(self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
}

impl Params for &[&dyn ToSql] {
    fn bind_values(self) -> Result<Vec<Value>> {
        self.iter().map(|value| bind(*value)).collect()
    }
}

macro_rules! params_for_arrays {
    ($($count:literal)+) => {$(
        impl<T: ToSql + ?Sized> Params for &[&T; $count] {
            fn bind_values(self) -> Result<Vec<Value>> {
                self.iter().map(|value| bind(*value)).collect()
            }
        }
        impl<T: ToSql> Params for [T; $count] {
            fn bind_values(self) -> Result<Vec<Value>> {
                self.iter().map(|value| bind(value)).collect()
            }
        }
    )+};
}
params_for_arrays!(1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31 32);

macro_rules! params_for_tuples {
    ($(($($field:tt $name:ident),+))+) => {$(
        impl<$($name: ToSql),+> Params for ($($name,)+) {
            fn bind_values(self) -> Result<Vec<Value>> {
                Ok(vec![$(bind(&self.$field)?),+])
            }
        }
    )+};
}
params_for_tuples! {
    (0 A)
    (0 A, 1 B)
    (0 A, 1 B, 2 C)
    (0 A, 1 B, 2 C, 3 D)
    (0 A, 1 B, 2 C, 3 D, 4 E)
    (0 A, 1 B, 2 C, 3 D, 4 E, 5 F)
    (0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G)
    (0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H)
    (0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I)
    (0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J)
    (0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K)
    (0 A, 1 B, 2 C, 3 D, 4 E, 5 F, 6 G, 7 H, 8 I, 9 J, 10 K, 11 L)
}

pub struct ParamsFromIter<I>(I);

pub fn params_from_iter<I>(iter: I) -> ParamsFromIter<I>
where
    I: IntoIterator,
    I::Item: ToSql,
{
    ParamsFromIter(iter)
}

impl<I> Params for ParamsFromIter<I>
where
    I: IntoIterator,
    I::Item: ToSql,
{
    fn bind_values(self) -> Result<Vec<Value>> {
        self.0.into_iter().map(|value| bind(&value)).collect()
    }
}

#[macro_export]
macro_rules! params {
    () => {
        &[] as &[&dyn $crate::ToSql]
    };
    ($($param:expr),+ $(,)?) => {
        &[$(&$param as &dyn $crate::ToSql),+] as &[&dyn $crate::ToSql]
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenFlags(u32);

impl OpenFlags {
    pub const SQLITE_OPEN_READ_ONLY: Self = Self(0x0000_0001);
    pub const SQLITE_OPEN_READ_WRITE: Self = Self(0x0000_0002);
    pub const SQLITE_OPEN_CREATE: Self = Self(0x0000_0004);
    pub const SQLITE_OPEN_URI: Self = Self(0x0000_0040);
    pub const SQLITE_OPEN_NO_MUTEX: Self = Self(0x0000_8000);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for OpenFlags {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl Default for OpenFlags {
    fn default() -> Self {
        Self::SQLITE_OPEN_READ_WRITE
            | Self::SQLITE_OPEN_CREATE
            | Self::SQLITE_OPEN_URI
            | Self::SQLITE_OPEN_NO_MUTEX
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropBehavior {
    Rollback,
    Commit,
    Ignore,
    Panic,
}

fn unguarded() -> ExecutionGuard {
    ExecutionGuard::new(None, Arc::new(AtomicBool::new(false)), None)
}

/// One thread-owned native connection with rusqlite's method surface.
pub struct Connection {
    inner: RefCell<super::Connection>,
    savepoint_depth: Cell<usize>,
    _database: Database,
}

impl Connection {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_flags(path, OpenFlags::default())
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::open_with_flags(":memory:", OpenFlags::default())
    }

    pub fn open_with_flags(path: impl AsRef<Path>, flags: OpenFlags) -> Result<Self> {
        let path = path.as_ref();
        let memory = path.as_os_str() == ":memory:";
        if !memory && !flags.contains(OpenFlags::SQLITE_OPEN_CREATE) && !path.exists() {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        let database = Database::open(path)?;
        let access = if flags.contains(OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Access::Reader
        } else {
            Access::Writer
        };
        Ok(Self {
            inner: RefCell::new(database.connect(access)?),
            savepoint_depth: Cell::new(0),
            _database: database,
        })
    }

    fn fetch(&self, sql: &str, values: Vec<Value>) -> Result<super::Rows> {
        Ok(self.inner.borrow_mut().query(sql, &values, &unguarded())?)
    }

    pub fn execute<P: Params>(&self, sql: &str, params: P) -> Result<usize> {
        let values = params.bind_values()?;
        let changes = self
            .inner
            .borrow_mut()
            .execute(sql, &values, &unguarded())?;
        Ok(usize::try_from(changes).unwrap_or(usize::MAX))
    }

    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        Ok(self.inner.borrow_mut().execute_batch(sql, &unguarded())?)
    }

    pub fn query_row<T, P, F>(&self, sql: &str, params: P, f: F) -> Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> Result<T>,
    {
        let mut rows = Rows::new(self.fetch(sql, params.bind_values()?)?);
        match rows.next()? {
            Some(row) => f(row),
            None => Err(Error::QueryReturnedNoRows),
        }
    }

    pub fn prepare(&self, sql: &str) -> Result<Statement<'_>> {
        self.inner.borrow_mut().validate(sql, &unguarded())?;
        Ok(Statement {
            connection: self,
            sql: sql.to_owned(),
        })
    }

    pub fn prepare_cached(&self, sql: &str) -> Result<CachedStatement<'_>> {
        self.prepare(sql).map(CachedStatement)
    }

    pub fn transaction(&mut self) -> Result<Transaction<'_>> {
        self.transaction_with_behavior(TransactionBehavior::Deferred)
    }

    pub fn transaction_with_behavior(
        &mut self,
        behavior: TransactionBehavior,
    ) -> Result<Transaction<'_>> {
        self.inner.borrow_mut().begin(behavior, &unguarded())?;
        Ok(Transaction {
            connection: self,
            drop_behavior: DropBehavior::Rollback,
        })
    }

    /// Outside a transaction this begins a deferred one, as SQLite's
    /// outermost `SAVEPOINT` does.
    pub fn savepoint(&mut self) -> Result<Savepoint<'_>> {
        Savepoint::open(self)
    }

    pub fn pragma_update<V: ToSql>(
        &self,
        schema: Option<&str>,
        name: &str,
        value: V,
    ) -> Result<()> {
        let literal = match bind(&value)? {
            Value::Null => "NULL".to_owned(),
            Value::Integer(value) => value.to_string(),
            Value::Real(value) => value.to_string(),
            Value::Text(value) => format!("'{}'", value.replace('\'', "''")),
            Value::Blob(_) => return Err(Error::unsupported("blob pragma value")),
        };
        self.execute_batch(&format!(
            "PRAGMA {}{name} = {literal}",
            schema_prefix(schema)
        ))
    }

    pub fn pragma_query_value<T, F>(&self, schema: Option<&str>, name: &str, f: F) -> Result<T>
    where
        F: FnOnce(&Row<'_>) -> Result<T>,
    {
        self.query_row(&format!("PRAGMA {}{name}", schema_prefix(schema)), [], f)
    }

    pub fn is_autocommit(&self) -> bool {
        self.inner.borrow().is_autocommit()
    }

    pub fn total_changes(&self) -> u64 {
        self.inner.borrow().total_changes()
    }

    pub fn busy_timeout(&self, _timeout: std::time::Duration) -> Result<()> {
        Err(Error::unsupported("busy_timeout"))
    }

    pub fn get_interrupt_handle(&self) -> InterruptHandle {
        self.inner.borrow().interrupt_handle()
    }

    fn next_savepoint_name(&self) -> String {
        let depth = self.savepoint_depth.get();
        self.savepoint_depth.set(depth + 1);
        format!("_td_sp_{depth}")
    }

    fn release_savepoint_name(&self) {
        self.savepoint_depth
            .set(self.savepoint_depth.get().saturating_sub(1));
    }
}

fn schema_prefix(schema: Option<&str>) -> String {
    schema
        .map(|schema| format!("{schema}."))
        .unwrap_or_default()
}

impl ConnectionControl for Connection {
    fn set_capability(&self, _capability: Capability) -> Result<()> {
        Err(Error::unsupported("set_capability"))
    }

    fn with_capability<T>(
        &self,
        _scoped: Capability,
        _base: Capability,
        _operation: impl FnOnce(&Self) -> Result<T>,
    ) -> Result<T> {
        Err(Error::unsupported("with_capability"))
    }

    fn guard_scope<T>(
        &self,
        _should_interrupt: impl FnMut() -> bool + Send + 'static,
        _operation: impl FnOnce(&Self) -> T,
    ) -> Result<T> {
        Err(Error::unsupported("guard_scope"))
    }

    fn checkpoint(&self, mode: CheckpointMode) -> Result<CheckpointOutcome> {
        if mode != CheckpointMode::Truncate {
            return Err(Error::unsupported("non-truncate checkpoint"));
        }
        let rows = self.inner.borrow_mut().checkpoint(&unguarded())?;
        let mut rows = Rows::new(rows);
        let row = rows.next()?.ok_or(Error::QueryReturnedNoRows)?;
        Ok(CheckpointOutcome {
            busy: row.get::<_, i64>(0)? != 0,
            log_frames: row.get(1)?,
            checkpointed_frames: row.get(2)?,
        })
    }

    fn page_stats(&self) -> Result<PageStats> {
        let pragma = |name: &str| {
            self.pragma_query_value(None, name, |row| row.get::<_, i64>(0))
                .map(i64::unsigned_abs)
        };
        Ok(PageStats {
            page_size: pragma("page_size")?,
            page_count: pragma("page_count")?,
            freelist_count: pragma("freelist_count")?,
        })
    }

    fn file_has_moved(&self) -> Result<bool> {
        Err(Error::unsupported("file_has_moved"))
    }
}

/// Drop applies the drop behavior, rollback by default.
pub struct Transaction<'c> {
    connection: &'c Connection,
    drop_behavior: DropBehavior,
}

impl Transaction<'_> {
    pub fn savepoint(&mut self) -> Result<Savepoint<'_>> {
        Savepoint::open(self.connection)
    }

    pub fn drop_behavior(&self) -> DropBehavior {
        self.drop_behavior
    }

    pub fn set_drop_behavior(&mut self, drop_behavior: DropBehavior) {
        self.drop_behavior = drop_behavior;
    }

    pub fn commit(mut self) -> Result<()> {
        self.drop_behavior = DropBehavior::Ignore;
        Ok(self.connection.inner.borrow_mut().commit(&unguarded())?)
    }

    pub fn rollback(mut self) -> Result<()> {
        self.drop_behavior = DropBehavior::Ignore;
        Ok(self.connection.inner.borrow_mut().rollback()?)
    }

    pub fn finish(mut self) -> Result<()> {
        let behavior = std::mem::replace(&mut self.drop_behavior, DropBehavior::Ignore);
        finish_transaction(self.connection, behavior)
    }
}

fn finish_transaction(connection: &Connection, behavior: DropBehavior) -> Result<()> {
    if connection.is_autocommit() {
        return Ok(());
    }
    let mut inner = connection.inner.borrow_mut();
    match behavior {
        DropBehavior::Commit => inner.commit(&unguarded()).or_else(|error| {
            inner.rollback()?;
            Err(error)
        })?,
        DropBehavior::Rollback => inner.rollback()?,
        DropBehavior::Ignore => {}
        DropBehavior::Panic => panic!("transaction dropped unexpectedly"),
    }
    Ok(())
}

impl Deref for Transaction<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.connection
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if let Err(error) = finish_transaction(self.connection, self.drop_behavior) {
            tracing::error!(%error, "native transaction drop failed");
        }
    }
}

/// A named savepoint; the outermost one owns the transaction it began.
pub struct Savepoint<'c> {
    connection: &'c Connection,
    name: String,
    owns_transaction: bool,
    drop_behavior: DropBehavior,
    finished: bool,
}

impl<'c> Savepoint<'c> {
    fn open(connection: &'c Connection) -> Result<Self> {
        let owns_transaction = connection.is_autocommit();
        let mut inner = connection.inner.borrow_mut();
        if owns_transaction {
            inner.begin(TransactionBehavior::Deferred, &unguarded())?;
        }
        let name = connection.next_savepoint_name();
        if let Err(error) = inner.savepoint(&name, &unguarded()) {
            connection.release_savepoint_name();
            if owns_transaction {
                inner.rollback()?;
            }
            return Err(error.into());
        }
        drop(inner);
        Ok(Self {
            connection,
            name,
            owns_transaction,
            drop_behavior: DropBehavior::Rollback,
            finished: false,
        })
    }

    pub fn savepoint(&mut self) -> Result<Savepoint<'_>> {
        Savepoint::open(self.connection)
    }

    pub fn drop_behavior(&self) -> DropBehavior {
        self.drop_behavior
    }

    pub fn set_drop_behavior(&mut self, drop_behavior: DropBehavior) {
        self.drop_behavior = drop_behavior;
    }

    pub fn commit(mut self) -> Result<()> {
        self.finish_with(DropBehavior::Commit)
    }

    /// Rolls back to the savepoint and keeps it active, as rusqlite does.
    pub fn rollback(&mut self) -> Result<()> {
        Ok(self
            .connection
            .inner
            .borrow_mut()
            .rollback_savepoint(&self.name)?)
    }

    pub fn finish(mut self) -> Result<()> {
        let behavior = self.drop_behavior;
        self.finish_with(behavior)
    }

    fn finish_with(&mut self, behavior: DropBehavior) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.connection.release_savepoint_name();
        let mut inner = self.connection.inner.borrow_mut();
        match behavior {
            DropBehavior::Commit => {
                inner.release_savepoint(&self.name, &unguarded())?;
                if self.owns_transaction {
                    inner.commit(&unguarded())?;
                }
            }
            DropBehavior::Rollback => {
                if self.owns_transaction {
                    inner.rollback()?;
                } else {
                    inner.abort_savepoint(&self.name)?;
                }
            }
            DropBehavior::Ignore => {}
            DropBehavior::Panic => panic!("savepoint dropped unexpectedly"),
        }
        Ok(())
    }
}

impl Deref for Savepoint<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.connection
    }
}

impl Drop for Savepoint<'_> {
    fn drop(&mut self) {
        let behavior = self.drop_behavior;
        if let Err(error) = self.finish_with(behavior) {
            tracing::error!(%error, "native savepoint drop failed");
        }
    }
}

pub struct Statement<'c> {
    connection: &'c Connection,
    sql: String,
}

impl Statement<'_> {
    pub fn execute<P: Params>(&mut self, params: P) -> Result<usize> {
        self.connection.execute(&self.sql, params)
    }

    pub fn query<P: Params>(&mut self, params: P) -> Result<Rows<'_>> {
        Ok(Rows::new(
            self.connection.fetch(&self.sql, params.bind_values()?)?,
        ))
    }

    pub fn query_map<T, P, F>(&mut self, params: P, f: F) -> Result<MappedRows<'_, F>>
    where
        P: Params,
        F: FnMut(&Row<'_>) -> Result<T>,
    {
        Ok(MappedRows {
            rows: self.query(params)?,
            map: f,
        })
    }

    pub fn query_row<T, P, F>(&mut self, params: P, f: F) -> Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> Result<T>,
    {
        self.connection.query_row(&self.sql, params, f)
    }

    pub fn exists<P: Params>(&mut self, params: P) -> Result<bool> {
        Ok(self.query(params)?.next()?.is_some())
    }
}

pub struct CachedStatement<'c>(Statement<'c>);

impl<'c> Deref for CachedStatement<'c> {
    type Target = Statement<'c>;
    fn deref(&self) -> &Statement<'c> {
        &self.0
    }
}

impl DerefMut for CachedStatement<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

pub struct Rows<'s> {
    columns: Rc<[String]>,
    values: std::vec::IntoIter<Vec<Value>>,
    current: Option<Row<'s>>,
}

impl<'s> Rows<'s> {
    fn new(rows: super::Rows) -> Self {
        Self {
            columns: rows.columns.into(),
            values: rows.values.into_iter(),
            current: None,
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<&Row<'s>>> {
        self.current = self.values.next().map(|values| Row {
            columns: Rc::clone(&self.columns),
            values,
            _statement: PhantomData,
        });
        Ok(self.current.as_ref())
    }
}

pub struct MappedRows<'s, F> {
    rows: Rows<'s>,
    map: F,
}

impl<T, F> Iterator for MappedRows<'_, F>
where
    F: FnMut(&Row<'_>) -> Result<T>,
{
    type Item = Result<T>;

    fn next(&mut self) -> Option<Result<T>> {
        let map = &mut self.map;
        self.rows.next().transpose().map(|row| row.and_then(map))
    }
}

pub struct Row<'s> {
    columns: Rc<[String]>,
    values: Vec<Value>,
    _statement: PhantomData<&'s ()>,
}

pub trait RowIndex {
    fn index(&self, columns: &[String]) -> Result<usize>;
}

impl RowIndex for usize {
    fn index(&self, columns: &[String]) -> Result<usize> {
        if *self < columns.len() {
            Ok(*self)
        } else {
            Err(Error::InvalidColumnIndex(*self))
        }
    }
}

impl RowIndex for &str {
    fn index(&self, columns: &[String]) -> Result<usize> {
        columns
            .iter()
            .position(|column| column.eq_ignore_ascii_case(self))
            .ok_or_else(|| Error::InvalidColumnName((*self).to_owned()))
    }
}

impl Row<'_> {
    pub fn get_ref<I: RowIndex>(&self, index: I) -> Result<ValueRef<'_>> {
        let index = index.index(&self.columns)?;
        Ok(ValueRef::from(&self.values[index]))
    }

    pub fn get<I: RowIndex, T: FromSql>(&self, index: I) -> Result<T> {
        let index = index.index(&self.columns)?;
        let value = ValueRef::from(&self.values[index]);
        T::column_result(value).map_err(|error| match error {
            FromSqlError::InvalidType => {
                Error::InvalidColumnType(index, self.columns[index].clone(), value.data_type())
            }
            FromSqlError::OutOfRange(value) => Error::IntegralValueOutOfRange(index, value),
            FromSqlError::Other(error) => {
                Error::FromSqlConversionFailure(index, value.data_type(), error)
            }
        })
    }
}
