use std::collections::HashMap;

use crate::util::normalize;

#[derive(Default)]
pub struct Store {
    items: HashMap<String, i64>,
}

impl Store {
    pub fn add(&mut self, key: &str, value: i64) {
        self.items.insert(normalize(key), value);
    }

    pub fn get(&self, key: &str) -> Option<i64> {
        self.items.get(&normalize(key)).copied()
    }
}
