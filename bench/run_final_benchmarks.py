#!/usr/bin/env python3
"""
RustForge Final Multi-Tier Realistic Production Benchmark Suite
Official Tools: redislabs/memtier_benchmark + redis-benchmark (Redis 7)

Methodology & Protocol (Part 3 Compliance):
  - 3 Hardware Tiers:
      Tier 1: 2 Cores / 4 GB RAM (--cpuset-cpus 0,2 --memory 4g)
      Tier 2.5: 3 Cores / 6 GB RAM (--cpuset-cpus 0,2,4 --memory 6g)
      Tier 2: 4 Cores / 8 GB RAM (--cpuset-cpus 0,2,4,6 --memory 8g)
  - 4 Database Engines:
      1. RustForge (io_uring Thread-Per-Core, scc::HashMap, ahash, Zero-Copy)
      2. DragonflyDB (io_uring Proactor Fibers, C++20)
      3. Redis 7 Default (epoll single-threaded on Core 0)
      4. Redis 7 Tuned (epoll multi-threaded io-threads matching tier core count)
  - 7 Realistic Suites:
      Suite 1: Web App Read-Heavy (1:10 Write:Read, Gaussian keys, c=96)
      Suite 2: Active Session Store (1:5 Write:Read, Zipfian hot keys, c=96)
      Suite 3: Client Pool Pipelining (P=4, 1:5, Gaussian keys, c=96)
      Suite 4: Extreme High Concurrency Spike (c=496 conns, 1:5 ratio)
      Suite 5: Realistic Key Distribution Ping-Pong (P=1, -r 100000, c=50)
      Suite 6: Production Payload Sweep (256B, 512B, 1KB, 4KB, -r 100000, c=50)
      Suite 7: High-Throughput Batch Ingestion (P=16, -r 100000, c=100)
  - Statistical Rigor:
      5 measurement runs per suite. Drop min & max, compute median of middle 3.
      Report variance and standard deviation for confidence intervals.
  - Client Pinning:
      Client pinned strictly to physical E-cores (taskset -c 8-15) to prevent client-server contention.
  - System Noise Logging:
      vmstat 1 and sar -n DEV 1 logged in real-time to results/vmstat_*.log and results/sar_*.log.
"""

import subprocess
import time
import json
import csv
import io
import os
import sys
import argparse
import statistics

RESULTS_DIR = "/home/akram/Desktop/rustforge/results"
CLIENT_TASKSET_CPUS = "8,9,10,11,12,13,14,15"

def run_cmd(cmd, check=True, timeout=60):
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
    if check and res.returncode != 0:
        raise RuntimeError(f"Command failed ({res.returncode}): {' '.join(cmd)}\nSTDERR: {res.stderr}\nSTDOUT: {res.stdout}")
    return res

