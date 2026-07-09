//! A small in-memory cache used to avoid re-reading the config store on
//! every lookup.

use std::collections::HashMap;

use crate::storage::KeyValueStore;

/// An in-memory key/value cache. Holds no reference to disk; entries are
/// lost on process exit.
pub struct Cache {
    entries: HashMap<String, String>,
}

impl Cache {
    pub fn new() -> Self {
        Cache {
            entries: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl KeyValueStore for Cache {
    fn write_value(&mut self, key: &str, value: &str) {
        self.entries.insert(key.to_string(), value.to_string());
    }

    fn read_value(&self, key: &str) -> Option<String> {
        self.entries.get(key).cloned()
    }
}
