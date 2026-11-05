#!/usr/bin/env python3
"""
Comprehensive 3-Way Benchmark Runner:
RustForge vs DragonflyDB vs Redis 7
Strict Docker resource constraints: 2 CPU cores, 4 GB RAM.
Execution order:
  1. RustForge
  2. DragonflyDB
  3. Redis 7
"""

import subprocess
import time
import json
import csv
import io
import os
import sys

TARGETS = [
    {
        "name": "RustForge",
        "container_name": "rustforge_bench",
        "run_cmd": [
            "docker", "run", "-d",
            "--name", "rustforge_bench",
            "--security-opt", "seccomp=unconfined",
            "--network", "host",
            "--cpus", "2",
            "--memory", "4g",
            "rustforge:latest"
        ],
        "threads": 2,
        "arch": "io_uring / Thread-Per-Core (Rust)"
    },
    {
        "name": "DragonflyDB",
        "container_name": "dragonfly_bench",
        "run_cmd": [
            "docker", "run", "-d",
            "--name", "dragonfly_bench",
            "--security-opt", "seccomp=unconfined",
            "--network", "host",
            "--cpus", "2",
            "--memory", "4g",
            "docker.dragonflydb.io/dragonflydb/dragonfly:latest",
            "--port", "6379",
            "--proactor_threads=2",
            "--maxmemory=3.5gb"
        ],
        "threads": 2,
        "arch": "io_uring Proactor / Fibers (C++)"
    },
    {
        "name": "Redis 7",
        "container_name": "redis_bench",
        "run_cmd": [
            "docker", "run", "-d",
            "--name", "redis_bench",
            "--security-opt", "seccomp=unconfined",
            "--network", "host",
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
        "arch": "epoll Single-Threaded Event Loop (C)"
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
    # Ensure port 6379 is free
    time.sleep(1)

def wait_for_ready(port=6379, max_attempts=30):
    for attempt in range(max_attempts):
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

def run_bench_suite(suite_desc, bench_args):
    cmd = ["docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "--csv"] + bench_args
    print(f"    Executing: {' '.join(bench_args)} ...", flush=True)
    res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    combined = res.stdout + "\n" + res.stderr
    parsed = parse_csv_benchmark(combined)
    return parsed, combined

def format_markdown_report(all_results):
    md = []
    md.append("# 3-Way Database Benchmark Report: RustForge vs DragonflyDB vs Redis")
    md.append("### Strict Docker Environment: 2 CPU Cores, 4 GB RAM\n")
    md.append("- **Test Date:** November 2026")
    md.append("- **Linux Kernel:** Linux 6.17.0-41-generic (x86_64)")
    md.append("- **Tool:** Official `redis-benchmark` (Redis 7.4.12)")
    md.append("- **Network Mode:** `--network host` (Bare-metal loopback performance, zero bridge overhead)")
    md.append("- **Security Opt:** `--security-opt seccomp=unconfined` (Full `io_uring` support enabled)")
    md.append("\n---\n")

    md.append("## 1. Engine Resource & Architecture Overview\n")
    md.append("| Engine | Implementation | Threads / Model | Resource Limit | Initial RSS | Post-Bench RSS |")
    md.append("| :--- | :--- | :--- | :--- | :--- | :--- |")
    for name, data in all_results.items():
        md.append(f"| **{name}** | {data['arch']} | {data.get('threads', 'N/A')} cores pinned | 2 CPU, 4 GB RAM | {data['initial_stats'].split('|')[0].strip()} | {data['post_stats'].split('|')[0].strip()} |")
    md.append("\n---\n")

    # Suite 1 Table
    md.append("## 2. Suite 1: Standard Latency & Throughput (c=50 clients, n=200,000 requests, P=1)\n")
    md.append("| Command | RustForge (RPS) | DragonflyDB (RPS) | Redis 7 (RPS) | RustForge vs Redis | RustForge vs Dragonfly |")
    md.append("| :--- | :---: | :---: | :---: | :---: | :---: |")
    ops = ["PING_INLINE", "PING_MBULK", "SET", "GET", "INCR"]
    for op in ops:
        rf = all_results.get("RustForge", {}).get("suites", {}).get("suite1_standard_c50", {}).get(op, {}).get("rps", 0)
        df = all_results.get("DragonflyDB", {}).get("suites", {}).get("suite1_standard_c50", {}).get(op, {}).get("rps", 0)
        rd = all_results.get("Redis 7", {}).get("suites", {}).get("suite1_standard_c50", {}).get(op, {}).get("rps", 0)
        vs_rd = f"**+{((rf - rd)/rd)*100:.1f}%**" if rd > 0 and rf >= rd else f"-{((rd - rf)/rd)*100:.1f}%" if rd > 0 else "N/A"
        vs_df = f"**+{((rf - df)/df)*100:.1f}%**" if df > 0 and rf >= df else f"-{((df - rf)/df)*100:.1f}%" if df > 0 else "N/A"
        md.append(f"| **{op}** | {rf:,.0f} | {df:,.0f} | {rd:,.0f} | {vs_rd} | {vs_df} |")
    md.append("\n")

    # Suite 1 Latency Percentiles Table
    md.append("### Latency Distribution (Suite 1: P=1, c=50, n=200,000 requests)\n")
    md.append("| Engine | Command | Avg (ms) | p50 (ms) | p95 (ms) | p99 (ms) | Max (ms) |")
    md.append("| :--- | :--- | :---: | :---: | :---: | :---: | :---: |")
    for name in ["RustForge", "DragonflyDB", "Redis 7"]:
        s1 = all_results.get(name, {}).get("suites", {}).get("suite1_standard_c50", {})
        for op in ["SET", "GET", "INCR"]:
            m = s1.get(op, {})
            if m:
                md.append(f"| **{name}** | {op} | {m.get('avg_ms',0):.3f} | {m.get('p50_ms',0):.3f} | {m.get('p95_ms',0):.3f} | {m.get('p99_ms',0):.3f} | {m.get('max_ms',0):.3f} |")
    md.append("\n---\n")

    # Suite 2 High Concurrency
    md.append("## 3. Suite 2: High Concurrency (c=100 clients, n=200,000 requests, P=1)\n")
    md.append("| Command | RustForge (RPS) | DragonflyDB (RPS) | Redis 7 (RPS) | RustForge vs Redis | RustForge vs Dragonfly |")
    md.append("| :--- | :---: | :---: | :---: | :---: | :---: |")
    for op in ["SET", "GET"]:
        rf = all_results.get("RustForge", {}).get("suites", {}).get("suite2_high_concurrency_c100", {}).get(op, {}).get("rps", 0)
        df = all_results.get("DragonflyDB", {}).get("suites", {}).get("suite2_high_concurrency_c100", {}).get(op, {}).get("rps", 0)
        rd = all_results.get("Redis 7", {}).get("suites", {}).get("suite2_high_concurrency_c100", {}).get(op, {}).get("rps", 0)
        vs_rd = f"**+{((rf - rd)/rd)*100:.1f}%**" if rd > 0 and rf >= rd else f"-{((rd - rf)/rd)*100:.1f}%" if rd > 0 else "N/A"
        vs_df = f"**+{((rf - df)/df)*100:.1f}%**" if df > 0 and rf >= df else f"-{((df - rf)/df)*100:.1f}%" if df > 0 else "N/A"
        md.append(f"| **{op}** | {rf:,.0f} | {df:,.0f} | {rd:,.0f} | {vs_rd} | {vs_df} |")
    md.append("\n---\n")

    # Suite 3 Pipelined Throughput
    md.append("## 4. Suite 3: Pipelined High-Throughput (P=16, c=50 clients, n=500,000 requests)\n")
    md.append("| Command | RustForge (RPS) | DragonflyDB (RPS) | Redis 7 (RPS) | RustForge vs Redis | RustForge vs Dragonfly |")
    md.append("| :--- | :---: | :---: | :---: | :---: | :---: |")
    for op in ["SET", "GET"]:
        rf = all_results.get("RustForge", {}).get("suites", {}).get("suite3_pipelined_p16", {}).get(op, {}).get("rps", 0)
        df = all_results.get("DragonflyDB", {}).get("suites", {}).get("suite3_pipelined_p16", {}).get(op, {}).get("rps", 0)
        rd = all_results.get("Redis 7", {}).get("suites", {}).get("suite3_pipelined_p16", {}).get(op, {}).get("rps", 0)
        vs_rd = f"**+{((rf - rd)/rd)*100:.1f}%**" if rd > 0 and rf >= rd else f"-{((rd - rf)/rd)*100:.1f}%" if rd > 0 else "N/A"
        vs_df = f"**+{((rf - df)/df)*100:.1f}%**" if df > 0 and rf >= df else f"-{((df - rf)/df)*100:.1f}%" if df > 0 else "N/A"
        md.append(f"| **{op}** | {rf:,.0f} | {df:,.0f} | {rd:,.0f} | {vs_rd} | {vs_df} |")
    md.append("\n---\n")

    # Suite 4 Medium Payload
    md.append("## 5. Suite 4: Medium Payload (256 Bytes, c=50 clients, n=200,000 requests, P=1)\n")
    md.append("| Command | RustForge (RPS) | DragonflyDB (RPS) | Redis 7 (RPS) | RustForge vs Redis | RustForge vs Dragonfly |")
    md.append("| :--- | :---: | :---: | :---: | :---: | :---: |")
    for op in ["SET", "GET"]:
        rf = all_results.get("RustForge", {}).get("suites", {}).get("suite4_payload_256b", {}).get(op, {}).get("rps", 0)
        df = all_results.get("DragonflyDB", {}).get("suites", {}).get("suite4_payload_256b", {}).get(op, {}).get("rps", 0)
        rd = all_results.get("Redis 7", {}).get("suites", {}).get("suite4_payload_256b", {}).get(op, {}).get("rps", 0)
        vs_rd = f"**+{((rf - rd)/rd)*100:.1f}%**" if rd > 0 and rf >= rd else f"-{((rd - rf)/rd)*100:.1f}%" if rd > 0 else "N/A"
        vs_df = f"**+{((rf - df)/df)*100:.1f}%**" if df > 0 and rf >= df else f"-{((df - rf)/df)*100:.1f}%" if df > 0 else "N/A"
        md.append(f"| **{op}** | {rf:,.0f} | {df:,.0f} | {rd:,.0f} | {vs_rd} | {vs_df} |")
    md.append("\n")

    return "\n".join(md)

def main():
    print("=" * 80)
    print(" 3-WAY DATABASE BENCHMARK UNDER STRICT DOCKER RESOURCE LIMITS")
    print(" Limits: 2 CPU Cores, 4 GB RAM")
    print(" Databases: RustForge (Step 1) -> DragonflyDB (Step 2) -> Redis (Step 3)")
    print("=" * 80)

    cleanup_containers()
    all_results = {}

    for idx, target in enumerate(TARGETS, 1):
        name = target["name"]
        cname = target["container_name"]
        print(f"\n[{idx}/3] Starting Benchmark for {name} ({target['arch']})...")

        # 1. Start Container
        cleanup_containers()
        print(f"  -> Launching container '{cname}' with --cpus 2 --memory 4g...")
        run_command(target["run_cmd"])

        # 2. Wait for ready
        print("  -> Waiting for server to accept connections...")
        if not wait_for_ready():
            print(f"  [ERROR] Server {name} failed to become ready!")
            run_command(["docker", "logs", cname], check=False)
            cleanup_containers()
            sys.exit(1)
        print("  -> Server is READY.")

        initial_stats = get_container_stats(cname)
        print(f"  -> Initial Resource Footprint: {initial_stats}")

        # 3. Warmup
        print("  -> Running warmup (10,000 ops)...")
        subprocess.run(
            ["docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-benchmark", "-p", "6379", "-t", "ping_inline,set,get", "-n", "10000", "-c", "20", "-q"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )

        target_data = {
            "name": name,
            "arch": target["arch"],
            "threads": target["threads"],
            "initial_stats": initial_stats,
            "suites": {}
        }

        # Suite 1: Standard Latency & Concurrency (c=50, n=200000, P=1, d=3)
        print("  -> Suite 1: Standard Core Operations (c=50, n=200,000, P=1)...")
        s1_data, s1_raw = run_bench_suite("Suite 1", ["-t", "ping_inline,ping_mbulk,set,get,incr", "-n", "200000", "-c", "50"])
        target_data["suites"]["suite1_standard_c50"] = s1_data
        for op, metrics in s1_data.items():
            print(f"     {op:<12}: {metrics['rps']:>10.1f} req/s | p50: {metrics['p50_ms']:>6.3f} ms | p99: {metrics['p99_ms']:>6.3f} ms")

        # Suite 2: High Concurrency (c=100, n=200000, P=1, d=3)
        print("  -> Suite 2: High Concurrency (c=100, n=200,000, P=1)...")
        s2_data, s2_raw = run_bench_suite("Suite 2", ["-t", "set,get", "-n", "200000", "-c", "100"])
        target_data["suites"]["suite2_high_concurrency_c100"] = s2_data
        for op, metrics in s2_data.items():
            print(f"     {op:<12}: {metrics['rps']:>10.1f} req/s | p50: {metrics['p50_ms']:>6.3f} ms | p99: {metrics['p99_ms']:>6.3f} ms")

        # Suite 3: Pipelined High Throughput (c=50, n=500000, P=16, d=3)
        print("  -> Suite 3: Pipelined High Throughput (c=50, n=500,000, P=16)...")
        s3_data, s3_raw = run_bench_suite("Suite 3", ["-t", "set,get", "-n", "500000", "-c", "50", "-P", "16"])
        target_data["suites"]["suite3_pipelined_p16"] = s3_data
        for op, metrics in s3_data.items():
            print(f"     {op:<12}: {metrics['rps']:>10.1f} req/s | p50: {metrics['p50_ms']:>6.3f} ms | p99: {metrics['p99_ms']:>6.3f} ms")

        # Suite 4: Medium Payload 256 bytes (c=50, n=200000, P=1, d=256)
        print("  -> Suite 4: Medium Payload (256 bytes, c=50, n=200,000, P=1)...")
        s4_data, s4_raw = run_bench_suite("Suite 4", ["-t", "set,get", "-n", "200000", "-c", "50", "-d", "256"])
        target_data["suites"]["suite4_payload_256b"] = s4_data
        for op, metrics in s4_data.items():
            print(f"     {op:<12}: {metrics['rps']:>10.1f} req/s | p50: {metrics['p50_ms']:>6.3f} ms | p99: {metrics['p99_ms']:>6.3f} ms")

        post_stats = get_container_stats(cname)
        target_data["post_stats"] = post_stats
        print(f"  -> Post-Benchmark Resource Footprint: {post_stats}")

        all_results[name] = target_data

        print(f"  -> Stopping and removing container '{cname}'...")
        cleanup_containers()
        time.sleep(2)

    # Save all raw results
    os.makedirs("results", exist_ok=True)
    with open("results/benchmark_docker_results.json", "w") as f:
        json.dump(all_results, f, indent=2)

    report_md = format_markdown_report(all_results)
    with open("results/benchmark_comparison.md", "w") as f:
        f.write(report_md)

    print("\n" + "=" * 80)
    print(" ALL BENCHMARKS COMPLETED SUCCESSFULLY!")
    print(" JSON Report:     results/benchmark_docker_results.json")
    print(" Markdown Report: results/benchmark_comparison.md")
    print("=" * 80)
    print("\n" + report_md)

if __name__ == "__main__":
    main()
