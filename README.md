# RustForge ⚡

<p align="center">
  <b>A Linux-Native, Thread-Per-Core, Tiered In-Memory Database in Rust</b><br>
  <i>Engineered for extreme pipelined throughput, zero-copy I/O, and NVMe-backed storage tiering.</i>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Language-Rust%201.80+-orange.svg" alt="Rust 1.80+">
  <img src="https://img.shields.io/badge/Kernel-Linux%206.0+-blue.svg" alt="Linux Kernel 6.0+">
  <img src="https://img.shields.io/badge/I%2FO-io__uring-brightgreen.svg" alt="io_uring">
  <img src="https://img.shields.io/badge/Protocol-RESP3%20%7C%20RFBP-purple.svg" alt="RESP3 & RFBP">
  <img src="https://img.shields.io/badge/License-MIT%20%2F%20Apache--2.0-blue.svg" alt="License">
</p>

---

## 🚀 What is RustForge?

**RustForge** is a production-grade, distributed in-memory key-value database built from scratch in Rust. It utilizes modern Linux kernel primitives (`io_uring`, `SO_REUSEPORT`, and thread-per-core core affinity) to deliver multi-million queries-per-second (QPS) performance while transparently tiering cold keys to NVMe disk via an embedded LSM-Tree engine.

RustForge serves two wire protocols concurrently:
1. **Port 6379 (RESP3 / RESP2)**: Full compatibility with all standard Redis clients (`redis-py`, `ioredis`, `go-redis`, `redis-rs`, `redis-cli`, `memtier_benchmark`).
2. **Port 7379 (RustForge Native Binary Protocol - RFBP)**: Zero-copy, length-prefixed, 20-byte packed header binary protocol for high-throughput internal microservices.

---

## ⚡ Performance At A Glance

Verified under official Redis benchmarking tools (`redislabs/memtier_benchmark` and `redis-benchmark`) under strict Docker 4 CPU core / 8 GB RAM constraints against DragonflyDB and Redis 7:

| Workload Dimension | RustForge (io_uring) | DragonflyDB (Proactor) | Redis 7 (Default) | RustForge Advantage |
| :--- | :---: | :---: | :---: | :---: |
| **Client Pool Pipelining ($P=4$)** | **843,766 ops/s** | 665,764 ops/s | 353,443 ops/s | **+26.7% vs DF** (+138.7% vs Redis) |
| **Batch Ingestion ($P=16$, GET)** | **1,041,667 ops/s** | 454,545 ops/s | 403,226 ops/s | **+129.2% vs DF (2.29x)** |
| **Active Session Store (Zipfian)** | **313,702 ops/s** | 278,608 ops/s | 114,110 ops/s | **+12.6% vs DF** (+174.9% vs Redis) |
| **Extreme Concurrency ($c=496$)** | **320,879 ops/s** | 267,665 ops/s | 103,219 ops/s | **+19.9% vs DF** (+210.9% vs Redis) |
| **Payload Sweep (4KB GET)** | **89,286 req/s** | 68,493 req/s | 97,403 req/s | **+30.4% vs DF** |

