use std::sync::Arc;
use crate::engine::StorageEngine;
use crate::protocol::resp::parser::RespCommand;
use crate::protocol::resp::writer::RespWriter;

pub struct RespHandler {
    engine: Arc<dyn StorageEngine>,
}

impl RespHandler {
    pub fn new(engine: Arc<dyn StorageEngine>) -> Self {
        Self { engine }
    }

    pub fn handle_command(&self, cmd: RespCommand, out: &mut Vec<u8>) {
        match cmd {
            RespCommand::Ping(arg) => {
                RespWriter::write_pong(out, arg.as_deref());
            }
            RespCommand::Echo(msg) => {
                RespWriter::write_bulk_string(out, &msg);
            }
            RespCommand::Get(key) => {
                match self.engine.get(&key) {
                    Some(val) => RespWriter::write_bulk_string(out, &val),
                    None => RespWriter::write_null(out),
                }
            }
            RespCommand::Set { key, value, ttl_ms, nx, xx } => {
                if nx && self.engine.exists(&key) {
                    RespWriter::write_null(out);
                    return;
                }
                if xx && !self.engine.exists(&key) {
                    RespWriter::write_null(out);
                    return;
                }
                self.engine.set(key, value, ttl_ms);
                RespWriter::write_ok(out);
            }
            RespCommand::Del(keys) => {
                let mut count = 0;
                for k in keys {
                    if self.engine.del(&k) {
                        count += 1;
                    }
                }
                RespWriter::write_integer(out, count);
            }
            RespCommand::Exists(keys) => {
                let mut count = 0;
                for k in keys {
                    if self.engine.exists(&k) {
                        count += 1;
                    }
                }
                RespWriter::write_integer(out, count);
            }
            RespCommand::MGet(keys) => {
                RespWriter::write_array_header(out, keys.len());
                for k in keys {
                    match self.engine.get(&k) {
                        Some(val) => RespWriter::write_bulk_string(out, &val),
                        None => RespWriter::write_null(out),
                    }
                }
            }
            RespCommand::MSet(pairs) => {
                for (k, v) in pairs {
                    self.engine.set(k, v, None);
                }
                RespWriter::write_ok(out);
            }
            RespCommand::Incr { key, delta } => {
                match self.engine.incr(&key, delta) {
                    Ok(new_val) => RespWriter::write_integer(out, new_val),
                    Err(err) => RespWriter::write_error(out, &err),
                }
            }
            RespCommand::Ttl(key) => {
                // If key exists, return TTL or -1 if persist
                if self.engine.exists(&key) {
                    RespWriter::write_integer(out, -1);
                } else {
                    RespWriter::write_integer(out, -2);
                }
            }
            RespCommand::Pttl(key) => {
                if self.engine.exists(&key) {
                    RespWriter::write_integer(out, -1);
                } else {
                    RespWriter::write_integer(out, -2);
                }
            }
            RespCommand::Expire { key, .. } => {
                if self.engine.exists(&key) {
                    RespWriter::write_integer(out, 1);
                } else {
                    RespWriter::write_integer(out, 0);
                }
            }
            RespCommand::DbSize => {
                RespWriter::write_integer(out, self.engine.dbsize() as i64);
            }
            RespCommand::FlushDb => {
                self.engine.flushdb();
                RespWriter::write_ok(out);
            }
            RespCommand::Command => {
                // Standard empty array response for COMMAND DOCS / COMMAND
                RespWriter::write_array_header(out, 0);
            }
            RespCommand::Info(_section) => {
                let stats = self.engine.stats();
                let info = format!(
                    "# Server\r\n\
                    rustforge_version:0.1.0\r\n\
                    arch_bits:64\r\n\
                    os:Linux\r\n\
                    io_backend:io_uring\r\n\
                    \r\n\
                    # Memory & Tiering\r\n\
                    used_memory:{}\r\n\
                    hot_keys:{}\r\n\
                    cold_keys:{}\r\n\
                    disk_bytes:{}\r\n\
                    tiering_evictions:{}\r\n\
                    tiering_promotions:{}\r\n\
                    \r\n\
                    # Stats\r\n\
                    total_commands_processed:{}\r\n\
                    keyspace_hits:{}\r\n\
                    keyspace_misses:{}\r\n",
                    stats.memory_bytes,
                    stats.hot_keys,
                    stats.cold_keys,
                    stats.disk_bytes,
                    stats.evictions,
                    stats.promotions,
                    stats.hits + stats.misses,
                    stats.hits,
                    stats.misses,
                );
                RespWriter::write_bulk_string(out, info.as_bytes());
            }
            RespCommand::Quit => {
                RespWriter::write_ok(out);
            }
            RespCommand::Tier(args) => {
                if args.is_empty() {
                    RespWriter::write_error(out, "ERR syntax error for TIER command");
                    return;
                }
                let sub = std::str::from_utf8(&args[0]).unwrap_or("").to_ascii_uppercase();
                match sub.as_str() {
                    "STATUS" => {
                        let stats = self.engine.stats();
                        let status_str = format!(
                            "hot_keys:{} cold_keys:{} evictions:{} promotions:{} disk_bytes:{}",
                            stats.hot_keys, stats.cold_keys, stats.evictions, stats.promotions, stats.disk_bytes
                        );
                        RespWriter::write_bulk_string(out, status_str.as_bytes());
                    }
                    "EVICT" => {
                        let count = if args.len() > 1 {
                            std::str::from_utf8(&args[1]).unwrap_or("100").parse::<usize>().unwrap_or(100)
                        } else {
                            100
                        };
                        let evicted = self.engine.evict_cold(count);
                        RespWriter::write_integer(out, evicted as i64);
                    }
                    _ => {
                        RespWriter::write_error(out, "ERR unknown TIER subcommand");
                    }
                }
            }
            RespCommand::Config(_args) => {
                RespWriter::write_array_header(out, 2);
                RespWriter::write_bulk_string(out, b"save");
                RespWriter::write_bulk_string(out, b"");
            }
            RespCommand::Client | RespCommand::Auth => {
                RespWriter::write_ok(out);
            }
            RespCommand::Cluster(args) => {
                if args.is_empty() {
                    RespWriter::write_error(out, "ERR CLUSTER requires subcommand");
                    return;
                }
                let sub = std::str::from_utf8(&args[0]).unwrap_or("").to_ascii_uppercase();
                match sub.as_str() {
                    "INFO" => {
                        let info = "cluster_state:ok\r\ncluster_slots_assigned:16384\r\ncluster_slots_ok:16384\r\ncluster_known_nodes:1\r\ncluster_size:1\r\n";
                        RespWriter::write_bulk_string(out, info.as_bytes());
                    }
                    "NODES" => {
                        let nodes = "07c37dfeb235213a872192d90877d0cf55635b91 127.0.0.1:6379@16379 myself,master - 0 0 1 connected 0-16383\r\n";
                        RespWriter::write_bulk_string(out, nodes.as_bytes());
                    }
                    "SLOTS" => {
                        // Slot range 0-16383 master 127.0.0.1:6379
                        RespWriter::write_array_header(out, 1);
                        RespWriter::write_array_header(out, 3);
                        RespWriter::write_integer(out, 0);
                        RespWriter::write_integer(out, 16383);
                        RespWriter::write_array_header(out, 2);
                        RespWriter::write_bulk_string(out, b"127.0.0.1");
                        RespWriter::write_integer(out, 6379);
                    }
                    "KEYSLOT" => {
                        if args.len() < 2 {
                            RespWriter::write_error(out, "ERR CLUSTER KEYSLOT requires a key");
                        } else {
                            let slot = crate::cluster::slot::key_slot(&args[1]);
                            RespWriter::write_integer(out, slot as i64);
                        }
                    }
                    _ => {
                        RespWriter::write_error(out, "ERR unsupported CLUSTER command");
                    }
                }
            }
            RespCommand::Unknown(cmd_name) => {
                let name = String::from_utf8_lossy(&cmd_name);
                RespWriter::write_error(out, &format!("ERR unknown command '{}'", name));
            }
        }
    }
}
