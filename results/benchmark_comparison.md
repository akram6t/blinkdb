# 3-Way Database Benchmark Report: RustForge vs DragonflyDB vs Redis
### Strict Docker Environment: 2 CPU Cores, 4 GB RAM

- **Test Date:** November 2026
- **Linux Kernel:** Linux 6.17.0-41-generic (x86_64)
- **Tool:** Official `redis-benchmark` (Redis 7.4.12)
- **Network Mode:** `--network host` (Bare-metal loopback performance, zero bridge overhead)
- **Security Opt:** `--security-opt seccomp=unconfined` (Full `io_uring` support enabled)

---

## 1. Engine Resource & Architecture Overview

| Engine | Implementation | Threads / Model | Resource Limit | Initial RSS | Post-Bench RSS |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **RustForge** | io_uring / Thread-Per-Core (Rust) | 2 cores pinned | 2 CPU, 4 GB RAM | 8.84MiB / 4GiB | 13.84MiB / 4GiB |
| **DragonflyDB** | io_uring Proactor / Fibers (C++) | 2 cores pinned | 2 CPU, 4 GB RAM | 16.34MiB / 4GiB | 20.51MiB / 4GiB |
| **Redis 7** | epoll Single-Threaded Event Loop (C) | 1 cores pinned | 2 CPU, 4 GB RAM | 4.824MiB / 4GiB | 5.934MiB / 4GiB |

---

## 2. Suite 1: Standard Latency & Throughput (c=50 clients, n=200,000 requests, P=1)

| Command | RustForge (RPS) | DragonflyDB (RPS) | Redis 7 (RPS) | RustForge vs Redis | RustForge vs Dragonfly |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **PING_INLINE** | 185,185 | 173,160 | 173,010 | **+7.0%** | **+6.9%** |
| **PING_MBULK** | 186,046 | 191,205 | 179,695 | **+3.5%** | -2.7% |
| **SET** | 183,655 | 192,123 | 192,308 | -4.5% | -4.4% |
| **GET** | 183,486 | 195,122 | 191,571 | -4.2% | -6.0% |
| **INCR** | 186,741 | 196,271 | 193,611 | -3.5% | -4.9% |


### Latency Distribution (Suite 1: P=1, c=50, n=200,000 requests)

| Engine | Command | Avg (ms) | p50 (ms) | p95 (ms) | p99 (ms) | Max (ms) |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: |
| **RustForge** | SET | 0.140 | 0.143 | 0.151 | 0.167 | 0.527 |
| **RustForge** | GET | 0.141 | 0.143 | 0.151 | 0.159 | 7.935 |
| **RustForge** | INCR | 0.140 | 0.143 | 0.151 | 0.159 | 2.999 |
| **DragonflyDB** | SET | 0.138 | 0.143 | 0.151 | 0.159 | 1.031 |
| **DragonflyDB** | GET | 0.136 | 0.135 | 0.151 | 0.159 | 0.959 |
| **DragonflyDB** | INCR | 0.136 | 0.135 | 0.151 | 0.159 | 1.151 |
| **Redis 7** | SET | 0.138 | 0.135 | 0.151 | 0.247 | 0.919 |
| **Redis 7** | GET | 0.139 | 0.135 | 0.151 | 0.239 | 1.239 |
| **Redis 7** | INCR | 0.135 | 0.135 | 0.151 | 0.175 | 0.639 |

---

## 3. Suite 2: High Concurrency (c=100 clients, n=200,000 requests, P=1)

| Command | RustForge (RPS) | DragonflyDB (RPS) | Redis 7 (RPS) | RustForge vs Redis | RustForge vs Dragonfly |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **SET** | 177,462 | 186,567 | 176,367 | **+0.6%** | -4.9% |
| **GET** | 180,995 | 189,753 | 187,441 | -3.4% | -4.6% |

---

## 4. Suite 3: Pipelined High-Throughput (P=16, c=50 clients, n=500,000 requests)

| Command | RustForge (RPS) | DragonflyDB (RPS) | Redis 7 (RPS) | RustForge vs Redis | RustForge vs Dragonfly |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **SET** | 2,016,129 | 1,086,956 | 1,240,695 | **+62.5%** | **+85.5%** |
| **GET** | 2,032,520 | 1,329,787 | 1,436,782 | **+41.5%** | **+52.8%** |

---

## 5. Suite 4: Medium Payload (256 Bytes, c=50 clients, n=200,000 requests, P=1)

| Command | RustForge (RPS) | DragonflyDB (RPS) | Redis 7 (RPS) | RustForge vs Redis | RustForge vs Dragonfly |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **SET** | 174,672 | 173,611 | 186,567 | -6.4% | **+0.6%** |
| **GET** | 182,482 | 186,220 | 192,308 | -5.1% | -2.0% |

