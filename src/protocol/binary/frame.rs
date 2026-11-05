use bytes::{Bytes, BytesMut};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const RF_MAGIC: [u8; 2] = [0x52, 0x46]; // "RF"
pub const RF_VERSION: u8 = 1;

pub const OP_GET: u8 = 0x01;
pub const OP_SET: u8 = 0x02;
pub const OP_DEL: u8 = 0x03;
pub const OP_EXISTS: u8 = 0x04;
pub const OP_INCR: u8 = 0x05;
pub const OP_PING: u8 = 0x06;
pub const OP_STATS: u8 = 0x07;

pub const STATUS_OK: u8 = 0x00;
pub const STATUS_NOT_FOUND: u8 = 0x01;
pub const STATUS_ERR: u8 = 0x02;

pub const REQ_HEADER_SIZE: usize = std::mem::size_of::<RequestHeader>();
pub const RESP_HEADER_SIZE: usize = std::mem::size_of::<ResponseHeader>();

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct RequestHeader {
    pub magic: [u8; 2],
    pub version: u8,
    pub opcode: u8,
    pub flags: u16,
    pub req_id: u32,
    pub ttl_secs: u32,
    pub key_len: u16,
    pub val_len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct ResponseHeader {
    pub magic: [u8; 2],
    pub status: u8,
    pub req_id: u32,
    pub flags: u16,
    pub val_len: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct BinaryRequest {
    pub opcode: u8,
    pub flags: u16,
    pub req_id: u32,
    pub ttl_secs: u32,
    pub key: Bytes,
    pub value: Bytes,
}

impl BinaryRequest {
    pub fn parse(input: &[u8]) -> Result<Option<(Self, usize)>, String> {
        if input.len() < REQ_HEADER_SIZE {
            return Ok(None);
        }

        let (header, _) = RequestHeader::read_from_prefix(input)
            .map_err(|_| "ERR failed to read binary header".to_string())?;

        if header.magic != RF_MAGIC {
            return Err("ERR invalid binary protocol magic".to_string());
        }

        let key_len = u16::from_le(header.key_len) as usize;
        let val_len = u32::from_le(header.val_len) as usize;
        let total_size = REQ_HEADER_SIZE + key_len + val_len;

        if input.len() < total_size {
            return Ok(None); // Need more data
        }

        let key_start = REQ_HEADER_SIZE;
        let key_end = key_start + key_len;
        let val_end = key_end + val_len;

        let key = Bytes::copy_from_slice(&input[key_start..key_end]);
        let value = Bytes::copy_from_slice(&input[key_end..val_end]);

        Ok(Some((
            Self {
                opcode: header.opcode,
                flags: u16::from_le(header.flags),
                req_id: u32::from_le(header.req_id),
                ttl_secs: u32::from_le(header.ttl_secs),
                key,
                value,
            },
            total_size,
        )))
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        let header = RequestHeader {
            magic: RF_MAGIC,
            version: RF_VERSION,
            opcode: self.opcode,
            flags: self.flags.to_le(),
            req_id: self.req_id.to_le(),
            ttl_secs: self.ttl_secs.to_le(),
            key_len: (self.key.len() as u16).to_le(),
            val_len: (self.value.len() as u32).to_le(),
        };

        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&self.key);
        out.extend_from_slice(&self.value);
    }
}

pub struct BinaryResponse;

impl BinaryResponse {
    pub fn write_response(out: &mut BytesMut, req_id: u32, status: u8, flags: u16, value: Option<&[u8]>) {
        let val_slice = value.unwrap_or(&[]);
        let header = ResponseHeader {
            magic: RF_MAGIC,
            status,
            req_id: req_id.to_le(),
            flags: flags.to_le(),
            val_len: (val_slice.len() as u32).to_le(),
        };

        out.extend_from_slice(header.as_bytes());
        if !val_slice.is_empty() {
            out.extend_from_slice(val_slice);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_frame_roundtrip() {
        let req = BinaryRequest {
            opcode: OP_SET,
            flags: 0,
            req_id: 42,
            ttl_secs: 60,
            key: Bytes::from("user:100"),
            value: Bytes::from("john_doe"),
        };

        let mut buf = Vec::new();
        req.encode(&mut buf);

        let (parsed, consumed) = BinaryRequest::parse(&buf).unwrap().unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(parsed, req);
    }
}
