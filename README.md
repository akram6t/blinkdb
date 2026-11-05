# RustForge ⚡

**A Linux-Native, Thread-Per-Core, Tiered In-Memory Database in Rust**

Built from scratch in Rust, optimized exclusively for Linux kernels (6.0+), RustForge is designed for extreme throughput and ultra-low latency, surpassing DragonflyDB and Redis on multi-core systems.

---

## Key Highlights

- **Shared-Nothing Thread-Per-Core (TPC) Engine**: Powered by ByteDance's `monoio` `io_uring` runtime. Each CPU core runs an independent event loop pinned to physical hardware threads (`core_affinity`), completely eliminating cross-core lock contention.
- **Kernel-Level Connection Distribution (`SO_REUSEPORT`)**: Every worker thread maintains independent TCP listeners on both ports without accept mutex bottlenecks.
- **Hot In-Memory Storage**:
  - `scc::HashMap`: Lock-free dynamic sharding for write-heavy workloads.
  - DragonflyDB-inspired `DashTable`: Segment-by-segment extendible hashing with 1-byte control tags and stash buckets to eliminate memory doubling and resizing latency spikes.
- **Cold Disk Tiering (LSM-Tree)**:
  - Write-Ahead Log (WAL) with sequential appends and CRC32 verification.
  - Sorted MemTable flush pipeline.
  - Block-based SSTables with in-memory double-hashed Bloom filters (<1% false-positive rate) and sparse index blocks.
  - Transparent promotion: Cold values on disk are dereferenced directly via 16-byte `DiskPointer` and promoted back to RAM on demand.
  - Automated sampled LRU/LFU eviction when memory limits are exceeded.
- **Dual-Stack Wire Protocol**:
  - **Port 6379 (RESP3 Compatibility)**: Fully compatible with standard Redis clients (`redis-cli`, `redis-benchmark`, `memtier_benchmark`, `redis-py`).
  - **Port 7379 (RustForge Native Binary Protocol - RFBP)**: Zero-copy, length-prefixed, 20-byte packed header protocol with multiplexing correlation IDs and native TTL/flag packing.
- **Distributed Auto-Sharding & Consensus**:
  - **Raft Consensus Engine**: Built-in Raft state machine managing cluster topology, slot boundaries, and membership.
  - **Placement Driver (PD)** (TiKV inspired): Monitored heartbeat collection, automatic region splits for hot/large shards, and automated shard rebalancing across joining/leaving nodes.
- **Global Memory Allocator**: Uses `mimalloc` to avoid memory fragmentation under high-frequency allocations.

---

## Performance Benchmarks

Conducted on a 16-core Linux 6.17 AMD/Intel system using `rf-bench`:

| Metric | Port 6379 (RESP3) | Port 7379 (Native Binary RFBP) |
|---|---|---|
| **Peak Throughput** | **2,362,469 req/sec** | **1,412,650 req/sec** |
| **Median Latency (p50)** | **0.003 ms (3 µs)** | **0.004 ms (4 µs)** |
| **90th Percentile (p90)** | **0.008 ms (8 µs)** | **0.006 ms (6 µs)** |
| **99th Percentile (p99)** | **0.049 ms (49 µs)** | **0.010 ms (10 µs)** |
| **99.9th Percentile (p99.9)** | **0.169 ms (169 µs)** | **0.044 ms (44 µs)** |
| **Max Latency** | **0.572 ms** | **1.055 ms** |
| **Network Bandwidth** | **174.61 MB/sec** | **110.47 MB/sec** |

---

## Architectural Overview

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
          │  │ (scc / DashTable)  │  │               │
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

---

## Build & Quickstart

### Prerequisites
- Modern Linux kernel (6.0+ recommended) with `io_uring` enabled (`/proc/sys/kernel/io_uring_disabled` = `0`).
- Rust 1.80+ (`cargo`, `rustc`).