*For complete benchmarks, hardware PMU profiling, and scaling efficiency metrics, read the [Production Benchmark Report](file:///home/akram/Desktop/rustforge/BENCHMARK_REPORT.md).*

---

## 🏗️ Architectural Highlights

```
                           Incoming Client Traffic
                     (Port 6379: RESP3 | Port 7379: RFBP)
                                      │
               ┌──────────────────────┴──────────────────────┐
               │    Linux Kernel 4-Tuple Hash (SO_REUSEPORT) │
               └───────┬──────────────┬──────────────┬───────┘
                       │              │              │
             ┌─────────▼────────┐     │     ┌────────▼─────────┐
             │ CPU Core 0 Shard │    ...    │ CPU Core N Shard │
             │  Pinned Thread   │           │  Pinned Thread   │
             └─────────┬────────┘           └────────┬─────────┘
                       │                             │
          ┌────────────┴─────────────┐               │
          │   monoio (io_uring Ring) │               │
          ├──────────────────────────┤               │
          │ Dual-Stack Connection    │               │
          │ - Port 6379 (RESP3)      │               │
          │ - Port 7379 (RFBP Binary)│               │
          ├──────────────────────────┤               │
          │  Tiered Storage Engine   │               │
          │  ┌────────────────────┐  │               │
          │  │ In-Memory Hot Index│  │               │
          │  │ (scc::HashMap)     │  │               │
          │  └─────────┬──────────┘  │               │
          │            │ (Evict)     │               │
          │            ▼             │               │
          │  ┌────────────────────┐  │               │
          │  │ Cold LSM-Tree      │  │               │
          │  │ - WAL + MemTable   │  │               │
          │  │ - SSTable + Bloom  │  │               │
          │  └────────────────────┘  │               │
          └──────────────────────────┘               │
                       │                             │
                       └──────────────┬──────────────┘
                                      │
                     ┌────────────────▼────────────────┐
                     │  Cluster & Sharding Coordinator │
                     │  - Raft Consensus State Machine │
                     │  - Placement Driver (PD)        │
                     │  - CRC16 Hash Slot Router       │
                     └─────────────────────────────────┘
```

- **Shared-Nothing Thread-Per-Core (TPC)**: Powered by ByteDance's `monoio` `io_uring` runtime. Each physical CPU core executes an isolated event loop pinned to physical hardware threads (`core_affinity`), eliminating cross-core lock bouncing.
- **Kernel-Level Connection Balancing (`SO_REUSEPORT`)**: Independent TCP listeners on each worker shard handle socket distribution at kernel speed.
- **Lock-Free Hot Storage (`scc::HashMap`)**: High-performance concurrency using non-cryptographic `ahash` indexing with zero read-lock contention.
- **Transparent Disk Tiering (LSM-Tree)**:
  - Write-Ahead Log (WAL) with CRC32 integrity verification.
  - Page-aligned SSTables with double-hashed Bloom filters (<1% false-positive rate).
  - Transparent promotion: Cold values are fetched on demand via 16-byte `DiskPointer` references without blocking hot operations.
- **Global Memory Allocator**: Integrated with `mimalloc` to prevent heap fragmentation under sustained allocation load.

---

## ⚡ Quickstart in 30 Seconds

### Running with Docker (Recommended)
```bash
# Pull or build the container
docker build -t rustforge:latest .

# Run on 4 dedicated cores with 8 GB RAM
docker run -d \
  --name rustforge \
  --network host \
  --security-opt seccomp=unconfined \
  --cpuset-cpus "0,2,4,6" \
  --memory 8g \
  -v /tmp/rustforge_data:/data \
  rustforge:latest \
  --threads 4 \
  --resp-port 6379 \
  --max-memory-mb 6144 \
  --data-dir /data
```

### Building From Source
```bash
# Prerequisites: Linux 6.0+, Rust 1.80+
cargo build --release

# Run standalone server
./target/release/rustforge-server \
  --resp-port 6379 \
  --binary-port 7379 \
  --threads 4 \
  --max-memory-mb 4096 \
  --data-dir ./data
```

Verify with `redis-cli`:
```bash
redis-cli -p 6379 PING
# Output: +PONG

redis-cli -p 6379 SET user:101 "John Doe"
# Output: OK

redis-cli -p 6379 GET user:101
# Output: "John Doe"
```

---

## 💻 Client Integration Examples

RustForge works with existing Redis libraries without modifications.

### Python (`redis-py`)
```python
import redis

client = redis.Redis(host="127.0.0.1", port=6379, decode_responses=True)

client.set("session:token", "xyz987", ex=1800)
print("Session:", client.get("session:token"))

# Pipelining
pipe = client.pipeline()
for i in range(100):
    pipe.set(f"key:{i}", f"val:{i}")
pipe.execute()
```

### Node.js / TypeScript (`ioredis`)
```typescript
import Redis from "ioredis";

const redis = new Redis({ host: "127.0.0.1", port: 6379 });

await redis.set("counter", 10);
await redis.incr("counter");
console.log("Counter:", await redis.get("counter")); // 11
```

### Go (`go-redis`)
```go
package main

import (
	"context"
	"fmt"
	"github.com/redis/go-redis/v9"
)

func main() {
	ctx := context.Background()
	rdb := redis.NewClient(&redis.Options{Addr: "127.0.0.1:6379"})

	rdb.Set(ctx, "cluster:status", "operational", 0)
	val, _ := rdb.Get(ctx, "cluster:status").Result()
	fmt.Println("Status:", val)
}
```

*For more client examples in Rust, Go, Java, and Native Binary Protocol (Port 7379), see the [Client Integration Guide](file:///home/akram/Desktop/rustforge/docs/CLIENT_GUIDE.md).*

---

## 🛠️ CLI Parameter Reference

| Parameter | Default | Description |
| :--- | :---: | :--- |
| `-p, --resp-port <PORT>` | `6379` | Port for Redis RESP3 / RESP2 client connections |
| `-b, --binary-port <PORT>` | `7379` | Port for Native Binary Protocol (RFBP) |
| `-t, --threads <COUNT>` | `0` | Worker shards (0 = auto-detect all physical cores) |
| `-d, --data-dir <PATH>` | `./data` | Filesystem path for cold LSM-tree storage and WAL |
| `-m, --max-memory-mb <MB>` | `1024` | Hot RAM threshold before cold tiering / eviction |
| `--cluster` | `false` | Enable distributed Raft clustering mode |
| `--node-id <ID>` | `1` | Unique node ID in the distributed cluster |

---

## 📖 Detailed Documentation

- 📘 [Installation, Configuration & Systemd Operations Guide](file:///home/akram/Desktop/rustforge/docs/INSTALLATION_AND_USAGE.md)
- 📗 [Client Integration Guide (Python, Node.js, Go, Rust, RFBP Binary)](file:///home/akram/Desktop/rustforge/docs/CLIENT_GUIDE.md)
- 📊 [Production 3-Way Benchmark Report & Scaling Analysis](file:///home/akram/Desktop/rustforge/BENCHMARK_REPORT.md)
- 🔬 [Tier 2 Scaling Diagnostics & PMU Profiling Report](file:///home/akram/Desktop/rustforge/docs/diagnostics_tier2_regression.md)

---

## 📜 License

Licensed under either of:
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
