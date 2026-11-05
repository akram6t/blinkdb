use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use bytes::Bytes;

use crate::engine::cold::lsm::LsmEngine;
use crate::engine::hot::scc_engine::SccIndex;
use crate::engine::{
    current_time_millis, EngineStats, StorageEngine, ValueEntry, ValueLocation,
};

pub struct TieredStorageEngine {
    hot_index: Arc<SccIndex>,
    cold_engine: Arc<LsmEngine>,
    max_hot_bytes: usize,
    hot_bytes_approx: AtomicUsize,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    promotions: AtomicU64,
}

impl TieredStorageEngine {
    pub fn open<P: AsRef<Path>>(data_dir: P, max_hot_bytes: usize) -> std::io::Result<Self> {
        let cold = Arc::new(LsmEngine::open(data_dir, 4 * 1024 * 1024)?);
        let hot = Arc::new(SccIndex::new());

        Ok(Self {
            hot_index: hot,
            cold_engine: cold,
            max_hot_bytes,
            hot_bytes_approx: AtomicUsize::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            promotions: AtomicU64::new(0),
        })
    }

    #[inline]
    fn check_eviction_threshold(&self) {
        if self.max_hot_bytes > 0 {
            let current = self.hot_bytes_approx.load(Ordering::Relaxed);
            if current > self.max_hot_bytes {
                // Trigger background eviction of 10% of keys
                let target = (self.hot_index.len() / 10).max(16);
                self.evict_cold(target);
            }
        }
    }
}

