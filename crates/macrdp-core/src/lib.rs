//! macrdp-core: unified RDP server library

pub mod bitrate_controller;
pub mod callbacks;
pub mod config;
pub mod display;
pub mod handler;
pub mod log_bridge;
pub mod perf_stats;
pub mod permissions;
pub mod server;
pub mod tls;

pub use callbacks::*;
pub use config::{config_dir, AudioConfig, ClipboardConfig, ServerConfig};
pub use log_bridge::{init_log_file, log_file_path, LogBridgeLayer};
pub use server::{resolve_resolution, start_server, ServerHandle};
