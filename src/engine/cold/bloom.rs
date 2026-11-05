use crc32fast::Hasher;

/// High-performance Bloom Filter with Murmur/CRC double-hashing.
#[derive(Clone, Debug)]
pub struct BloomFilter {
    bits: Vec<u8>,
    num_bits: usize,
    num_hashes: usize,
}

impl BloomFilter {
    pub fn new(expected_entries: usize, false_positive_rate: f64) -> Self {
        let entries = expected_entries.max(64) as f64;
        let fp = false_positive_rate.clamp(0.0001, 0.5);

        // Optimal bit count m = - (n * ln(p)) / (ln(2)^2)
        let m = (-1.0 * entries * fp.ln() / (2.0f64.ln().powi(2))).ceil() as usize;
        let num_bits = m.max(64);
        let num_bytes = (num_bits + 7) / 8;

        // Optimal hash count k = (m / n) * ln(2)
        let k = ((num_bits as f64 / entries) * 2.0f64.ln()).round() as usize;
        let num_hashes = k.clamp(1, 16);

        Self {
            bits: vec![0u8; num_bytes],
            num_bits,
            num_hashes,
        }
    }

    #[inline]
    fn hash_pair(data: &[u8]) -> (u32, u32) {
        let mut h1 = Hasher::new();
        h1.update(data);
        let hash1 = h1.finalize();

        let mut h2 = Hasher::new();
        h2.update(&hash1.to_le_bytes());
        h2.update(data);
        let hash2 = h2.finalize();

        (hash1, hash2)
    }

    pub fn insert(&mut self, data: &[u8]) {
        let (h1, h2) = Self::hash_pair(data);
        for i in 0..self.num_hashes {
            let combined = h1.wrapping_add((i as u32).wrapping_mul(h2)) as usize;
            let bit_idx = combined % self.num_bits;
            self.bits[bit_idx / 8] |= 1 << (bit_idx % 8);
        }
    }

    pub fn contains(&self, data: &[u8]) -> bool {
        if self.num_bits == 0 {
            return false;
        }
        let (h1, h2) = Self::hash_pair(data);
        for i in 0..self.num_hashes {
            let combined = h1.wrapping_add((i as u32).wrapping_mul(h2)) as usize;
            let bit_idx = combined % self.num_bits;
            if (self.bits[bit_idx / 8] & (1 << (bit_idx % 8))) == 0 {
                return false;
            }
        }
        true
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + 8 + self.bits.len());
        out.extend_from_slice(&(self.num_bits as u64).to_le_bytes());
        out.extend_from_slice(&(self.num_hashes as u64).to_le_bytes());
        out.extend_from_slice(&self.bits);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 16 {
            return None;
        }
        let num_bits = u64::from_le_bytes(bytes[0..8].try_into().ok()?) as usize;
        let num_hashes = u64::from_le_bytes(bytes[8..16].try_into().ok()?) as usize;
        let expected_bytes = (num_bits + 7) / 8;
        if bytes.len() - 16 < expected_bytes {
            return None;
        }
        let bits = bytes[16..16 + expected_bytes].to_vec();
        Some(Self {
            bits,
            num_bits,
            num_hashes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_filter() {
        let mut filter = BloomFilter::new(1000, 0.01);
        for i in 0..500 {
            let key = format!("bloom_key_{}", i);
            filter.insert(key.as_bytes());
        }

        for i in 0..500 {
            let key = format!("bloom_key_{}", i);
            assert!(filter.contains(key.as_bytes()));
        }

        let mut false_positives = 0;
        for i in 500..1500 {
            let key = format!("bloom_key_{}", i);
            if filter.contains(key.as_bytes()) {
                false_positives += 1;
            }
        }
        // False positive rate should be below ~3%
        assert!(false_positives < 30);

        let bytes = filter.to_bytes();
        let restored = BloomFilter::from_bytes(&bytes).unwrap();
        assert!(restored.contains(b"bloom_key_0"));
        assert!(!restored.contains(b"non_existent_key_xyz"));
    }
}
