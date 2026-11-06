#!/usr/bin/env python3
"""
RustForge Comprehensive Diagnostic Runner for Tier 2 Regression Investigation
Diagnosing:
  1.1 Memory Bandwidth Saturation (uncore_imc data_total, LLC load misses)
  1.2 Cross-Core Cache Coherency (xsnp_hitm, lock_loads, perf c2c)
  1.3 Scheduler and NUMA (cpuset.cpus.effective, thread TID affinity)
  1.4 Shard & Hash Distribution (key distribution, connection distribution)
  1.5 io_uring Ring Capacity & Backpressure
"""

import subprocess
import time
import os
import json
import re

RESULTS_DIR = "/home/akram/Desktop/rustforge/results"
DOCS_DIR = "/home/akram/Desktop/rustforge/docs"
os.makedirs(RESULTS_DIR, exist_ok=True)
os.makedirs(DOCS_DIR, exist_ok=True)

CLIENT_CPUS = "8,9,10,11,12,13,14,15"

def clean():
    subprocess.run("docker ps -q | xargs -r docker rm -f", shell=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["pkill", "-9", "-f", "rustforge-server"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["rm", "-rf", "/tmp/rf_diag_data"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["mkdir", "-p", "/tmp/rf_diag_data"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(["sync"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)

def wait_ready(port=6379):
    for _ in range(30):
        try:
            res = subprocess.run(
                ["docker", "run", "--rm", "--network", "host", "redis:7-alpine", "redis-cli", "-p", str(port), "PING"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=3
            )
            if "PONG" in res.stdout:
                return True
        except Exception:
            pass
        time.sleep(0.3)
    return False

def get_server_pid():
    res = subprocess.run(["pgrep", "rustforge-serve"], stdout=subprocess.PIPE, text=True)
    pids = res.stdout.strip().split()
    if pids:
        return pids[0]
    return None

def run_perf_stat(pid, duration_sec=8):
    events = (
        "cpu_core/cycles/,"
        "cpu_core/instructions/,"
        "cpu_core/cache-references/,"
        "cpu_core/cache-misses/,"
        "cpu_core/LLC-loads/,"
        "cpu_core/LLC-load-misses/,"
        "cpu_core/mem_load_l3_hit_retired.xsnp_hitm/,"
        "cpu_core/mem_inst_retired.lock_loads/"
    )
    cmd = ["perf", "stat", "-e", events, "-p", str(pid), "sleep", str(duration_sec)]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return proc

def run_dram_bw_stat(duration_sec=8):
    events = "uncore_imc_free_running/data_read/,uncore_imc_free_running/data_write/,uncore_imc_free_running/data_total/"
    cmd = ["perf", "stat", "-a", "-e", events, "sleep", str(duration_sec)]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return proc

def parse_perf_output(stderr_text):
    metrics = {}
    for line in stderr_text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        
        # Check MiB events
        m_mib = re.search(r"([\d,\.]+)\s+MiB\s+([\w_/-]+)", line)
        if m_mib:
            val = float(m_mib.group(1).replace(",", ""))
            evt = m_mib.group(2)
            metrics[evt] = val
            continue

        # Check count events
        m_cnt = re.search(r"([\d,]+)\s+([\w_/-]+)", line)
        if m_cnt:
            try:
                val = int(m_cnt.group(1).replace(",", ""))
                evt = m_cnt.group(2)
                metrics[evt] = val
            except ValueError:
                pass
    return metrics

def run_diagnostic_tier(cores, cpuset, tier_name):
    print(f"\n=======================================================", flush=True)
    print(f" DIAGNOSTIC: {tier_name} (cores={cores}, cpuset={cpuset})", flush=True)
    print(f"=======================================================", flush=True)

    clean()
    cmd = [
        "docker", "run", "-d", "--name", "rf_diag",
        "--security-opt", "seccomp=unconfined",
        "--network", "host",
        "--cpuset-cpus", cpuset,
        "--memory", "4g",
        "rustforge:latest",
        "--threads", str(cores),
        "--resp-port", "6379",
        "--max-memory-mb", "3500",
        "--data-dir", "/tmp/rf_diag_data"
    ]
    subprocess.run(cmd, check=True)
    if not wait_ready():
        print("Failed to start server!", flush=True)
        return None

    pid = get_server_pid()
    print(f"Server PID on host: {pid}", flush=True)

    # Diagnostic 1.3: Check cpuset effective & thread affinity
    eff_cpuset = subprocess.run(
        ["docker", "exec", "rf_diag", "cat", "/sys/fs/cgroup/cpuset.cpus.effective"],
        stdout=subprocess.PIPE, text=True
    ).stdout.strip()
    print(f"  -> cpuset.cpus.effective: {eff_cpuset}", flush=True)

    tids_res = subprocess.run(["ps", "-T", "-p", str(pid), "-o", "spid,psr,comm"], stdout=subprocess.PIPE, text=True)
    print(f"  -> Thread-to-Core Mapping:\n{tids_res.stdout.strip()}", flush=True)

    diag_results = {
        "tier_name": tier_name,
        "cores": cores,
        "cpuset": cpuset,
        "effective_cpuset": eff_cpuset,
        "threads": tids_res.stdout.strip()
    }

    # Diagnostic 1.1 & 1.2 during Suite 3 (P=4 pool pipelining)
    print(f"\n  [Diagnostic 1.1 & 1.2: Suite 3 (P=4, 1:5, 100 conns)]...", flush=True)
    perf_core = run_perf_stat(pid, duration_sec=8)
    perf_dram = run_dram_bw_stat(duration_sec=8)
    time.sleep(0.5)

    memtier_cmd = [
        "taskset", "-c", CLIENT_CPUS,
        "docker", "run", "--rm", "--network", "host",
        "redislabs/memtier_benchmark:latest",
        "-s", "127.0.0.1", "-p", "6379",
        "-t", "4", "-c", "25", "-n", "30000",
        "--ratio=1:5", "--pipeline=4",
        "--key-pattern=G:G", "--data-size=256"
    ]
    t0 = time.time()
    m3_res = subprocess.run(memtier_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    t_elapsed = time.time() - t0

    _, core_err = perf_core.communicate()
    _, dram_err = perf_dram.communicate()

    p3_core = parse_perf_output(core_err)
    p3_dram = parse_perf_output(dram_err)
    p3_core.update(p3_dram)

    print(f"     memtier finished in {t_elapsed:.2f}s", flush=True)
    for line in m3_res.stdout.splitlines():
        if "Totals" in line:
            print(f"     memtier Totals: {line}", flush=True)
    
    print(f"     Perf Stat (Suite 3):", flush=True)
    for k, v in p3_core.items():
        print(f"       {k:<45}: {v}", flush=True)

    diag_results["suite_3_perf"] = p3_core

    # Diagnostic 1.1 & 1.2 during Suite 5 (P=1 Random Keys SET)
    print(f"\n  [Diagnostic 1.1 & 1.2: Suite 5 (P=1 Random Keys, -r 100000, c=50, n=200000)]...", flush=True)
    perf_core5 = run_perf_stat(pid, duration_sec=8)
    perf_dram5 = run_dram_bw_stat(duration_sec=8)
    time.sleep(0.5)

    bench_cmd = [
        "taskset", "-c", CLIENT_CPUS,
        "docker", "run", "--rm", "--network", "host",
        "redis:7-alpine",
        "redis-benchmark", "-p", "6379",
        "-t", "set", "-r", "100000",
        "-c", "50", "-n", "200000", "--csv"
    ]
    t0 = time.time()
    b5_res = subprocess.run(bench_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    t_elapsed = time.time() - t0

    _, core_err5 = perf_core5.communicate()
    _, dram_err5 = perf_dram5.communicate()

    p5_core = parse_perf_output(core_err5)
    p5_dram = parse_perf_output(dram_err5)
    p5_core.update(p5_dram)

    print(f"     redis-benchmark finished in {t_elapsed:.2f}s", flush=True)
    print(f"     redis-benchmark output: {b5_res.stdout.strip()}", flush=True)
    print(f"     Perf Stat (Suite 5):", flush=True)
    for k, v in p5_core.items():
        print(f"       {k:<45}: {v}", flush=True)

    diag_results["suite_5_perf"] = p5_core

    # Diagnostic 1.2: perf c2c record
    print(f"\n  [Diagnostic 1.2: perf c2c record (cache coherency)]...", flush=True)
    c2c_file = f"/tmp/perf_c2c_{tier_name.replace(' ', '_').lower()}.data"
    c2c_cmd = ["perf", "c2c", "record", "-p", str(pid), "-o", c2c_file, "--", "sleep", "4"]
    c2c_proc = subprocess.Popen(c2c_cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    time.sleep(0.5)
    # Generate background load during c2c
    subprocess.run(
        ["taskset", "-c", CLIENT_CPUS, "docker", "run", "--rm", "--network", "host",
         "redislabs/memtier_benchmark:latest", "-s", "127.0.0.1", "-p", "6379",
         "-t", "4", "-c", "25", "-n", "20000", "--ratio=1:5", "--pipeline=4",
         "--key-pattern=G:G", "--data-size=256"],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    c2c_proc.communicate()

    # Generate c2c report
    if os.path.exists(c2c_file):
        rep = subprocess.run(["perf", "c2c", "report", "-i", c2c_file, "--stdio"], stdout=subprocess.PIPE, text=True)
        diag_results["c2c_report"] = rep.stdout[:4000]
        print(f"     perf c2c captured ({os.path.getsize(c2c_file)} bytes)", flush=True)

    clean()
    return diag_results

def main():
    print("STARTING FULL DIAGNOSTICS INVESTIGATION...", flush=True)
    t1 = run_diagnostic_tier(cores=2, cpuset="0,2", tier_name="Tier 1 (2 Cores)")
    t2 = run_diagnostic_tier(cores=4, cpuset="0,2,4,6", tier_name="Tier 2 (4 Cores)")

    out_file = os.path.join(RESULTS_DIR, "diagnostics_data.json")
    with open(out_file, "w") as f:
        json.dump({"tier1": t1, "tier2": t2}, f, indent=2)
    print(f"\n=======================================================", flush=True)
    print(f" Diagnostics Completed! Saved raw data to {out_file}", flush=True)
    print(f"=======================================================", flush=True)

if __name__ == "__main__":
    main()
