//! Disk-backed configuration storage: reads and writes a config file.

use std::collections::HashMap;

use crate::storage::KeyValueStore;

/// Reads and writes configuration key/value pairs to a file on disk.
pub struct ConfigStore {
    path: String,
    values: HashMap<String, String>,
}

impl ConfigStore {
    pub fn new(path: &str) -> Self {
        ConfigStore {
            path: path.to_string(),
            values: HashMap::new(),
        }
    }

    /// Reads the full config file contents from disk.
    pub fn read_config(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path).map_err(|err| err.to_string())
    }

    /// Writes a single key/value pair to the config file on disk. This is
    /// the entry point for "who writes to the config file" style questions.
    pub fn write_config(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.write_value(key, value);
        std::fs::write(&self.path, format!("{key}={value}\n")).map_err(|err| err.to_string())
    }
}

impl KeyValueStore for ConfigStore {
    fn write_value(&mut self, key: &str, value: &str) {
        self.values.insert(key.to_string(), value.to_string());
    }

    fn read_value(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}
