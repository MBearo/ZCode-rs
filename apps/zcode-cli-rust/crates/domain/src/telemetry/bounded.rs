//! Node `BoundedKeySet` / `BoundedValueMap`: at most 2000 keys, the oldest
//! insertion evicted first; `set` re-inserts a key as the newest.
use std::collections::{HashMap, VecDeque};

const MAX_KEYS: usize = 2_000;

#[derive(Debug)]
pub(super) struct BoundedMap<T> {
    order: VecDeque<String>,
    values: HashMap<String, T>,
}

impl<T> Default for BoundedMap<T> {
    fn default() -> Self {
        Self {
            order: VecDeque::new(),
            values: HashMap::new(),
        }
    }
}

impl<T> BoundedMap<T> {
    pub fn get(&self, key: &str) -> Option<&T> {
        self.values.get(key)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    pub fn set(&mut self, key: String, value: T) {
        if self.values.insert(key.clone(), value).is_some() {
            self.order.retain(|k| *k != key);
        }
        self.order.push_back(key);
        if self.order.len() > MAX_KEYS
            && let Some(oldest) = self.order.pop_front()
        {
            self.values.remove(&oldest);
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<T> {
        let value = self.values.remove(key)?;
        self.order.retain(|k| k != key);
        Some(value)
    }

    pub fn remove_prefix(&mut self, prefix: &str) {
        self.order.retain(|k| !k.starts_with(prefix));
        self.values.retain(|k, _| !k.starts_with(prefix));
    }
}

/// Node `BoundedKeySet`: `add` is true only the first time a key is seen.
#[derive(Debug, Default)]
pub(super) struct BoundedSet(BoundedMap<()>);

impl BoundedSet {
    pub fn add(&mut self, key: String) -> bool {
        if self.0.contains(&key) {
            return false;
        }
        self.0.set(key, ());
        true
    }

    pub fn remove_prefix(&mut self, prefix: &str) {
        self.0.remove_prefix(prefix);
    }
}
