#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# RustForge 3-Way Benchmark Runner Script (Production-Grade)
# Evaluates RustForge vs DragonflyDB vs Redis 7 (Default & Tuned io-threads 2)
# Environment: Linux Kernel 6.0+, Docker 2 CPU Cores, 4 GB RAM
# ==============================================================================

echo "=============================================================================="
echo " Starting Automated 3-Way Database Benchmark Suite"
echo " Date: $(date)"
echo " Host Kernel: $(uname -r) ($(uname -m))"
echo " Available CPUs: $(nproc)"
echo "=============================================================================="

# 1. Clean previous containers
docker rm -f rustforge_bench dragonfly_bench redis_bench redis_tuned_bench 2>/dev/null || true

# 2. Verify Docker images
echo "[*] Checking Docker images..."
docker images | grep -E "(rustforge|dragonfly|redis)" || true

# 3. Ensure bin/rf-bench exists
if [ ! -f "bin/rf-bench" ]; then
    echo "[*] Building rf-bench..."
    CARGO_TARGET_DIR=/tmp/cargo_target cargo build --release --bin rf-bench
    cp /tmp/cargo_target/release/rf-bench bin/rf-bench
fi

# 4. Run Benchmark Runner
echo "[*] Executing benchmark runner..."
python3 bench/benchmark_runner.py

echo "[*] Benchmark suite execution complete."
