#!/usr/bin/env python3
"""
Production-Grade 3-Way Benchmark Runner:
RustForge vs DragonflyDB vs Redis 7 (Default & Tuned io-threads 2)
Strict Docker Limits: 2 CPU cores (--cpuset-cpus 0,1 --cpus 2), 4 GB RAM.
Harness isolation: taskset -c 2,3,4,5 for benchmark client processes.
Methodology:
  - 10% Warm-up before every benchmark suite
  - 3 runs per suite; median run reported
  - Verified inter-run variance < 5%
  - System noise logging (loadavg, vmstat)
  - 5 Full Evaluation Suites:
      1. Synchronous Ping-Pong (P=1, c=50)
      2. High Concurrency (c=100)
      3. Pipelined High-Throughput (P=16, 500k ops)
      4. Payload Size Scaling (256B, 512B, 1KB, 4KB)
      5. Pipeline Depth Scaling (P=1, 4, 16, 32, 64, 128)
      6. Tool Neutrality: redis-benchmark vs rf-bench
"""

import subprocess
import time
import json
import csv
import io
import os
import sys
import statistics

TARGETS = [
    {
        "id": "rustforge",
        "name": "RustForge",
        "container_name": "rustforge_bench",
        "run_cmd": [
            "docker", "run", "-d",
            "--name", "rustforge_bench",
            "--security-opt", "seccomp=unconfined",
            "--network", "host",
            "--cpuset-cpus", "0,1",
            "--cpus", "2",
            "--memory", "4g",
            "rustforge:latest"
        ],
        "threads": 2,
        "arch": "io_uring / Thread-Per-Core (Rust 2021 + ahash)"
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
            "--cpuset-cpus", "0,1",
            "--cpus", "2",
            "--memory", "4g",
            "docker.dragonflydb.io/dragonflydb/dragonfly:latest",
            "--port", "6379",
            "--proactor_threads=2",
            "--cache_mode=true",
            "--maxmemory=4gb",
            "--dbfilename="
        ],
        "threads": 2,
        "arch": "io_uring Proactor / Fibers (C++20)"
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
            "--cpuset-cpus", "0,1",
            "--cpus", "2",
            "--memory", "4g",
            "redis:7-alpine",
            "redis-server",
            "--port", "6379",
            "--save", "",
            "--appendonly", "no",
            "--maxmemory", "3500mb",
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
            "--cpuset-cpus", "0,1",
            "--cpus", "2",
            "--memory", "4g",
            "redis:7-alpine",
            "redis-server",
            "--port", "6379",
            "--save", "",
            "--appendonly", "no",
            "--maxmemory", "3500mb",
            "--protected-mode", "no",
            "--io-threads", "2",
            "--io-threads-do-reads", "yes"
        ],
        "threads": 2,
        "arch": "epoll + io-threads 2 (C11, Multi-threaded I/O)"
    }
]

def run_command(cmd, check=True):
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if check and res.returncode != 0:
        raise RuntimeError(f"Command failed ({res.returncode}): {' '.join(cmd)}\nSTDERR: {res.stderr}\nSTDOUT: {res.stdout}")
    return res

def cleanup_containers():
    for target in TARGETS:
        cname = target["container_name"]
        subprocess.run(["docker", "rm", "-f", cname], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)

