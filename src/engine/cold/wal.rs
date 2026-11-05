use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use bytes::Bytes;
use crc32fast::Hasher;

const WAL_OP_PUT: u8 = 1;
const WAL_OP_DEL: u8 = 2;

pub struct Wal {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl Wal {
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&path)?;

        Ok(Self {
            path: path.as_ref().to_path_buf(),
            writer: BufWriter::with_capacity(64 * 1024, file),
        })
    }

    pub fn append_put(&mut self, key: &[u8], value: &[u8], expire_at_ms: Option<u64>) -> std::io::Result<()> {
        let mut payload = Vec::with_capacity(1 + 2 + key.len() + 4 + value.len() + 8);
        payload.push(WAL_OP_PUT);
        payload.extend_from_slice(&(key.len() as u16).to_le_bytes());
        payload.extend_from_slice(key);
        payload.extend_from_slice(&(value.len() as u32).to_le_bytes());
        payload.extend_from_slice(value);
        payload.extend_from_slice(&expire_at_ms.unwrap_or(0).to_le_bytes());

        let mut hasher = Hasher::new();
        hasher.update(&payload);
        let crc = hasher.finalize();

        self.writer.write_all(&crc.to_le_bytes())?;
        self.writer.write_all(&(payload.len() as u32).to_le_bytes())?;
        self.writer.write_all(&payload)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn append_del(&mut self, key: &[u8]) -> std::io::Result<()> {
        let mut payload = Vec::with_capacity(1 + 2 + key.len());
        payload.push(WAL_OP_DEL);
        payload.extend_from_slice(&(key.len() as u16).to_le_bytes());
        payload.extend_from_slice(key);

        let mut hasher = Hasher::new();
        hasher.update(&payload);
        let crc = hasher.finalize();

        self.writer.write_all(&crc.to_le_bytes())?;
        self.writer.write_all(&(payload.len() as u32).to_le_bytes())?;
        self.writer.write_all(&payload)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn recover<P: AsRef<Path>>(path: P) -> std::io::Result<Vec<(Bytes, Option<Bytes>, Option<u64>)>> {
        let path_ref = path.as_ref();
        if !path_ref.exists() {
            return Ok(Vec::new());
        }

        let mut file = BufReader::new(File::open(path_ref)?);
        let mut records = Vec::new();

        loop {
            let mut crc_buf = [0u8; 4];
            if file.read_exact(&mut crc_buf).is_err() {
                break; // EOF
            }
            let expected_crc = u32::from_le_bytes(crc_buf);

            let mut len_buf = [0u8; 4];
            file.read_exact(&mut len_buf)?;
            let payload_len = u32::from_le_bytes(len_buf) as usize;

            let mut payload = vec![0u8; payload_len];
            file.read_exact(&mut payload)?;

            let mut hasher = Hasher::new();
            hasher.update(&payload);
            if hasher.finalize() != expected_crc {
                log::warn!("WAL corruption detected at offset; truncating replay");
                break;
            }

            let op = payload[0];
            let key_len = u16::from_le_bytes(payload[1..3].try_into().unwrap()) as usize;
            let key = Bytes::copy_from_slice(&payload[3..3 + key_len]);

            if op == WAL_OP_PUT {
                let mut offset = 3 + key_len;
                let val_len = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let val = Bytes::copy_from_slice(&payload[offset..offset + val_len]);
                offset += val_len;
                let exp = u64::from_le_bytes(payload[offset..offset + 8].try_into().unwrap());
                let expire_at = if exp == 0 { None } else { Some(exp) };
                records.push((key, Some(val), expire_at));
            } else if op == WAL_OP_DEL {
                records.push((key, None, None));
            }
        }

        Ok(records)
    }

    pub fn truncate(&mut self) -> std::io::Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        self.writer = BufWriter::with_capacity(64 * 1024, file);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_wal_write_and_recover() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append_put(b"foo", b"bar", Some(12345678)).unwrap();
            wal.append_put(b"alpha", b"beta", None).unwrap();
            wal.append_del(b"foo").unwrap();
        }

        let recovered = Wal::recover(&path).unwrap();
        assert_eq!(recovered.len(), 3);
        assert_eq!(recovered[0].0, Bytes::from("foo"));
        assert_eq!(recovered[0].1, Some(Bytes::from("bar")));
        assert_eq!(recovered[0].2, Some(12345678));
        assert_eq!(recovered[1].0, Bytes::from("alpha"));
        assert_eq!(recovered[1].1, Some(Bytes::from("beta")));
        assert_eq!(recovered[1].2, None);
        assert_eq!(recovered[2].0, Bytes::from("foo"));
        assert_eq!(recovered[2].1, None);
    }
}
