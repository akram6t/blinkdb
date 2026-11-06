# BlinkDB Client Integration & Developer Guide

BlinkDB exposes two high-performance network interfaces:
1. **Port 6379 (RESP3 / RESP2 Compatibility)**: Fully compliant with standard Redis client libraries. Works out of the box with any Redis-compatible client without code changes.
2. **Port 7379 (BlinkDB Native Binary Protocol - RFBP)**: Length-prefixed 20-byte packed header binary protocol designed for zero-copy deserialization and microsecond latency in performance-critical microservices.

---

## Table of Contents
1. [Supported Redis Commands](#1-supported-redis-commands)
2. [Python Integration (`redis-py`)](#2-python-integration-redis-py)
3. [Node.js / TypeScript Integration (`ioredis`)](#3-nodejs--typescript-integration-ioredis)
4. [Go Integration (`go-redis / redis/v9`)](#4-go-integration-go-redis--redisv9)
5. [Rust Integration (`fred` and `redis-rs`)](#5-rust-integration-fred-and-redis-rs)
6. [Command Line Usage (`redis-cli`)](#6-command-line-usage-redis-cli)
7. [Native Binary Protocol (Port 7379) Implementation](#7-native-binary-protocol-port-7379-implementation)
8. [Tiering Control Commands (`TIER`)](#8-tiering-control-commands-tier)

---

## 1. Supported Redis Commands

| Command | Syntax | Description |
| :--- | :--- | :--- |
| **PING** | `PING [message]` | Tests connection health; replies with `+PONG` or echoed message. |
| **ECHO** | `ECHO message` | Echoes the given string. |
| **SET** | `SET key value [EX s] [PX ms] [NX\|XX]` | Sets string value with optional TTL expiration and existence conditionals. |
| **GET** | `GET key` | Retrieves the value of a key; returns nil if not present. |
| **DEL** | `DEL key [key ...]` | Removes specified keys; returns count of deleted keys. |
| **EXISTS** | `EXISTS key [key ...]` | Tests existence of keys; returns number of existing keys. |
| **INCR** | `INCR key` | Atomically increments integer value by 1. |
| **INCRBY** | `INCRBY key delta` | Atomically increments integer value by specified delta. |
| **DECR** | `DECR key` | Atomically decrements integer value by 1. |
| **DECRBY** | `DECRBY key delta` | Atomically decrements integer value by specified delta. |
| **MSET** | `MSET key value [key value ...]` | Atomically sets multiple key-value pairs. |
| **MGET** | `MGET key [key ...]` | Atomically retrieves multiple keys in a single round-trip. |
| **TTL** | `TTL key` | Returns remaining time-to-live in seconds (-1 if persist, -2 if missing). |
| **PTTL** | `PTTL key` | Returns remaining time-to-live in milliseconds. |
| **EXPIRE** | `EXPIRE key seconds` | Sets an expiration timeout on a key. |
| **DBSIZE** | `DBSIZE` | Returns total number of active keys in database. |
| **FLUSHDB** | `FLUSHDB` | Clears all keys from both hot in-memory and cold disk tiers. |
| **FLUSHALL**| `FLUSHALL` | Clears all databases and resets state. |
| **INFO** | `INFO [section]` | Returns server runtime statistics, memory tiering, and shard metrics. |
| **HELLO** | `HELLO [proto]` | Handshake protocol for RESP2 and RESP3 protocol negotiation. |
| **CLUSTER**| `CLUSTER <INFO\|NODES\|SLOTS\|KEYSLOT>`| Redis Cluster discovery and CRC16 slot mapping commands. |
| **TIER** | `TIER <STATUS\|EVICT count>` | Native command to inspect and control tiered storage eviction. |

---

## 2. Python Integration (`redis-py`)

Install `redis`:
```bash
pip install redis
```

### Basic CRUD & Pipelining:
```python
import redis

# Connect to BlinkDB on Port 6379
client = redis.Redis(
    host='127.0.0.1',
    port=6379,
    decode_responses=True,
    max_connections=50
)

# 1. Health check
assert client.ping() is True
print("Connected to BlinkDB!")

# 2. String operations
client.set("user:1001", "Alice", ex=3600)  # TTL 1 hour
val = client.get("user:1001")
print(f"Retrieved user: {val}")

# 3. Atomic counters
client.set("metrics:page_views", 100)
client.incr("metrics:page_views")
client.incrby("metrics:page_views", 25)
print(f"Page views: {client.get('metrics:page_views')}")  # Output: 126

# 4. Multi-key operations
client.mset({"key1": "val1", "key2": "val2"})
print(client.mget(["key1", "key2"]))

# 5. High-Throughput Pipelining (P=32 batching)
pipe = client.pipeline()
for i in range(1000):
    pipe.set(f"batch:{i}", f"data_{i}")
responses = pipe.execute()
print(f"Pipelined {len(responses)} writes in a single network batch.")
```

---

## 3. Node.js / TypeScript Integration (`ioredis`)

Install `ioredis`:
```bash
npm install ioredis
```

### TypeScript Example:
```typescript
import Redis from "ioredis";

const redis = new Redis({
  host: "127.0.0.1",
  port: 6379,
  enableAutoPipelining: true, // Automatically amortizes round trips
  maxRetriesPerRequest: 3,
});

async function main() {
  // Test connection
  const pong = await redis.ping();
  console.log("BlinkDB Ping Response:", pong);

  // Set with conditional NX (Not Exists)
  await redis.set("session:token:abc", "user_42", "EX", 1800);

  const session = await redis.get("session:token:abc");
  console.log("Active Session:", session);

  // Pipelining
  const pipeline = redis.pipeline();
  for (let i = 0; i < 500; i++) {
    pipeline.set(`session:${i}`, `user_payload_${i}`);
  }
  const results = await pipeline.exec();
  console.log(`Successfully committed ${results?.length} pipelined operations.`);

  await redis.quit();
}

main().catch(console.error);
```

---

## 4. Go Integration (`go-redis / redis/v9`)

Install `go-redis`:
```bash
go get github.com/redis/go-redis/v9
```

### Go Implementation:
```go
package main

import (
	"context"
	"fmt"
	"time"

	"github.com/redis/go-redis/v9"
)

func main() {
	ctx := context.Background()

	// Initialize connection pool
	rdb := redis.NewClient(&redis.Options{
		Addr:         "127.0.0.1:6379",
		PoolSize:     64,
		MinIdleConns: 16,
	})
	defer rdb.Close()

	// Ping
	if err := rdb.Ping(ctx).Err(); err != nil {
		panic(err)
	}
	fmt.Println("Connected to BlinkDB!")

	// Set & Get
	err := rdb.Set(ctx, "device:sensors:temp", "24.5", 10*time.Minute).Err()
	if err != nil {
		panic(err)
	}

	val, err := rdb.Get(ctx, "device:sensors:temp").Result()
	if err != nil {
		panic(err)
	}
	fmt.Printf("Temperature: %s C\n", val)

	// Pipeline
	pipe := rdb.Pipeline()
	for i := 0; i < 1000; i++ {
		pipe.Set(ctx, fmt.Sprintf("stream:%d", i), i*2, 0)
	}
	_, err = pipe.Exec(ctx)
	if err != nil {
		panic(err)
	}
	fmt.Println("Committed 1000 pipelined records.")
}
```

---

## 5. Rust Integration (`fred` and `redis-rs`)

Add to `Cargo.toml`:
```toml
[dependencies]
fred = { version = "9.0", features = ["enable-rustls"] }
tokio = { version = "1.0", features = ["full"] }
```

### High-Performance Async Rust Example:
```rust
use fred::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_url("redis://127.0.0.1:6379")?;
    let client = Builder::from_config(config)
        .with_connection_config(|config| {
            config.max_command_attempts = 3;
        })
        .build()?;

    client.connect();
    client.wait_for_connect().await?;

    // Basic Ping
    let pong: String = client.ping().await?;
    println!("Ping result: {}", pong);

    // Set and Get
    client.set("rust:agent:id", "Agent-007", None, None, false).await?;
    let val: Option<String> = client.get("rust:agent:id").await?;
    println!("Retrieved value: {:?}", val);

    // Pipeline
    let pipeline = client.pipeline();
    for i in 0..1000 {
        pipeline.set(format!("key:{}", i), format!("val:{}", i), None, None, false).await?;
    }
    pipeline.all().await?;
    println!("Completed 1,000 async pipelined operations.");

    client.quit().await?;
    Ok(())
}
```

---

## 6. Command Line Usage (`redis-cli`)

You can interact directly with BlinkDB via standard `redis-cli`:

```bash
# Connect to default port
redis-cli -h 127.0.0.1 -p 6379

# Interactive session
127.0.0.1:6379> PING
PONG
127.0.0.1:6379> SET cluster:region "us-east-1"
OK
127.0.0.1:6379> GET cluster:region
"us-east-1"
127.0.0.1:6379> INCR counter:active_connections
(integer) 1
127.0.0.1:6379> INCR counter:active_connections
(integer) 2
127.0.0.1:6379> DBSIZE
(integer) 2
127.0.0.1:6379> INFO
# Server
blinkdb_version:0.1.0
...
```

---

## 7. Native Binary Protocol (Port 7379) Implementation

For maximum throughput without text parsing overhead, clients can connect directly to **Port 7379**.

### Header Structure (20-byte packed binary):
```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          Magic ('RF')         |    Version    |    Opcode     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|             Flags             |           Request ID          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          TTL Seconds                          |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|          Key Length           |          Value Length         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                      Payload (Key + Value)                    |
+---------------------------------------------------------------+
```

### Python Binary Client (`rf_binary.py`):
```python
import socket
import struct

RF_MAGIC = b'RF'
OP_GET = 0x01
OP_SET = 0x02
OP_DEL = 0x03
OP_PING = 0x06

STATUS_OK = 0x00

def encode_request(opcode: int, req_id: int, key: bytes = b"", value: bytes = b"", ttl: int = 0) -> bytes:
    # Layout: 2s (magic) + B (ver) + B (opcode) + H (flags) + I (req_id) + I (ttl) + H (key_len) + I (val_len)
    header = struct.pack("<2sBBHIIHI", RF_MAGIC, 1, opcode, 0, req_id, ttl, len(key), len(value))
    return header + key + value

def decode_response(data: bytes):
    magic, status, req_id, flags, val_len = struct.unpack("<2sBIHI", data[:13])
    val = data[13:13 + val_len]
    return status, req_id, val

# Connect to Port 7379
s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.connect(('127.0.0.1', 7379))

# 1. Binary PING
s.sendall(encode_request(OP_PING, req_id=1))
status, req_id, val = decode_response(s.recv(1024))
print(f"Binary PING Status: {status}, Response: {val.decode()}")

# 2. Binary SET
s.sendall(encode_request(OP_SET, req_id=2, key=b"sensor_1", value=b"99.87"))
status, req_id, _ = decode_response(s.recv(1024))
print(f"Binary SET Status: {status}")

# 3. Binary GET
s.sendall(encode_request(OP_GET, req_id=3, key=b"sensor_1"))
status, req_id, val = decode_response(s.recv(1024))
print(f"Binary GET Result: {val.decode()}")
```

---

## 8. Tiering Control Commands (`TIER`)

BlinkDB automatically offloads cold data to the disk LSM-Tree tier when memory pressure exceeds `--max-memory-mb`. Operators can also manually trigger and inspect tiering:

### Query Tiering Status:
```bash
redis-cli -p 6379 TIER STATUS
```
Returns:
```
hot_keys:100000 cold_keys:25000 evictions:25000 promotions:1420 disk_bytes:2621440
```

### Manually Force Cold Eviction:
```bash
# Evicts 1,000 cold keys from RAM to NVMe disk SSTables
redis-cli -p 6379 TIER EVICT 1000
```
Returns:
```
(integer) 1000
```
When an evicted key is accessed via `GET`, BlinkDB automatically reads the disk block via direct I/O, verifies the checksum, and promotes the value back into the hot memory table.
