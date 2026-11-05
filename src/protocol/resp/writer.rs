use bytes::BytesMut;

pub struct RespWriter;

impl RespWriter {
    #[inline]
    pub fn write_ok(buf: &mut BytesMut) {
        buf.extend_from_slice(b"+OK\r\n");
    }

    #[inline]
    pub fn write_pong(buf: &mut BytesMut, arg: Option<&[u8]>) {
        match arg {
            Some(msg) => Self::write_bulk_string(buf, msg),
            None => buf.extend_from_slice(b"+PONG\r\n"),
        }
    }

    #[inline]
    pub fn write_simple_string(buf: &mut BytesMut, s: &str) {
        buf.extend_from_slice(b"+");
        buf.extend_from_slice(s.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    #[inline]
    pub fn write_error(buf: &mut BytesMut, err: &str) {
        buf.extend_from_slice(b"-");
        buf.extend_from_slice(err.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    #[inline]
    pub fn write_integer(buf: &mut BytesMut, val: i64) {
        buf.extend_from_slice(b":");
        buf.extend_from_slice(val.to_string().as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    #[inline]
    pub fn write_bulk_string(buf: &mut BytesMut, data: &[u8]) {
        buf.extend_from_slice(b"$");
        buf.extend_from_slice(data.len().to_string().as_bytes());
        buf.extend_from_slice(b"\r\n");
        buf.extend_from_slice(data);
        buf.extend_from_slice(b"\r\n");
    }

    #[inline]
    pub fn write_null(buf: &mut BytesMut) {
        buf.extend_from_slice(b"$-1\r\n");
    }

    #[inline]
    pub fn write_array_header(buf: &mut BytesMut, len: usize) {
        buf.extend_from_slice(b"*");
        buf.extend_from_slice(len.to_string().as_bytes());
        buf.extend_from_slice(b"\r\n");
    }

    #[inline]
    pub fn write_moved(buf: &mut BytesMut, slot: u16, addr: &str) {
        buf.extend_from_slice(b"-MOVED ");
        buf.extend_from_slice(slot.to_string().as_bytes());
        buf.extend_from_slice(b" ");
        buf.extend_from_slice(addr.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resp_writer() {
        let mut buf = BytesMut::new();
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
