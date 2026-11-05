use std::sync::Arc;
use crate::cluster::raft::state_machine::RaftEngine;
use crate::cluster::slot::key_slot;

pub enum RouteDecision {
    Local,
    Moved {
        slot: u16,
        target_node_id: u64,
        target_addr: String,
    },
}

#[derive(Clone)]
pub struct ClusterRouter {
    local_node_id: u64,
    raft: Arc<RaftEngine>,
}

impl ClusterRouter {
    pub fn new(local_node_id: u64, raft: Arc<RaftEngine>) -> Self {
        Self {
            local_node_id,
            raft,
        }
    }

    pub fn route_key(&self, key: &[u8]) -> RouteDecision {
        let slot = key_slot(key);
        let meta = self.raft.metadata.read();

        if let Some(region) = meta.find_region_for_slot(slot) {
            if region.leader_node == self.local_node_id {
                RouteDecision::Local
            } else {
                let target_addr = meta
                    .nodes
                    .get(&region.leader_node)
                    .cloned()
                    .unwrap_or_else(|| "127.0.0.1:6379".to_string());

                RouteDecision::Moved {
                    slot,
                    target_node_id: region.leader_node,
                    target_addr,
                }
            }
        } else {
            RouteDecision::Local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::raft::message::RaftCommand;

    #[test]
    fn test_cluster_router_redirection() {
        let raft = Arc::new(RaftEngine::new(1, "127.0.0.1:6379".to_string()));
        let router = ClusterRouter::new(1, raft.clone());

        // Single node: all keys are local
        match router.route_key(b"foo") {
            RouteDecision::Local => {}
            _ => panic!("Expected local route"),
        }

        // Split and assign slots 8192..16383 to node 2
        raft.propose(RaftCommand::RegisterNode {
            node_id: 2,
            addr: "127.0.0.1:6380".to_string(),
        }).unwrap();

        raft.propose(RaftCommand::SplitRegion {
            region_id: 1,
            split_slot: 8192,
            new_region_id: 2,
        }).unwrap();

        raft.propose(RaftCommand::MigrateRegion {
            region_id: 2,
            target_node_id: 2,
        }).unwrap();

        // Key "foo" has slot 12182 -> belonging to Region 2 (slots 8192-16383) on Node 2!
        match router.route_key(b"foo") {
            RouteDecision::Moved { slot, target_node_id, target_addr } => {
                assert_eq!(slot, 12182);
                assert_eq!(target_node_id, 2);
                assert_eq!(target_addr, "127.0.0.1:6380");
            }
            _ => panic!("Expected Moved route for key 'foo'"),
        }
    }
}
