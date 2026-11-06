#!/usr/bin/env python3
"""
Runs Redis 7 (Default) and Redis 7 (Tuned) benchmarks across Tier 1 and Tier 2,
merging the results directly into results/tiered_benchmark_results.json.
"""

import json
import os
import sys
import time

from run_tiered_benchmarks import (
    get_targets_for_tier,
    run_suite_for_target,
    cleanup_all,
    get_system_noise,
    CLIENT_TASKSET_CPUS,
)

def main():
    json_path = "results/tiered_benchmark_results.json"
    if not os.path.exists(json_path):
        print(f"Error: {json_path} does not exist!")
        sys.exit(1)

    with open(json_path, "r") as f:
        data = json.load(f)

    # Redis targets to run
    redis_ids = ["redis_default", "redis_tuned"]

    tiers_to_run = [
        ("tier1_2cpu_4gb", 2, 4),
        ("tier2_4cpu_8gb", 4, 8),
    ]

    for t_key, cores, mem_gb in tiers_to_run:
        tier_data = data["tiers"][t_key]
        t_name = tier_data["tier_name"]
        cfg = tier_data["config"]

        print("\n" + "#" * 80, flush=True)
        print(f" RUNNING REDIS TARGETS FOR: {t_name}", flush=True)
        print("#" * 80, flush=True)

        all_targets = get_targets_for_tier(cores, mem_gb)
        targets = [t for t in all_targets if t["id"] in redis_ids]

        for target in targets:
            res = run_suite_for_target(target, t_name, cfg)
            if res:
                tier_data["engines"][target["id"]] = res
                # Persist incrementally
                with open(json_path, "w") as f:
                    json.dump(data, f, indent=2)
                print(f"  -> Incremental update saved to {json_path} for {target['id']}", flush=True)
            time.sleep(1)

    print("\n" + "=" * 80, flush=True)
    print(" ALL REDIS TARGETS COMPLETED AND MERGED SUCCESSFULLY!", flush=True)
    print("=" * 80, flush=True)

if __name__ == "__main__":
    main()
