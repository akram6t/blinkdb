pub mod core;
pub mod engine;
pub mod protocol;
pub mod cluster;
pub mod config;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
