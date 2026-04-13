mod bridge;
mod config;
mod status;

use std::time::Duration;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load config (CLI args + optional TOML file)
    let cfg = config::BridgeConfig::load()?;

    // Initialize tracing/logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&cfg.log_level))
        .init();

    info!("BeatBridge v{}", env!("CARGO_PKG_VERSION"));
    info!("Sync mode: {:?}", cfg.sync_mode);
    info!("Quantum: {}", cfg.quantum);

    // Create BridgeEngine
    let engine = bridge::BridgeEngine::new(cfg.sync_mode, cfg.quantum, cfg.initial_bpm);
    let state_rx = engine.state_receiver();
    let shutdown_tx = engine.shutdown_handle();

    // Create Ableton Link session (engine.run() will enable it)
    info!("Starting Ableton Link at {:.1} BPM...", cfg.initial_bpm);
    let link = ableton_link_rs::link::BasicLink::new(cfg.initial_bpm).await;
    info!("Ableton Link created");

    // Create ProDjLink session
    info!("Starting Pro DJ Link...");
    let mut pdl_builder = prodjlink_rs::ProDjLink::builder()
        .device_name(&cfg.device_name)
        .device_number(cfg.device_number);

    if let Some(addr) = cfg.interface {
        pdl_builder = pdl_builder.interface_address(addr);
    }

    let pdl = pdl_builder
        .build()
        .await
        .map_err(|e| format!("Failed to start Pro DJ Link (check network interface): {e}"))?;
    info!(
        "Pro DJ Link active as '{}' (device #{})",
        cfg.device_name, cfg.device_number
    );

    // Spawn status display task
    let status_interval = Duration::from_millis(cfg.status_interval_ms);
    let shutdown_rx = shutdown_tx.subscribe();
    let status_handle = tokio::spawn(status::run_status_display(
        state_rx,
        status_interval,
        shutdown_rx,
    ));

    // Ctrl+C handler
    let ctrlc_shutdown = shutdown_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received SIGINT, shutting down...");
        let _ = ctrlc_shutdown.send(());
    });

    // Run the bridge (blocks until shutdown)
    info!("Bridge running — press Ctrl+C to stop");
    engine.run(pdl, link).await?;

    // Clean up
    status_handle.abort();
    info!("BeatBridge stopped");

    Ok(())
}
