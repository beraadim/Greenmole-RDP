mod config;
mod metrics_recorder;

use anyhow::{Context, Result};
use clap::Parser;
use macrdp_core::{ConnectionEvent, ServerEventHandler, ServerStatus};
use std::io::{IsTerminal, Write};

struct CliEvents {
    recorder: std::sync::Mutex<Option<metrics_recorder::MetricsRecorder>>,
}

impl ServerEventHandler for CliEvents {
    fn on_performance_summary(&self, report: String) {
        println!("{report}");
    }
    fn on_metrics(&self, metrics: macrdp_core::Metrics) {
        let mut recorder = self.recorder.lock().unwrap();
        if let Some(active) = recorder.as_mut() {
            if let Err(error) = active.record(&metrics) {
                tracing::error!(%error, "Benchmark metrics recording stopped");
                *recorder = None;
            }
        }
    }
    fn on_status_change(&self, status: ServerStatus) {
        tracing::info!(state = %status.state, port = status.port, "Server status changed");
    }

    fn on_connection(&self, event: ConnectionEvent) {
        tracing::info!(?event, "Connection changed");
    }
}

fn main() -> Result<()> {
    if let Some(result) = macrdp_core::run_guard_helper_if_requested() {
        return result;
    }
    // Display helpers must dispatch before any framework queries the desktop.
    if let Some(result) = macrdp_core::run_display_helper_if_requested() {
        return result;
    }
    if let Some(result) = macrdp_core::run_server_helper_if_requested() {
        return result;
    }
    run()
}

#[tokio::main]
async fn run() -> Result<()> {
    let cli = config::Cli::parse();
    let mut config = cli.load_config()?;
    let log_level = config.log_level.as_deref().unwrap_or("info");
    let filter = macrdp_core::with_transition_diagnostics(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
    );
    let log_path = macrdp_core::init_log_file();
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(macrdp_core::LogBridgeLayer::new())
        .init();
    tracing::info!(path = %log_path.display(), "Diagnostic log initialized");

    if config.username.is_none() && config.password.is_none() {
        // Never send generated credentials to service logs or redirected output.
        anyhow::ensure!(std::io::stderr().is_terminal(),
            "Set username and password in a protected configuration file before starting unattended");
        let password = macrdp_core::credentials::generate_password()?;
        let mut terminal = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .context(
                "Cannot display one-time credentials; configure username and password explicitly",
            )?;
        writeln!(
            terminal,
            "One-time RDP credentials: username macrdp, password {password}"
        )?;
        config.username = Some("macrdp".into());
        config.password = Some(password);
    }

    let events = CliEvents {
        recorder: std::sync::Mutex::new(
            cli.metrics_jsonl
                .as_deref()
                .map(metrics_recorder::MetricsRecorder::create)
                .transpose()?,
        ),
    };
    let server = macrdp_core::start_supervised_server_with_perf(config, events, cli.perf).await?;
    tracing::info!(port = server.port(), "RDP server ready");
    let result = tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("Failed to wait for Ctrl-C"),
        result = server.wait_until_stopped() => result,
    };
    // Keep the owner alive until native restoration completes. Exiting merely
    // because the bounded UI-facing Stop timed out could strand the desktop.
    let stopped = loop {
        match server.stop().await {
            Ok(macrdp_core::StopOutcome::Complete) => break Ok(()),
            Ok(macrdp_core::StopOutcome::Pending { reason }) => {
                tracing::warn!(reason, "Shutdown pending; waiting for native cleanup");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(error) => break Err(error),
        }
    };
    let _ = tokio::task::spawn_blocking(macrdp_core::flush_logs).await;
    result.and(stopped)
}
