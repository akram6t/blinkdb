#[derive(Clone, Debug)]
pub struct Config {
    pub resp_port: u16,
    pub binary_port: u16,
    pub threads: usize,
    pub data_dir: String,
    pub max_memory_mb: usize,
    pub cluster_enabled: bool,
    pub node_id: u64,
    pub peers: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            resp_port: 6379,
            binary_port: 7379,
            threads: 0, // Auto-detect available cores
            data_dir: "./data".to_string(),
            max_memory_mb: 1024,
            cluster_enabled: false,
            node_id: 1,
            peers: Vec::new(),
        }
    }
}
