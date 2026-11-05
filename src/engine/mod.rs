pub mod cold;
pub mod hot;
pub mod tiering;

use bytes::Bytes;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static CACHED_TIME_MS: AtomicU64 = AtomicU64::new(0);

pub fn update_cached_time() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    CACHED_TIME_MS.store(now, Ordering::Relaxed);
}

#[inline(always)]
pub fn current_time_millis() -> u64 {
    let cached = CACHED_TIME_MS.load(Ordering::Relaxed);
    if cached != 0 {
        cached
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskPointer {
    pub file_id: u32,
    pub offset: u64,
    pub len: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueLocation {
    Hot(Bytes),
    Cold(DiskPointer),
}

#[derive(Clone, Debug)]
pub struct ValueEntry {
    pub location: ValueLocation,
    pub expire_at_ms: Option<u64>,
    pub last_accessed_secs: u32,
    pub access_count: u32,
}

impl ValueEntry {
    #[inline(always)]
    pub fn new_hot(value: Bytes, ttl_ms: Option<u64>) -> Self {
        let (expire_at_ms, now_secs) = match ttl_ms {
            Some(ttl) => {
                let now_ms = current_time_millis();
                (Some(now_ms + ttl), (now_ms / 1000) as u32)
            }
            None => (None, 0),
        };

        Self {
            location: ValueLocation::Hot(value),
            expire_at_ms,
            last_accessed_secs: now_secs,
            access_count: 1,
        }
    }

    pub fn new_cold(ptr: DiskPointer, ttl_ms: Option<u64>) -> Self {
        let (expire_at_ms, now_secs) = match ttl_ms {
            Some(ttl) => {
                let now_ms = current_time_millis();
                (Some(now_ms + ttl), (now_ms / 1000) as u32)
            }
            None => (None, 0),
        };

        Self {
            location: ValueLocation::Cold(ptr),
            expire_at_ms,
            last_accessed_secs: now_secs,
            access_count: 0,
        }
    }

    #[inline(always)]
    pub fn is_expired(&self, now_ms: u64) -> bool {
        match self.expire_at_ms {
            Some(exp) => now_ms >= exp,
            None => false,
        }
    }

    #[inline(always)]
    pub fn touch(&mut self, now_secs: u32) {
        self.last_accessed_secs = now_secs;
        self.access_count = self.access_count.saturating_add(1);
    }
}

#[derive(Default, Debug, Clone)]
pub struct EngineStats {
    pub hot_keys: usize,
    pub cold_keys: usize,
    pub memory_bytes: usize,
    pub disk_bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub promotions: u64,
}

pub trait StorageEngine: Send + Sync + 'static {
    fn get(&self, key: &[u8]) -> Option<Bytes>;
    fn set(&self, key: Bytes, value: Bytes, ttl_ms: Option<u64>);
    fn del(&self, key: &[u8]) -> bool;
    fn exists(&self, key: &[u8]) -> bool;
    fn incr(&self, key: &[u8], delta: i64) -> Result<i64, String>;
    fn dbsize(&self) -> usize;
    fn flushdb(&self);
    fn stats(&self) -> EngineStats;
    fn evict_cold(&self, target_count: usize) -> usize;
}
