use std::time::Duration;
use tokio::sync::{broadcast, watch};

use crate::bridge::BridgeState;
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
                print_status(&state);
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

fn print_status(state: &BridgeState) {
    use std::io::Write;

    let mode_icon = match state.sync_mode {
        SyncMode::Master => "CDJ→Link",
        SyncMode::Slave => "Link→CDJ",
        SyncMode::Bidirectional => "CDJ⇄Link",
    };

    let playing = if state.is_playing { "▶" } else { "⏸" };

    let master = state
        .master_device
        .map(|d| format!("P{d}"))
        .unwrap_or_else(|| "---".to_string());

    let phase_bar = render_phase_bar(state.beat_phase, state.quantum);

    let drift = state.prodjlink_bpm - state.link_bpm;
    let sync_indicator = if drift.abs() > 0.1 {
        format!("⚠ drift {drift:.1}")
    } else {
        "✓ synced".to_string()
    };

    print!(
        "\r  {playing} {:.1} BPM │ {mode_icon} │ Master: {master} │ Link: {} peer{} │ Phase: {phase_bar} │ {sync_indicator}   ",
        state.prodjlink_bpm,
        state.link_peers,
        if state.link_peers == 1 { "" } else { "s" },
    );

    std::io::stdout().flush().ok();
}

/// Render a simple ASCII phase bar like `[█░░░]` for a 4-beat quantum.
fn render_phase_bar(phase: f64, quantum: f64) -> String {
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

    #[test]
    fn phase_bar_first_beat() {
        assert_eq!(render_phase_bar(0.0, 4.0), "[█░░░]");
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
    fn default_state() {
        let s = BridgeState::default();
        assert!(!s.is_playing);
        assert_eq!(s.link_peers, 0);
        assert_eq!(s.quantum, 4.0);
        assert_eq!(s.prodjlink_bpm, 0.0);
        assert_eq!(s.link_bpm, 0.0);
        assert_eq!(s.beat_phase, 0.0);
        assert!(s.master_device.is_none());
        assert!(matches!(s.sync_mode, SyncMode::Master));
    }

    #[test]
    fn phase_bar_second_beat() {
        assert_eq!(render_phase_bar(1.0, 4.0), "[░█░░]");
    }

    #[test]
    fn phase_bar_single_beat_quantum() {
        assert_eq!(render_phase_bar(0.0, 1.0), "[█]");
    }

    #[test]
    fn phase_bar_large_quantum() {
        let bar = render_phase_bar(5.0, 8.0);
        assert_eq!(bar, "[░░░░░█░░]");
        assert_eq!(bar.len(), "[░░░░░█░░]".len());
    }

    #[test]
    fn phase_bar_phase_exceeds_quantum_clamps() {
        // phase >= quantum should clamp to last beat
        let bar = render_phase_bar(5.0, 4.0);
        assert_eq!(bar, "[░░░█]");
    }

    #[test]
    fn phase_bar_negative_phase() {
        // Negative phase floors to 0 via usize conversion clamping
        let bar = render_phase_bar(-1.0, 4.0);
        // -1.0 floors to -1, as usize wraps — .min() clamps to last beat
        assert_eq!(bar.len(), "[░░░░]".len());
    }

    #[test]
    fn phase_bar_fractional_phase() {
        // 1.99 floors to 1
        assert_eq!(render_phase_bar(1.99, 4.0), "[░█░░]");
    }
}
