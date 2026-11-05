use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use bytes::Bytes;
use parking_lot::RwLock;
use crate::engine::{current_time_millis, ValueEntry};

const BUCKETS_PER_SEGMENT: usize = 64;
const SLOTS_PER_BUCKET: usize = 8;
const STASH_BUCKETS_PER_SEGMENT: usize = 4;

/// Slot containing a 1-byte fingerprint tag and the entry.
#[derive(Clone, Default)]
struct Slot {
    tag: u8,
    data: Option<(Bytes, ValueEntry)>,
}

/// A fixed-size Segment containing regular buckets and overflow stash buckets.
struct Segment {
    local_depth: usize,
    buckets: [RwLock<[Slot; SLOTS_PER_BUCKET]>; BUCKETS_PER_SEGMENT],
    stash: RwLock<[Slot; SLOTS_PER_BUCKET * STASH_BUCKETS_PER_SEGMENT]>,
}

impl Segment {
    fn new(local_depth: usize) -> Self {
        // Initialize fixed-size bucket arrays
        let buckets = std::array::from_fn(|_| RwLock::new(std::array::from_fn(|_| Slot::default())));
        let stash = RwLock::new(std::array::from_fn(|_| Slot::default()));

        Self {
            local_depth,
            buckets,
            stash,
        }
    }

    #[inline]
    fn get(&self, tag: u8, bucket_idx: usize, key: &[u8], now_ms: u64) -> Option<ValueEntry> {
        // 1. Fast check in primary bucket using 1-byte fingerprint
        {
            let b = self.buckets[bucket_idx].read();
            for slot in b.iter() {
                if slot.tag == tag {
                    if let Some((k, entry)) = &slot.data {
                        if k.as_ref() == key {
                            if entry.is_expired(now_ms) {
                                return None;
                            }
                            return Some(entry.clone());
                        }
                    }
                }
            }
        }

        // 2. Check overflow stash bucket
        {
            let s = self.stash.read();
            for slot in s.iter() {
                if slot.tag == tag {
                    if let Some((k, entry)) = &slot.data {
                        if k.as_ref() == key {
                            if entry.is_expired(now_ms) {
                                return None;
                            }
                            return Some(entry.clone());
                        }
                    }
                }
            }
        }

        None
    }

    #[inline]
    fn insert(&self, tag: u8, bucket_idx: usize, key: Bytes, entry: ValueEntry) -> Result<Option<ValueEntry>, (Bytes, ValueEntry)> {
        // 1. Check if key already exists in primary bucket or find empty slot
        {
            let mut b = self.buckets[bucket_idx].write();
            let mut empty_idx = None;

            for (i, slot) in b.iter_mut().enumerate() {
                if slot.tag == tag {
                    if let Some((k, old_entry)) = &mut slot.data {
                        if k == &key {
                            let prev = old_entry.clone();
                            *old_entry = entry;
                            return Ok(Some(prev));
                        }
                    }
                } else if slot.data.is_none() && empty_idx.is_none() {
                    empty_idx = Some(i);
                }
            }

            if let Some(idx) = empty_idx {
                b[idx].tag = tag;
                b[idx].data = Some((key, entry));
                return Ok(None);
            }
        }

        // 2. Check if key already exists in stash or find empty stash slot
        {
            let mut s = self.stash.write();
            let mut empty_stash_idx = None;

            for (i, slot) in s.iter_mut().enumerate() {
                if slot.tag == tag {
                    if let Some((k, old_entry)) = &mut slot.data {
                        if k == &key {
                            let prev = old_entry.clone();
                            *old_entry = entry;
                            return Ok(Some(prev));
                        }
                    }
                } else if slot.data.is_none() && empty_stash_idx.is_none() {
                    empty_stash_idx = Some(i);
                }
            }

            if let Some(idx) = empty_stash_idx {
                s[idx].tag = tag;
                s[idx].data = Some((key, entry));
                return Ok(None);
            }
        }

        // Segment is full, needs split
        Err((key, entry))
    }

