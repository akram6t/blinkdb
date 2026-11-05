use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use bytes::Bytes;
use parking_lot::RwLock;

use crate::engine::cold::memtable::{MemTable, MemTableEntry};
use crate::engine::cold::sstable::{SSTableReader, SSTableWriter};
use crate::engine::cold::wal::Wal;
use crate::engine::{current_time_millis, DiskPointer};

pub struct LsmEngine {
    dir: PathBuf,
    memtable: RwLock<MemTable>,
    wal: RwLock<Wal>,
    sstables: RwLock<Vec<SSTableReader>>,
    next_file_id: AtomicU32,
    max_memtable_bytes: usize,
}

impl LsmEngine {
    pub fn open<P: AsRef<Path>>(dir: P, max_memtable_bytes: usize) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        let wal_path = dir.join("current.wal");
        let recovered = Wal::recover(&wal_path).unwrap_or_default();
        let wal = Wal::open(&wal_path)?;

        let mut memtable = MemTable::new();
        for (k, v, exp) in recovered {
            match v {
                Some(val) => memtable.insert(k, val, exp),
                None => memtable.delete(k),
            }
        }

        // Discover existing SSTables in directory
        let mut sstables = Vec::new();
        let mut max_id = 0u32;

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("sst") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if let Ok(id) = stem.parse::<u32>() {
                            max_id = max_id.max(id);
                            if let Ok(reader) = SSTableReader::open(&path, id) {
                                sstables.push(reader);
                            }
                        }
                    }
                }
            }
        }

        // Sort SSTables by file_id descending (newest first for reads)
        sstables.sort_by(|a, b| b.file_id.cmp(&a.file_id));

        Ok(Self {
            dir,
            memtable: RwLock::new(memtable),
            wal: RwLock::new(wal),
            sstables: RwLock::new(sstables),
            next_file_id: AtomicU32::new(max_id + 1),
            max_memtable_bytes: max_memtable_bytes.max(1024 * 1024),
        })
    }

    pub fn put(&self, key: Bytes, value: Bytes, expire_at_ms: Option<u64>) -> std::io::Result<()> {
        {
            let mut wal = self.wal.write();
            wal.append_put(key.as_ref(), value.as_ref(), expire_at_ms)?;
        }

        let should_flush = {
            let mut mem = self.memtable.write();
            mem.insert(key, value, expire_at_ms);
            mem.approximate_size() >= self.max_memtable_bytes
        };

        if should_flush {
            self.flush_memtable()?;
        }

        Ok(())
    }

    pub fn delete(&self, key: Bytes) -> std::io::Result<()> {
        {
            let mut wal = self.wal.write();
            wal.append_del(key.as_ref())?;
        }

        let should_flush = {
            let mut mem = self.memtable.write();
            mem.delete(key);
            mem.approximate_size() >= self.max_memtable_bytes
        };

        if should_flush {
            self.flush_memtable()?;
        }

        Ok(())
    }

    pub fn get(&self, key: &[u8]) -> std::io::Result<Option<(Option<Bytes>, Option<u64>, Option<DiskPointer>)>> {
        let now_ms = current_time_millis();

        // 1. Search MemTable
        {
            let mem = self.memtable.read();
            if let Some(entry) = mem.get(key) {
                return match entry {
                    MemTableEntry::Put(v, exp) => {
                        if let Some(exp_ms) = exp {
                            if now_ms >= *exp_ms {
                                return Ok(Some((None, *exp, None)));
                            }
                        }
                        Ok(Some((Some(v.clone()), *exp, None)))
                    }
                    MemTableEntry::Delete => Ok(Some((None, None, None))),
                };
            }
        }

        // 2. Search SSTables (from newest to oldest)
        let mut sstables = self.sstables.write();
        for sst in sstables.iter_mut() {
            if let Some((val_opt, exp, ptr)) = sst.get(key)? {
                if let Some(exp_ms) = exp {
                    if now_ms >= exp_ms {
                        return Ok(Some((None, exp, Some(ptr))));
                    }
                }
                return Ok(Some((val_opt, exp, Some(ptr))));
            }
        }

        Ok(None)
    }

    /// Direct point read given a DiskPointer (bypasses all Bloom filters / searches)
    pub fn read_pointer(&self, ptr: DiskPointer) -> std::io::Result<Option<Bytes>> {
        let mut sstables = self.sstables.write();
        for sst in sstables.iter_mut() {
            if sst.file_id == ptr.file_id {
                return sst.read_at_pointer(ptr);
            }
        }
        Ok(None)
    }

    pub fn flush_memtable(&self) -> std::io::Result<Option<u32>> {
        let entries = {
            let mut mem = self.memtable.write();
            if mem.is_empty() {
                return Ok(None);
            }
            mem.drain_all()
        };

        let file_id = self.next_file_id.fetch_add(1, Ordering::SeqCst);
        let sst_path = self.dir.join(format!("{:06}.sst", file_id));

        let mut writer = SSTableWriter::create(&sst_path, file_id, entries.len())?;
        for (k, entry) in &entries {
            match entry {
                MemTableEntry::Put(v, exp) => {
                    writer.append(k.as_ref(), Some(v.as_ref()), *exp)?;
                }
                MemTableEntry::Delete => {
                    writer.append(k.as_ref(), None, None)?;
                }
            }
        }

        let reader = writer.finish()?;
        {
            let mut sstables = self.sstables.write();
            sstables.insert(0, reader); // Newest first
        }

        {
            let mut wal = self.wal.write();
            wal.truncate()?;
        }

        log::info!("Flushed MemTable to SSTable {} ({} entries)", sst_path.display(), entries.len());
        Ok(Some(file_id))
    }

    /// Spills an explicit batch of key-value pairs directly into an SSTable
    pub fn spill_cold_batch(&self, batch: &[(Bytes, Bytes, Option<u64>)]) -> std::io::Result<Vec<(Bytes, DiskPointer)>> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }

        // Sort batch by key for SSTable order
        let mut sorted_batch = batch.to_vec();
        sorted_batch.sort_by(|a, b| a.0.cmp(&b.0));

        let file_id = self.next_file_id.fetch_add(1, Ordering::SeqCst);
        let sst_path = self.dir.join(format!("{:06}.sst", file_id));

        let mut writer = SSTableWriter::create(&sst_path, file_id, sorted_batch.len())?;
        for (k, v, exp) in &sorted_batch {
            writer.append(k.as_ref(), Some(v.as_ref()), *exp)?;
        }

        let mut reader = writer.finish()?;
        let mut pointers = Vec::with_capacity(sorted_batch.len());

        for (k, _, _) in &sorted_batch {
            if let Some((_, _, ptr)) = reader.get(k.as_ref())? {
                pointers.push((k.clone(), ptr));
            }
        }

        {
            let mut sstables = self.sstables.write();
            sstables.insert(0, reader);
        }

        log::info!("Spilled {} cold items to SSTable {:06}.sst", pointers.len(), file_id);
        Ok(pointers)
    }

    /// Compaction: Merges all SSTables into a single compacted SSTable,
    /// deduplicating keys and dropping tombstones and expired values.
    pub fn compact(&self) -> std::io::Result<usize> {
        let mut sstables = self.sstables.write();
        if sstables.len() <= 1 {
            return Ok(0);
        }

        let mut merged: BTreeMap<Bytes, (Option<Bytes>, Option<u64>)> = BTreeMap::new();
        let now_ms = current_time_millis();

        // Process from oldest to newest so newer updates overwrite older ones
        for sst in sstables.iter_mut().rev() {
            let entries = sst.read_all_entries()?;
            for (k, v, exp) in entries {
                if let Some(exp_ms) = exp {
                    if now_ms >= exp_ms {
                        merged.insert(k, (None, None)); // Expired -> drop
                        continue;
                    }
                }
                merged.insert(k, (v, exp));
            }
        }

        // Filter out tombstones
        let active_entries: Vec<(Bytes, Bytes, Option<u64>)> = merged
            .into_iter()
            .filter_map(|(k, (v, exp))| v.map(|val| (k, val, exp)))
            .collect();

        let old_sst_count = sstables.len();
        let old_paths: Vec<PathBuf> = sstables.iter().map(|s| s.path.clone()).collect();

        let file_id = self.next_file_id.fetch_add(1, Ordering::SeqCst);
        let compacted_path = self.dir.join(format!("{:06}.sst", file_id));

        let mut writer = SSTableWriter::create(&compacted_path, file_id, active_entries.len())?;
        for (k, v, exp) in &active_entries {
            writer.append(k.as_ref(), Some(v.as_ref()), *exp)?;
        }
        let reader = writer.finish()?;

        // Replace sstables with single compacted table
        *sstables = vec![reader];

        // Clean up old files on disk
        for p in old_paths {
            let _ = std::fs::remove_file(p);
        }

        log::info!("Compaction completed: merged {} SSTables into {:06}.sst ({} active items)",
            old_sst_count, file_id, active_entries.len());

        Ok(active_entries.len())
    }

    pub fn sstable_count(&self) -> usize {
        self.sstables.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_lsm_full_lifecycle() {
        let dir = tempdir().unwrap();
        let lsm = LsmEngine::open(dir.path(), 4096).unwrap();

        // Insert items
        for i in 0..100 {
            let k = Bytes::from(format!("key_{:03}", i));
            let v = Bytes::from(format!("val_{:03}", i));
            lsm.put(k, v, None).unwrap();
        }

        // Verify reads from memtable
        let res = lsm.get(b"key_050").unwrap().unwrap();
        assert_eq!(res.0.unwrap(), Bytes::from("val_050"));

        // Flush to SSTable
        lsm.flush_memtable().unwrap();
        assert_eq!(lsm.sstable_count(), 1);

        // Read from SSTable
        let res2 = lsm.get(b"key_050").unwrap().unwrap();
        assert_eq!(res2.0.unwrap(), Bytes::from("val_050"));
        assert!(res2.2.is_some()); // DiskPointer present

        // Overwrite and delete
        lsm.put(Bytes::from("key_050"), Bytes::from("val_updated"), None).unwrap();
        let res3 = lsm.get(b"key_050").unwrap().unwrap();
        assert_eq!(res3.0.unwrap(), Bytes::from("val_updated"));

        lsm.delete(Bytes::from("key_050")).unwrap();
        let res4 = lsm.get(b"key_050").unwrap().unwrap();
        assert!(res4.0.is_none()); // Tombstone

        // Flush and compact
        lsm.flush_memtable().unwrap();
        assert_eq!(lsm.sstable_count(), 2);
        lsm.compact().unwrap();
        assert_eq!(lsm.sstable_count(), 1);

        // Verify key_050 is completely gone
        assert!(lsm.get(b"key_050").unwrap().is_none());
        // Verify key_049 still intact
        let res_other = lsm.get(b"key_049").unwrap().unwrap();
        assert_eq!(res_other.0.unwrap(), Bytes::from("val_049"));
    }
}
