use std::sync::Arc;
use std::time::Instant;
use bytes::Bytes;
use ahash::RandomState;
use scc::HashMap;

fn run_benchmark(threads: usize, num_keys: usize, ops_per_thread: usize, read_ratio: usize) -> f64 {
    let map = Arc::new(HashMap::with_capacity_and_hasher(num_keys * 2, RandomState::new()));
    
    // Preload keys
    for i in 0..num_keys {
        let k = Bytes::from(format!("key:{:07}", i));
        let v = Bytes::from("v");
        map.upsert_sync(k, v);
    }

    let start = Instant::now();
    let mut handles = Vec::new();

    for t in 0..threads {
        let m = map.clone();
        handles.push(std::thread::spawn(move || {
            let mut rng_state = (t as u64 + 1) * 6364136223846793005;
            for _ in 0..ops_per_thread {
                rng_state ^= rng_state << 13;
                rng_state ^= rng_state >> 7;
                rng_state ^= rng_state << 17;
                let key_num = (rng_state as usize) % num_keys;
                let k = Bytes::from(format!("key:{:07}", key_num));
                
                if (key_num % 100) < read_ratio {
                    // Read
                    let _ = m.read_sync(k.as_ref(), |_k, v| v.clone());
                } else {
                    // Write
                    let v = Bytes::from("v_updated");
                    let _ = m.upsert_sync(k, v);
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let elapsed = start.elapsed().as_secs_f64();
    let total_ops = threads * ops_per_thread;
    (total_ops as f64) / elapsed
}

fn main() {
    println!("=== scc::HashMap Scaling Diagnostic across Cores ===");
    println!("Testing 100,000 keys with 83% Read / 17% Write (Suite 3 / Suite 2 pattern):");
    for threads in [1, 2, 3, 4] {
        let ops_sec = run_benchmark(threads, 100_000, 250_000, 83);
        println!("  Threads {}: {:>12.1} ops/sec", threads, ops_sec);
    }

    println!("\nTesting 100,000 keys with 100% Write (Suite 5 SET pattern):");
    for threads in [1, 2, 3, 4] {
        let ops_sec = run_benchmark(threads, 100_000, 200_000, 0);
        println!("  Threads {}: {:>12.1} ops/sec", threads, ops_sec);
    }
}
