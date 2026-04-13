use std::time::Duration;
use tokio::sync::{broadcast, watch};

use crate::bridge::{
    compute_drift, format_master_device, format_sync_indicator, BridgeState,
};
use crate::config::SyncMode;

/// Run the status display loop.
/// Prints a compact status line at the configured interval, using `\r` to
/// overwrite in-place — efficient for headless Pi terminals.
pub async fn run_status_display(
    mut state_rx: watch::Receiver<BridgeState>,
    interval: Duration,
    mut shutdown: broadcast::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(interval);

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║                    B E A T B R I D G E                  ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let state = state_rx.borrow().clone();
                let line = format_status_line(&state);
                print!("\r{line}   ");
                std::io::Write::flush(&mut std::io::stdout()).ok();
            }
            _ = state_rx.changed() => {
                // State changed — will print on next tick
            }
            _ = shutdown.recv() => {
                println!("\n🛑 Shutting down...");
                break;
            }
        }
    }
}

/// Format a single status line from the bridge state.
/// Returns a String so it can be tested without side effects.
pub fn format_status_line(state: &BridgeState) -> String {
    let mode_icon = match state.sync_mode {
        SyncMode::Master => "CDJ→Link",
        SyncMode::Slave => "Link→CDJ",
        SyncMode::Bidirectional => "CDJ⇄Link",
    };

    let playing = if state.is_playing { "▶" } else { "⏸" };
    let master = format_master_device(state.master_device);
    let phase_bar = render_phase_bar(state.beat_phase, state.quantum);
    let drift = compute_drift(state.prodjlink_bpm, state.link_bpm);
    let sync_indicator = format_sync_indicator(drift);

    format!(
        "  {playing} {:.1} BPM │ {mode_icon} │ Master: {master} │ Link: {} peer{} │ Phase: {phase_bar} │ {sync_indicator}",
        state.prodjlink_bpm,
        state.link_peers,
        if state.link_peers == 1 { "" } else { "s" },
    )
}

/// Render a simple ASCII phase bar like `[█░░░]` for a 4-beat quantum.
pub fn render_phase_bar(phase: f64, quantum: f64) -> String {
    let beats = quantum as usize;
    if beats == 0 {
        return String::new();
    }
    let current = (phase.floor() as usize).min(beats.saturating_sub(1));
    let mut bar = String::with_capacity(beats + 2);
    bar.push('[');
    for i in 0..beats {
        if i == current {
            bar.push('█');
        } else {
            bar.push('░');
        }
    }
    bar.push(']');
    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    // ================================================================
    // render_phase_bar
    // ================================================================

    #[test]
    fn phase_bar_first_beat() {
        assert_eq!(render_phase_bar(0.0, 4.0), "[█░░░]");
    }

    #[test]
    fn phase_bar_second_beat() {
        assert_eq!(render_phase_bar(1.0, 4.0), "[░█░░]");
    }

    #[test]
    fn phase_bar_third_beat() {
        assert_eq!(render_phase_bar(2.5, 4.0), "[░░█░]");
    }

    #[test]
    fn phase_bar_last_beat() {
        assert_eq!(render_phase_bar(3.9, 4.0), "[░░░█]");
    }

    #[test]
    fn phase_bar_zero_quantum() {
        assert_eq!(render_phase_bar(0.0, 0.0), "");
    }

    #[test]
    fn phase_bar_single_beat_quantum() {
        assert_eq!(render_phase_bar(0.0, 1.0), "[█]");
    }

    #[test]
    fn phase_bar_large_quantum() {
        let bar = render_phase_bar(5.0, 8.0);
        assert_eq!(bar, "[░░░░░█░░]");
    }

    #[test]
    fn phase_bar_phase_exceeds_quantum_clamps() {
        let bar = render_phase_bar(5.0, 4.0);
        assert_eq!(bar, "[░░░█]");
    }

    #[test]
    fn phase_bar_negative_phase() {
        let bar = render_phase_bar(-1.0, 4.0);
        assert_eq!(bar.len(), "[░░░░]".len());
    }

    #[test]
    fn phase_bar_fractional_phase() {
        assert_eq!(render_phase_bar(1.99, 4.0), "[░█░░]");
    }

    // ================================================================
    // format_status_line — full status string tests
    // ================================================================

    #[test]
    fn status_line_default_state() {
        let state = BridgeState::default();
        let line = format_status_line(&state);
        assert!(line.contains("0.0 BPM"));
        assert!(line.contains("CDJ→Link")); // default is Master
        assert!(line.contains("Master: ---")); // no master device
        assert!(line.contains("0 peers")); // plural
        assert!(line.contains("✓ synced")); // 0.0 - 0.0 = 0.0 drift
        assert!(line.contains("⏸")); // not playing
    }

    #[test]
    fn status_line_playing_with_master() {
        let state = BridgeState {
            prodjlink_bpm: 128.0,
            link_bpm: 128.0,
            link_peers: 3,
            beat_phase: 2.0,
            quantum: 4.0,
            is_playing: true,
            sync_mode: SyncMode::Master,
            master_device: Some(1),
        };
        let line = format_status_line(&state);
        assert!(line.contains("128.0 BPM"));
        assert!(line.contains("▶")); // playing
        assert!(line.contains("Master: P1"));
        assert!(line.contains("3 peers")); // plural
        assert!(line.contains("✓ synced"));
        assert!(line.contains("[░░█░]")); // phase bar beat 3
    }

    #[test]
    fn status_line_single_peer() {
        let state = BridgeState {
            link_peers: 1,
            ..BridgeState::default()
        };
        let line = format_status_line(&state);
        assert!(line.contains("1 peer")); // singular
        assert!(!line.contains("1 peers")); // NOT plural
    }

    #[test]
    fn status_line_drift_warning() {
        let state = BridgeState {
            prodjlink_bpm: 130.0,
            link_bpm: 128.0,
            ..BridgeState::default()
        };
        let line = format_status_line(&state);
        assert!(line.contains("⚠ drift 2.0"));
    }

    #[test]
    fn status_line_negative_drift() {
        let state = BridgeState {
            prodjlink_bpm: 126.5,
            link_bpm: 128.0,
            ..BridgeState::default()
        };
        let line = format_status_line(&state);
        assert!(line.contains("⚠ drift -1.5"));
    }

    #[test]
    fn status_line_slave_mode() {
        let state = BridgeState {
            sync_mode: SyncMode::Slave,
            ..BridgeState::default()
        };
        let line = format_status_line(&state);
        assert!(line.contains("Link→CDJ"));
    }

    #[test]
    fn status_line_bidirectional_mode() {
        let state = BridgeState {
            sync_mode: SyncMode::Bidirectional,
            ..BridgeState::default()
        };
        let line = format_status_line(&state);
        assert!(line.contains("CDJ⇄Link"));
    }

    #[test]
    fn status_line_high_bpm() {
        let state = BridgeState {
            prodjlink_bpm: 174.5,
            link_bpm: 174.5,
            is_playing: true,
            master_device: Some(2),
            ..BridgeState::default()
        };
        let line = format_status_line(&state);
        assert!(line.contains("174.5 BPM"));
        assert!(line.contains("Master: P2"));
    }

    #[test]
    fn status_line_zero_peers() {
        let state = BridgeState {
            link_peers: 0,
            ..BridgeState::default()
        };
        let line = format_status_line(&state);
        assert!(line.contains("0 peers")); // plural for 0
    }
}
