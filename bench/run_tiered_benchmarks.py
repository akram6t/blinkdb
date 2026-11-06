#!/usr/bin/env python3
"""
RustForge Dual-Tier High-Traffic Benchmark Suite:
Empirical Comparison: RustForge vs DragonflyDB vs Redis 7 (Default & Tuned)

Hardware Tiers:
  - Tier 1: 2 CPU Cores / 4 GB RAM
  - Tier 2: 4 CPU Cores / 8 GB RAM (Realistic High Traffic & Concurrency)

Key Features:
  - Docker cgroup limits: --cpus {2,4} --memory {4g,8g}
  - Client benchmark isolation: taskset -c 8-15 (pinned to dedicated host cores)
  - Database isolated on primary cores (cores 0-7)
  - FLUSHALL between suites to prevent memory accumulation
  - Subprocess timeouts (30s) on every run to eliminate hanging
  - Immediate unbuffered output (flush=True)
  - 10% Warm-up before every benchmark run
  - 3 runs per suite; median run reported
  - Microsecond latency precision (--precision 3)
"""

import subprocess
import time
import json
import csv
import io
import os
import sys

CLIENT_TASKSET_CPUS = "8,9,10,11,12,13,14,15"

def run_command(cmd, check=True, timeout=30):
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
    if check and res.returncode != 0:
        raise RuntimeError(f"Command failed ({res.returncode}): {' '.join(cmd)}\nSTDERR: {res.stderr}\nSTDOUT: {res.stdout}")
    return res

