use bytes::BytesMut;
use std::sync::Arc;
use crate::engine::StorageEngine;
use crate::protocol::binary::frame::{
    BinaryRequest, BinaryResponse, OP_DEL, OP_EXISTS, OP_GET, OP_INCR, OP_PING, OP_SET, OP_STATS,
    STATUS_ERR, STATUS_NOT_FOUND, STATUS_OK,
};

pub struct BinaryHandler {
    engine: Arc<dyn StorageEngine>,
}

impl BinaryHandler {
    pub fn new(engine: Arc<dyn StorageEngine>) -> Self {
        Self { engine }
    }

    pub fn handle_request(&self, req: BinaryRequest, out: &mut BytesMut) {
        match req.opcode {
            OP_GET => {
                match self.engine.get(&req.key) {
                    Some(val) => {
                        BinaryResponse::write_response(out, req.req_id, STATUS_OK, 0, Some(&val));
                    }
                    None => {
                        BinaryResponse::write_response(out, req.req_id, STATUS_NOT_FOUND, 0, None);
                    }
                }
            }
            OP_SET => {
                let ttl_ms = if req.ttl_secs > 0 {
                    Some((req.ttl_secs as u64) * 1000)
                } else {
                    None
                };
                self.engine.set(req.key, req.value, ttl_ms);
                BinaryResponse::write_response(out, req.req_id, STATUS_OK, 0, None);
            }
            OP_DEL => {
                let deleted = self.engine.del(&req.key);
                let status = if deleted { STATUS_OK } else { STATUS_NOT_FOUND };
                BinaryResponse::write_response(out, req.req_id, status, 0, None);
            }
            OP_EXISTS => {
                let exists = self.engine.exists(&req.key);
                let val = if exists { [1u8] } else { [0u8] };
                BinaryResponse::write_response(out, req.req_id, STATUS_OK, 0, Some(&val));
            }
            OP_INCR => {
                let delta = if req.value.is_empty() {
                    1i64
                } else {
                    let s = std::str::from_utf8(&req.value).unwrap_or("1");
                    s.parse::<i64>().unwrap_or(1)
                };
                match self.engine.incr(&req.key, delta) {
                    Ok(new_val) => {
                        let val_bytes = new_val.to_string();
                        BinaryResponse::write_response(out, req.req_id, STATUS_OK, 0, Some(val_bytes.as_bytes()));
                    }
                    Err(e) => {
                        BinaryResponse::write_response(out, req.req_id, STATUS_ERR, 0, Some(e.as_bytes()));
                    }
                }
            }
            OP_PING => {
                BinaryResponse::write_response(out, req.req_id, STATUS_OK, 0, Some(b"PONG"));
            }
            OP_STATS => {
                let stats = self.engine.stats();
                let stats_str = format!(
                    "dbsize:{} hits:{} misses:{} evictions:{}",
                    self.engine.dbsize(), stats.hits, stats.misses, stats.evictions
                );
                BinaryResponse::write_response(out, req.req_id, STATUS_OK, 0, Some(stats_str.as_bytes()));
            }
            _ => {
                BinaryResponse::write_response(out, req.req_id, STATUS_ERR, 0, Some(b"Unknown opcode"));
            }
        }
    }
}
