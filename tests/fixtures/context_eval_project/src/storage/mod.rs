//! Storage module: a shared key/value trait plus two implementations, a
//! disk-backed config store and an in-memory cache.

pub mod cache;
pub mod config_store;

/// Shared interface for anything that stores string values by key.
/// Implemented by both `ConfigStore` (disk-backed) and `Cache` (in-memory).
pub trait KeyValueStore {
    fn write_value(&mut self, key: &str, value: &str);
    fn read_value(&self, key: &str) -> Option<String>;
}