    #[inline]
    fn remove(&self, tag: u8, bucket_idx: usize, key: &[u8]) -> Option<ValueEntry> {
        // Check primary bucket
        {
            let mut b = self.buckets[bucket_idx].write();
            for slot in b.iter_mut() {
                if slot.tag == tag {
                    if let Some((k, _)) = &slot.data {
                        if k.as_ref() == key {
                            slot.tag = 0;
                            return slot.data.take().map(|(_, e)| e);
                        }
                    }
                }
            }
        }

        // Check stash bucket
        {
            let mut s = self.stash.write();
            for slot in s.iter_mut() {
                if slot.tag == tag {
                    if let Some((k, _)) = &slot.data {
                        if k.as_ref() == key {
                            slot.tag = 0;
                            return slot.data.take().map(|(_, e)| e);
                        }
                    }
                }
            }
        }

        None
    }

    fn collect_all(&self) -> Vec<(Bytes, ValueEntry)> {
        let mut items = Vec::new();
        for b in &self.buckets {
            let lock = b.read();
            for slot in lock.iter() {
                if let Some((k, v)) = &slot.data {
                    items.push((k.clone(), v.clone()));
                }
            }
        }
        let s = self.stash.read();
        for slot in s.iter() {
            if let Some((k, v)) = &slot.data {
                items.push((k.clone(), v.clone()));
            }
        }
        items
    }
}

/// DashTable: DragonflyDB-inspired segmented extendible hashing.
/// Cuts memory overhead by segment-by-segment allocation and prevents latency spikes.
pub struct DashTable {
    global_depth: AtomicUsize,
    dir: RwLock<Vec<Arc<Segment>>>,
    count: AtomicUsize,
}

impl Default for DashTable {
    fn default() -> Self {
        Self::new(4)
    }
}

