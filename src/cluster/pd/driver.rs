use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::cluster::raft::message::RaftCommand;
use crate::cluster::raft::state_machine::RaftEngine;
use crate::cluster::slot::Region;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegionReport {
    pub region_id: u64,
    pub key_count: usize,
    pub byte_size: usize,
    pub qps: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeartbeatReport {
    pub node_id: u64,
    pub addr: String,
    pub capacity_mb: u64,
    pub used_mb: u64,
    pub regions: Vec<RegionReport>,
}

pub struct PlacementDriver {
    raft: Arc<RaftEngine>,
    next_region_id: AtomicU64,
    node_heartbeats: RwLock<BTreeMap<u64, (HeartbeatReport, u64)>>, // report, last_heartbeat_ms
    max_region_keys: usize,
}

impl PlacementDriver {
    pub fn new(raft: Arc<RaftEngine>, max_region_keys: usize) -> Self {
        Self {
            raft,
            next_region_id: AtomicU64::new(100),
            node_heartbeats: RwLock::new(BTreeMap::new()),
            max_region_keys: max_region_keys.max(1),
        }
    }

    /// Receives a heartbeat from a node and evaluates auto-sharding policies
    pub fn handle_heartbeat(&self, report: HeartbeatReport, now_ms: u64) -> Vec<RaftCommand> {
        let node_id = report.node_id;
        let addr = report.addr.clone();

        // 1. Auto-register node if unknown to Raft metadata (release read lock before proposing)
        let needs_register = {
            let meta = self.raft.metadata.read();
            !meta.nodes.contains_key(&node_id)
        };

        if needs_register {
            let _ = self.raft.propose(RaftCommand::RegisterNode {
                node_id,
                addr: addr.clone(),
            });
        }

        // 2. Update heartbeat tracker
        {
            let mut hb = self.node_heartbeats.write();
            hb.insert(node_id, (report.clone(), now_ms));
        }

        let mut planned_actions = Vec::new();

        // 3. Evaluate Auto-Split for oversized regions (extract plan then release read lock)
        let regions_to_split: Vec<(u64, u16)> = {
            let meta = self.raft.metadata.read();
            let mut splits = Vec::new();
            for reg_report in &report.regions {
                if reg_report.key_count >= self.max_region_keys {
                    if let Some(reg) = meta.regions.iter().find(|r| r.id == reg_report.region_id) {
                        if reg.slot_count() > 1 {
                            let split_slot = reg.start_slot + (reg.slot_count() / 2);
                            splits.push((reg.id, split_slot));
                        }
                    }
                }
            }
            splits
        };

        for (reg_id, split_slot) in regions_to_split {
            let new_id = self.next_region_id.fetch_add(1, Ordering::SeqCst);
            let cmd = RaftCommand::SplitRegion {
                region_id: reg_id,
                split_slot,
                new_region_id: new_id,
            };
            let _ = self.raft.propose(cmd.clone());
            planned_actions.push(cmd);
        }

        // 4. Evaluate Auto-Rebalance across nodes (extract plan then release read lock)
        let migration_to_plan: Option<(u64, u64)> = {
            let meta = self.raft.metadata.read();
            let active_node_count = meta.nodes.len();
            if active_node_count > 1 {
                let mut node_region_counts: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
                for nid in meta.nodes.keys() {
                    node_region_counts.insert(*nid, Vec::new());
                }
                for reg in &meta.regions {
                    node_region_counts.entry(reg.leader_node).or_default().push(reg.id);
                }

                if let (Some((&max_node, max_regs)), Some((&min_node, min_regs))) = (
                    node_region_counts.iter().max_by_key(|(_, v)| v.len()),
                    node_region_counts.iter().min_by_key(|(_, v)| v.len()),
                ) {
                    if max_node != min_node && max_regs.len() > min_regs.len() + 1 {
                        max_regs.last().map(|&reg_id| (reg_id, min_node))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some((reg_id, target_node)) = migration_to_plan {
            let cmd = RaftCommand::MigrateRegion {
                region_id: reg_id,
                target_node_id: target_node,
            };
            let _ = self.raft.propose(cmd.clone());
            planned_actions.push(cmd);
        }

        planned_actions
    }

    pub fn get_cluster_topology(&self) -> Vec<Region> {
        self.raft.metadata.read().regions.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pd_auto_split_and_rebalance() {
        let raft = Arc::new(RaftEngine::new(1, "127.0.0.1:6379".to_string()));
        let pd = PlacementDriver::new(raft.clone(), 100);

        // Heartbeat with oversized region -> triggers auto-split
        let hb1 = HeartbeatReport {
            node_id: 1,
            addr: "127.0.0.1:6379".to_string(),
            capacity_mb: 4096,
            used_mb: 100,
            regions: vec![RegionReport {
                region_id: 1,
                key_count: 150, // exceeds max_region_keys = 100
                byte_size: 1024 * 1024,
                qps: 5000,
            }],
        };

        let actions = pd.handle_heartbeat(hb1, 1000);
        assert!(!actions.is_empty());
        match &actions[0] {
            RaftCommand::SplitRegion { region_id, .. } => assert_eq!(*region_id, 1),
            _ => panic!("Expected SplitRegion"),
        }

        // Verify raft metadata updated with 2 regions
        {
            let meta = raft.metadata.read();
            assert_eq!(meta.regions.len(), 2);
        }

        // New node 2 joins cluster
        let hb2 = HeartbeatReport {
            node_id: 2,
            addr: "127.0.0.1:6380".to_string(),
            capacity_mb: 4096,
            used_mb: 0,
            regions: Vec::new(),
        };

        let actions2 = pd.handle_heartbeat(hb2, 2000);
        // PD should auto-rebalance region from node 1 to node 2!
        assert!(!actions2.is_empty());
        match &actions2[0] {
            RaftCommand::MigrateRegion { target_node_id, .. } => assert_eq!(*target_node_id, 2),
            _ => panic!("Expected MigrateRegion to node 2"),
        }
    }
}
