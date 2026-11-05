use log::{info, warn};

/// Pins the current thread to the specified CPU core ID.
pub fn pin_to_core(core_id: usize) -> bool {
    let core_ids = match core_affinity::get_core_ids() {
        Some(ids) if !ids.is_empty() => ids,
        _ => {
            warn!("Failed to retrieve CPU core topology from OS");
            return false;
        }
    };

    let target_core = if core_id < core_ids.len() {
        core_ids[core_id]
    } else {
        core_ids[core_id % core_ids.len()]
    };

    if core_affinity::set_for_current(target_core) {
        info!("Worker thread successfully pinned to CPU core {}", target_core.id);
        true
    } else {
        warn!("Failed to pin thread to CPU core {}", target_core.id);
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
}
