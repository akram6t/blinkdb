use log::{info, warn};
use std::collections::HashSet;

/// Retrieves CPU core IDs ordered with physical cores first, followed by SMT hyperthread siblings.
pub fn get_physical_first_core_ids() -> Vec<core_affinity::CoreId> {
    let ids = match core_affinity::get_core_ids() {
        Some(ids) if !ids.is_empty() => ids,
        _ => return Vec::new(),
    };

    let mut primary_cpus = Vec::new();
    let mut secondary_cpus = Vec::new();
    let mut seen_cores = HashSet::new();

    for id in &ids {
        let path = format!("/sys/devices/system/cpu/cpu{}/topology/core_id", id.id);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(core_id) = content.trim().parse::<u32>() {
                if seen_cores.insert(core_id) {
                    primary_cpus.push(*id);
                } else {
                    secondary_cpus.push(*id);
                }
                continue;
            }
        }
        primary_cpus.push(*id);
    }

    primary_cpus.extend(secondary_cpus);
    primary_cpus
}

/// Pins the current thread to a distinct physical CPU core according to shard index.
pub fn pin_to_core(shard_index: usize) -> bool {
    let core_ids = get_physical_first_core_ids();
    if core_ids.is_empty() {
        warn!("Failed to retrieve CPU core topology from OS");
        return false;
    }

    let target_core = if shard_index < core_ids.len() {
        core_ids[shard_index]
    } else {
        core_ids[shard_index % core_ids.len()]
    };

    if core_affinity::set_for_current(target_core) {
        info!("Worker shard {} successfully pinned to CPU core {}", shard_index, target_core.id);
        true
    } else {
        warn!("Failed to pin worker shard {} to CPU core {}", shard_index, target_core.id);
        false
    }
}

/// Returns the number of physical/logical CPU cores available.
pub fn available_cores() -> usize {
    core_affinity::get_core_ids()
        .map(|ids| ids.len())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_affinity() {
        let count = available_cores();
        assert!(count >= 1);
        let pinned = pin_to_core(0);
        assert!(pinned);
    }

    #[test]
    fn test_physical_first_ordering() {
        let cores = get_physical_first_core_ids();
        assert!(!cores.is_empty());
    }
}
