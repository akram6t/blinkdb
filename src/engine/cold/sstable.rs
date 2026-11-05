use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use bytes::Bytes;
use crc32fast::Hasher;
use crate::engine::cold::bloom::BloomFilter;
use crate::engine::DiskPointer;

const SSTABLE_MAGIC: &[u8; 4] = b"RFSS";
const FOOTER_SIZE: usize = 36;
const TARGET_BLOCK_SIZE: usize = 4096;

#[derive(Clone, Debug)]
pub struct BlockIndexEntry {
    pub last_key: Bytes,
    pub offset: u64,
    pub len: u32,
}

pub struct SSTableWriter {
    file: File,
    file_id: u32,
    path: PathBuf,
    current_block: Vec<u8>,
    current_offset: u64,
    index: Vec<BlockIndexEntry>,
    bloom: BloomFilter,
    record_count: u32,
    last_key: Option<Bytes>,
}

impl SSTableWriter {
    pub fn create<P: AsRef<Path>>(path: P, file_id: u32, expected_entries: usize) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .read(true)
            .open(&path)?;

        Ok(Self {
            file,
            file_id,
            path: path.as_ref().to_path_buf(),
            current_block: Vec::with_capacity(TARGET_BLOCK_SIZE),
            current_offset: 0,
            index: Vec::new(),
            bloom: BloomFilter::new(expected_entries, 0.01),
            record_count: 0,
            last_key: None,
        })
    }

    pub fn append(&mut self, key: &[u8], value: Option<&[u8]>, expire_at_ms: Option<u64>) -> std::io::Result<()> {
        let key_bytes = Bytes::copy_from_slice(key);
        self.bloom.insert(key);
        self.record_count += 1;

        let is_del = value.is_none();
        let val_slice = value.unwrap_or(&[]);

        // Serialize entry: [key_len: u16, key, val_len: u32, val, expire_at: u64, is_del: u8]
        self.current_block.extend_from_slice(&(key.len() as u16).to_le_bytes());
        self.current_block.extend_from_slice(key);
        self.current_block.extend_from_slice(&(val_slice.len() as u32).to_le_bytes());
        self.current_block.extend_from_slice(val_slice);
        self.current_block.extend_from_slice(&expire_at_ms.unwrap_or(0).to_le_bytes());
        self.current_block.push(if is_del { 1 } else { 0 });

        self.last_key = Some(key_bytes);

        if self.current_block.len() >= TARGET_BLOCK_SIZE {
            self.flush_current_block()?;
        }

        Ok(())
    }

    fn flush_current_block(&mut self) -> std::io::Result<()> {
        if self.current_block.is_empty() {
            return Ok(());
        }

        let block_len = self.current_block.len() as u32;
        let block_offset = self.current_offset;
        self.file.write_all(&self.current_block)?;
        self.current_offset += block_len as u64;

        if let Some(ref lk) = self.last_key {
            self.index.push(BlockIndexEntry {
                last_key: lk.clone(),
                offset: block_offset,
                len: block_len,
            });
        }

        self.current_block.clear();
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<SSTableReader> {
        self.flush_current_block()?;

        // 1. Serialize index block
        let index_offset = self.current_offset;
        let mut index_bytes = Vec::new();
        index_bytes.extend_from_slice(&(self.index.len() as u32).to_le_bytes());
        for entry in &self.index {
            index_bytes.extend_from_slice(&(entry.last_key.len() as u16).to_le_bytes());
            index_bytes.extend_from_slice(&entry.last_key);
            index_bytes.extend_from_slice(&entry.offset.to_le_bytes());
            index_bytes.extend_from_slice(&entry.len.to_le_bytes());
        }
        self.file.write_all(&index_bytes)?;
        let index_len = index_bytes.len() as u32;
        self.current_offset += index_len as u64;

        // 2. Serialize bloom filter
        let bloom_offset = self.current_offset;
        let bloom_bytes = self.bloom.to_bytes();
        let bloom_len = bloom_bytes.len() as u32;
        self.file.write_all(&bloom_bytes)?;
        self.current_offset += bloom_len as u64;

        // 3. Serialize footer (36 bytes)
        let mut footer = Vec::with_capacity(FOOTER_SIZE);
        footer.extend_from_slice(&bloom_offset.to_le_bytes());
        footer.extend_from_slice(&bloom_len.to_le_bytes());
        footer.extend_from_slice(&index_offset.to_le_bytes());
        footer.extend_from_slice(&index_len.to_le_bytes());
        footer.extend_from_slice(&self.record_count.to_le_bytes());
        footer.extend_from_slice(SSTABLE_MAGIC);

        let mut hasher = Hasher::new();
        hasher.update(&footer);
        let crc = hasher.finalize();
        footer.extend_from_slice(&crc.to_le_bytes());

        self.file.write_all(&footer)?;
        self.file.flush()?;

        SSTableReader::open(&self.path, self.file_id)
    }
}

