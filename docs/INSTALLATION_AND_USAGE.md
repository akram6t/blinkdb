# BlinkDB: Installation, Configuration & Production Operations Guide

This guide provides end-to-end instructions for installing, configuring, deploying, and maintaining **BlinkDB** in production environments.

---

## Table of Contents
1. [System Prerequisites & Kernel Requirements](#1-system-prerequisites--kernel-requirements)
2. [Installation Methods](#2-installation-methods)
   - [Method A: Docker Container (Recommended for Quickstart)](#method-a-docker-container-recommended-for-quickstart)
   - [Method B: Building from Source (Cargo)](#method-b-building-from-source-cargo)
   - [Method C: Pre-Compiled Binary](#method-c-pre-compiled-binary)
3. [Configuration & CLI Parameter Reference](#3-configuration--cli-parameter-reference)
4. [OS Kernel & Host Performance Tuning](#4-os-kernel--host-performance-tuning)
5. [Systemd Production Service Setup](#5-systemd-production-service-setup)
6. [Cluster Deployment (Raft & Placement Driver)](#6-cluster-deployment-raft--placement-driver)
7. [Health Checks, Monitoring & Telemetry](#7-health-checks-monitoring--telemetry)
8. [Troubleshooting & FAQ](#8-troubleshooting--faq)

---

## 1. System Prerequisites & Kernel Requirements

BlinkDB is architected specifically for high-throughput Linux environments utilizing modern kernel primitives:

| Component | Minimum Requirement | Recommended Production Target |
| :--- | :--- | :--- |
| **Operating System** | Linux Kernel 5.10+ | Linux Kernel 6.0+ (Ubuntu 22.04 LTS / 24.04 LTS, Debian 12) |
| **I/O Engine** | `io_uring` enabled | Kernel `io_uring` support without restrictive seccomp |
| **CPU Architecture** | x86_64 or aarch64 (ARM64) | 4+ Physical Cores with dedicated cpuset pinning |
| **Memory** | 1 GB RAM | 4 GB to 128 GB DDR4 / DDR5 RAM |
| **Disk** | Any persistent filesystem | NVMe PCIe Gen4/Gen5 SSD (ext4 or XFS with direct I/O) |

### Verifying `io_uring` Support
Verify that `io_uring` is active on your host:
```bash
# Check kernel version
uname -r

# Verify io_uring is not globally disabled (should return 0)
cat /proc/sys/kernel/io_uring_disabled
```
If this file returns `1` or `2`, enable `io_uring` via sysctl:
```bash
sudo sysctl -w kernel.io_uring_disabled=0
```

---

## 2. Installation Methods

### Method A: Docker Container (Recommended for Quickstart)

The official Docker image packages the release binary with `mimalloc` and minimal Debian/Ubuntu runtime dependencies.

#### 1. Build the Docker Image Locally:
```bash
git clone https://github.com/akram6t/blinkdb.git
cd blinkdb
docker build -t akram6t/blinkdb:latest -f Dockerfile .
```

#### 2. Run Standalone BlinkDB Container:
```bash
docker run -d \
  --name blinkdb \
  --restart always \
  --network host \
  --security-opt seccomp=unconfined \
  --cpuset-cpus "0,2,4,6" \
  --memory 8g \
  -v /var/lib/blinkdb/data:/data \
  akram6t/blinkdb:latest \
  --threads 4 \
  --resp-port 6379 \
  --binary-port 7379 \
  --max-memory-mb 7168 \
  --data-dir /data
```

> [!TIP]
> **Container Parameters**:
> - `--network host`: Eliminates Docker bridge NAT packet-inspection overhead, matching bare-metal networking speed.
> - `--security-opt seccomp=unconfined`: Ensures that the `io_uring_setup` and `io_uring_enter` syscalls are not blocked by standard container profiles.
> - `--cpuset-cpus "0,2,4,6"`: Pins the database container strictly to physical performance cores.

---

### Method B: Building from Source (Cargo)

#### 1. Install Build Dependencies:
```bash
# Ubuntu / Debian
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev git clang curl

# Install Rust toolchain (1.80+)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env
```

#### 2. Clone and Compile Optimized Release Binary:
```bash
git clone https://github.com/akram6t/blinkdb.git
cd blinkdb

# Build with maximum CPU optimizations
RUSTFLAGS="-C target-cpu=native" cargo build --release

# Binaries will be generated at target/release/
./target/release/blinkdb-server --version
```

#### 3. Install to System Path:
```bash
sudo cp target/release/blinkdb-server /usr/local/bin/
sudo cp target/release/rf-bench /usr/local/bin/
```

---

### Method C: Pre-Compiled Binary

1. Download the release tarball for your architecture:
```bash
wget https://github.com/akram6t/blinkdb/releases/latest/download/blinkdb-linux-x86_64.tar.gz
tar -zxvf blinkdb-linux-x86_64.tar.gz
sudo mv blinkdb-server /usr/local/bin/
```
2. Verify executable permissions:
```bash
chmod +x /usr/local/bin/blinkdb-server
blinkdb-server --help
```

---

## 3. Configuration & CLI Parameter Reference

BlinkDB is configured through command-line parameters or environment overrides.

```
Usage: blinkdb-server [OPTIONS]

Options:
  -p, --resp-port <PORT>        Port for RESP3/RESP2 Redis compatibility [default: 6379]
  -b, --binary-port <PORT>      Port for Native Binary Protocol (RFBP) [default: 7379]
  -t, --threads <COUNT>         Number of worker shard threads [default: 0 = auto-detect all physical cores]
  -d, --data-dir <PATH>         Filesystem directory for LSM-tree cold storage & WAL [default: ./data]
  -m, --max-memory-mb <MB>      Hot in-memory RAM quota before tiering/eviction triggers [default: 1024]
      --cluster                 Enable multi-node distributed clustering mode [default: false]
      --node-id <ID>            Unique node identifier in the cluster [default: 1]
  -h, --help                    Print help
  -V, --version                 Print version
```

### Sizing and Memory Recommendations:
* **`--threads`**: Set this to the number of physical CPU Performance cores. Avoid setting threads higher than physical cores to eliminate SMT thread contention.
* **`--max-memory-mb`**: Set this to **70%–80%** of total container RAM limit. The remaining 20%–30% accommodates registered `io_uring` fixed ring buffers (~8 MB/shard) and memory allocator fragmentation.
* **`--data-dir`**: Mount on high-IOPS NVMe SSD storage for low-latency cold SSTable lookups and WAL flushing.

---

## 4. OS Kernel & Host Performance Tuning

For production environments targeting > 1,000,000 ops/sec, apply these kernel parameters:

### 1. Network & Socket Tuning (`/etc/sysctl.d/99-blinkdb.conf`):
```ini
# Maximum socket receive/send buffers
net.core.rmem_max = 16777216
net.core.wmem_max = 16777216
net.core.rmem_default = 1048576
net.core.wmem_default = 1048576

# Maximum connection backlog
net.core.somaxconn = 65535
net.ipv4.tcp_max_syn_backlog = 65535

# Fast TCP reuse
net.ipv4.tcp_tw_reuse = 1
net.ipv4.tcp_fin_timeout = 15

# File descriptors and memory map limits
fs.file-max = 2097152
vm.max_map_count = 262144

# Ensure io_uring is active
kernel.io_uring_disabled = 0
```
Apply the settings:
```bash
sudo sysctl --system
```

### 2. File Descriptor Limits (`/etc/security/limits.d/99-blinkdb.conf`):
```ini
* soft nofile 1048576
* hard nofile 1048576
* soft memlock unlimited
* hard memlock unlimited
```

### 3. CPU Governor:
Set CPU scaling governor to `performance` to eliminate clock-frequency throttling:
```bash
sudo apt install -y cpufrequtils
sudo cpufreq-set -r -g performance
```

---

## 5. Systemd Production Service Setup

Create `/etc/systemd/system/blinkdb.service`:

```ini
[Unit]
Description=BlinkDB In-Memory Tiered Database
After=network.target network-online.target
Wants=network-online.target

[Service]
Type=simple
User=blinkdb
Group=blinkdb
LimitNOFILE=1048576
LimitMEMLOCK=infinity
CPUSchedulingPolicy=other
Nice=-20

# Command Line Execution
ExecStart=/usr/local/bin/blinkdb-server \
    --resp-port 6379 \
    --binary-port 7379 \
    --threads 4 \
    --max-memory-mb 6144 \
    --data-dir /var/lib/blinkdb/data

Restart=always
RestartSec=3s

# Sandboxing & Security
ProtectSystem=full
ProtectHome=true
NoNewPrivileges=true
ReadWritePaths=/var/lib/blinkdb/data

[Install]
WantedBy=multi-user.target
```

Enable and start the service:
```bash
# Create dedicated system user and storage directory
sudo useradd -r -s /bin/false blinkdb
sudo mkdir -p /var/lib/blinkdb/data
sudo chown -R blinkdb:blinkdb /var/lib/blinkdb

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable --now blinkdb
sudo systemctl status blinkdb
```

---

## 6. Cluster Deployment (Raft & Placement Driver)

BlinkDB features integrated distributed clustering based on the Raft consensus algorithm and a Placement Driver (PD) for slot topology management:

### 3-Node Cluster Example:

#### Node 1 (Host: 10.0.0.1):
```bash
blinkdb-server \
  --resp-port 6379 \
  --binary-port 7379 \
  --threads 4 \
  --max-memory-mb 8192 \
  --data-dir /data/rf1 \
  --cluster \
  --node-id 1
```

#### Node 2 (Host: 10.0.0.2):
```bash
blinkdb-server \
  --resp-port 6379 \
  --binary-port 7379 \
  --threads 4 \
  --max-memory-mb 8192 \
  --data-dir /data/rf2 \
  --cluster \
  --node-id 2
```

#### Node 3 (Host: 10.0.0.3):
```bash
blinkdb-server \
  --resp-port 6379 \
  --binary-port 7379 \
  --threads 4 \
  --max-memory-mb 8192 \
  --data-dir /data/rf3 \
  --cluster \
  --node-id 3
```

Standard Redis cluster client libraries (e.g., `redis-py` `RedisCluster`, `ioredis.Cluster`) automatically discover cluster topology via `CLUSTER SLOTS` and execute `-MOVED` slot redirections.

---

## 7. Health Checks, Monitoring & Telemetry

### 1. Basic Health Check:
```bash
redis-cli -p 6379 PING
# Expected response: PONG
```

### 2. Runtime Telemetry (`INFO`):
```bash
redis-cli -p 6379 INFO
```
Sample Output:
```ini
# Server
blinkdb_version:0.1.0
arch_bits:64
os:Linux
io_backend:io_uring

# Memory & Tiering
used_memory:45281920
hot_keys:100000
cold_keys:0
disk_bytes:0
tiering_evictions:0
tiering_promotions:0

# Stats
total_commands_processed:1500000
keyspace_hits:1450000
keyspace_misses:50000
shard_0_conns:24
shard_0_cmds:375000
shard_1_conns:24
shard_1_cmds:375000
shard_2_conns:24
shard_2_cmds:375000
shard_3_conns:24
shard_3_cmds:375000
```

### 3. Tiering Status Inspection:
```bash
# Query storage tiering telemetry
redis-cli -p 6379 TIER STATUS
# Output: "hot_keys:100000 cold_keys:15000 evictions:15000 promotions:342 disk_bytes:1572864"

# Manually trigger cold tier eviction of 500 keys
redis-cli -p 6379 TIER EVICT 500
```

---

## 8. Troubleshooting & FAQ

### Q1: Server fails to bind port with `Address already in use`
* **Cause**: Another Redis or Dragonfly instance is already running on port 6379.
* **Resolution**: Check running processes with `sudo lsof -i :6379` or `sudo ss -tulpn | grep 6379` and terminate or configure BlinkDB to use `--resp-port 6380`.

### Q2: `io_uring_setup failed: Permission denied` or `Operation not permitted`
* **Cause**: Container runtime or Linux seccomp profile is intercepting `io_uring` syscalls.
* **Resolution**: When running in Docker, pass `--security-opt seccomp=unconfined`. In bare-metal, ensure `/proc/sys/kernel/io_uring_disabled` is set to `0`.

### Q3: Why is BlinkDB memory usage ~40 MB for 100k keys while Redis uses 16 MB?
* **Explanation**: Redis is a single-threaded in-memory only database using standard `libc` malloc. BlinkDB is a multi-threaded, thread-per-core engine with:
  1. High-concurrency `scc::HashMap` bucket structures (~15 MB)
  2. Registered `io_uring` fixed ring submission and completion buffers (~8 MB)
  3. Tiered cold storage LSM MemTables and Bloom filters (~12 MB)
  This memory investment enables **2.3M QPS** throughput and NVMe disk overflow without out-of-memory crashes.

### Q4: Tail latency spikes under high connection counts
* **Cause**: Worker threads may be sharing cores with client benchmarks or background OS daemons.
* **Resolution**: Pin server worker threads strictly to physical Performance cores with `taskset -c 0,2,4,6` or Docker `--cpuset-cpus`. Pin client benchmark tools (e.g. `memtier`) to separate physical cores.
