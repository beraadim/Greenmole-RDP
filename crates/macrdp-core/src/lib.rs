//! macrdp-core: unified RDP server library

pub mod bitrate_controller;
pub mod callbacks;
pub mod config;
pub mod credentials;
pub mod display;
pub mod handler;
pub mod log_bridge;
pub mod log_filter;
pub mod native_watchdog;
pub mod perf_stats;
pub mod permissions;
pub mod server;
pub mod supervised;
pub mod tls;

pub use callbacks::*;
pub use config::{config_dir, AudioConfig, ClipboardConfig, ServerConfig};
pub use log_bridge::{flush_logs, init_log_file, log_file_path, LogBridgeLayer};
pub use log_filter::with_transition_diagnostics;
pub use macrdp_display::{
    list_owned_virtual_displays, physical_display_geometry,
    recover_stranded_windows_on_physical_display, run_display_helper_if_requested,
    wait_for_owned_virtual_display_removal, DisplayBackend, DisplayGeometry, WindowMoveReport,
};
pub use macrdp_privacy::{lock_session_and_verify, run_guard_helper_if_requested};
pub use server::{
    resolve_resolution, server_resources_pending, start_server, ServerHandle, StopOutcome,
};
pub use supervised::{
    run_server_helper_if_requested, start_supervised_server, start_supervised_server_with_perf,
    supervisor_resources_pending, SupervisedServerHandle,
};