impl DashTable {
    pub fn new(initial_depth: usize) -> Self {
        let depth = initial_depth.max(1);
        let num_segments = 1 << depth;
        let mut dir = Vec::with_capacity(num_segments);

        for _ in 0..num_segments {
            dir.push(Arc::new(Segment::new(depth)));
        }

        Self {
            global_depth: AtomicUsize::new(depth),
            dir: RwLock::new(dir),
            count: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn hash_key(key: &[u8]) -> u64 {
        // Fast hash using crc32fast / fxhash style
        let mut h = crc32fast::Hasher::new();
        h.update(key);
        let c = h.finalize() as u64;
        // Fold into 64-bit
        c ^ (c << 32)
    }

    #[inline]
    fn tag_from_hash(hash: u64) -> u8 {
        let t = (hash >> 56) as u8;
        if t == 0 { 1 } else { t }
    }

    #[inline]
    fn segment_index(&self, hash: u64, depth: usize) -> usize {
        (hash as usize) & ((1 << depth) - 1)
    }

    #[inline]
    fn bucket_index(hash: u64) -> usize {
        ((hash >> 16) as usize) % BUCKETS_PER_SEGMENT
    }

    pub fn get(&self, key: &[u8]) -> Option<ValueEntry> {
        let hash = Self::hash_key(key);
        let tag = Self::tag_from_hash(hash);
        let bucket_idx = Self::bucket_index(hash);
        let depth = self.global_depth.load(Ordering::Relaxed);
        let seg_idx = self.segment_index(hash, depth);

        let seg = {
            let dir = self.dir.read();
            dir[seg_idx].clone()
        };

        let now_ms = current_time_millis();
        seg.get(tag, bucket_idx, key, now_ms)
    }

    pub fn insert(&self, key: Bytes, entry: ValueEntry) -> Option<ValueEntry> {
        let mut curr_key = key;
        let mut curr_entry = entry;

        loop {
            let hash = Self::hash_key(curr_key.as_ref());
            let tag = Self::tag_from_hash(hash);
            let bucket_idx = Self::bucket_index(hash);
            let depth = self.global_depth.load(Ordering::Relaxed);
            let seg_idx = self.segment_index(hash, depth);

            let seg = {
                let dir = self.dir.read();
                dir[seg_idx].clone()
            };

            match seg.insert(tag, bucket_idx, curr_key, curr_entry) {
                Ok(prev) => {
                    if prev.is_none() {
                        self.count.fetch_add(1, Ordering::Relaxed);
                    }
                    return prev;
                }
                Err((k, e)) => {
                    // Segment overflow: perform segment-by-segment extendible split
                    curr_key = k;
                    curr_entry = e;
                    self.split_segment(seg_idx, &seg);
                }
            }
        }
    }

    fn split_segment(&self, seg_idx: usize, seg: &Arc<Segment>) {
        let mut dir = self.dir.write();
        let local_depth = seg.local_depth;
        let global_depth = self.global_depth.load(Ordering::SeqCst);

        // If local_depth == global_depth, double directory pointers (directory doubling only)
        if local_depth == global_depth {
            let clones: Vec<_> = dir.iter().cloned().collect();
            dir.extend(clones);
            self.global_depth.fetch_add(1, Ordering::SeqCst);
        }

        let new_depth = local_depth + 1;
        let seg0 = Arc::new(Segment::new(new_depth));
        let seg1 = Arc::new(Segment::new(new_depth));

        // Redistribute items from old segment
        let items = seg.collect_all();
        for (k, v) in items {
            let h = Self::hash_key(k.as_ref());
            let tag = Self::tag_from_hash(h);
            let b_idx = Self::bucket_index(h);
            let target = if (h & (1 << local_depth)) == 0 {
                &seg0
            } else {
                &seg1
            };
            let _ = target.insert(tag, b_idx, k, v);
        }

        // Update directory pointers
        let step = 1 << new_depth;
        let base = seg_idx & ((1 << local_depth) - 1);
        let mut idx = base;
        let dir_len = dir.len();
        while idx < dir_len {
            dir[idx] = seg0.clone();
            if idx + (1 << local_depth) < dir_len {
                dir[idx + (1 << local_depth)] = seg1.clone();
            }
            idx += step;
        }
    }

    pub fn remove(&self, key: &[u8]) -> Option<ValueEntry> {
        let hash = Self::hash_key(key);
        let tag = Self::tag_from_hash(hash);
        let bucket_idx = Self::bucket_index(hash);
        let depth = self.global_depth.load(Ordering::Relaxed);
        let seg_idx = self.segment_index(hash, depth);

        let seg = {
            let dir = self.dir.read();
            dir[seg_idx].clone()
        };

        let removed = seg.remove(tag, bucket_idx, key);
        if removed.is_some() {
            self.count.fetch_sub(1, Ordering::Relaxed);
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.count.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        let depth = 4;
        let num_segments = 1 << depth;
        let mut dir = Vec::with_capacity(num_segments);
        for _ in 0..num_segments {
            dir.push(Arc::new(Segment::new(depth)));
        }
        *self.dir.write() = dir;
        self.global_depth.store(depth, Ordering::SeqCst);
        self.count.store(0, Ordering::Relaxed);
    }

    pub fn sample_keys(&self, sample_size: usize) -> Vec<(Bytes, ValueEntry)> {
        let dir = self.dir.read();
        let mut samples = Vec::with_capacity(sample_size);
        for seg in dir.iter() {
            let items = seg.collect_all();
            for item in items {
                samples.push(item);
                if samples.len() >= sample_size {
                    return samples;
                }
            }
        }
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dashtable_basic() {
        let table = DashTable::new(2);
        for i in 0..1000 {
            let k = Bytes::from(format!("key:{}", i));
            let v = Bytes::from(format!("val:{}", i));
            table.insert(k.clone(), ValueEntry::new_hot(v.clone(), None));
        }

        assert_eq!(table.len(), 1000);

        for i in 0..1000 {
            let k = format!("key:{}", i);
            let entry = table.get(k.as_bytes()).expect("key must exist");
            match entry.location {
                crate::engine::ValueLocation::Hot(b) => {
                    assert_eq!(b.as_ref(), format!("val:{}", i).as_bytes());
                }
                _ => panic!("Expected hot entry"),
            }
        }

        for i in 0..500 {
            let k = format!("key:{}", i);
            assert!(table.remove(k.as_bytes()).is_some());
        }
        assert_eq!(table.len(), 500);
    }
}
