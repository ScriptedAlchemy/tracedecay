use super::{Error, Result, Value};

pub trait IntoValue {
    fn into_value(self) -> Result<Value>;
}

macro_rules! infallible_value {
    ($type:ty, $variant:path) => {
        impl IntoValue for $type {
            fn into_value(self) -> Result<Value> {
                Ok($variant(self.into()))
            }
        }
    };
}

infallible_value!(String, Value::Text);
infallible_value!(&str, Value::Text);
infallible_value!(i64, Value::Integer);
infallible_value!(i32, Value::Integer);
infallible_value!(u32, Value::Integer);
infallible_value!(f64, Value::Real);
infallible_value!(Vec<u8>, Value::Blob);

impl IntoValue for &String {
    fn into_value(self) -> Result<Value> {
        Ok(Value::Text(self.clone()))
    }
}

impl IntoValue for &Vec<u8> {
    fn into_value(self) -> Result<Value> {
        Ok(Value::Blob(self.clone()))
    }
}

impl IntoValue for &[u8] {
    fn into_value(self) -> Result<Value> {
        Ok(Value::Blob(self.to_vec()))
    }
}

impl IntoValue for Value {
    fn into_value(self) -> Result<Value> {
        Ok(self)
    }
}

impl IntoValue for u64 {
    fn into_value(self) -> Result<Value> {
        i64::try_from(self)
            .map(Value::Integer)
            .map_err(|_| Error::Runtime("parameter exceeds SQLite INTEGER".to_string()))
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Result<Value> {
        self.map_or(Ok(Value::Null), IntoValue::into_value)
    }
}

#[derive(Debug)]
pub struct Params {
    values: Result<Vec<Value>>,
}

impl Params {
    pub(crate) fn from_results(values: Vec<Result<Value>>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }
}

// Visible outside the crate: integration suites calling the fixture-facing
// `Database::execute_write` need this bound visible for their param types.
#[doc(hidden)]
pub trait IntoParams {
    fn into_params(self) -> Result<Vec<Value>>;
}

impl IntoParams for Params {
    fn into_params(self) -> Result<Vec<Value>> {
        self.values
    }
}

impl IntoParams for () {
    fn into_params(self) -> Result<Vec<Value>> {
        Ok(Vec::new())
    }
}

impl<T: IntoValue, const N: usize> IntoParams for [T; N] {
    fn into_params(self) -> Result<Vec<Value>> {
        self.into_iter().map(IntoValue::into_value).collect()
    }
}

impl<T: IntoValue> IntoParams for Vec<T> {
    fn into_params(self) -> Result<Vec<Value>> {
        self.into_iter().map(IntoValue::into_value).collect()
    }
}

macro_rules! tuple_params {
    ($($type:ident),+ $(,)?) => {
        impl<$($type: IntoValue),+> IntoParams for ($($type,)+) {
            #[allow(non_snake_case)]
            fn into_params(self) -> Result<Vec<Value>> {
                let ($($type,)+) = self;
                Ok(vec![$($type.into_value()?),+])
            }
        }
    };
}

tuple_params!(A);
tuple_params!(A, B);
tuple_params!(A, B, C);
tuple_params!(A, B, C, D);
tuple_params!(A, B, C, D, E);
tuple_params!(A, B, C, D, E, F);
tuple_params!(A, B, C, D, E, F, G);
tuple_params!(A, B, C, D, E, F, G, H);
tuple_params!(A, B, C, D, E, F, G, H, I);
tuple_params!(A, B, C, D, E, F, G, H, I, J);
tuple_params!(A, B, C, D, E, F, G, H, I, J, K);
tuple_params!(A, B, C, D, E, F, G, H, I, J, K, L);

pub fn params_from_iter<I>(values: I) -> Params
where
    I: IntoIterator,
    I::Item: IntoValue,
{
    Params::from_results(
        values
            .into_iter()
            .map(IntoValue::into_value)
            .collect::<Vec<_>>(),
    )
}

// Exported at the crate root so the root crate's `global_db` SQL sites keep
// using `db::engine::params!` across the split; the module-level `pub use`
// below keeps the historical path working.
#[macro_export]
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        $crate::db::engine::Params::from_results(vec![
            $($crate::db::engine::IntoValue::into_value($value)),*
        ])
    };
}

pub use crate::params;
