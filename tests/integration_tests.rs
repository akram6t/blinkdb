use bytes::Bytes;
use tempfile::tempdir;
use std::sync::Arc;

use blinkdb::cluster::pd::driver::{HeartbeatReport, PlacementDriver, RegionReport};
use blinkdb::cluster::raft::state_machine::RaftEngine;
use blinkdb::cluster::router::{ClusterRouter, RouteDecision};
use blinkdb::engine::tiering::coordinator::TieredStorageEngine;
use blinkdb::engine::StorageEngine;
use blinkdb::protocol::binary::frame::{
    BinaryRequest, OP_GET, OP_DEL, STATUS_OK,
};
use blinkdb::protocol::binary::handler::BinaryHandler;
use blinkdb::protocol::resp::handler::RespHandler;
use blinkdb::protocol::resp::parser::RespCommand;

#[test]
fn test_integration_full_tiered_lifecycle() {
    let dir = tempdir().unwrap();
    let engine = TieredStorageEngine::open(dir.path(), 512 * 1024).unwrap();

    // 1. Ingest 500 keys
    for i in 0..500 {
        let k = Bytes::from(format!("e2e:key:{:04}", i));
        let v = Bytes::from(format!("e2e:val:{:04}", i));
        engine.set(k, v, None);
    }
    assert_eq!(engine.dbsize(), 500);

    // 2. Trigger tier eviction to LSM-Tree SSTable
    let evicted = engine.evict_cold(200);
    assert_eq!(evicted, 200);

    let stats = engine.stats();
    assert_eq!(stats.evictions, 200);

    // 3. Read an evicted key -> verify transparent promote from disk SSTable
    let cold_key = engine
        .stats()
        .hot_keys; // Just to inspect stats
    let _ = cold_key;

    // Pick an evicted key to query
    let val = engine.get(b"e2e:key:0050").expect("must find key");
    assert_eq!(val, Bytes::from("e2e:val:0050"));

    // 4. Update key
    engine.set(Bytes::from("e2e:key:0050"), Bytes::from("e2e:val:updated"), None);
    let val_updated = engine.get(b"e2e:key:0050").unwrap();
    assert_eq!(val_updated, Bytes::from("e2e:val:updated"));

    // 5. Delete key
    assert!(engine.del(b"e2e:key:0050"));
    assert!(!engine.exists(b"e2e:key:0050"));
    assert!(engine.get(b"e2e:key:0050").is_none());
}

#[test]
fn test_integration_dual_protocol_interaction() {
    let dir = tempdir().unwrap();
    let engine = Arc::new(TieredStorageEngine::open(dir.path(), 1024 * 1024).unwrap());

    let resp_handler = RespHandler::new(engine.clone());
    let bin_handler = BinaryHandler::new(engine.clone());

    // Write via RESP3
    let mut resp_out = Vec::new();
    let set_cmd = RespCommand::Set {
        key: Bytes::from("shared_key"),
        value: Bytes::from("shared_val_123"),
        ttl_ms: None,
        nx: false,
        xx: false,
    };
    resp_handler.handle_command(set_cmd, &mut resp_out);
    assert_eq!(&resp_out[..], b"+OK\r\n");

    // Read via Native Binary Protocol
    let mut bin_out = bytes::BytesMut::new();
    let bin_get = BinaryRequest {
        opcode: OP_GET,
        flags: 0,
        req_id: 999,
        ttl_secs: 0,
        key: Bytes::from("shared_key"),
        value: Bytes::new(),
    };
    bin_handler.handle_request(bin_get, &mut bin_out);
    assert!(bin_out.len() >= 13);
    assert_eq!(&bin_out[13..], b"shared_val_123");

    // Delete via Binary Protocol
    bin_out.clear();
    let bin_del = BinaryRequest {
        opcode: OP_DEL,
        flags: 0,
        req_id: 1000,
        ttl_secs: 0,
        key: Bytes::from("shared_key"),
        value: Bytes::new(),
    };
    bin_handler.handle_request(bin_del, &mut bin_out);
    assert_eq!(bin_out[2], STATUS_OK);

    // Verify gone in RESP3
    resp_out.clear();
    resp_handler.handle_command(RespCommand::Get(Bytes::from("shared_key")), &mut resp_out);
    assert_eq!(&resp_out[..], b"$-1\r\n");
}

#[test]
fn test_integration_placement_driver_clustering() {
    let raft = Arc::new(RaftEngine::new(1, "127.0.0.1:6379".to_string()));
    let pd = PlacementDriver::new(raft.clone(), 50);

    // Initial region covers 0..=16383 on node 1
    let topo = pd.get_cluster_topology();
    assert_eq!(topo.len(), 1);
    assert_eq!(topo[0].start_slot, 0);
    assert_eq!(topo[0].end_slot, 16383);

    // Node 1 reports region 1 has 80 keys (> 50 threshold)
    let hb1 = HeartbeatReport {
        node_id: 1,
        addr: "127.0.0.1:6379".to_string(),
        capacity_mb: 4096,
        used_mb: 10,
        regions: vec![RegionReport {
            region_id: 1,
            key_count: 80,
            byte_size: 4096,
            qps: 1000,
        }],
    };
    let actions = pd.handle_heartbeat(hb1, 100);
    assert_eq!(actions.len(), 1);

    // Check topology now has 2 regions
    let topo2 = pd.get_cluster_topology();
    assert_eq!(topo2.len(), 2);
    assert_eq!(topo2[0].start_slot, 0);
    assert_eq!(topo2[0].end_slot, 8191);
    assert_eq!(topo2[1].start_slot, 8192);
    assert_eq!(topo2[1].end_slot, 16383);

    // Node 2 joins cluster
    let hb2 = HeartbeatReport {
        node_id: 2,
        addr: "127.0.0.1:6380".to_string(),
        capacity_mb: 4096,
        used_mb: 0,
        regions: Vec::new(),
    };
    let actions2 = pd.handle_heartbeat(hb2, 200);
    assert_eq!(actions2.len(), 1);

    // Check router redirection for slot 12182 ("foo")
    let router = ClusterRouter::new(1, raft);
    match router.route_key(b"foo") {
        RouteDecision::Moved { slot, target_node_id, target_addr } => {
            assert_eq!(slot, 12182);
            assert_eq!(target_node_id, 2);
            assert_eq!(target_addr, "127.0.0.1:6380");
        }
        _ => panic!("Expected Moved redirection for key 'foo'"),
    }
}
