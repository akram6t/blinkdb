#!/usr/bin/env python3
"""
RustForge Dual-Tier Realistic Production Benchmark Suite:
Official Tools: memtier_benchmark (Redis Labs) + redis-benchmark (Redis 7)

Key Real-World Internet Traffic Characteristics:
  - Realistic Read/Write ratios (1:10 web cache, 1:5 active session store)
  - Realistic Key Distributions: Gaussian/Zipfian hot keys (Pareto 80/20 rule)
  - Realistic Random Keyspace: -r 100000 keys
  - Realistic Payloads: 64B, 128B, 512B, 1KB, 4KB
  - Realistic Concurrency: c=40, 50, 100, 200, 500 connections
  - Strict Data & Cache Sanitation: disk wipe, container recreation, FLUSHALL, sync

Engines Tested:
  - RustForge (io_uring Thread-Per-Core)
  - DragonflyDB (io_uring Proactor Fibers)
  - Redis 7 (Default single-threaded)
  - Redis 7 (Tuned io-threads)
"""

import subprocess
import time
import json
import csv
import io
import os
import sys

CLIENT_TASKSET_CPUS = "8,9,10,11,12,13,14,15"
RESULTS_DIR = "/home/akram/Desktop/rustforge/results"

def run_command(cmd, check=True, timeout=60):
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
    if check and res.returncode != 0:
        raise RuntimeError(f"Command failed ({res.returncode}): {' '.join(cmd)}\nSTDERR: {res.stderr}\nSTDOUT: {res.stdout}")
    return res

