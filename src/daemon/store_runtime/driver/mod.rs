#[allow(dead_code)] // Registered before production callers move behind the driver.
mod libsql_compat;

#[allow(unused_imports)] // Registered for future daemon call sites.
pub(crate) use libsql_compat::{GraphLibsqlCompatDriver, GraphStoreOpenMode};