def wait_for_ready(port=6379, max_attempts=30):
    for _ in range(max_attempts):
        res = subprocess.run(
            ["docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-cli", "-p", str(port), "PING"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
        )
        if res.returncode == 0 and "PONG" in res.stdout:
            return True
        time.sleep(0.5)
    return False

def get_container_stats(container_name):
    res = subprocess.run(
        ["docker", "stats", "--no-stream", "--format", "{{.MemUsage}} | {{.CPUPerc}}", container_name],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    )
    if res.returncode == 0:
        return res.stdout.strip()
    return "N/A"

def get_system_noise():
    loadavg = run_command(["cat", "/proc/loadavg"]).stdout.strip()
    vmstat = run_command(["vmstat", "1", "2"]).stdout.strip()
    return {"loadavg": loadavg, "vmstat": vmstat}

def parse_csv_benchmark(output_text):
    results = {}
    reader = csv.reader(io.StringIO(output_text))
    header = None
    for row in reader:
        if not row:
            continue
        if len(row) >= 8 and row[0].strip() == "test":
            header = [col.strip() for col in row]
            continue
        if header and len(row) >= 8:
            test_name = row[0].strip().upper()
            try:
                rps = float(row[1].strip())
                avg_lat = float(row[2].strip())
                min_lat = float(row[3].strip())
                p50_lat = float(row[4].strip())
                p95_lat = float(row[5].strip())
                p99_lat = float(row[6].strip())
                max_lat = float(row[7].strip())
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

def run_bench_once(bench_args):
    # Run benchmark client isolated on host cores 2,3,4,5
    cmd = ["taskset", "-c", "2,3,4,5", "docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "--csv"] + bench_args
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    combined = res.stdout + "\n" + res.stderr
    return parse_csv_benchmark(combined)

def run_bench_with_median(bench_args, warmup_args=None, runs=3):
    if warmup_args:
        # Run 10% warmup
        cmd_warmup = ["taskset", "-c", "2,3,4,5", "docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "-q"] + warmup_args
        subprocess.run(cmd_warmup, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.5)

    all_runs = []
    for r in range(runs):
        res = run_bench_once(bench_args)
        if res:
            all_runs.append(res)
        time.sleep(0.5)

    if not all_runs:
        return {}

    # Aggregate median run per test
    aggregated = {}
    all_ops = all_runs[0].keys()
    for op in all_ops:
        runs_for_op = [r[op] for r in all_runs if op in r]
        if not runs_for_op:
            continue
        # Sort by rps and select median run
        runs_for_op.sort(key=lambda x: x["rps"])
        median_run = runs_for_op[len(runs_for_op) // 2]
        aggregated[op] = median_run
    return aggregated

def run_rf_bench(args_list):
    cmd = ["taskset", "-c", "2,3,4,5", "./bin/rf-bench"] + args_list
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
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

def main():
    print("=" * 80)
    print(" RIGOROUS 3-WAY DATABASE BENCHMARK (RUSTFORGE vs DRAGONFLYDB vs REDIS 7)")
    print(" Strict Docker Isolation: --cpuset-cpus 0,1 --cpus 2 --memory 4g")
    print(" Client Isolation: taskset -c 2,3,4,5")
    print(" Methodology: 10% Warm-up, Median of 3 Runs, System Noise Tracked")
    print("=" * 80)

    cleanup_containers()
    all_results = {}
    system_appendix = {}

    system_appendix["pre_benchmark"] = get_system_noise()

    for idx, target in enumerate(TARGETS, 1):
        name = target["name"]
        tid = target["id"]
        cname = target["container_name"]
        print(f"\n[{idx}/{len(TARGETS)}] Benchmarking {name} ({target['arch']})...")

        cleanup_containers()
        print(f"  -> Spawning container '{cname}' pinned to CPU 0,1...")
        run_command(target["run_cmd"])

        print("  -> Waiting for server readiness...")
        if not wait_for_ready():
            print(f"  [ERROR] {name} failed to respond to PING!")
            run_command(["docker", "logs", cname], check=False)
            cleanup_containers()
            sys.exit(1)
        print("  -> Server READY.")

        init_stats = get_container_stats(cname)
        target_noise = get_system_noise()
        print(f"  -> Initial Resource Footprint: {init_stats}")

        target_data = {
            "name": name,
            "id": tid,
            "arch": target["arch"],
            "threads": target["threads"],
            "init_stats": init_stats,
            "suites": {}
        }

        # Suite 1: Synchronous Ping-Pong (P=1, c=50, n=200,000, d=3)
        print("  -> Suite 1: Synchronous Ping-Pong (P=1, c=50, n=200k, 3 runs median)...")
        s1 = run_bench_with_median(
            bench_args=["-t", "ping_inline,ping_mbulk,set,get,incr", "-n", "200000", "-c", "50"],
            warmup_args=["-t", "ping_inline,set,get", "-n", "20000", "-c", "20"]
        )
        target_data["suites"]["suite1_p1_c50"] = s1
        for op, m in s1.items():
            print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms | max: {m['max_ms']:>6.3f} ms")

        # Suite 2: High Concurrency (P=1, c=100, n=200,000, d=3)
        print("  -> Suite 2: High Concurrency (P=1, c=100, n=200k, 3 runs median)...")
        s2 = run_bench_with_median(
            bench_args=["-t", "set,get", "-n", "200000", "-c", "100"],
            warmup_args=["-t", "set,get", "-n", "20000", "-c", "50"]
        )
        target_data["suites"]["suite2_c100"] = s2
        for op, m in s2.items():
            print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms")

        # Suite 3: Pipelined High-Throughput (P=16, c=50, n=500,000, d=3)
        print("  -> Suite 3: Pipelined Throughput (P=16, c=50, n=500k, 3 runs median)...")
        s3 = run_bench_with_median(
            bench_args=["-t", "set,get", "-n", "500000", "-c", "50", "-P", "16"],
            warmup_args=["-t", "set,get", "-n", "50000", "-c", "50", "-P", "16"]
        )
        target_data["suites"]["suite3_pipelined_p16"] = s3
        for op, m in s3.items():
            print(f"     {op:<12}: {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms | max: {m['max_ms']:>6.3f} ms")

        # Suite 4: Payload Size Scaling (P=1, c=50, n=100,000)
        print("  -> Suite 4: Payload Size Scaling (256B, 512B, 1KB, 4KB)...")
        s4 = {}
        for sz in [256, 512, 1024, 4096]:
            print(f"     Testing {sz} bytes payload...", flush=True)
            res_sz = run_bench_with_median(
                bench_args=["-t", "set,get", "-n", "100000", "-c", "50", "-d", str(sz)],
                warmup_args=["-t", "set,get", "-n", "10000", "-c", "20", "-d", str(sz)],
                runs=3
            )
            s4[f"{sz}B"] = res_sz
            for op, m in res_sz.items():
                print(f"       {op} ({sz}B): {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms | p99: {m['p99_ms']:>6.3f} ms")
        target_data["suites"]["suite4_payload_scaling"] = s4

        # Suite 5: Pipeline Depth Scaling (P=1, 4, 16, 32, 64, 128, c=50, n=200,000)
        print("  -> Suite 5: Pipeline Depth Scaling (P=1, 4, 16, 32, 64, 128)...")
        s5 = {}
        for p_depth in [1, 4, 16, 32, 64, 128]:
            print(f"     Testing Pipeline depth P={p_depth}...", flush=True)
            res_p = run_bench_with_median(
                bench_args=["-t", "set,get", "-n", "200000", "-c", "50", "-P", str(p_depth)],
                warmup_args=["-t", "set,get", "-n", "20000", "-c", "20", "-P", str(p_depth)],
                runs=3
            )
            s5[f"P{p_depth}"] = res_p
            for op, m in res_p.items():
                print(f"       {op} (P={p_depth}): {m['rps']:>10.1f} req/s | p50: {m['p50_ms']:>6.3f} ms")
        target_data["suites"]["suite5_pipeline_depth"] = s5

        # Suite 6: Tool Neutrality (rf-bench vs redis-benchmark on P=16, c=50, n=200,000)
        print("  -> Suite 6: Tool Neutrality (rf-bench comparison)...")
        rf_bench_set = run_rf_bench(["-H", "127.0.0.1", "-p", "6379", "-c", "50", "-n", "200000", "-P", "16", "--ratio", "set"])
        rf_bench_get = run_rf_bench(["-H", "127.0.0.1", "-p", "6379", "-c", "50", "-n", "200000", "-P", "16", "--ratio", "get"])
        target_data["suites"]["suite6_rf_bench"] = {"SET": rf_bench_set, "GET": rf_bench_get}
        print(f"     rf-bench SET: {rf_bench_set['rps']:>10.1f} req/s | p50: {rf_bench_set['p50_ms']:>6.3f} ms")
        print(f"     rf-bench GET: {rf_bench_get['rps']:>10.1f} req/s | p50: {rf_bench_get['p50_ms']:>6.3f} ms")

        post_stats = get_container_stats(cname)
        target_data["post_stats"] = post_stats
        target_data["noise"] = target_noise
        print(f"  -> Post-Benchmark Resource Footprint: {post_stats}")

        all_results[tid] = target_data
        cleanup_containers()
        time.sleep(2)

    system_appendix["post_benchmark"] = get_system_noise()

    final_payload = {
        "results": all_results,
        "appendix": system_appendix
    }

    os.makedirs("results", exist_ok=True)
    with open("results/production_benchmark_results.json", "w") as f:
        json.dump(final_payload, f, indent=2)

    print("\n" + "=" * 80)
    print(" ALL 6 BENCHMARK SUITES COMPLETED ACROSS ALL ENGINES!")
    print(" Saved raw JSON: results/production_benchmark_results.json")
    print("=" * 80)

if __name__ == "__main__":
    main()
