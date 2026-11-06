use std::net::SocketAddr;
use std::sync::Arc;
use log::{error, info};
use monoio::io::{AsyncReadRent, AsyncWriteRentExt};
use monoio::net::{TcpListener, TcpStream};

use crate::cluster::router::{ClusterRouter, RouteDecision};
use crate::core::affinity::pin_to_core;
use crate::core::io_engine::{IoEngine, MonoioEngine};
use crate::engine::tiering::coordinator::TieredStorageEngine;
use crate::protocol::binary::frame::BinaryRequest;
use crate::protocol::binary::handler::BinaryHandler;
use crate::protocol::resp::handler::RespHandler;
use crate::protocol::resp::parser::{RespCommand, RespParser};
use crate::protocol::resp::writer::RespWriter;

pub struct ShardWorker {
    pub shard_id: usize,
    pub core_id: usize,
    pub resp_addr: SocketAddr,
    pub binary_addr: SocketAddr,
    pub engine: Arc<TieredStorageEngine>,
    pub router: Option<ClusterRouter>,
}

impl ShardWorker {
    pub fn run(self) {
        // Pin worker thread to target CPU core
        pin_to_core(self.core_id);
        crate::engine::tiering::coordinator::set_local_shard_id(self.shard_id);

        let mut rt = monoio::RuntimeBuilder::<monoio::FusionDriver>::new()
            .enable_all()
            .build()
            .expect("Failed to build monoio io_uring runtime");

        rt.block_on(async move {
            info!("Shard {} running on CPU core {} with io_uring", self.shard_id, self.core_id);

            // Lightweight timer to update cached timestamp every 10ms (zero syscalls on hot path)
            monoio::spawn(async {
                loop {
                    crate::engine::update_cached_time();
                    monoio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            });

            let io_backend = MonoioEngine::new();

            // Create SO_REUSEPORT listeners for Port 6379 (RESP) and Port 7379 (Binary)
            let std_resp_listener = match io_backend.create_reuseport_listener(self.resp_addr) {
                Ok(l) => l,
                Err(e) => {
                    error!("Shard {} failed to bind RESP port {}: {}", self.shard_id, self.resp_addr, e);
                    return;
                }
            };

            let std_bin_listener = match io_backend.create_reuseport_listener(self.binary_addr) {
                Ok(l) => l,
                Err(e) => {
                    error!("Shard {} failed to bind Binary port {}: {}", self.shard_id, self.binary_addr, e);
                    return;
                }
            };

            let resp_listener = match TcpListener::from_std(std_resp_listener) {
                Ok(l) => l,
                Err(e) => {
                    error!("Failed to convert RESP listener to monoio: {}", e);
                    return;
                }
            };

            let bin_listener = match TcpListener::from_std(std_bin_listener) {
                Ok(l) => l,
                Err(e) => {
                    error!("Failed to convert Binary listener to monoio: {}", e);
                    return;
                }
            };

            let engine_resp = self.engine.clone();
            let router_resp = self.router.clone();

            // Spawn RESP3 listener task on this core's io_uring ring
            monoio::spawn(async move {
                let handler = Arc::new(RespHandler::new(engine_resp));
                loop {
                    match resp_listener.accept().await {
                        Ok((stream, _peer_addr)) => {
                            let _ = stream.set_nodelay(true);
                            let h = handler.clone();
                            let r = router_resp.clone();
                            monoio::spawn(async move {
                                Self::handle_resp_connection(stream, h, r).await;
                            });
                        }
                        Err(e) => {
                            error!("RESP accept error: {}", e);
                            break;
                        }
                    }
                }
            });

            // Spawn Native Binary listener task on this core's io_uring ring
            let engine_bin = self.engine.clone();
            monoio::spawn(async move {
                let handler = Arc::new(BinaryHandler::new(engine_bin));
                loop {
                    match bin_listener.accept().await {
                        Ok((stream, _peer_addr)) => {
                            let _ = stream.set_nodelay(true);
                            let h = handler.clone();
                            monoio::spawn(async move {
                                Self::handle_binary_connection(stream, h).await;
                            });
                        }
                        Err(e) => {
                            error!("Binary accept error: {}", e);
                            break;
                        }
                    }
                }
            });

            // Keep shard event loop running
            loop {
                monoio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }

    async fn handle_resp_connection(
        mut stream: TcpStream,
        handler: Arc<RespHandler>,
        router: Option<ClusterRouter>,
    ) {
        let _ = stream.set_nodelay(true);
        let mut read_buf = vec![0u8; 32768];
        let mut accumulated = Vec::with_capacity(65536);
        let mut write_buf = Vec::with_capacity(65536);

        loop {
            // Read from io_uring stream into buffer
            let slice = std::mem::take(&mut read_buf);
            let (res, returned_buf) = stream.read(slice).await;
            read_buf = returned_buf;

            let n = match res {
                Ok(0) => break, // Connection closed by client
                Ok(n) => n,
                Err(_) => break,
            };

            // Fast path: if accumulated buffer is empty, parse directly from read_buf
            if accumulated.is_empty() {
                let mut cursor = 0;
                while cursor < n {
                    match RespParser::parse(&read_buf[cursor..n]) {
                        Ok(Some((cmd, consumed))) => {
                            cursor += consumed;

                            // Check cluster routing if router is enabled
                            if let Some(ref r) = router {
                                let key_to_check = match &cmd {
                                    RespCommand::Get(k) | RespCommand::Set { key: k, .. } => Some(k.as_ref()),
                                    RespCommand::Del(keys) => keys.first().map(|k| k.as_ref()),
                                    RespCommand::Incr { key: k, .. } | RespCommand::Ttl(k) | RespCommand::Expire { key: k, .. } => {
                                        Some(k.as_ref())
                                    }
                                    _ => None,
                                };

                                if let Some(key) = key_to_check {
                                    if let RouteDecision::Moved { slot, target_addr, .. } = r.route_key(key) {
                                        RespWriter::write_moved(&mut write_buf, slot, &target_addr);
                                        continue;
                                    }
                                }
                            }

                            if let RespCommand::Quit = cmd {
                                RespWriter::write_ok(&mut write_buf);
                                let slice = std::mem::take(&mut write_buf);
                                let _ = stream.write_all(slice).await;
                                return;
                            }

                            handler.handle_command(cmd, &mut write_buf);
                        }
                        Ok(None) => {
                            // Incomplete command, push remaining bytes to accumulated
                            accumulated.extend_from_slice(&read_buf[cursor..n]);
                            break;
                        }
                        Err(err) => {
                            RespWriter::write_error(&mut write_buf, &err);
                            break;
                        }
                    }
                }
            } else {
                accumulated.extend_from_slice(&read_buf[..n]);
                let mut cursor = 0;

                while cursor < accumulated.len() {
                    match RespParser::parse(&accumulated[cursor..]) {
                        Ok(Some((cmd, consumed))) => {
                            cursor += consumed;

                            if let Some(ref r) = router {
                                let key_to_check = match &cmd {
                                    RespCommand::Get(k) | RespCommand::Set { key: k, .. } => Some(k.as_ref()),
                                    RespCommand::Del(keys) => keys.first().map(|k| k.as_ref()),
                                    RespCommand::Incr { key: k, .. } | RespCommand::Ttl(k) | RespCommand::Expire { key: k, .. } => {
                                        Some(k.as_ref())
                                    }
                                    _ => None,
                                };

                                if let Some(key) = key_to_check {
                                    if let RouteDecision::Moved { slot, target_addr, .. } = r.route_key(key) {
                                        RespWriter::write_moved(&mut write_buf, slot, &target_addr);
                                        continue;
                                    }
                                }
                            }

                            if let RespCommand::Quit = cmd {
                                RespWriter::write_ok(&mut write_buf);
                                let slice = std::mem::take(&mut write_buf);
                                let _ = stream.write_all(slice).await;
                                return;
                            }

                            handler.handle_command(cmd, &mut write_buf);
                        }
                        Ok(None) => break,
                        Err(err) => {
                            RespWriter::write_error(&mut write_buf, &err);
                            cursor = accumulated.len();
                            break;
                        }
                    }
                }

                if cursor == accumulated.len() {
                    accumulated.clear();
                } else if cursor > 0 {
                    accumulated.drain(..cursor);
                }
            }

            // Zero-allocation buffer reuse on write_all
            if !write_buf.is_empty() {
                let slice = std::mem::take(&mut write_buf);
                let (w_res, mut returned_buf) = stream.write_all(slice).await;
                if w_res.is_err() {
                    break;
                }
                returned_buf.clear();
                write_buf = returned_buf;
            }
        }
    }

    async fn handle_binary_connection(
        mut stream: TcpStream,
        handler: Arc<BinaryHandler>,
    ) {
        let _ = stream.set_nodelay(true);
        let mut read_buf = vec![0u8; 32768];
        let mut accumulated = Vec::with_capacity(65536);
        let mut write_buf = Vec::with_capacity(65536);

        loop {
            let slice = std::mem::take(&mut read_buf);
            let (res, returned_buf) = stream.read(slice).await;
            read_buf = returned_buf;

            let n = match res {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };

            accumulated.extend_from_slice(&read_buf[..n]);
            let mut cursor = 0;
            let mut out_buf = bytes::BytesMut::with_capacity(32768);

            while cursor < accumulated.len() {
                match BinaryRequest::parse(&accumulated[cursor..]) {
                    Ok(Some((req, consumed))) => {
                        cursor += consumed;
                        handler.handle_request(req, &mut out_buf);
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }

            if !out_buf.is_empty() {
                write_buf.extend_from_slice(&out_buf);
                out_buf.clear();
            }

            if !write_buf.is_empty() {
                let slice = std::mem::take(&mut write_buf);
                let (w_res, mut returned_buf) = stream.write_all(slice).await;
                if w_res.is_err() {
                    break;
                }
                returned_buf.clear();
                write_buf = returned_buf;
            }

            if cursor == accumulated.len() {
                accumulated.clear();
            } else if cursor > 0 {
                accumulated.drain(..cursor);
            }
        }
    }
}
