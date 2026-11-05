use std::collections::BTreeMap;
use parking_lot::RwLock;
use crate::cluster::raft::message::{
    AppendEntriesArgs, AppendEntriesReply, LogEntry, RaftCommand, RequestVoteArgs, RequestVoteReply,
};
use crate::cluster::slot::{Region, TOTAL_SLOTS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RaftRole {
    Follower,
    Candidate,
    Leader,
}

#[derive(Clone, Debug)]
pub struct ClusterMetadata {
    pub nodes: BTreeMap<u64, String>,
    pub regions: Vec<Region>,
    pub epoch: u64,
}

impl Default for ClusterMetadata {
    fn default() -> Self {
        Self {
            nodes: BTreeMap::new(),
            regions: vec![Region::new(1, 0, TOTAL_SLOTS - 1, 1)],
            epoch: 1,
        }
    }
}

impl ClusterMetadata {
    pub fn find_region_for_slot(&self, slot: u16) -> Option<&Region> {
        self.regions.iter().find(|r| r.contains_slot(slot))
    }

    pub fn apply_command(&mut self, cmd: &RaftCommand) {
        match cmd {
            RaftCommand::RegisterNode { node_id, addr } => {
                self.nodes.insert(*node_id, addr.clone());
                self.epoch += 1;
            }
            RaftCommand::RemoveNode { node_id } => {
                self.nodes.remove(node_id);
                self.epoch += 1;
            }
            RaftCommand::AssignRegion(region) => {
                self.regions.retain(|r| r.id != region.id);
                self.regions.push(region.clone());
                self.regions.sort_by_key(|r| r.start_slot);
                self.epoch += 1;
            }
            RaftCommand::SplitRegion { region_id, split_slot, new_region_id } => {
                if let Some(pos) = self.regions.iter().position(|r| r.id == *region_id) {
                    let old = &self.regions[pos];
                    if *split_slot > old.start_slot && *split_slot <= old.end_slot {
                        let reg1 = Region::new(old.id, old.start_slot, split_slot - 1, old.leader_node);
                        let reg2 = Region::new(*new_region_id, *split_slot, old.end_slot, old.leader_node);
                        self.regions.remove(pos);
                        self.regions.push(reg1);
                        self.regions.push(reg2);
                        self.regions.sort_by_key(|r| r.start_slot);
                        self.epoch += 1;
                    }
                }
            }
            RaftCommand::MigrateRegion { region_id, target_node_id } => {
                if let Some(reg) = self.regions.iter_mut().find(|r| r.id == *region_id) {
                    reg.leader_node = *target_node_id;
                    if !reg.peers.contains(target_node_id) {
                        reg.peers.push(*target_node_id);
                    }
                    self.epoch += 1;
                }
            }
        }
    }
}

pub struct RaftEngine {
    pub node_id: u64,
    pub term: RwLock<u64>,
    pub voted_for: RwLock<Option<u64>>,
    pub role: RwLock<RaftRole>,
    pub log: RwLock<Vec<LogEntry>>,
    pub commit_index: RwLock<u64>,
    pub last_applied: RwLock<u64>,
    pub metadata: RwLock<ClusterMetadata>,
}

impl RaftEngine {
    pub fn new(node_id: u64, initial_addr: String) -> Self {
        let mut meta = ClusterMetadata::default();
        meta.nodes.insert(node_id, initial_addr);

        Self {
            node_id,
            term: RwLock::new(1),
            voted_for: RwLock::new(Some(node_id)),
            role: RwLock::new(RaftRole::Leader), // Single node starts as leader
            log: RwLock::new(Vec::new()),
            commit_index: RwLock::new(0),
            last_applied: RwLock::new(0),
            metadata: RwLock::new(meta),
        }
    }

    pub fn is_leader(&self) -> bool {
        *self.role.read() == RaftRole::Leader
    }

    pub fn handle_request_vote(&self, args: RequestVoteArgs) -> RequestVoteReply {
        let mut curr_term = self.term.write();
        let mut voted_for = self.voted_for.write();
        let mut role = self.role.write();

        if args.term > *curr_term {
            *curr_term = args.term;
            *voted_for = None;
            *role = RaftRole::Follower;
        }

        let mut vote_granted = false;
        if args.term == *curr_term && (voted_for.is_none() || *voted_for == Some(args.candidate_id)) {
            let log = self.log.read();
            let last_index = log.last().map(|e| e.index).unwrap_or(0);
            let last_term = log.last().map(|e| e.term).unwrap_or(0);

            let log_ok = args.last_log_term > last_term
                || (args.last_log_term == last_term && args.last_log_index >= last_index);

            if log_ok {
                vote_granted = true;
                *voted_for = Some(args.candidate_id);
            }
        }

        RequestVoteReply {
            term: *curr_term,
            vote_granted,
        }
    }

    pub fn handle_append_entries(&self, args: AppendEntriesArgs) -> AppendEntriesReply {
        let mut curr_term = self.term.write();
        let mut role = self.role.write();
        let mut log = self.log.write();

        if args.term > *curr_term {
            *curr_term = args.term;
            *self.voted_for.write() = None;
            *role = RaftRole::Follower;
        }

        if args.term < *curr_term {
            return AppendEntriesReply {
                term: *curr_term,
                success: false,
                match_index: 0,
            };
        }

        *role = RaftRole::Follower;

        // Check log consistency at prev_log_index
        if args.prev_log_index > 0 {
            let prev_entry = log.iter().find(|e| e.index == args.prev_log_index);
            match prev_entry {
                Some(e) if e.term != args.prev_log_term => {
                    return AppendEntriesReply {
                        term: *curr_term,
                        success: false,
                        match_index: 0,
                    };
                }
                None => {
                    return AppendEntriesReply {
                        term: *curr_term,
                        success: false,
                        match_index: 0,
                    };
                }
                _ => {}
            }
        }

        // Append new entries
        for entry in args.entries {
            if let Some(pos) = log.iter().position(|e| e.index == entry.index) {
                if log[pos].term != entry.term {
                    log.truncate(pos);
                    log.push(entry);
                }
            } else {
                log.push(entry);
            }
        }

        let match_index = log.last().map(|e| e.index).unwrap_or(0);

        // Update commit index
        if args.leader_commit > *self.commit_index.read() {
            let mut ci = self.commit_index.write();
            *ci = args.leader_commit.min(match_index);
            self.apply_committed_entries(&log, *ci);
        }

        AppendEntriesReply {
            term: *curr_term,
            success: true,
            match_index,
        }
    }

    pub fn propose(&self, cmd: RaftCommand) -> Result<u64, String> {
        if !self.is_leader() {
            return Err("ERR Not cluster leader".to_string());
        }

        let mut log = self.log.write();
        let next_index = log.last().map(|e| e.index + 1).unwrap_or(1);
        let term = *self.term.read();

        let entry = LogEntry {
            term,
            index: next_index,
            command: cmd,
        };

        log.push(entry);

        // In single-node / local leader mode, immediately advance commit
        let mut ci = self.commit_index.write();
        *ci = next_index;
        self.apply_committed_entries(&log, *ci);

        Ok(next_index)
    }

    fn apply_committed_entries(&self, log: &[LogEntry], commit_index: u64) {
        let mut last_applied = self.last_applied.write();
        let mut meta = self.metadata.write();

        while *last_applied < commit_index {
            let target_idx = *last_applied + 1;
            if let Some(entry) = log.iter().find(|e| e.index == target_idx) {
                meta.apply_command(&entry.command);
                *last_applied = target_idx;
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raft_propose_and_metadata() {
        let raft = RaftEngine::new(1, "127.0.0.1:6379".to_string());
        assert!(raft.is_leader());

        // Propose a new node
        let cmd = RaftCommand::RegisterNode {
            node_id: 2,
            addr: "127.0.0.1:6380".to_string(),
        };
        let idx = raft.propose(cmd).unwrap();
        assert_eq!(idx, 1);

        {
            let meta = raft.metadata.read();
            assert_eq!(meta.nodes.len(), 2);
            assert_eq!(meta.nodes.get(&2), Some(&"127.0.0.1:6380".to_string()));
        }

        // Propose region split
        let split_cmd = RaftCommand::SplitRegion {
            region_id: 1,
            split_slot: 8192,
            new_region_id: 2,
        };
        raft.propose(split_cmd).unwrap();

        {
            let meta2 = raft.metadata.read();
            assert_eq!(meta2.regions.len(), 2);
            assert_eq!(meta2.regions[0].start_slot, 0);
            assert_eq!(meta2.regions[0].end_slot, 8191);
            assert_eq!(meta2.regions[1].start_slot, 8192);
            assert_eq!(meta2.regions[1].end_slot, 16383);
        }
    }
}
