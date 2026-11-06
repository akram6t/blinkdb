use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use clap::Parser;
use hdrhistogram::Histogram;
use parking_lot::Mutex;

use blinkdb::protocol::binary::frame::{
    BinaryRequest, OP_GET, OP_SET, RESP_HEADER_SIZE, RF_MAGIC,
};

#[derive(Parser, Debug)]
#[command(name = "blink-bench")]
#[command(about = "BlinkDB High-Performance Benchmarking Tool (RESP3 & Native Binary)")]
struct BenchArgs {
    #[arg(short = 'H', long, default_value = "127.0.0.1")]
    host: String,

    #[arg(short = 'p', long, default_value_t = 0)]
    port: u16,

    #[arg(long, default_value = "resp")]
    protocol: String, // "resp" or "binary"

    #[arg(short = 'c', long, default_value_t = 50)]
    clients: usize,

    #[arg(short = 't', long, default_value_t = 4)]
    threads: usize,

    #[arg(short = 'n', long, default_value_t = 200_000)]
    requests: usize,

    #[arg(short = 'P', long, default_value_t = 16)]
    pipeline: usize,

    #[arg(short = 'd', long, default_value_t = 64)]
    data_size: usize,

    #[arg(long, default_value = "1:1")]
    ratio: String, // "1:1", "1:4" (80% GET), "get", "set"

    #[arg(long, default_value_t = 50_000)]
    keyspace: usize,
}

fn parse_ratio(ratio_str: &str) -> (u32, u32) {
    if ratio_str.eq_ignore_ascii_case("get") || ratio_str == "0:1" {
        (0, 1)
    } else if ratio_str.eq_ignore_ascii_case("set") || ratio_str == "1:0" {
        (1, 0)
    } else if let Some((s, g)) = ratio_str.split_once(':') {
        let s_val = s.parse::<u32>().unwrap_or(1);
        let g_val = g.parse::<u32>().unwrap_or(1);
        (s_val, g_val)
    } else {
        (1, 1)
    }
}

