use std::net::SocketAddr;
use std::sync::Arc;
use clap::Parser;
use log::info;

use blinkdb::cluster::raft::state_machine::RaftEngine;
use blinkdb::cluster::router::ClusterRouter;
use blinkdb::core::affinity::available_cores;
use blinkdb::core::shard::ShardWorker;
use blinkdb::engine::tiering::coordinator::TieredStorageEngine;

#[derive(Parser, Debug)]
#[command(name = "blinkdb-server")]
#[command(about = "BlinkDB: A Linux-Native, Thread-Per-Core, Tiered In-Memory Database")]
struct Args {
    #[arg(short = 'p', long, default_value_t = 6379)]
    resp_port: u16,

    #[arg(short = 'b', long, default_value_t = 7379)]
    binary_port: u16,

    #[arg(short = 't', long, default_value_t = 0)]
    threads: usize,

    #[arg(short = 'd', long, default_value = "./data")]
    data_dir: String,

    #[arg(short = 'm', long, default_value_t = 1024)]
    max_memory_mb: usize,

    #[arg(long, default_value_t = false)]
    cluster: bool,

    #[arg(long, default_value_t = 1)]
    node_id: u64,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let num_cores = available_cores();
    let worker_count = if args.threads == 0 { num_cores } else { args.threads };

    info!("============================================================");
    info!("  BlinkDB Server - Linux io_uring Thread-Per-Core Database  ");
    info!("============================================================");
    info!("RESP3 Compatibility Port: {}", args.resp_port);
    info!("Native Binary Port:       {}", args.binary_port);
    info!("CPU Hardware Cores:       {}", num_cores);
    info!("Worker Shard Threads:     {}", worker_count);
    info!("Max Memory (Hot RAM):     {} MB", args.max_memory_mb);
    info!("Disk Tiering Storage:     {}", args.data_dir);
    info!("Cluster Mode:             {}", if args.cluster { "Enabled" } else { "Standalone" });
    info!("Node ID:                  {}", args.node_id);
    info!("============================================================");

    let max_hot_bytes = args.max_memory_mb * 1024 * 1024;
    let engine = Arc::new(
        TieredStorageEngine::open(&args.data_dir, max_hot_bytes)
            .expect("Failed to initialize tiered storage engine"),
    );

    let resp_addr: SocketAddr = format!("0.0.0.0:{}", args.resp_port)
        .parse()
        .expect("Invalid RESP address");
    let binary_addr: SocketAddr = format!("0.0.0.0:{}", args.binary_port)
        .parse()
        .expect("Invalid Binary address");

    let router = if args.cluster {
        let raft = Arc::new(RaftEngine::new(args.node_id, resp_addr.to_string()));
        Some(ClusterRouter::new(args.node_id, raft))
    } else {
        None
    };

    let mut thread_handles = Vec::with_capacity(worker_count);

    for shard_id in 0..worker_count {
        let core_id = shard_id % num_cores;
        let eng = engine.clone();
        let r = router.clone();

        let handle = std::thread::Builder::new()
            .name(format!("blinkdb-worker-{}", shard_id))
            .spawn(move || {
                let worker = ShardWorker {
                    shard_id,
                    core_id,
                    resp_addr,
                    binary_addr,
                    engine: eng,
                    router: r,
                };
                worker.run();
            })
            .expect("Failed to spawn worker thread");

        thread_handles.push(handle);
    }

    info!("All {} worker shards running. Ready to serve traffic.", worker_count);

    // Join worker threads (or wait indefinitely)
    for handle in thread_handles {
        let _ = handle.join();
    }
}