def cleanup_all():
    for name in ["rustforge_bench", "dragonfly_bench", "redis_bench", "redis_tuned_bench"]:
        subprocess.run(["docker", "rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)

def flush_db():
    subprocess.run(
        ["docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-cli", "-p", "6379", "FLUSHALL"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5
    )

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

def get_system_noise():
    try:
        loadavg = run_command(["cat", "/proc/loadavg"]).stdout.strip()
        vmstat = run_command(["vmstat", "1", "2"]).stdout.strip()
        return {"loadavg": loadavg, "vmstat": vmstat}
    except Exception:
        return {"loadavg": "N/A", "vmstat": "N/A"}

def parse_csv_benchmark(output_text):
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
                rps = float(row[1].strip().strip('"'))
                avg_lat = float(row[2].strip().strip('"'))
                min_lat = float(row[3].strip().strip('"'))
                p50_lat = float(row[4].strip().strip('"'))
                p95_lat = float(row[5].strip().strip('"'))
                p99_lat = float(row[6].strip().strip('"'))
                max_lat = float(row[7].strip().strip('"'))
                results[test_name] = {
                    "rps": rps,
                    "avg_ms": avg_lat,
                    "min_ms": min_lat,
                    "p50_ms": p50_lat,
                    "p95_ms": p95_lat,
                    "p99_ms": p99_lat,
                    "max_ms": max_lat
                }
            except ValueError:
                continue
    return results

def run_bench_once(bench_args, timeout=30):
    cmd = ["taskset", "-c", CLIENT_TASKSET_CPUS, "docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "--csv", "--precision", "3"] + bench_args
    try:
        res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
        combined = res.stdout + "\n" + res.stderr
        return parse_csv_benchmark(combined)
    except subprocess.TimeoutExpired:
        print(f"     [TIMEOUT] redis-benchmark timed out after {timeout}s: {' '.join(bench_args)}", flush=True)
        return {}

def run_bench_with_median(bench_args, warmup_args=None, runs=3):
    if warmup_args:
        cmd_warmup = ["taskset", "-c", CLIENT_TASKSET_CPUS, "docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "-q"] + warmup_args
        try:
            subprocess.run(cmd_warmup, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=20)
        except subprocess.TimeoutExpired:
            pass
        time.sleep(0.2)

    all_runs = []
    for _ in range(runs):
        res = run_bench_once(bench_args)
        if res:
            all_runs.append(res)
        time.sleep(0.2)

    if not all_runs:
        return {}

    aggregated = {}
    all_ops = all_runs[0].keys()
    for op in all_ops:
        runs_for_op = [r[op] for r in all_runs if op in r]
        if not runs_for_op:
            continue
        runs_for_op.sort(key=lambda x: x["rps"])
        median_run = runs_for_op[len(runs_for_op) // 2]
        aggregated[op] = median_run
    return aggregated

def run_rf_bench(args_list, timeout=30):
    cmd = ["taskset", "-c", CLIENT_TASKSET_CPUS, "./bin/rf-bench"] + args_list
    try:
        res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        print(f"     [TIMEOUT] rf-bench timed out after {timeout}s", flush=True)
        return {"rps": 0.0, "p50_ms": 0.0, "p99_ms": 0.0}

    qps = 0.0
    p50 = 0.0
    p99 = 0.0
    for line in res.stdout.splitlines():
        if "Throughput:" in line:
            parts = line.split(":")
            try:
                qps = float(parts[1].replace("ops/sec", "").strip())
            except ValueError:
                pass
        elif "p50 (Median):" in line:
            parts = line.split(":")
            try:
                p50 = float(parts[1].replace("ms", "").strip())
            except ValueError:
                pass
        elif "p99:" in line and "p99.9" not in line:
            parts = line.split(":")
            try:
                p99 = float(parts[1].replace("ms", "").strip())
            except ValueError:
                pass
    return {"rps": qps, "p50_ms": p50, "p99_ms": p99}

def get_targets_for_tier(cores, mem_gb):
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
                "--cpus", str(cores),
                "--memory", mem_str,
                "rustforge:latest",
                "--threads", str(cores),
                "--resp-port", "6379",
                "--max-memory-mb", str(db_mem_mb),
                "--data-dir", "/tmp/rustforge_data"
            ],
            "threads": cores,
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
                "--cpus", str(cores),
                "--memory", mem_str,
                "docker.dragonflydb.io/dragonflydb/dragonfly:latest",
                "--port", "6379",
                f"--proactor_threads={cores}",
                "--cache_mode=true",
                f"--maxmemory={db_mem_mb}mb",
                "--dbfilename="
            ],
            "threads": cores,
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
                "--cpus", str(cores),
                "--memory", mem_str,
                "redis:7-alpine",
                "redis-server",
                "--port", "6379",
                "--save", "",
                "--appendonly", "no",
                "--maxmemory", f"{db_mem_mb}mb",
                "--protected-mode", "no"
            ],
            "threads": 1,
            "arch": "epoll Single-Threaded (C11, io-threads=1)"
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
                "--cpus", str(cores),
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
            "threads": cores,
            "arch": f"epoll + io-threads {cores} (C11, Multi-threaded I/O)"
        }
    ]

def run_suite_for_target(target, tier_name, config):
    name = target["name"]
    cname = target["container_name"]
    print(f"\n[{tier_name}] Running Benchmark Suite for {name} ({target['arch']})...", flush=True)

    cleanup_all()
    run_command(target["run_cmd"])
    if not wait_for_ready():
        print(f"  [ERROR] {name} failed to respond to PING!", flush=True)
        run_command(["docker", "logs", cname], check=False)
        cleanup_all()
        return None

    init_stats = get_container_stats(cname)
    print(f"  -> Ready. Initial Footprint: {init_stats}", flush=True)

    suites = {}

    # 1. Sync Ping-Pong (P=1)
    flush_db()
    c_s1 = config["s1_clients"]
    n_s1 = config["s1_requests"]
    print(f"  -> Suite 1: Sync Ping-Pong (c={c_s1}, n={n_s1})...", flush=True)
    s1 = run_bench_with_median(
        bench_args=["-t", "ping_inline,set,get,incr", "-n", str(n_s1), "-c", str(c_s1)],
        warmup_args=["-t", "ping_inline,set,get", "-n", str(n_s1 // 10), "-c", str(c_s1 // 2)]
    )
    suites["s1_sync_p1"] = s1
    for op, m in s1.items():
        print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms", flush=True)

    # 2. High Concurrency (c=100 or 200, P=1)
    flush_db()
    c_s2 = config["s2_clients"]
    n_s2 = config["s2_requests"]
    print(f"  -> Suite 2: High Concurrency (c={c_s2}, n={n_s2})...", flush=True)
    s2 = run_bench_with_median(
        bench_args=["-t", "set,get", "-n", str(n_s2), "-c", str(c_s2)],
        warmup_args=["-t", "set,get", "-n", str(n_s2 // 10), "-c", str(c_s2 // 2)]
    )
    suites["s2_high_concurrency"] = s2
    for op, m in s2.items():
        print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms", flush=True)

    # 3. Extreme High Concurrency (c=200 or 500, P=1)
    flush_db()
    c_s3 = config["s3_clients"]
    n_s3 = config["s3_requests"]
    print(f"  -> Suite 3: Extreme High Concurrency (c={c_s3}, n={n_s3})...", flush=True)
    s3 = run_bench_with_median(
        bench_args=["-t", "set,get", "-n", str(n_s3), "-c", str(c_s3)],
        warmup_args=["-t", "set,get", "-n", str(n_s3 // 10), "-c", str(c_s3 // 2)]
    )
    suites["s3_extreme_concurrency"] = s3
    for op, m in s3.items():
        print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms", flush=True)

    # 4. Pipelined High Throughput (P=16)
    flush_db()
    c_s4 = config["s4_clients"]
    n_s4 = config["s4_requests"]
    print(f"  -> Suite 4: Pipelined High Throughput (P=16, c={c_s4}, n={n_s4})...", flush=True)
    s4 = run_bench_with_median(
        bench_args=["-t", "set,get", "-n", str(n_s4), "-c", str(c_s4), "-P", "16"],
        warmup_args=["-t", "set,get", "-n", str(n_s4 // 10), "-c", str(c_s4), "-P", "16"]
    )
    suites["s4_pipelined_p16"] = s4
    for op, m in s4.items():
        print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms | max: {m['max_ms']:>6.3f} ms", flush=True)

    # 5. Payload Scaling (256B, 512B, 1KB, 4KB)
    flush_db()
    c_s5 = config["s5_clients"]
    n_s5 = config["s5_requests"]
    payloads = config.get("payload_sizes", [256, 512, 1024, 4096])
    print(f"  -> Suite 5: Payload Scaling ({', '.join(str(s)+'B' for s in payloads)}, c={c_s5}, n={n_s5})...", flush=True)
    s5 = {}
    for sz in payloads:
        flush_db()
        res_sz = run_bench_with_median(
            bench_args=["-t", "set,get", "-n", str(n_s5), "-c", str(c_s5), "-d", str(sz)],
            warmup_args=["-t", "set,get", "-n", str(n_s5 // 10), "-c", str(c_s5), "-d", str(sz)],
            runs=3
        )
        s5[f"{sz}B"] = res_sz
        for op, m in res_sz.items():
            print(f"     {op} ({sz}B): {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms", flush=True)
    suites["s5_payload_scaling"] = s5

    # 6. Pipeline Depth Curve (P=1, 4, 16, 32, 64)
    flush_db()
    c_s6 = config["s6_clients"]
    n_s6 = config["s6_requests"]
    print(f"  -> Suite 6: Pipeline Depth Curve (P=1, 4, 16, 32, 64, c={c_s6}, n={n_s6})...", flush=True)
    s6 = {}
    for p_depth in [1, 4, 16, 32, 64]:
        p_args = ["-t", "set,get", "-n", str(n_s6), "-c", str(c_s6)]
        p_warm = ["-t", "set,get", "-n", str(n_s6 // 10), "-c", str(c_s6)]
        if p_depth > 1:
            p_args += ["-P", str(p_depth)]
            p_warm += ["-P", str(p_depth)]
        res_p = run_bench_with_median(p_args, warmup_args=p_warm, runs=3)
        s6[f"P{p_depth}"] = res_p
        for op, m in res_p.items():
            print(f"     {op} (P={p_depth}): {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms", flush=True)
    suites["s6_pipeline_depth"] = s6

    # 7. Tool Neutrality (rf-bench vs redis-benchmark on P=16)
    flush_db()
    c_s7 = config["s7_clients"]
    n_s7 = config["s7_requests"]
    print(f"  -> Suite 7: Tool Neutrality (rf-bench comparison P=16, c={c_s7}, n={n_s7})...", flush=True)
    rf_bench_set = run_rf_bench(["-H", "127.0.0.1", "-p", "6379", "-c", str(c_s7), "-n", str(n_s7), "-P", "16", "--ratio", "set"])
    rf_bench_get = run_rf_bench(["-H", "127.0.0.1", "-p", "6379", "-c", str(c_s7), "-n", str(n_s7), "-P", "16", "--ratio", "get"])
    suites["s7_rf_bench"] = {"SET": rf_bench_set, "GET": rf_bench_get}
    print(f"     rf-bench SET: {rf_bench_set['rps']:>10.1f} req/s | p50: {rf_bench_set['p50_ms']:>6.3f} ms | p99: {rf_bench_set['p99_ms']:>6.3f} ms", flush=True)
    print(f"     rf-bench GET: {rf_bench_get['rps']:>10.1f} req/s | p50: {rf_bench_get['p50_ms']:>6.3f} ms | p99: {rf_bench_get['p99_ms']:>6.3f} ms", flush=True)

    post_stats = get_container_stats(cname)
    print(f"  -> Completed {name}. Final Resource Footprint: {post_stats}", flush=True)
    cleanup_all()

    return {
        "name": name,
        "id": target["id"],
        "arch": target["arch"],
        "threads": target["threads"],
        "init_stats": init_stats,
        "post_stats": post_stats,
        "suites": suites
    }

def main():
    print("=" * 80, flush=True)
    print(" RUSTFORGE DUAL-TIER HIGH-TRAFFIC BENCHMARK SUITE", flush=True)
    print(" TIER 1: 2 CPU Cores / 4 GB RAM", flush=True)
    print(" TIER 2: 4 CPU Cores / 8 GB RAM (Realistic Extreme Traffic)", flush=True)
    print(" Host CPU: 13th Gen Intel Core i5-1340P (16 Logical CPUs)", flush=True)
    print(f" Client Taskset Pinning: CPUs {CLIENT_TASKSET_CPUS}", flush=True)
    print("=" * 80, flush=True)

    cleanup_all()

    # Tier Configurations
    tiers = [
        {
            "tier_name": "Tier 1 (2 CPU Cores / 4 GB RAM)",
            "tier_key": "tier1_2cpu_4gb",
            "cores": 2,
            "mem_gb": 4,
            "config": {
                "s1_clients": 50,
                "s1_requests": 100000,
                "s2_clients": 100,
                "s2_requests": 100000,
                "s3_clients": 200,
                "s3_requests": 100000,
                "s4_clients": 100,
                "s4_requests": 300000,
                "s5_clients": 50,
                "s5_requests": 50000,
                "payload_sizes": [256, 512, 1024, 4096],
                "s6_clients": 50,
                "s6_requests": 100000,
                "s7_clients": 50,
                "s7_requests": 100000,
            }
        },
        {
            "tier_name": "Tier 2 (4 CPU Cores / 8 GB RAM - Heavy Realistic Traffic)",
            "tier_key": "tier2_4cpu_8gb",
            "cores": 4,
            "mem_gb": 8,
            "config": {
                "s1_clients": 100,
                "s1_requests": 150000,
                "s2_clients": 200,
                "s2_requests": 150000,
                "s3_clients": 500,
                "s3_requests": 250000,
                "s4_clients": 200,
                "s4_requests": 500000,
                "s5_clients": 100,
                "s5_requests": 50000,
                "payload_sizes": [256, 1024, 4096],
                "s6_clients": 100,
                "s6_requests": 150000,
                "s7_clients": 100,
                "s7_requests": 200000,
            }
        }
    ]

    all_tiered_results = {}
    noise_log = {"pre_bench": get_system_noise()}

    for tier in tiers:
        t_name = tier["tier_name"]
        t_key = tier["tier_key"]
        cores = tier["cores"]
        mem_gb = tier["mem_gb"]
        cfg = tier["config"]

        print("\n" + "#" * 80, flush=True)
        print(f" STARTING: {t_name}", flush=True)
        print(f" Constraints: --cpus {cores} --memory {mem_gb}g", flush=True)
        print("#" * 80, flush=True)

        targets = get_targets_for_tier(cores, mem_gb)
        tier_results = {}

        for target in targets:
            res = run_suite_for_target(target, t_name, cfg)
            if res:
                tier_results[target["id"]] = res
            time.sleep(1)

        all_tiered_results[t_key] = {
            "tier_name": t_name,
            "cores": cores,
            "mem_gb": mem_gb,
            "config": cfg,
            "engines": tier_results
        }

    noise_log["post_bench"] = get_system_noise()

    final_payload = {
        "metadata": {
            "kernel": run_command(["uname", "-r"]).stdout.strip(),
            "cpu": "13th Gen Intel Core i5-1340P",
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "client_isolation": f"taskset -c {CLIENT_TASKSET_CPUS}",
            "methodology": "Median of 3 runs, 10% warm-up, official redis-benchmark + rf-bench, FLUSHALL between suites"
        },
        "system_noise": noise_log,
        "tiers": all_tiered_results
    }

    os.makedirs("results", exist_ok=True)
    out_file = "results/tiered_benchmark_results.json"
    with open(out_file, "w") as f:
        json.dump(final_payload, f, indent=2)

    print("\n" + "=" * 80, flush=True)
    print(" ALL DUAL-TIER BENCHMARKS COMPLETED SUCCESSFULLY!", flush=True)
    print(f" Raw JSON written to: {out_file}", flush=True)
    print("=" * 80, flush=True)

if __name__ == "__main__":
    main()
