import json

with open("results/realistic_benchmark_results.json") as f:
    data = json.load(f)

tiers = data["tiers"]
for t_key, t_val in tiers.items():
    t_name = t_val["tier_name"]
    print(f"\n=================== {t_name} ===================")
    for eng_id, eng_val in t_val["engines"].items():
        name = eng_val["name"]
        print(f"\n--- {name} ---")
        init_st = eng_val.get("init_stats")
        k100_st = eng_val.get("keys_100k_stats")
        swp_st = eng_val.get("payload_sweep_stats")
        post_st = eng_val.get("post_stats")
        print(f"  RAM -> Init: {init_st} | 100k keys: {k100_st} | Post-4KB: {swp_st} | Final: {post_st}")
        suites = eng_val["suites"]
        if "memtier_read_heavy_1_10" in suites:
            m1 = suites["memtier_read_heavy_1_10"]
            ops = m1.get("total_ops_sec", 0)
            p50 = m1.get("total_p50_ms", 0)
            p99 = m1.get("total_p99_ms", 0)
            print(f"  Suite 1 (Web 1:10):      {ops:>10.1f} ops/s, p50: {p50:>5.3f}ms, p99: {p99:>5.3f}ms")
        if "memtier_session_store_1_5" in suites:
            m2 = suites["memtier_session_store_1_5"]
            ops = m2.get("total_ops_sec", 0)
            p50 = m2.get("total_p50_ms", 0)
            p99 = m2.get("total_p99_ms", 0)
            print(f"  Suite 2 (Session 1:5):  {ops:>10.1f} ops/s, p50: {p50:>5.3f}ms, p99: {p99:>5.3f}ms")
        if "memtier_pipelined_p4" in suites:
            m3 = suites["memtier_pipelined_p4"]
            ops = m3.get("total_ops_sec", 0)
            p50 = m3.get("total_p50_ms", 0)
            p99 = m3.get("total_p99_ms", 0)
            print(f"  Suite 3 (Pool P=4):     {ops:>10.1f} ops/s, p50: {p50:>5.3f}ms, p99: {p99:>5.3f}ms")
        if "memtier_extreme_concurrency_c500" in suites:
            m4 = suites["memtier_extreme_concurrency_c500"]
            ops = m4.get("total_ops_sec", 0)
            p50 = m4.get("total_p50_ms", 0)
            p99 = m4.get("total_p99_ms", 0)
            print(f"  Suite 4 (Spike c=500):  {ops:>10.1f} ops/s, p50: {p50:>5.3f}ms, p99: {p99:>5.3f}ms")
        if "redis_bench_random_p1" in suites:
            r5 = suites["redis_bench_random_p1"]
            set_rps = r5.get("SET", {}).get("rps", 0)
            get_rps = r5.get("GET", {}).get("rps", 0)
            incr_rps = r5.get("INCR", {}).get("rps", 0)
            print(f"  Suite 5 (P=1 Random):   SET: {set_rps:>9.1f}, GET: {get_rps:>9.1f}, INCR: {incr_rps:>9.1f}")
        if "redis_bench_payload_sweep" in suites:
            r6 = suites["redis_bench_payload_sweep"]
            for sz in ["256B", "1024B", "4096B"]:
                if sz in r6:
                    s_rps = r6[sz].get("SET", {}).get("rps", 0)
                    g_rps = r6[sz].get("GET", {}).get("rps", 0)
                    print(f"  Suite 6 ({sz}):          SET: {s_rps:>9.1f}, GET: {g_rps:>9.1f}")
        if "redis_bench_pipelined_p16" in suites:
            r7 = suites["redis_bench_pipelined_p16"]
            s_rps = r7.get("SET", {}).get("rps", 0)
            g_rps = r7.get("GET", {}).get("rps", 0)
            print(f"  Suite 7 (P=16 Batch):   SET: {s_rps:>9.1f}, GET: {g_rps:>9.1f}")
