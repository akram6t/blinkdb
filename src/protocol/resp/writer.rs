use bytes::BytesMut;

pub struct RespWriter;

impl RespWriter {
    #[inline(always)]
    pub fn write_ok(buf: &mut Vec<u8>) {
        buf.extend_from_slice(b"+OK\r\n");
    }

    #[inline(always)]
    pub fn write_pong(buf: &mut Vec<u8>, arg: Option<&[u8]>) {
        match arg {
            Some(msg) => Self::write_bulk_string(buf, msg),
            None => buf.extend_from_slice(b"+PONG\r\n"),
        }
    }

    #[inline(always)]
    pub fn write_simple_string(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(b"+");
        buf.extend_from_slice(s.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_error(buf: &mut Vec<u8>, err: &str) {
        buf.extend_from_slice(b"-");
        buf.extend_from_slice(err.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_integer(buf: &mut Vec<u8>, val: i64) {
        buf.extend_from_slice(b":");
        let mut itoa_buf = itoa::Buffer::new();
        buf.extend_from_slice(itoa_buf.format(val).as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_bulk_string(buf: &mut Vec<u8>, data: &[u8]) {
        buf.extend_from_slice(b"$");
        let mut itoa_buf = itoa::Buffer::new();
        buf.extend_from_slice(itoa_buf.format(data.len()).as_bytes());
        buf.extend_from_slice(b"\r\n");
        buf.extend_from_slice(data);
        buf.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_null(buf: &mut Vec<u8>) {
        buf.extend_from_slice(b"$-1\r\n");
    }

    #[inline(always)]
    pub fn write_array_header(buf: &mut Vec<u8>, len: usize) {
        buf.extend_from_slice(b"*");
        let mut itoa_buf = itoa::Buffer::new();
        buf.extend_from_slice(itoa_buf.format(len).as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    #[inline(always)]
    pub fn write_moved(buf: &mut Vec<u8>, slot: u16, addr: &str) {
        buf.extend_from_slice(b"-MOVED ");
        let mut itoa_buf = itoa::Buffer::new();
        buf.extend_from_slice(itoa_buf.format(slot).as_bytes());
        buf.extend_from_slice(b" ");
        buf.extend_from_slice(addr.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    // Compatibility adapter for BytesMut if needed
    #[inline(always)]
    pub fn write_ok_bytes_mut(buf: &mut BytesMut) {
        buf.extend_from_slice(b"+OK\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resp_writer() {
        let mut buf = Vec::new();
        RespWriter::write_ok(&mut buf);
        assert_eq!(&buf[..], b"+OK\r\n");

        buf.clear();
        RespWriter::write_bulk_string(&mut buf, b"hello");
        assert_eq!(&buf[..], b"$5\r\nhello\r\n");

        buf.clear();
        RespWriter::write_integer(&mut buf, 42);
        assert_eq!(&buf[..], b":42\r\n");
    }
}
