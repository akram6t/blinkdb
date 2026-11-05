use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use bytes::Bytes;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemTableEntry {
    Put(Bytes, Option<u64>), // Value, expire_at_ms
    Delete,
}

pub struct MemTable {
    map: BTreeMap<Bytes, MemTableEntry>,
    size_bytes: AtomicUsize,
}

impl Default for MemTable {
    fn default() -> Self {
        Self::new()
    }
}

impl MemTable {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            size_bytes: AtomicUsize::new(0),
        }
    }

    pub fn insert(&mut self, key: Bytes, value: Bytes, expire_at_ms: Option<u64>) {
        let entry_size = key.len() + value.len() + 16;
        if let Some(old) = self.map.insert(key, MemTableEntry::Put(value, expire_at_ms)) {
            match old {
                MemTableEntry::Put(v, _) => {
                    self.size_bytes.fetch_sub(v.len(), Ordering::Relaxed);
                }
                MemTableEntry::Delete => {}
            }
        }
        self.size_bytes.fetch_add(entry_size, Ordering::Relaxed);
    }

    pub fn delete(&mut self, key: Bytes) {
        let entry_size = key.len() + 16;
        if let Some(old) = self.map.insert(key, MemTableEntry::Delete) {
            match old {
                MemTableEntry::Put(v, _) => {
                    self.size_bytes.fetch_sub(v.len(), Ordering::Relaxed);
                }
                MemTableEntry::Delete => {}
            }
        }
        self.size_bytes.fetch_add(entry_size, Ordering::Relaxed);
    }

    #[inline]
    pub fn get(&self, key: &[u8]) -> Option<&MemTableEntry> {
        self.map.get(key)
    }

    #[inline]
    pub fn approximate_size(&self) -> usize {
        self.size_bytes.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn drain_all(&mut self) -> BTreeMap<Bytes, MemTableEntry> {
        self.size_bytes.store(0, Ordering::Relaxed);
        std::mem::take(&mut self.map)
    }

    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, Bytes, MemTableEntry> {
        self.map.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memtable() {
        let mut mem = MemTable::new();
        mem.insert(Bytes::from("k1"), Bytes::from("v1"), None);
        mem.insert(Bytes::from("k2"), Bytes::from("v2"), Some(100));
        assert_eq!(mem.len(), 2);
        assert!(mem.approximate_size() > 0);

        match mem.get(b"k1") {
            Some(MemTableEntry::Put(v, None)) => assert_eq!(v, &Bytes::from("v1")),
            _ => panic!("Expected Put"),
        }

        mem.delete(Bytes::from("k1"));
        assert_eq!(mem.get(b"k1"), Some(&MemTableEntry::Delete));
    }
}
