use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use bytes::Bytes;

use crate::engine::cold::lsm::LsmEngine;
use crate::engine::hot::scc_engine::SccIndex;
use crate::engine::{
    current_time_millis, EngineStats, StorageEngine, ValueEntry, ValueLocation,
};

pub const NUM_COUNTERS: usize = 64;

#[repr(align(64))]
pub struct CacheAligned<T>(pub T);

impl<T> CacheAligned<T> {
    pub const fn new(val: T) -> Self {
        Self(val)
    }
}

thread_local! {
    static LOCAL_SHARD_ID: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[inline(always)]
pub fn set_local_shard_id(id: usize) {
    LOCAL_SHARD_ID.with(|cell| cell.set(id));
}

#[inline(always)]
pub fn get_local_shard_id() -> usize {
    LOCAL_SHARD_ID.with(|cell| cell.get())
}

pub struct TieredStorageEngine {
    hot_index: Arc<SccIndex>,
    cold_engine: Arc<LsmEngine>,
    max_hot_bytes: usize,
    hot_bytes_approx: [CacheAligned<AtomicUsize>; NUM_COUNTERS],
    is_evicting: CacheAligned<AtomicBool>,
    hits: [CacheAligned<AtomicU64>; NUM_COUNTERS],
    misses: [CacheAligned<AtomicU64>; NUM_COUNTERS],
    evictions: CacheAligned<AtomicU64>,
    promotions: CacheAligned<AtomicU64>,
}

impl TieredStorageEngine {
    pub fn open<P: AsRef<Path>>(data_dir: P, max_hot_bytes: usize) -> std::io::Result<Self> {
        let cold = Arc::new(LsmEngine::open(data_dir, 4 * 1024 * 1024)?);
        let hot = Arc::new(SccIndex::new());

        Ok(Self {
            hot_index: hot,
            cold_engine: cold,
            max_hot_bytes,
            hot_bytes_approx: [const { CacheAligned::new(AtomicUsize::new(0)) }; NUM_COUNTERS],
            is_evicting: CacheAligned::new(AtomicBool::new(false)),
            hits: [const { CacheAligned::new(AtomicU64::new(0)) }; NUM_COUNTERS],
            misses: [const { CacheAligned::new(AtomicU64::new(0)) }; NUM_COUNTERS],
            evictions: CacheAligned::new(AtomicU64::new(0)),
            promotions: CacheAligned::new(AtomicU64::new(0)),
        })
    }

    #[inline]
    fn check_eviction_threshold(&self) {
        if self.is_evicting.0.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            // Evict a bounded batch of keys (max 256) so worker latency never spikes
            let target = (self.hot_index.len() / 100).clamp(16, 256);
            self.evict_cold(target);
            self.is_evicting.0.store(false, Ordering::Release);
        }
    }
}

impl StorageEngine for TieredStorageEngine {
    #[inline(always)]
    fn get(&self, key: &[u8]) -> Option<Bytes> {
        let shard = get_local_shard_id() & (NUM_COUNTERS - 1);

        // 1. Fast hot index lookup (zero struct-clone fast-path)
        if let Some(res) = self.hot_index.get_value_fast(key) {
            match res {
                Ok(val) => {
                    self.hits[shard].0.fetch_add(1, Ordering::Relaxed);
                    return Some(val);
                }
                Err(ptr) => {
                    let now_ms = current_time_millis();
                    let now_secs = (now_ms / 1000) as u32;
                    // Read from Cold layer via fast direct disk pointer
                    if let Ok(Some(val)) = self.cold_engine.read_pointer(ptr) {
                        self.hits[shard].0.fetch_add(1, Ordering::Relaxed);
                        self.promotions.0.fetch_add(1, Ordering::Relaxed);

                        // Promote back to Hot memory
                        let key_bytes = Bytes::copy_from_slice(key);
                        let val_size = val.len();
                        self.hot_index.insert(key_bytes, ValueEntry {
                            location: ValueLocation::Hot(val.clone()),
                            expire_at_ms: None,
                            last_accessed_secs: now_secs,
                            access_count: 2,
                        });
                        self.hot_bytes_approx[shard].0.fetch_add(val_size, Ordering::Relaxed);

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
                self.hits[shard].0.fetch_add(1, Ordering::Relaxed);
                self.promotions.0.fetch_add(1, Ordering::Relaxed);

                let key_bytes = Bytes::copy_from_slice(key);
                let val_size = val.len();
                self.hot_index.insert(key_bytes, ValueEntry {
                    location: ValueLocation::Hot(val.clone()),
                    expire_at_ms: exp,
                    last_accessed_secs: now_secs,
                    access_count: 1,
                });
                self.hot_bytes_approx[shard].0.fetch_add(val_size, Ordering::Relaxed);

                return Some(val);
            }
        }

        self.misses[shard].0.fetch_add(1, Ordering::Relaxed);
        None
    }

    #[inline(always)]
    fn set(&self, key: Bytes, value: Bytes, ttl_ms: Option<u64>) {
        let val_len = value.len();
        let key_len = key.len();

        let entry = ValueEntry::new_hot(value, ttl_ms);
        self.hot_index.insert(key, entry);

        if self.max_hot_bytes > 0 {
            let shard = get_local_shard_id() & (NUM_COUNTERS - 1);
            let delta = key_len + val_len + 32;
            let prev = self.hot_bytes_approx[shard].0.fetch_add(delta, Ordering::Relaxed);

            // Check eviction threshold only once every 64KB allocated per shard to eliminate bus locks
            if (prev / 65536) != ((prev + delta) / 65536) {
                let total_hot: usize = self.hot_bytes_approx.iter().map(|c| c.0.load(Ordering::Relaxed)).sum();
                if total_hot > self.max_hot_bytes {
                    self.check_eviction_threshold();
                }
            }
        }
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

        let mut itoa_buf = itoa::Buffer::new();
        let formatted = itoa_buf.format(new_num);
        let new_bytes = Bytes::copy_from_slice(formatted.as_bytes());
        self.set(Bytes::copy_from_slice(key), new_bytes, None);
        Ok(new_num)
    }

    fn dbsize(&self) -> usize {
        self.hot_index.len()
    }

    fn flushdb(&self) {
        self.hot_index.clear();
        for b in &self.hot_bytes_approx {
            b.0.store(0, Ordering::Relaxed);
        }
        let _ = self.cold_engine.clear();
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
        let total_memory_bytes: usize = self.hot_bytes_approx.iter().map(|b| b.0.load(Ordering::Relaxed)).sum();
        let total_hits: u64 = self.hits.iter().map(|h| h.0.load(Ordering::Relaxed)).sum();
        let total_misses: u64 = self.misses.iter().map(|m| m.0.load(Ordering::Relaxed)).sum();

        EngineStats {
            hot_keys: est_hot,
            cold_keys: est_cold,
            memory_bytes: total_memory_bytes,
            disk_bytes: (self.cold_engine.sstable_count() * 4096) as u64,
            hits: total_hits,
            misses: total_misses,
            evictions: self.evictions.0.load(Ordering::Relaxed),
            promotions: self.promotions.0.load(Ordering::Relaxed),
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
                let shard = get_local_shard_id() & (NUM_COUNTERS - 1);
                let current_approx = self.hot_bytes_approx[shard].0.load(Ordering::Relaxed);
                self.hot_bytes_approx[shard].0.fetch_sub(freed_bytes.min(current_approx), Ordering::Relaxed);
                self.evictions.0.fetch_add(count as u64, Ordering::Relaxed);
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
