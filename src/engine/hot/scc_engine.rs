use std::sync::atomic::{AtomicUsize, Ordering};
use bytes::Bytes;
use scc::HashMap;
use crate::engine::{current_time_millis, ValueEntry};

pub struct SccIndex {
    map: HashMap<Bytes, ValueEntry>,
    count: AtomicUsize,
}

impl Default for SccIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SccIndex {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            count: AtomicUsize::new(0),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            map: HashMap::with_capacity(capacity),
            count: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn get(&self, key: &[u8]) -> Option<ValueEntry> {
        let now_ms = current_time_millis();
        let now_secs = (now_ms / 1000) as u32;

        let mut expired = false;
        let result = self.map.read_sync(key, |_k, entry| {
            if entry.is_expired(now_ms) {
                expired = true;
                None
            } else {
                Some(entry.clone())
            }
        }).flatten();

        if expired {
            let _ = self.remove(key);
            None
        } else if let Some(mut entry) = result {
            entry.touch(now_secs);
            Some(entry)
        } else {
            None
        }
    }

    #[inline]
    pub fn insert(&self, key: Bytes, entry: ValueEntry) -> Option<ValueEntry> {
        let prev = self.map.read_sync(key.as_ref(), |_k, v| v.clone());
        let _ = self.map.upsert_sync(key, entry);
        if prev.is_none() {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
        prev
    }

    #[inline]
    pub fn remove(&self, key: &[u8]) -> Option<ValueEntry> {
        let removed = self.map.remove_sync(key).map(|entry| entry.1);
        if removed.is_some() {
            self.count.fetch_sub(1, Ordering::Relaxed);
        }
        removed
    }

    #[inline]
    pub fn contains_key(&self, key: &[u8]) -> bool {
        let now_ms = current_time_millis();
        self.map.read_sync(key, |_k, v| !v.is_expired(now_ms)).unwrap_or(false)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&self) {
        self.map.clear_sync();
        self.count.store(0, Ordering::Relaxed);
    }

    /// Sample a set of keys for eviction (Sampled Clock / LRU)
    pub fn sample_keys(&self, sample_size: usize) -> Vec<(Bytes, ValueEntry)> {
        let mut samples = Vec::with_capacity(sample_size);
        self.map.any_sync(|k, v| {
            samples.push((k.clone(), v.clone()));
            samples.len() >= sample_size
        });
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scc_basic() {
        let index = SccIndex::new();
        let key = Bytes::from("key1");
        let val = Bytes::from("val1");
        let entry = ValueEntry::new_hot(val.clone(), None);

        index.insert(key.clone(), entry);
        assert_eq!(index.len(), 1);

        let retrieved = index.get(key.as_ref()).unwrap();
        match retrieved.location {
            crate::engine::ValueLocation::Hot(b) => assert_eq!(b, val),
            _ => panic!("Expected Hot"),
        }

        assert!(index.remove(key.as_ref()).is_some());
        assert_eq!(index.len(), 0);
    }
}