impl StorageEngine for TieredStorageEngine {
    #[inline(always)]
    fn get(&self, key: &[u8]) -> Option<Bytes> {
        // 1. Check in-memory Hot index (hottest path)
        if let Some(entry) = self.hot_index.get(key) {
            match entry.location {
                ValueLocation::Hot(val) => {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return Some(val);
                }
                ValueLocation::Cold(ptr) => {
                    let now_ms = current_time_millis();
                    let now_secs = (now_ms / 1000) as u32;
                    // Read from Cold layer via fast direct disk pointer
                    if let Ok(Some(val)) = self.cold_engine.read_pointer(ptr) {
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        self.promotions.fetch_add(1, Ordering::Relaxed);

                        // Promote back to Hot memory
                        let key_bytes = Bytes::copy_from_slice(key);
                        let val_size = val.len();
                        self.hot_index.insert(key_bytes, ValueEntry {
                            location: ValueLocation::Hot(val.clone()),
                            expire_at_ms: entry.expire_at_ms,
                            last_accessed_secs: now_secs,
                            access_count: entry.access_count.saturating_add(1),
                        });
                        self.hot_bytes_approx.fetch_add(val_size, Ordering::Relaxed);

                        return Some(val);
                    }
                }
            }
        }

        // 2. Check Cold LSM engine fallback
        let now_ms = current_time_millis();
        let now_secs = (now_ms / 1000) as u32;
        if let Ok(Some((val_opt, exp, _ptr_opt))) = self.cold_engine.get(key) {
            if let Some(val) = val_opt {
                self.hits.fetch_add(1, Ordering::Relaxed);
                self.promotions.fetch_add(1, Ordering::Relaxed);

                let key_bytes = Bytes::copy_from_slice(key);
                let val_size = val.len();
                self.hot_index.insert(key_bytes, ValueEntry {
                    location: ValueLocation::Hot(val.clone()),
                    expire_at_ms: exp,
                    last_accessed_secs: now_secs,
                    access_count: 1,
                });
                self.hot_bytes_approx.fetch_add(val_size, Ordering::Relaxed);

                return Some(val);
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    fn set(&self, key: Bytes, value: Bytes, ttl_ms: Option<u64>) {
        let val_len = value.len();
        let key_len = key.len();

        let entry = ValueEntry::new_hot(value, ttl_ms);
        self.hot_index.insert(key, entry);

        self.hot_bytes_approx.fetch_add(key_len + val_len + 32, Ordering::Relaxed);
        self.check_eviction_threshold();
    }

    fn del(&self, key: &[u8]) -> bool {
        let removed_hot = self.hot_index.remove(key).is_some();
        let key_bytes = Bytes::copy_from_slice(key);
        let _ = self.cold_engine.delete(key_bytes);
        removed_hot
    }

    fn exists(&self, key: &[u8]) -> bool {
        if self.hot_index.contains_key(key) {
            return true;
        }
        if let Ok(Some((val_opt, _, _))) = self.cold_engine.get(key) {
            return val_opt.is_some();
        }
        false
    }

    fn incr(&self, key: &[u8], delta: i64) -> Result<i64, String> {
        let current_val = self.get(key);
        let new_num = match current_val {
            Some(v) => {
                let s = std::str::from_utf8(&v)
                    .map_err(|_| "ERR value is not an integer or out of range".to_string())?;
                let n: i64 = s.trim().parse()
                    .map_err(|_| "ERR value is not an integer or out of range".to_string())?;
                n.checked_add(delta)
                    .ok_or_else(|| "ERR increment or decrement would overflow".to_string())?
            }
            None => delta,
        };

        let new_bytes = Bytes::from(new_num.to_string());
        self.set(Bytes::copy_from_slice(key), new_bytes, None);
        Ok(new_num)
    }

    fn dbsize(&self) -> usize {
        self.hot_index.len()
    }

    fn flushdb(&self) {
        self.hot_index.clear();
        self.hot_bytes_approx.store(0, Ordering::Relaxed);
    }

    fn stats(&self) -> EngineStats {
        let mut hot_keys = 0;

        // Sample hot index to calculate hot vs cold breakdown
        let samples = self.hot_index.sample_keys(200);
        for (_, entry) in &samples {
            match entry.location {
                ValueLocation::Hot(_) => hot_keys += 1,
                ValueLocation::Cold(_) => {},
            }
        }

        let total_keys = self.hot_index.len();
        let hot_ratio = if !samples.is_empty() {
            hot_keys as f64 / samples.len() as f64
        } else {
            1.0
        };

        let est_hot = (total_keys as f64 * hot_ratio) as usize;
        let est_cold = total_keys.saturating_sub(est_hot);

        EngineStats {
            hot_keys: est_hot,
            cold_keys: est_cold,
            memory_bytes: self.hot_bytes_approx.load(Ordering::Relaxed),
            disk_bytes: (self.cold_engine.sstable_count() * 4096) as u64,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            promotions: self.promotions.load(Ordering::Relaxed),
        }
    }

    /// Evicts the coldest entries from RAM into the LSM-Tree SSTable,
    /// replacing the in-memory values with compact DiskPointers.
    fn evict_cold(&self, target_count: usize) -> usize {
        let sample_pool = self.hot_index.sample_keys((target_count * 3).max(64));
        if sample_pool.is_empty() {
            return 0;
        }

        // Filter only items currently Hot in RAM
        let mut candidates: Vec<(Bytes, Bytes, Option<u64>, u32, u32)> = sample_pool
            .into_iter()
            .filter_map(|(k, entry)| match entry.location {
                ValueLocation::Hot(v) => Some((k, v, entry.expire_at_ms, entry.last_accessed_secs, entry.access_count)),
                ValueLocation::Cold(_) => None,
            })
            .collect();

        if candidates.is_empty() {
            return 0;
        }

        // Sort by access_count ascending (LFU), then last_accessed ascending (LRU)
        candidates.sort_by(|a, b| {
            a.4.cmp(&b.4).then_with(|| a.3.cmp(&b.3))
        });

        let to_evict: Vec<(Bytes, Bytes, Option<u64>)> = candidates
            .into_iter()
            .take(target_count)
            .map(|(k, v, exp, _, _)| (k, v, exp))
            .collect();

        let count = to_evict.len();
        match self.cold_engine.spill_cold_batch(&to_evict) {
            Ok(pointers) => {
                let mut freed_bytes = 0;
                for (k, ptr) in pointers {
                    if let Some(entry) = self.hot_index.get(k.as_ref()) {
                        if let ValueLocation::Hot(ref v) = entry.location {
                            freed_bytes += v.len();
                        }
                        self.hot_index.insert(k, ValueEntry {
                            location: ValueLocation::Cold(ptr),
                            expire_at_ms: entry.expire_at_ms,
                            last_accessed_secs: entry.last_accessed_secs,
                            access_count: entry.access_count,
                        });
                    }
                }
                self.hot_bytes_approx.fetch_sub(freed_bytes.min(self.hot_bytes_approx.load(Ordering::Relaxed)), Ordering::Relaxed);
                self.evictions.fetch_add(count as u64, Ordering::Relaxed);
                count
            }
            Err(e) => {
                log::error!("Failed to spill cold batch to disk: {}", e);
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_tiering_evict_and_promote() {
        let dir = tempdir().unwrap();
        let engine = TieredStorageEngine::open(dir.path(), 1024 * 1024).unwrap();

        // Write 100 entries
        for i in 0..100 {
            let k = Bytes::from(format!("key_{:03}", i));
            let v = Bytes::from(format!("val_{:03}", i));
            engine.set(k, v, None);
        }

        assert_eq!(engine.dbsize(), 100);

        // Manually evict 50 cold items
        let evicted = engine.evict_cold(50);
        assert!(evicted > 0);

        let stats = engine.stats();
        assert!(stats.evictions > 0);
        assert!(stats.cold_keys > 0);

        // Find a key that is Cold in the index to test transparent promotion from disk
        let cold_key = engine
            .hot_index
            .sample_keys(100)
            .into_iter()
            .find(|(_, entry)| matches!(entry.location, ValueLocation::Cold(_)))
            .map(|(k, _)| k)
            .expect("Must have at least one cold key after eviction");

        let val = engine.get(&cold_key).expect("must find evicted key");
        assert!(!val.is_empty());

        let stats_after = engine.stats();
        assert!(stats_after.promotions > 0);
    }
}