fn main() {
    let args = BenchArgs::parse();

    let default_port = if args.protocol == "binary" { 7379 } else { 6379 };
    let port = if args.port == 0 { default_port } else { args.port };

    let (set_w, get_w) = parse_ratio(&args.ratio);
    let total_w = set_w + get_w;

    let ratio_display = if set_w == 0 {
        "100% GET (Read-Only)".to_string()
    } else if get_w == 0 {
        "100% SET (Write-Only)".to_string()
    } else {
        format!("{:.1}% SET, {:.1}% GET", (set_w as f64 / total_w as f64) * 100.0, (get_w as f64 / total_w as f64) * 100.0)
    };

    println!("============================================================");
    println!("  BlinkDB Benchmark Client ");
    println!("============================================================");
    println!("Target:             {}:{}", args.host, port);
    println!("Protocol:           {}", args.protocol.to_uppercase());
    println!("Total Requests:     {}", args.requests);
    println!("Concurrent Clients: {}", args.clients);
    println!("Client Threads:     {}", args.threads);
    println!("Pipeline Depth:     {}", args.pipeline);
    println!("Payload Size:       {} bytes", args.data_size);
    println!("Keyspace:           {} keys", args.keyspace);
    println!("Workload Profile:   {}", ratio_display);
    println!("============================================================");

    // Warm-up / Pre-seed keyspace if GET is involved
    if get_w > 0 && set_w == 0 {
        print!("[*] Pre-populating keyspace with {} keys... ", args.keyspace.min(20_000));
        let _ = std::io::stdout().flush();
        if let Ok(mut stream) = TcpStream::connect(format!("{}:{}", args.host, port)) {
            let _ = stream.set_nodelay(true);
            let val_payload = "x".repeat(args.data_size);
            let seed_count = args.keyspace.min(20_000);
            let mut seed_buf = Vec::with_capacity(seed_count * (args.data_size + 64));
            for i in 0..seed_count {
                let k = format!("bench:key:{:07}", i);
                if args.protocol == "binary" {
                    let req = BinaryRequest {
                        opcode: OP_SET,
                        flags: 0,
                        req_id: i as u32,
                        ttl_secs: 0,
                        key: bytes::Bytes::copy_from_slice(k.as_bytes()),
                        value: bytes::Bytes::copy_from_slice(val_payload.as_bytes()),
                    };
                    req.encode(&mut seed_buf);
                } else {
                    seed_buf.extend_from_slice(
                        format!("*3\r\n$3\r\nSET\r\n${}\r\n{}\r\n${}\r\n{}\r\n",
                            k.len(), k, val_payload.len(), val_payload).as_bytes()
                    );
                }
            }
            let _ = stream.write_all(&seed_buf);
            let mut tmp = vec![0u8; 16384];
            let _ = stream.read(&mut tmp);
            println!("Done.");
        }
    }

    let completed_reqs = Arc::new(AtomicU64::new(0));
    let total_bytes = Arc::new(AtomicU64::new(0));
    let histogram = Arc::new(Mutex::new(
        Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap(),
    ));

    let clients_per_thread = (args.clients + args.threads - 1) / args.threads;
    let reqs_per_client = args.requests / args.clients;

    let start_time = Instant::now();
    let mut thread_handles = Vec::with_capacity(args.threads);

    for t_idx in 0..args.threads {
        let host = args.host.clone();
        let proto = args.protocol.clone();
        let completed = completed_reqs.clone();
        let bytes_acc = total_bytes.clone();
        let hist = histogram.clone();
        let pipe_depth = args.pipeline.max(1);
        let d_size = args.data_size;
        let keyspace = args.keyspace.max(1);

        let num_clients_this_thread = if (t_idx + 1) * clients_per_thread <= args.clients {
            clients_per_thread
        } else {
            args.clients.saturating_sub(t_idx * clients_per_thread)
        };

        if num_clients_this_thread == 0 {
            continue;
        }

        let handle = std::thread::spawn(move || {
            let mut local_hist = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
            let mut streams = Vec::with_capacity(num_clients_this_thread);

            for _ in 0..num_clients_this_thread {
                match TcpStream::connect(format!("{}:{}", host, port)) {
                    Ok(s) => {
                        let _ = s.set_nodelay(true);
                        streams.push(s);
                    }
                    Err(e) => {
                        eprintln!("Failed to connect to {}:{}: {}", host, port, e);
                        return;
                    }
                }
            }

            let val_payload = "x".repeat(d_size);
            let mut req_counter = 0usize;

            for stream in &mut streams {
                let mut remaining = reqs_per_client;

                while remaining > 0 {
                    let batch_size = remaining.min(pipe_depth);
                    let mut write_buf = Vec::with_capacity(batch_size * (d_size + 64));

                    let req_start = Instant::now();

                    for _ in 0..batch_size {
                        let key = format!("bench:key:{:07}", req_counter % keyspace);
                        let is_set = if total_w == 0 || get_w == 0 {
                            true
                        } else if set_w == 0 {
                            false
                        } else {
                            ((req_counter as u32) % total_w) < set_w
                        };

                        if proto == "binary" {
                            let (op, val) = if is_set {
                                (OP_SET, val_payload.as_bytes())
                            } else {
                                (OP_GET, &[][..])
                            };

                            let req = BinaryRequest {
                                opcode: op,
                                flags: 0,
                                req_id: req_counter as u32,
                                ttl_secs: 0,
                                key: bytes::Bytes::copy_from_slice(key.as_bytes()),
                                value: bytes::Bytes::copy_from_slice(val),
                            };
                            req.encode(&mut write_buf);
                        } else {
                            // RESP protocol
                            if is_set {
                                write_buf.extend_from_slice(
                                    format!("*3\r\n$3\r\nSET\r\n${}\r\n{}\r\n${}\r\n{}\r\n",
                                        key.len(), key, val_payload.len(), val_payload).as_bytes()
                                );
                            } else {
                                write_buf.extend_from_slice(
                                    format!("*2\r\n$3\r\nGET\r\n${}\r\n{}\r\n", key.len(), key).as_bytes()
                                );
                            }
                        }

                        req_counter += 1;
                    }

                    // Send pipeline batch
                    if stream.write_all(&write_buf).is_err() {
                        break;
                    }
                    bytes_acc.fetch_add(write_buf.len() as u64, Ordering::Relaxed);

                    // Read responses
                    let mut resp_buf = vec![0u8; 16384];
                    let mut responses_read = 0;

                    while responses_read < batch_size {
                        let n = match stream.read(&mut resp_buf) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(_) => break,
                        };

                        bytes_acc.fetch_add(n as u64, Ordering::Relaxed);

                        if proto == "binary" {
                            let mut offset = 0;
                            while offset + RESP_HEADER_SIZE <= n {
                                if resp_buf[offset..offset + 2] == RF_MAGIC {
                                    let vlen = u32::from_le_bytes(
                                        resp_buf[offset + 9..offset + 13].try_into().unwrap(),
                                    ) as usize;
                                    offset += RESP_HEADER_SIZE + vlen;
                                    responses_read += 1;
                                } else {
                                    offset += 1;
                                }
                            }
                        } else {
                            for &b in &resp_buf[..n] {
                                if b == b'+' || b == b'$' || b == b':' || b == b'-' {
                                    responses_read += 1;
                                }
                            }
                        }
                    }

                    let elapsed_micros = req_start.elapsed().as_micros() as u64;
                    let latency_per_op = elapsed_micros / (batch_size as u64).max(1);
                    for _ in 0..batch_size {
                        let _ = local_hist.record(latency_per_op.max(1));
                    }

                    completed.fetch_add(batch_size as u64, Ordering::Relaxed);
                    remaining -= batch_size;
                }
            }

            let mut global = hist.lock();
            let _ = global.add(&local_hist);
        });

        thread_handles.push(handle);
    }

    for h in thread_handles {
        let _ = h.join();
    }

    let elapsed = start_time.elapsed();
    let total_done = completed_reqs.load(Ordering::Relaxed);
    let bytes = total_bytes.load(Ordering::Relaxed);
    let qps = (total_done as f64) / elapsed.as_secs_f64();
    let mb_per_sec = (bytes as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();

    let hist = histogram.lock();

    println!("\n================ Benchmark Results ================");
    println!("Total Requests:     {}", total_done);
    println!("Elapsed Time:       {:.3} seconds", elapsed.as_secs_f64());
    println!("Throughput:         {:.2} ops/sec", qps);
    println!("Bandwidth:          {:.2} MB/sec", mb_per_sec);
    println!("---------------- Latency Summary ------------------");
    println!("Min:                {:.3} ms", (hist.min() as f64) / 1000.0);
    println!("Avg:                {:.3} ms", (hist.mean()) / 1000.0);
    println!("p50 (Median):       {:.3} ms", (hist.value_at_quantile(0.50) as f64) / 1000.0);
    println!("p90:                {:.3} ms", (hist.value_at_quantile(0.90) as f64) / 1000.0);
    println!("p99:                {:.3} ms", (hist.value_at_quantile(0.99) as f64) / 1000.0);
    println!("p99.9:              {:.3} ms", (hist.value_at_quantile(0.999) as f64) / 1000.0);
    println!("Max:                {:.3} ms", (hist.max() as f64) / 1000.0);
    println!("====================================================");
}