pub struct SSTableReader {
    pub file_id: u32,
    pub path: PathBuf,
    file: File,
    index: Vec<BlockIndexEntry>,
    bloom: BloomFilter,
    record_count: u32,
}

impl SSTableReader {
    pub fn open<P: AsRef<Path>>(path: P, file_id: u32) -> std::io::Result<Self> {
        let mut file = File::open(&path)?;
        let file_len = file.metadata()?.len();

        if file_len < FOOTER_SIZE as u64 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "SSTable too small for footer"));
        }

        // Read footer
        file.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))?;
        let mut footer = [0u8; FOOTER_SIZE];
        file.read_exact(&mut footer)?;

        let expected_crc = u32::from_le_bytes(footer[32..36].try_into().unwrap());
        let mut hasher = Hasher::new();
        hasher.update(&footer[0..32]);
        if hasher.finalize() != expected_crc {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "SSTable footer CRC mismatch"));
        }

        if &footer[28..32] != SSTABLE_MAGIC {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid SSTable magic"));
        }

        let bloom_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
        let bloom_len = u32::from_le_bytes(footer[8..12].try_into().unwrap()) as usize;
        let index_offset = u64::from_le_bytes(footer[12..20].try_into().unwrap());
        let index_len = u32::from_le_bytes(footer[20..24].try_into().unwrap()) as usize;
        let record_count = u32::from_le_bytes(footer[24..28].try_into().unwrap());

        // Read Bloom Filter
        file.seek(SeekFrom::Start(bloom_offset))?;
        let mut bloom_buf = vec![0u8; bloom_len];
        file.read_exact(&mut bloom_buf)?;
        let bloom = BloomFilter::from_bytes(&bloom_buf)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "Corrupt bloom filter"))?;

        // Read Index Block
        file.seek(SeekFrom::Start(index_offset))?;
        let mut index_buf = vec![0u8; index_len];
        file.read_exact(&mut index_buf)?;

        let mut offset = 0;
        let num_index_entries = u32::from_le_bytes(index_buf[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;

        let mut index = Vec::with_capacity(num_index_entries);
        for _ in 0..num_index_entries {
            let klen = u16::from_le_bytes(index_buf[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            let last_key = Bytes::copy_from_slice(&index_buf[offset..offset + klen]);
            offset += klen;
            let boff = u64::from_le_bytes(index_buf[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let blen = u32::from_le_bytes(index_buf[offset..offset + 4].try_into().unwrap());
            offset += 4;
            index.push(BlockIndexEntry {
                last_key,
                offset: boff,
                len: blen,
            });
        }

        Ok(Self {
            file_id,
            path: path.as_ref().to_path_buf(),
            file,
            index,
            bloom,
            record_count,
        })
    }

    /// Fast lookup using Bloom filter in RAM -> Sparse Index -> Single Data Block read.
    pub fn get(&mut self, key: &[u8]) -> std::io::Result<Option<(Option<Bytes>, Option<u64>, DiskPointer)>> {
        // 1. Fast negative lookup via in-memory Bloom filter
        if !self.bloom.contains(key) {
            return Ok(None);
        }

        // 2. Binary search Sparse Index to find candidate block
        let block_idx = match self.index.binary_search_by(|entry| entry.last_key.as_ref().cmp(key)) {
            Ok(idx) => idx,
            Err(idx) => {
                if idx < self.index.len() {
                    idx
                } else {
                    return Ok(None);
                }
            }
        };

        let block_entry = &self.index[block_idx];
        self.file.seek(SeekFrom::Start(block_entry.offset))?;
        let mut block_buf = vec![0u8; block_entry.len as usize];
        self.file.read_exact(&mut block_buf)?;

        // 3. Scan block entries
        let mut offset = 0;
        while offset < block_buf.len() {
            let entry_offset = block_entry.offset + offset as u64;
            let klen = u16::from_le_bytes(block_buf[offset..offset + 2].try_into().unwrap()) as usize;
            offset += 2;
            let curr_key = &block_buf[offset..offset + klen];
            offset += klen;
            let vlen = u32::from_le_bytes(block_buf[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            let curr_val = &block_buf[offset..offset + vlen];
            offset += vlen;
            let expire_at_raw = u64::from_le_bytes(block_buf[offset..offset + 8].try_into().unwrap());
            offset += 8;
            let is_del = block_buf[offset] == 1;
            offset += 1;

            if curr_key == key {
                let entry_len = (2 + klen + 4 + vlen + 8 + 1) as u32;
                let ptr = DiskPointer {
                    file_id: self.file_id,
                    offset: entry_offset,
                    len: entry_len,
                };
                let expire_at = if expire_at_raw == 0 { None } else { Some(expire_at_raw) };

                if is_del {
                    return Ok(Some((None, expire_at, ptr)));
                } else {
                    return Ok(Some((Some(Bytes::copy_from_slice(curr_val)), expire_at, ptr)));
                }
            }
        }

        Ok(None)
    }

    /// Read raw value directly given a DiskPointer (zero search overhead!)
    pub fn read_at_pointer(&mut self, ptr: DiskPointer) -> std::io::Result<Option<Bytes>> {
        self.file.seek(SeekFrom::Start(ptr.offset))?;
        let mut buf = vec![0u8; ptr.len as usize];
        self.file.read_exact(&mut buf)?;

        let klen = u16::from_le_bytes(buf[0..2].try_into().unwrap()) as usize;
        let vlen_offset = 2 + klen;
        let vlen = u32::from_le_bytes(buf[vlen_offset..vlen_offset + 4].try_into().unwrap()) as usize;
        let val_offset = vlen_offset + 4;
        let val = &buf[val_offset..val_offset + vlen];
        let is_del_offset = val_offset + vlen + 8;
        let is_del = buf[is_del_offset] == 1;

        if is_del {
            Ok(None)
        } else {
            Ok(Some(Bytes::copy_from_slice(val)))
        }
    }

    pub fn record_count(&self) -> u32 {
        self.record_count
    }

    pub fn read_all_entries(&mut self) -> std::io::Result<Vec<(Bytes, Option<Bytes>, Option<u64>)>> {
        let mut entries = Vec::new();
        for block_entry in &self.index {
            self.file.seek(SeekFrom::Start(block_entry.offset))?;
            let mut block_buf = vec![0u8; block_entry.len as usize];
            self.file.read_exact(&mut block_buf)?;

            let mut offset = 0;
            while offset < block_buf.len() {
                let klen = u16::from_le_bytes(block_buf[offset..offset + 2].try_into().unwrap()) as usize;
                offset += 2;
                let k = Bytes::copy_from_slice(&block_buf[offset..offset + klen]);
                offset += klen;
                let vlen = u32::from_le_bytes(block_buf[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let v = Bytes::copy_from_slice(&block_buf[offset..offset + vlen]);
                offset += vlen;
                let exp = u64::from_le_bytes(block_buf[offset..offset + 8].try_into().unwrap());
                offset += 8;
                let is_del = block_buf[offset] == 1;
                offset += 1;

                let val = if is_del { None } else { Some(v) };
                let expire_at = if exp == 0 { None } else { Some(exp) };
                entries.push((k, val, expire_at));
            }
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_sstable_build_and_read() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let mut writer = SSTableWriter::create(&path, 1, 500).unwrap();
        for i in 0..500 {
            let k = format!("key_{:05}", i);
            let v = format!("val_{:05}", i);
            writer.append(k.as_bytes(), Some(v.as_bytes()), None).unwrap();
        }
        writer.append(b"key_tombstone", None, None).unwrap();
        let mut reader = writer.finish().unwrap();

        for i in 0..500 {
            let k = format!("key_{:05}", i);
            let (val, _, ptr) = reader.get(k.as_bytes()).unwrap().expect("must find key");
            assert_eq!(val.unwrap(), format!("val_{:05}", i).as_bytes());

            // Test fast pointer dereference
            let direct_val = reader.read_at_pointer(ptr).unwrap().unwrap();
            assert_eq!(direct_val, format!("val_{:05}", i).as_bytes());
        }

        // Test tombstone
        let (val, _, _) = reader.get(b"key_tombstone").unwrap().unwrap();
        assert!(val.is_none());

        // Test non-existent key (filtered by Bloom filter!)
        assert!(reader.get(b"missing_key_xyz").unwrap().is_none());
    }
}