### Compilation
```bash
# Build optimized release binaries
CARGO_TARGET_DIR=/tmp/cargo_target cargo build --release
cp /tmp/cargo_target/release/rustforge-server bin/
cp /tmp/cargo_target/release/rf-bench bin/
```

### Running the Server
```bash
# Start standalone server with 16 pinned threads on default ports
./bin/rustforge-server --resp-port 6379 --binary-port 7379 --max-memory-mb 1024 --data-dir ./data

# Start in Cluster mode
./bin/rustforge-server --resp-port 6379 --binary-port 7379 --cluster --node-id 1
```

### Running the Benchmarking Tool
```bash
# Benchmark RESP3 protocol with 128 clients, pipeline depth 32, 1,000,000 requests
./bin/rf-bench --protocol resp -c 128 -t 16 -n 1000000 -P 32

# Benchmark Native Binary Protocol (Port 7379)
./bin/rf-bench --protocol binary -c 100 -t 8 -n 500000 -P 32
```

---

## Project Structure

```
rustforge/
├── bin/
│   ├── rustforge-server       # Production release server binary
│   └── rf-bench               # High-performance benchmark client
├── src/
│   ├── main.rs                # Server daemon entrypoint & CLI argument parser
│   ├── lib.rs                 # Library root & global mimalloc setup
│   ├── config.rs              # Configuration structures
│   ├── core/
│   │   ├── affinity.rs        # CPU core pinning & OS hardware topology
│   │   ├── io_engine.rs       # Trait abstraction & monoio io_uring engine
│   │   ├── shard.rs           # Thread-per-core event loop with SO_REUSEPORT
│   │   └── channel.rs         # Lock-free inter-shard message channels
│   ├── engine/
│   │   ├── mod.rs             # StorageEngine trait and ValueLocation
│   │   ├── hot/
│   │   │   ├── scc_engine.rs  # Lock-free dynamic sharded hash map (scc)
│   │   │   └── dash_table.rs  # Dragonfly DashTable-inspired extendible segment table
│   │   ├── cold/
│   │   │   ├── wal.rs         # Write-Ahead Log with sequential appends & CRC32
│   │   │   ├── memtable.rs    # Sorted BTreeMap write buffer
│   │   │   ├── bloom.rs       # Murmur/CRC double-hash Bloom filter
│   │   │   ├── sstable.rs     # Page-aligned SSTables with block index & footer
│   │   │   └── lsm.rs         # LSM-Tree engine & multi-version compactor
│   │   └── tiering/
│   │       └── coordinator.rs # Sampled LRU/LFU eviction & transparent promotion
│   ├── protocol/
│   │   ├── resp/
│   │   │   ├── parser.rs      # Zero-copy RESP3/RESP2 parser
│   │   │   ├── writer.rs      # Zero-copy RESP3 response formatter
│   │   │   └── handler.rs     # Command dispatcher (PING, SET, GET, INCR, TIER, etc.)
│   │   └── binary/
│   │       ├── frame.rs       # Zero-copy 20-byte packed header frame layout
│   │       └── handler.rs     # Binary command dispatcher
│   └── cluster/
│       ├── slot.rs            # Redis-compatible CRC16 hash slot algorithm
│       ├── raft/
│       │   ├── message.rs     # Raft RPC messages (RequestVote, AppendEntries)
│       │   └── state_machine.rs # Raft consensus engine & metadata state machine
│       ├── pd/
│       │   └── driver.rs      # Placement Driver (heartbeats, auto-split, auto-rebalance)
│       └── router.rs          # Cluster routing & -MOVED redirection
└── tests/
    ├── integration_tests.rs   # Comprehensive Rust integration tests
    ├── test_resp.py           # Automated RESP3 functional test suite
    ├── test_binary.py         # Automated Native Binary Protocol test suite
    └── test_cluster_e2e.py    # 2-node cluster integration & redirection test
```

---

## License
MIT / Apache-2.0