def clean_data_and_containers():
    # Remove containers
    for name in ["rustforge_bench", "dragonfly_bench", "redis_bench", "redis_tuned_bench", "test_rf", "test_redis"]:
        subprocess.run(["docker", "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    
    # Wipe data directories
    subprocess.run(["rm", "-rf", "/tmp/rustforge_data", "/tmp/dragonfly_data", "/tmp/redis_data"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["mkdir", "-p", "/tmp/rustforge_data"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    
    # Sync host buffers
    subprocess.run(["sync"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)

def flush_db(port=6379):
    try:
        subprocess.run(
            ["docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-cli", "-p", str(port), "FLUSHALL"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5
        )
    except Exception:
        pass

def wait_for_ready(port=6379, max_attempts=30):
    for _ in range(max_attempts):
        try:
            res = subprocess.run(
                ["docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-cli", "-p", str(port), "PING"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=3
            )
            if res.returncode == 0 and "PONG" in res.stdout:
                return True
        except Exception:
            pass
        time.sleep(0.5)
    return False

def get_container_stats(container_name):
    try:
        res = subprocess.run(
            ["docker", "stats", "--no-stream", "--format", "{{.MemUsage}} | {{.CPUPerc}}", container_name],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=5
        )
        if res.returncode == 0:
            return res.stdout.strip()
    except Exception:
        pass
    return "N/A"

def parse_redis_bench_csv(output_text):
    results = {}
    reader = csv.reader(io.StringIO(output_text))
    header = None
    for row in reader:
        if not row:
            continue
        if len(row) >= 8 and row[0].strip().strip('"').lower() == "test":
            header = [col.strip().strip('"').lower() for col in row]
            continue
        if header and len(row) >= 8:
            test_name = row[0].strip().strip('"').upper()
            try:
                results[test_name] = {
                    "rps": float(row[1].strip().strip('"')),
                    "avg_ms": float(row[2].strip().strip('"')),
                    "min_ms": float(row[3].strip().strip('"')),
                    "p50_ms": float(row[4].strip().strip('"')),
                    "p95_ms": float(row[5].strip().strip('"')),
                    "p99_ms": float(row[6].strip().strip('"')),
                    "max_ms": float(row[7].strip().strip('"'))
                }
            except ValueError:
                continue
    return results

def run_redis_benchmark(args, warmup_args=None, timeout=40):
    if warmup_args:
        w_cmd = ["taskset", "-c", CLIENT_TASKSET_CPUS, "docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "-q"] + warmup_args
        try:
            subprocess.run(w_cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
        except Exception:
            pass
        time.sleep(0.2)

    cmd = ["taskset", "-c", CLIENT_TASKSET_CPUS, "docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "--csv", "--precision", "3"] + args
    try:
        res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
        return parse_redis_bench_csv(res.stdout + "\n" + res.stderr)
    except subprocess.TimeoutExpired:
        print(f"     [TIMEOUT] redis-benchmark timed out: {' '.join(args)}", flush=True)
        return {}

def run_memtier(args, json_name="memtier_run.json", timeout=60):
    json_path_host = os.path.join(RESULTS_DIR, json_name)
    json_path_docker = f"/results/{json_name}"
    
    if os.path.exists(json_path_host):
        os.remove(json_path_host)

    cmd = [
        "taskset", "-c", CLIENT_TASKSET_CPUS,
        "docker", "run", "--rm", "--network", "host",
        "-v", f"{RESULTS_DIR}:/results",
        "redislabs/memtier_benchmark:latest",
        "-s", "127.0.0.1", "-p", "6379",
        f"--json-out-file={json_path_docker}"
    ] + args

    try:
        subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        print(f"     [TIMEOUT] memtier_benchmark timed out: {' '.join(args)}", flush=True)
        return None

    if os.path.exists(json_path_host):
        try:
            with open(json_path_host, "r") as f:
                raw = json.load(f)
            stats = raw.get("ALL STATS", {})
            totals = stats.get("Totals", {})
            sets = stats.get("Sets", {})
            gets = stats.get("Gets", {})

            return {
                "total_ops_sec": totals.get("Ops/sec", 0.0),
                "total_avg_lat_ms": totals.get("Average Latency", 0.0),
                "total_p50_ms": totals.get("Percentile Latencies", {}).get("p50.00", 0.0),
                "total_p99_ms": totals.get("Percentile Latencies", {}).get("p99.00", 0.0),
                "total_p999_ms": totals.get("Percentile Latencies", {}).get("p99.90", 0.0),
                "total_kb_sec": totals.get("KB/sec", 0.0),
                "set_ops_sec": sets.get("Ops/sec", 0.0),
                "set_avg_lat_ms": sets.get("Average Latency", 0.0),
                "get_ops_sec": gets.get("Ops/sec", 0.0),
                "get_avg_lat_ms": gets.get("Average Latency", 0.0),
            }
        except Exception as e:
            print(f"     [ERROR] failed to parse memtier json: {e}", flush=True)
            return None
    return None

def get_targets(cores, mem_gb, cpuset):
    mem_str = f"{mem_gb}g"
    db_mem_mb = int((mem_gb - 0.5) * 1024)

    return [
        {
            "id": "rustforge",
            "name": "RustForge",
            "container_name": "rustforge_bench",
            "run_cmd": [
                "docker", "run", "-d",
                "--name", "rustforge_bench",
                "--security-opt", "seccomp=unconfined",
                "--network", "host",
                "--cpuset-cpus", cpuset,
                "--memory", mem_str,
                "rustforge:latest",
                "--threads", str(cores),
                "--resp-port", "6379",
                "--max-memory-mb", str(db_mem_mb),
                "--data-dir", "/tmp/rustforge_data"
            ],
            "arch": f"io_uring Thread-Per-Core (Rust, {cores} Shards, ahash, Zero-Copy)"
        },
        {
            "id": "dragonfly",
            "name": "DragonflyDB",
            "container_name": "dragonfly_bench",
            "run_cmd": [
                "docker", "run", "-d",
                "--name", "dragonfly_bench",
                "--security-opt", "seccomp=unconfined",
                "--network", "host",
                "--cpuset-cpus", cpuset,
                "--memory", mem_str,
                "docker.dragonflydb.io/dragonflydb/dragonfly:latest",
                "--port", "6379",
                f"--proactor_threads={cores}",
                "--cache_mode=true",
                f"--maxmemory={db_mem_mb}mb",
                "--dbfilename="
            ],
            "arch": f"io_uring Proactor Fibers (C++20, {cores} Threads)"
        },
        {
            "id": "redis_default",
            "name": "Redis 7 (Default)",
            "container_name": "redis_bench",
            "run_cmd": [
                "docker", "run", "-d",
                "--name", "redis_bench",
                "--security-opt", "seccomp=unconfined",
                "--network", "host",
                "--cpuset-cpus", "0",
                "--memory", mem_str,
                "redis:7-alpine",
                "redis-server",
                "--port", "6379",
                "--save", "",
                "--appendonly", "no",
                "--maxmemory", f"{db_mem_mb}mb",
                "--protected-mode", "no"
            ],
            "arch": "epoll Single-Threaded (C11, io-threads=1, Core 0)"
        },
        {
            "id": "redis_tuned",
            "name": "Redis 7 (Tuned)",
            "container_name": "redis_tuned_bench",
            "run_cmd": [
                "docker", "run", "-d",
                "--name", "redis_tuned_bench",
                "--security-opt", "seccomp=unconfined",
                "--network", "host",
                "--cpuset-cpus", cpuset,
                "--memory", mem_str,
                "redis:7-alpine",
                "redis-server",
                "--port", "6379",
                "--save", "",
                "--appendonly", "no",
                "--maxmemory", f"{db_mem_mb}mb",
                "--protected-mode", "no",
                "--io-threads", str(cores),
                "--io-threads-do-reads", "yes"
            ],
            "arch": f"epoll + io-threads {cores} (C11, Multi-threaded I/O)"
        }
    ]

def run_realistic_tier_suite(target, tier_name, cores, cfg):
    name = target["name"]
    cname = target["container_name"]
    print(f"\n[{tier_name}] Running Realistic Benchmark Suite for {name} ({target['arch']})...", flush=True)

    clean_data_and_containers()
    run_command(target["run_cmd"])
    if not wait_for_ready():
        print(f"  [ERROR] {name} failed to respond to PING!", flush=True)
        run_command(["docker", "logs", cname], check=False)
        clean_data_and_containers()
        return None

    init_stats = get_container_stats(cname)
    print(f"  -> Ready. Clean Baseline RAM: {init_stats}", flush=True)

    suites = {}

    # =========================================================================
    # PART A: memtier_benchmark Realistic Internet Web Workloads
    # =========================================================================

    # Workload 1: Typical Read-Heavy Internet App (1:10 Write:Read, Gaussian distribution)
    flush_db()
    c_m1 = cfg["m1_conns_per_thread"]
    t_m1 = cfg["m1_threads"]
    n_m1 = cfg["m1_reqs"]
    print(f"  -> memtier Suite 1: Web App Read-Heavy (1:10 Write:Read, Gaussian keys, {t_m1*c_m1} conns, n={n_m1})...", flush=True)
    m1 = run_memtier(
        args=["-t", str(t_m1), "-c", str(c_m1), "-n", str(n_m1), "--ratio=1:10", "--key-pattern=G:G", "--data-size-range=64-512"],
        json_name=f"{target['id']}_m1.json"
    )
    if m1:
        suites["memtier_read_heavy_1_10"] = m1
        print(f"     Total Throughput: {m1['total_ops_sec']:>10.1f} ops/sec | p50: {m1['total_p50_ms']:>6.3f} ms | p99: {m1['total_p99_ms']:>6.3f} ms | Bandwidth: {m1['total_kb_sec']/1024:>6.2f} MB/s", flush=True)

    # Workload 2: Active Web Session Store (1:5 Write:Read, Zipfian distribution)
    flush_db()
    c_m2 = cfg["m2_conns_per_thread"]
    t_m2 = cfg["m2_threads"]
    n_m2 = cfg["m2_reqs"]
    print(f"  -> memtier Suite 2: Active Session Store (1:5 Write:Read, Zipfian hot keys, {t_m2*c_m2} conns, n={n_m2})...", flush=True)
    m2 = run_memtier(
        args=["-t", str(t_m2), "-c", str(c_m2), "-n", str(n_m2), "--ratio=1:5", "--key-pattern=Z:Z", "--data-size-range=128-1024"],
        json_name=f"{target['id']}_m2.json"
    )
    if m2:
        suites["memtier_session_store_1_5"] = m2
        print(f"     Total Throughput: {m2['total_ops_sec']:>10.1f} ops/sec | p50: {m2['total_p50_ms']:>6.3f} ms | p99: {m2['total_p99_ms']:>6.3f} ms", flush=True)

    # Workload 3: Connection Pool Pipeline Batching (P=4, 1:5 Ratio)
    flush_db()
    c_m3 = cfg["m3_conns_per_thread"]
    t_m3 = cfg["m3_threads"]
    n_m3 = cfg["m3_reqs"]
    print(f"  -> memtier Suite 3: Client Pool Pipelining (P=4, 1:5 Ratio, {t_m3*c_m3} conns, n={n_m3})...", flush=True)
    m3 = run_memtier(
        args=["-t", str(t_m3), "-c", str(c_m3), "-n", str(n_m3), "--ratio=1:5", "--pipeline=4", "--key-pattern=G:G", "--data-size=256"],
        json_name=f"{target['id']}_m3.json"
    )
    if m3:
        suites["memtier_pipelined_p4"] = m3
        print(f"     Total Throughput: {m3['total_ops_sec']:>10.1f} ops/sec | p50: {m3['total_p50_ms']:>6.3f} ms | p99: {m3['total_p99_ms']:>6.3f} ms", flush=True)

    # Workload 4: High Concurrency Internet Spike (c=500 Connections)
    flush_db()
    print(f"  -> memtier Suite 4: Extreme Internet Spike (c=500 concurrent connections, 1:5 Ratio)...", flush=True)
    m4 = run_memtier(
        args=["-t", "4", "-c", "125", "-n", "2000", "--ratio=1:5", "--key-pattern=G:G", "--data-size=128"],
        json_name=f"{target['id']}_m4.json"
    )
    if m4:
        suites["memtier_extreme_concurrency_c500"] = m4
        print(f"     Total Throughput: {m4['total_ops_sec']:>10.1f} ops/sec | p50: {m4['total_p50_ms']:>6.3f} ms | p99: {m4['total_p99_ms']:>6.3f} ms", flush=True)

    # =========================================================================
    # PART B: Official redis-benchmark with Realistic Random Keyspace (-r 100000)
    # =========================================================================

    # Workload 5: Realistic Key Distribution Ping-Pong (P=1, -r 100000, c=50)
    flush_db()
    n_r5 = cfg["r5_reqs"]
    print(f"  -> redis-benchmark Suite 5: Realistic Key Distribution Ping-Pong (-r 100000, c=50, n={n_r5})...", flush=True)
    r5 = run_redis_benchmark(
        args=["-t", "set,get,incr", "-r", "100000", "-c", "50", "-n", str(n_r5)],
        warmup_args=["-t", "set,get", "-r", "100000", "-c", "20", "-n", str(n_r5 // 10)]
    )
    suites["redis_bench_random_p1"] = r5
    for op, m in r5.items():
        print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms", flush=True)

    # Measure intermediate RAM with 100,000 keys loaded
    keys_100k_stats = get_container_stats(cname)
    print(f"  -> Intermediate RAM (100k keys active): {keys_100k_stats}", flush=True)

    # Workload 6: Realistic Payload Sweep (-r 100000, 256B, 512B, 1KB, 4KB)
    flush_db()
    print(f"  -> redis-benchmark Suite 6: Production Payload Sweep (-r 100000, c=50, n=50000)...", flush=True)
    r6 = {}
    for sz in [256, 512, 1024, 4096]:
        res_sz = run_redis_benchmark(
            args=["-t", "set,get", "-r", "100000", "-c", "50", "-n", "50000", "-d", str(sz)],
            warmup_args=["-t", "set,get", "-r", "100000", "-c", "20", "-n", "5000", "-d", str(sz)]
        )
        r6[f"{sz}B"] = res_sz
        for op, m in res_sz.items():
            print(f"     {op} ({sz}B): {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms", flush=True)
    suites["redis_bench_payload_sweep"] = r6

    # Measure RAM after 4KB payload sweep (high-water mark)
    payload_sweep_stats = get_container_stats(cname)
    print(f"  -> RAM after 4KB payload sweep (allocator high-water mark): {payload_sweep_stats}", flush=True)

    # Workload 7: High-Throughput Batch Ingestion (P=16, -r 100000, c=100, n=300000)
    flush_db()
    n_r7 = cfg["r7_reqs"]
    print(f"  -> redis-benchmark Suite 7: High Batch Ingestion (P=16, -r 100000, c=100, n={n_r7})...", flush=True)
    r7 = run_redis_benchmark(
        args=["-t", "set,get", "-r", "100000", "-c", "100", "-n", str(n_r7), "-P", "16"],
        warmup_args=["-t", "set,get", "-r", "100000", "-c", "50", "-n", str(n_r7 // 10), "-P", "16"]
    )
    suites["redis_bench_pipelined_p16"] = r7
    for op, m in r7.items():
        print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms | max: {m['max_ms']:>6.3f} ms", flush=True)

    post_stats = get_container_stats(cname)
    print(f"  -> Completed {name}. Final Resource Footprint: {post_stats}", flush=True)
    clean_data_and_containers()

    return {
        "name": name,
        "id": target["id"],
        "arch": target["arch"],
        "init_stats": init_stats,
        "keys_100k_stats": keys_100k_stats,
        "payload_sweep_stats": payload_sweep_stats,
        "post_stats": post_stats,
        "suites": suites
    }

def main():
    print("=" * 80, flush=True)
    print(" RUSTFORGE DUAL-TIER REALISTIC PRODUCTION BENCHMARK SUITE", flush=True)
    print(" OFFICIAL TOOLS: redislabs/memtier_benchmark + redis-benchmark", flush=True)
    print(" Realistic Key Access: Gaussian/Zipfian Hot Keys + Random Keyspace (-r 100000)", flush=True)
    print(" Realistic Workload Ratios: 1:10 (Read-Heavy Web) & 1:5 (Active Session)", flush=True)
    print(" Extreme High Concurrency: Up to c=500 Connections", flush=True)
    print(f" Client Taskset Pinning: CPUs {CLIENT_TASKSET_CPUS}", flush=True)
    print("=" * 80, flush=True)

    os.makedirs(RESULTS_DIR, exist_ok=True)
    clean_data_and_containers()

    tiers = [
        {
            "tier_name": "Tier 1 (2 CPU Cores / 4 GB RAM)",
            "tier_key": "tier1_2cpu_4gb",
            "cores": 2,
            "mem_gb": 4,
            "cpuset": "0,2",
            "config": {
                "m1_threads": 4,
                "m1_conns_per_thread": 25,  # 100 conns total
                "m1_reqs": 2500,            # 250,000 requests
                "m2_threads": 4,
                "m2_conns_per_thread": 25,  # 100 conns total
                "m2_reqs": 2500,            # 250,000 requests
                "m3_threads": 4,
                "m3_conns_per_thread": 25,  # 100 conns total
                "m3_reqs": 2500,            # 250,000 requests
                "r5_reqs": 100000,
                "r7_reqs": 300000,
            }
        },
        {
            "tier_name": "Tier 2 (4 CPU Cores / 8 GB RAM - Heavy Traffic)",
            "tier_key": "tier2_4cpu_8gb",
            "cores": 4,
            "mem_gb": 8,
            "cpuset": "0,2,4,6",
            "config": {
                "m1_threads": 4,
                "m1_conns_per_thread": 25,  # 100 conns total (identical to Tier 1 for clean vertical scaling)
                "m1_reqs": 2500,            # 250,000 requests
                "m2_threads": 4,
                "m2_conns_per_thread": 25,  # 100 conns total
                "m2_reqs": 2500,            # 250,000 requests
                "m3_threads": 4,
                "m3_conns_per_thread": 25,  # 100 conns total
                "m3_reqs": 2500,            # 250,000 requests
                "r5_reqs": 100000,
                "r7_reqs": 300000,
            }
        }
    ]

    all_results = {}

    for tier in tiers:
        t_name = tier["tier_name"]
        t_key = tier["tier_key"]
        cores = tier["cores"]
        mem_gb = tier["mem_gb"]
        cpuset = tier["cpuset"]
        cfg = tier["config"]

        print("\n" + "#" * 80, flush=True)
        print(f" STARTING: {t_name}", flush=True)
        print(f" Resource Quota: --cpuset-cpus {cpuset} --memory {mem_gb}g", flush=True)
        print("#" * 80, flush=True)

        targets = get_targets(cores, mem_gb, cpuset)
        tier_data = {}

        for target in targets:
            res = run_realistic_tier_suite(target, t_name, cores, cfg)
            if res:
                tier_data[target["id"]] = res
                # Incremental persist
                out_path = os.path.join(RESULTS_DIR, "realistic_benchmark_results.json")
                with open(out_path, "w") as f:
                    json.dump({"tiers": all_results, "active_tier": t_key, "active_engine": target["id"]}, f, indent=2)
            time.sleep(1)

        all_results[t_key] = {
            "tier_name": t_name,
            "cores": cores,
            "mem_gb": mem_gb,
            "config": cfg,
            "engines": tier_data
        }

    final_payload = {
        "metadata": {
            "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "kernel": run_command(["uname", "-r"]).stdout.strip(),
            "cpu": "13th Gen Intel Core i5-1340P",
            "tools": ["redislabs/memtier_benchmark:latest", "redis:7-alpine redis-benchmark"],
            "features": "Gaussian/Zipfian hot keys, random keyspace (-r 100000), 1:10 & 1:5 ratios, c=500 spike"
        },
        "tiers": all_results
    }

    final_file = os.path.join(RESULTS_DIR, "realistic_benchmark_results.json")
    with open(final_file, "w") as f:
        json.dump(final_payload, f, indent=2)

    print("\n" + "=" * 80, flush=True)
    print(" ALL REALISTIC BENCHMARKS COMPLETED SUCCESSFULLY!", flush=True)
    print(f" Results saved to: {final_file}", flush=True)
    print("=" * 80, flush=True)

if __name__ == "__main__":
    main()
