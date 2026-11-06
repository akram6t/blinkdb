#!/usr/bin/env bash
# ==============================================================================
# RustForge Official Production Benchmark Reproduction Script
# Protocol: 3 Tiers, 4 Engines, 7 Realistic Workloads, Median-of-5 Aggregation
# Strict cpuset pinning: Server on P-cores (0,2,4,6), Client on E-cores (8-15)
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
RESULTS_DIR="${ROOT_DIR}/results"
mkdir -p "${RESULTS_DIR}"

echo "=============================================================================="
echo " Starting Full Production Benchmark Suite (RustForge vs DragonflyDB vs Redis 7)"
echo " Methodology: 5 runs per suite (median-of-5, variance, std dev)"
echo " Pinning: Server on physical P-cores, Client taskset on E-cores (8-15)"
echo " Noise Logging: vmstat 1 and sar -n DEV 1"
echo "=============================================================================="

# Check Docker permissions
if ! docker ps > /dev/null 2>&1; then
    echo "Error: Docker daemon is not accessible or current user lacks permissions." >&2
    exit 1
fi

# Ensure RustForge image is built and fresh
echo "Checking RustForge Docker image..."
if ! docker image inspect rustforge:latest > /dev/null 2>&1; then
    echo "Building rustforge:latest..."
    docker build -t rustforge:latest -f "${ROOT_DIR}/Dockerfile" "${ROOT_DIR}"
fi

# Run python orchestration script
python3 "${SCRIPT_DIR}/run_final_benchmarks.py" "$@"

echo "=============================================================================="
echo " Benchmark run complete. Output files generated in: ${RESULTS_DIR}"
echo "   - JSON Data: ${RESULTS_DIR}/final_benchmark_results.json"
echo "   - System Noise Logs: ${RESULTS_DIR}/vmstat_*.log and ${RESULTS_DIR}/sar_*.log"
echo "=============================================================================="