def clean_data_and_containers():
    for name in ["rustforge_bench", "dragonfly_bench", "redis_bench", "redis_tuned_bench", "test_rf", "test_df", "test_redis"]:
        subprocess.run(["docker", "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["rm", "-rf", "/tmp/rustforge_data", "/tmp/dragonfly_data", "/tmp/redis_data"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["mkdir", "-p", "/tmp/rustforge_data"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["sync"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(0.5)

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
        time.sleep(0.3)
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

def run_redis_benchmark_single(args, timeout=40):
    cmd = ["taskset", "-c", CLIENT_TASKSET_CPUS, "docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "--csv", "--precision", "3"] + args
    try:
        res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
        return parse_redis_bench_csv(res.stdout + "\n" + res.stderr)
    except subprocess.TimeoutExpired:
        return {}

def run_memtier_single(args, json_name="memtier_run.json", timeout=60):
    json_path_host = os.path.join(RESULTS_DIR, json_name)
    json_path_docker = f"/results/{json_name}"
    
    if os.path.exists(json_path_host):
        try:
            os.remove(json_path_host)
        except OSError:
            pass

    cmd = [
        "taskset", "-c", CLIENT_TASKSET_CPUS,
        "docker", "run", "--rm", "--network", "host",
        "-v", f"{RESULTS_DIR}:/results",
        "redislabs/memtier_benchmark:latest",
        "-s", "127.0.0.1", "-p", "6379",
        f"--json-out-file={json_path_docker}",
        "--hide-histogram"
    ] + args

    try:
        subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
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
                "total_ops_sec": float(totals.get("Ops/sec", 0.0)),
                "total_avg_lat_ms": float(totals.get("Average Latency", 0.0)),
                "total_p50_ms": float(totals.get("Percentile Latencies", {}).get("p50.00", 0.0)),
                "total_p99_ms": float(totals.get("Percentile Latencies", {}).get("p99.00", 0.0)),
                "total_p999_ms": float(totals.get("Percentile Latencies", {}).get("p99.90", 0.0)),
                "total_kb_sec": float(totals.get("KB/sec", 0.0)),
                "set_ops_sec": float(sets.get("Ops/sec", 0.0)),
                "get_ops_sec": float(gets.get("Ops/sec", 0.0)),
            }
        except Exception as e:
            print(f"     [WARN] JSON parse error: {e}", flush=True)
            return None
    return None

def aggregate_metric(val_list):
    """Computes median of middle 3 (after dropping min and max of 5 runs), mean, std_dev, and variance."""
    if not val_list:
        return {"median": 0.0, "mean": 0.0, "std_dev": 0.0, "variance": 0.0, "min": 0.0, "max": 0.0, "raw": []}
    sorted_v = sorted(val_list)
    if len(sorted_v) >= 5:
        middle_3 = sorted_v[1:4]
        med = middle_3[1]
    elif len(sorted_v) >= 3:
        med = sorted_v[len(sorted_v) // 2]
    else:
        med = sorted_v[0]
    
    mean = statistics.mean(val_list) if len(val_list) > 0 else 0.0
    var = statistics.variance(val_list) if len(val_list) > 1 else 0.0
    std = statistics.stdev(val_list) if len(val_list) > 1 else 0.0

    return {
        "median": round(med, 2),
        "mean": round(mean, 2),
        "std_dev": round(std, 2),
        "variance": round(var, 2),
        "min": round(sorted_v[0], 2),
        "max": round(sorted_v[-1], 2),
        "raw": [round(x, 2) for x in val_list]
    }

def aggregate_memtier_runs(runs):
    keys = ["total_ops_sec", "total_p50_ms", "total_p99_ms", "total_p999_ms", "total_avg_lat_ms", "total_kb_sec", "set_ops_sec", "get_ops_sec"]
    result = {}
    for k in keys:
        vals = [r[k] for r in runs if r and k in r]
        result[k] = aggregate_metric(vals)
    return result

def aggregate_redis_bench_runs(runs):
    # runs is a list of dicts: [{ "SET": {...}, "GET": {...} }, ...]
    # find all ops
    ops = set()
    for r in runs:
        if r:
            ops.update(r.keys())
    
    result = {}
    for op in ops:
        result[op] = {}
        for metric in ["rps", "p50_ms", "p99_ms", "avg_ms"]:
            vals = [r[op][metric] for r in runs if r and op in r and metric in r[op]]
            result[op][metric] = aggregate_metric(vals)
    return result

def start_noise_logging(tier_key, engine_id):
    vmstat_path = os.path.join(RESULTS_DIR, f"vmstat_{tier_key}_{engine_id}.log")
    sar_path = os.path.join(RESULTS_DIR, f"sar_{tier_key}_{engine_id}.log")
    
    f_vm = open(vmstat_path, "w")
    f_sar = open(sar_path, "w")
    
    p_vm = subprocess.Popen(["vmstat", "1"], stdout=f_vm, stderr=subprocess.DEVNULL)
    p_sar = subprocess.Popen(["sar", "-n", "DEV", "1"], stdout=f_sar, stderr=subprocess.DEVNULL)
    
    return p_vm, p_sar, f_vm, f_sar

def stop_noise_logging(p_vm, p_sar, f_vm, f_sar):
    for p in [p_vm, p_sar]:
        try:
            p.terminate()
            p.wait(timeout=2)
        except Exception:
            p.kill()
    for f in [f_vm, f_sar]:
        try:
            f.close()
        except Exception:
            pass

def get_engine_targets(cores, mem_gb, cpuset):
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
            "arch": "epoll Single-Threaded (C11, Core 0)"
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

def run_suite_with_5_runs(run_func, runs_count=5, warmup_func=None):
    if warmup_func:
        warmup_func()
        time.sleep(0.2)
    runs = []
    for r in range(runs_count):
        res = run_func(r)
        if res:
            runs.append(res)
        time.sleep(0.15)
    return runs

def execute_tier_for_engine(target, tier_name, tier_key, cores, n_runs=5):
    name = target["name"]
    cname = target["container_name"]
    print(f"\n[{tier_name}] Running Benchmark Protocol for {name} ({target['arch']}) | {n_runs} runs per suite...", flush=True)

    clean_data_and_containers()
    run_cmd(target["run_cmd"])
    if not wait_for_ready():
        print(f"  [ERROR] {name} failed to respond to PING!", flush=True)
        clean_data_and_containers()
        return None

    # Start system noise logs
    p_vm, p_sar, f_vm, f_sar = start_noise_logging(tier_key, target["id"])

    init_stats = get_container_stats(cname)
    print(f"  -> Ready. Clean Baseline RAM: {init_stats}", flush=True)

    suites = {}

    try:
        # =========================================================================
        # Suite 1: Web App Read-Heavy (1:10 Write:Read, Gaussian keys, c=96)
        # =========================================================================
        flush_db()
        print(f"  -> Suite 1: Web App Read-Heavy (1:10 Write:Read, Gaussian keys, 96 conns, n=1500) [5 runs]...", flush=True)
        def s1_warmup():
            run_memtier_single(args=["-t", "8", "-c", "12", "-n", "300", "--ratio=1:10", "--key-pattern=G:G", "--data-size-range=64-512"], json_name="warmup.json")
        def s1_run(iter_idx):
            return run_memtier_single(
                args=["-t", "8", "-c", "12", "-n", "1500", "--ratio=1:10", "--key-pattern=G:G", "--data-size-range=64-512"],
                json_name=f"{target['id']}_s1_r{iter_idx}.json"
            )
        s1_runs = run_suite_with_5_runs(s1_run, runs_count=n_runs, warmup_func=s1_warmup)
        s1_agg = aggregate_memtier_runs(s1_runs)
        suites["suite_1_read_heavy_1_10"] = s1_agg
        print(f"     Median: {s1_agg['total_ops_sec']['median']:>10.1f} ops/sec (std: {s1_agg['total_ops_sec']['std_dev']}) | p50: {s1_agg['total_p50_ms']['median']:>6.3f} ms | p99: {s1_agg['total_p99_ms']['median']:>6.3f} ms", flush=True)

        # =========================================================================
        # Suite 2: Active Session Store (1:5 Write:Read, Zipfian hot keys, c=96)
        # =========================================================================
        flush_db()
        print(f"  -> Suite 2: Active Session Store (1:5 Write:Read, Zipfian hot keys, 96 conns, n=1500) [5 runs]...", flush=True)
        def s2_warmup():
            run_memtier_single(args=["-t", "8", "-c", "12", "-n", "300", "--ratio=1:5", "--key-pattern=Z:Z", "--data-size-range=128-1024"], json_name="warmup.json")
        def s2_run(iter_idx):
            return run_memtier_single(
                args=["-t", "8", "-c", "12", "-n", "1500", "--ratio=1:5", "--key-pattern=Z:Z", "--data-size-range=128-1024"],
                json_name=f"{target['id']}_s2_r{iter_idx}.json"
            )
        s2_runs = run_suite_with_5_runs(s2_run, runs_count=n_runs, warmup_func=s2_warmup)
        s2_agg = aggregate_memtier_runs(s2_runs)
        suites["suite_2_session_store_1_5"] = s2_agg
        print(f"     Median: {s2_agg['total_ops_sec']['median']:>10.1f} ops/sec (std: {s2_agg['total_ops_sec']['std_dev']}) | p50: {s2_agg['total_p50_ms']['median']:>6.3f} ms | p99: {s2_agg['total_p99_ms']['median']:>6.3f} ms", flush=True)

        # =========================================================================
        # Suite 3: Client Pool Pipelining (P=4, 1:5 Ratio, c=96)
        # =========================================================================
        flush_db()
        print(f"  -> Suite 3: Client Pool Pipelining (P=4, 1:5 Ratio, 96 conns, n=2500) [5 runs]...", flush=True)
        def s3_warmup():
            run_memtier_single(args=["-t", "8", "-c", "12", "-n", "500", "--ratio=1:5", "--pipeline=4", "--key-pattern=G:G", "--data-size=256"], json_name="warmup.json")
        def s3_run(iter_idx):
            return run_memtier_single(
                args=["-t", "8", "-c", "12", "-n", "2500", "--ratio=1:5", "--pipeline=4", "--key-pattern=G:G", "--data-size=256"],
                json_name=f"{target['id']}_s3_r{iter_idx}.json"
            )
        s3_runs = run_suite_with_5_runs(s3_run, runs_count=n_runs, warmup_func=s3_warmup)
        s3_agg = aggregate_memtier_runs(s3_runs)
        suites["suite_3_pipelined_p4"] = s3_agg
        print(f"     Median: {s3_agg['total_ops_sec']['median']:>10.1f} ops/sec (std: {s3_agg['total_ops_sec']['std_dev']}) | p50: {s3_agg['total_p50_ms']['median']:>6.3f} ms | p99: {s3_agg['total_p99_ms']['median']:>6.3f} ms", flush=True)

        # =========================================================================
        # Suite 4: Extreme High Concurrency Spike (c=496 Connections)
        # =========================================================================
        flush_db()
        print(f"  -> Suite 4: Extreme Internet Spike (c=496 concurrent connections, 1:5 Ratio, n=800) [5 runs]...", flush=True)
        def s4_warmup():
            run_memtier_single(args=["-t", "8", "-c", "62", "-n", "200", "--ratio=1:5", "--key-pattern=G:G", "--data-size=128"], json_name="warmup.json")
        def s4_run(iter_idx):
            return run_memtier_single(
                args=["-t", "8", "-c", "62", "-n", "800", "--ratio=1:5", "--key-pattern=G:G", "--data-size=128"],
                json_name=f"{target['id']}_s4_r{iter_idx}.json"
            )
        s4_runs = run_suite_with_5_runs(s4_run, runs_count=n_runs, warmup_func=s4_warmup)
        s4_agg = aggregate_memtier_runs(s4_runs)
        suites["suite_4_concurrency_c500"] = s4_agg
        print(f"     Median: {s4_agg['total_ops_sec']['median']:>10.1f} ops/sec (std: {s4_agg['total_ops_sec']['std_dev']}) | p50: {s4_agg['total_p50_ms']['median']:>6.3f} ms | p99: {s4_agg['total_p99_ms']['median']:>6.3f} ms", flush=True)

        # =========================================================================
        # Suite 5: Realistic Key Distribution Ping-Pong (P=1, -r 100000, c=50)
        # =========================================================================
        flush_db()
        print(f"  -> Suite 5: Realistic Key Distribution Ping-Pong (-r 100000, c=50, n=25000) [5 runs]...", flush=True)
        def s5_warmup():
            run_redis_benchmark_single(args=["-t", "set,get", "-r", "100000", "-c", "20", "-n", "3000"])
        def s5_run(iter_idx):
            return run_redis_benchmark_single(args=["-t", "set,get,incr", "-r", "100000", "-c", "50", "-n", "25000"])
        s5_runs = run_suite_with_5_runs(s5_run, runs_count=n_runs, warmup_func=s5_warmup)
        s5_agg = aggregate_redis_bench_runs(s5_runs)
        suites["suite_5_random_p1"] = s5_agg
        for op in ["SET", "GET", "INCR"]:
            if op in s5_agg:
                m = s5_agg[op]
                print(f"     {op:<8}: Median {m['rps']['median']:>10.1f} req/s (std: {m['rps']['std_dev']}) | p50: {m['p50_ms']['median']:>6.3f} ms | p99: {m['p99_ms']['median']:>6.3f} ms", flush=True)

        # Intermediate stats
        keys_100k_stats = get_container_stats(cname)
        print(f"  -> Intermediate RAM (100k keys active): {keys_100k_stats}", flush=True)

        # =========================================================================
        # Suite 6: Production Payload Sweep (256B, 512B, 1KB, 4KB, -r 100000)
        # =========================================================================
        flush_db()
        print(f"  -> Suite 6: Production Payload Sweep (-r 100000, c=50, n=15000) [5 runs per size]...", flush=True)
        s6_results = {}
        for sz in [256, 512, 1024, 4096]:
            sz_key = f"{sz}B"
            def s6_warmup(sz=sz):
                run_redis_benchmark_single(args=["-t", "set,get", "-r", "100000", "-c", "20", "-n", "2000", "-d", str(sz)])
            def s6_run(iter_idx, sz=sz):
                return run_redis_benchmark_single(args=["-t", "set,get", "-r", "100000", "-c", "50", "-n", "15000", "-d", str(sz)])
            s6_runs = run_suite_with_5_runs(s6_run, runs_count=n_runs, warmup_func=s6_warmup)
            s6_results[sz_key] = aggregate_redis_bench_runs(s6_runs)
            for op in ["SET", "GET"]:
                if op in s6_results[sz_key]:
                    m = s6_results[sz_key][op]
                    print(f"     {op} ({sz_key:>5}): Median {m['rps']['median']:>10.1f} req/s | p50: {m['p50_ms']['median']:>6.3f} ms | p99: {m['p99_ms']['median']:>6.3f} ms", flush=True)
        suites["suite_6_payload_sweep"] = s6_results

        payload_sweep_stats = get_container_stats(cname)
        print(f"  -> RAM after 4KB payload sweep: {payload_sweep_stats}", flush=True)

        # =========================================================================
        # Suite 7: High-Throughput Batch Ingestion (P=16, -r 100000, c=100)
        # =========================================================================
        flush_db()
        print(f"  -> Suite 7: High Batch Ingestion (P=16, -r 100000, c=100, n=50000) [5 runs]...", flush=True)
        def s7_warmup():
            run_redis_benchmark_single(args=["-t", "set,get", "-r", "100000", "-c", "50", "-n", "10000", "-P", "16"])
        def s7_run(iter_idx):
            return run_redis_benchmark_single(args=["-t", "set,get", "-r", "100000", "-c", "100", "-n", "50000", "-P", "16"])
        s7_runs = run_suite_with_5_runs(s7_run, runs_count=n_runs, warmup_func=s7_warmup)
        s7_agg = aggregate_redis_bench_runs(s7_runs)
        suites["suite_7_batch_p16"] = s7_agg
        for op in ["SET", "GET"]:
            if op in s7_agg:
                m = s7_agg[op]
                print(f"     {op:<8}: Median {m['rps']['median']:>10.1f} req/s (std: {m['rps']['std_dev']}) | p50: {m['p50_ms']['median']:>6.3f} ms | p99: {m['p99_ms']['median']:>6.3f} ms", flush=True)

        post_stats = get_container_stats(cname)
        print(f"  -> Completed {name}. Final Footprint: {post_stats}", flush=True)

    finally:
        stop_noise_logging(p_vm, p_sar, f_vm, f_sar)
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
    parser = argparse.ArgumentParser(description="RustForge Multi-Tier Benchmark Runner")
    parser.add_argument("--tier", choices=["tier1", "tier2.5", "tier2", "all"], default="all")
    parser.add_argument("--engine", choices=["rustforge", "dragonfly", "redis_default", "redis_tuned", "all"], default="all")
    parser.add_argument("--runs", type=int, default=5, help="Number of runs per suite (default 5)")
    args = parser.parse_args()

    os.makedirs(RESULTS_DIR, exist_ok=True)

    tiers = [
        {
            "tier_name": "Tier 1 (2 CPU Cores / 4 GB RAM)",
            "tier_key": "tier1_2cpu_4gb",
            "cores": 2,
            "mem_gb": 4,
            "cpuset": "0,2",
        },
        {
            "tier_name": "Tier 2.5 (3 CPU Cores / 6 GB RAM - Scaling Analysis)",
            "tier_key": "tier2_5_3cpu_6gb",
            "cores": 3,
            "mem_gb": 6,
            "cpuset": "0,2,4",
        },
        {
            "tier_name": "Tier 2 (4 CPU Cores / 8 GB RAM - Heavy Traffic)",
            "tier_key": "tier2_4cpu_8gb",
            "cores": 4,
            "mem_gb": 8,
            "cpuset": "0,2,4,6",
        }
    ]

    tier_map = {
        "tier1": "tier1_2cpu_4gb",
        "tier2.5": "tier2_5_3cpu_6gb",
        "tier2_5": "tier2_5_3cpu_6gb",
        "tier2": "tier2_4cpu_8gb",
    }
    selected_tiers = [t for t in tiers if args.tier == "all" or t["tier_key"] == tier_map.get(args.tier)]
    out_file = os.path.join(RESULTS_DIR, "final_benchmark_results.json")

    all_data = {}
    if os.path.exists(out_file):
        try:
            with open(out_file, "r") as f:
                loaded = json.load(f)
                all_data = loaded.get("tiers", {})
        except Exception:
            all_data = {}

    print("=" * 80, flush=True)
    print(" RUSTFORGE PRODUCTION BENCHMARK SUITE (Part 3 Protocol)", flush=True)
    print(f" Execution: {args.runs} runs per suite (median-of-5 with variance)", flush=True)
    print(f" Client CPU Pinning: taskset -c {CLIENT_TASKSET_CPUS} (8 Physical E-Cores)", flush=True)
    print("=" * 80, flush=True)

    for tier in selected_tiers:
        t_name = tier["tier_name"]
        t_key = tier["tier_key"]
        cores = tier["cores"]
        mem_gb = tier["mem_gb"]
        cpuset = tier["cpuset"]

        print("\n" + "#" * 80, flush=True)
        print(f" STARTING: {t_name}", flush=True)
        print(f" Pinning: --cpuset-cpus {cpuset} | Memory: {mem_gb} GB", flush=True)
        print("#" * 80, flush=True)

        if t_key not in all_data:
            all_data[t_key] = {
                "tier_name": t_name,
                "cores": cores,
                "mem_gb": mem_gb,
                "cpuset": cpuset,
                "engines": {}
            }

        targets = get_engine_targets(cores, mem_gb, cpuset)
        selected_targets = [tg for tg in targets if args.engine == "all" or tg["id"] == args.engine]

        for target in selected_targets:
            res = execute_tier_for_engine(target, t_name, t_key, cores, n_runs=args.runs)
            if res:
                all_data[t_key]["engines"][target["id"]] = res

                # Write progress incrementally
                payload = {
                    "metadata": {
                        "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                        "kernel": run_cmd(["uname", "-r"]).stdout.strip(),
                        "cpu": "13th Gen Intel Core i5-1340P",
                        "runs_per_suite": args.runs,
                        "aggregation": "median of middle 3 (drop min and max) with variance & std dev",
                        "client_cpuset": CLIENT_TASKSET_CPUS,
                        "system_noise_logs": f"results/vmstat_*.log and results/sar_*.log"
                    },
                    "tiers": all_data
                }
                with open(out_file, "w") as f:
                    json.dump(payload, f, indent=2)
            time.sleep(1)

    print("\n" + "=" * 80, flush=True)
    print(" ALL PRODUCTION BENCHMARKS COMPLETED SUCCESSFULLY!", flush=True)
    print(f" Complete data saved to: {out_file}", flush=True)
    print("=" * 80, flush=True)

if __name__ == "__main__":
    main()
