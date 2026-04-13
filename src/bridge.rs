use std::time::Instant;

use tokio::sync::{broadcast, watch};
use tracing::{debug, info, warn};

use ableton_link_rs::link::BasicLink;
use prodjlink_rs::{BeatEvent, Bpm, DeviceNumber, DeviceType, DeviceUpdate, ProDjLink};

use crate::config::SyncMode;

/// Minimum BPM difference before we propagate a tempo change.
/// Avoids feedback loops from floating-point rounding.
const TEMPO_EPSILON: f64 = 0.05;

/// Cooldown after our own write before we accept changes from the other side
/// (bidirectional mode echo-loop suppression).
const ECHO_GUARD_MS: u128 = 100;

/// Current bridge state exposed for status display.
#[derive(Debug, Clone)]
pub struct BridgeState {
    pub prodjlink_bpm: f64,
    pub link_bpm: f64,
    pub link_peers: usize,
    pub beat_phase: f64,
    pub quantum: f64,
    pub is_playing: bool,
    pub sync_mode: SyncMode,
    pub master_device: Option<u8>,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self {
            prodjlink_bpm: 0.0,
            link_bpm: 0.0,
            link_peers: 0,
            beat_phase: 0.0,
            quantum: 4.0,
            is_playing: false,
            sync_mode: SyncMode::Master,
            master_device: None,
        }
    }
}

pub struct BridgeEngine {
    sync_mode: SyncMode,
    quantum: f64,
    state_tx: watch::Sender<BridgeState>,
    state_rx: watch::Receiver<BridgeState>,
    shutdown_tx: broadcast::Sender<()>,
}

impl BridgeEngine {
    /// Create the engine with initial state.
    pub fn new(sync_mode: SyncMode, quantum: f64, initial_bpm: f64) -> Self {
        let initial_state = BridgeState {
            prodjlink_bpm: initial_bpm,
            link_bpm: initial_bpm,
            link_peers: 0,
            beat_phase: 0.0,
            quantum,
            is_playing: false,
            sync_mode,
            master_device: None,
        };

        let (state_tx, state_rx) = watch::channel(initial_state);
        let (shutdown_tx, _) = broadcast::channel(1);

        Self {
            sync_mode,
            quantum,
            state_tx,
            state_rx,
            shutdown_tx,
        }
    }

    /// Clone the watch receiver for status display.
    pub fn state_receiver(&self) -> watch::Receiver<BridgeState> {
        self.state_rx.clone()
    }

    /// Get a shutdown sender so callers can signal a clean stop.
    pub fn shutdown_handle(&self) -> broadcast::Sender<()> {
        self.shutdown_tx.clone()
    }

    /// Main async loop bridging ProDjLink ↔ Ableton Link.
    pub async fn run(
        self,
        pdl: ProDjLink,
        mut link: BasicLink,
    ) -> Result<(), Box<dyn std::error::Error>> {
        link.enable().await;
        link.enable_start_stop_sync(true);

        // Warn about modes that aren't fully functional yet
        match self.sync_mode {
            SyncMode::Slave | SyncMode::Bidirectional => {
                // Enable status broadcasting so CDJs see our tempo/master state
                let vcdj = pdl.virtual_cdj();
                vcdj.set_sending_status(true).await;

                // Claim tempo master and set initial BPM from Link
                let initial_bpm = link.capture_app_session_state().tempo();
                if let Err(e) = vcdj.request_master_role(Bpm(initial_bpm)).await {
                    warn!("Failed to claim master role: {e}. CDJs may not follow tempo changes.");
                } else {
                    info!(bpm = initial_bpm, "Claimed tempo master on DJ Link network");
                }
            }
            SyncMode::Master => {}
        }

        if !can_phase_sync(self.quantum) {
            info!(
                quantum = self.quantum,
                "Phase sync disabled — CDJs only report 4-beat bars; quantum != 4.0"
            );
        }

        info!(
            mode = ?self.sync_mode,
            quantum = self.quantum,
            "bridge engine started"
        );

        match self.sync_mode {
            SyncMode::Master => self.run_master(pdl, link).await,
            SyncMode::Slave => self.run_slave(pdl, link).await,
            SyncMode::Bidirectional => self.run_bidirectional(pdl, link).await,
        }
    }

    // ------------------------------------------------------------------
    // Master mode: CDJ → Link
    // ------------------------------------------------------------------

    async fn run_master(
        self,
        pdl: ProDjLink,
        mut link: BasicLink,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut beats_rx = pdl.subscribe_beats();
        let mut status_rx = pdl.subscribe_status();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        let mut last_playing = false;

        loop {
            tokio::select! {
                beat_result = beats_rx.recv() => {
                    match beat_result {
                        Ok(BeatEvent::NewBeat(beat)) => {
                            let master_dev = pdl.virtual_cdj().tempo_master().master_device();
                            // Only sync from the tempo master player
                            if master_dev.is_some_and(|d| d == beat.device_number) {
                                let cdj_bpm = beat.effective_tempo();
                                Self::sync_tempo_to_link(&mut link, cdj_bpm).await;

                                // Only sync phase from CDJs/players — mixer
                                // beat_within_bar is not musically meaningful.
                                // Also skip when quantum != 4 (CDJ only has 4-beat bars).
                                if beat.device_type != DeviceType::Mixer && can_phase_sync(self.quantum) {
                                    Self::sync_phase_to_link(
                                        &mut link,
                                        beat.beat_within_bar,
                                        self.quantum,
                                    ).await;
                                }
                            }
                            self.publish_state(&pdl, &link);
                        }
                        Ok(BeatEvent::PrecisePosition(_)) => {
                            // Precise position is informational; we sync on beats
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "beat channel lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("beat channel closed, stopping bridge");
                            break;
                        }
                    }
                }
                status_result = status_rx.recv() => {
                    match status_result {
                        Ok(DeviceUpdate::Cdj(status)) => {
                            if status.is_tempo_master() {
                                let playing = status.is_playing();
                                if playing != last_playing {
                                    last_playing = playing;
                                    let time = link.clock().micros();
                                    let mut session = link.capture_app_session_state();
                                    session.set_is_playing(playing, time);
                                    link.commit_app_session_state(session).await;
                                    debug!(playing, device = %status.device_number, "play state synced to link");
                                }
                            }
                            self.publish_state(&pdl, &link);
                        }
                        Ok(DeviceUpdate::Mixer(_)) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "status channel lagged");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            info!("status channel closed, stopping bridge");
                            break;
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("shutdown signal received");
                    break;
                }
            }
        }

        link.disable().await;
        pdl.shutdown();
        info!("bridge engine stopped");
        Ok(())
    }

    // ------------------------------------------------------------------
    // Slave mode: Link → CDJ
    // ------------------------------------------------------------------

    async fn run_slave(
        self,
        pdl: ProDjLink,
        mut link: BasicLink,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let mut last_link_bpm: f64 = link.capture_app_session_state().tempo();
        let mut last_playing = link.capture_app_session_state().is_playing();
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(20));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let session = link.capture_app_session_state();
                    let link_bpm = session.tempo();
                    let playing = session.is_playing();

                    if should_sync_tempo(last_link_bpm, link_bpm) {
                        debug!(bpm = link_bpm, "link tempo changed, relaying to CDJs");
                        last_link_bpm = link_bpm;
                        let vcdj = pdl.virtual_cdj();
                        vcdj.set_tempo(Bpm(link_bpm));
                    }

                    if playing != last_playing {
                        debug!(playing, "link play state changed, relaying to CDJs");
                        last_playing = playing;
                        let vcdj = pdl.virtual_cdj();
                        vcdj.set_playing(playing);
                        // Send fader start/stop to CDJs on channels 1-4
                        for ch in 1..=4u8 {
                            if let Err(e) = vcdj.fader_start(DeviceNumber(ch), playing).await {
                                debug!(channel = ch, error = %e, "fader_start failed");
                            }
                        }
                    }

                    self.publish_state(&pdl, &link);
                }
                _ = shutdown_rx.recv() => {
                    info!("shutdown signal received");
                    break;
                }
            }
        }

        link.disable().await;
        pdl.shutdown();
        info!("bridge engine stopped");
        Ok(())
    }

    // ------------------------------------------------------------------
    // Bidirectional mode: last writer wins
    // ------------------------------------------------------------------

    async fn run_bidirectional(
        self,
        pdl: ProDjLink,
        mut link: BasicLink,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut beats_rx = pdl.subscribe_beats();
        let mut status_rx = pdl.subscribe_status();
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        // Timestamps of our last writes — used to suppress echoes
        let mut last_cdj_to_link = Instant::now() - std::time::Duration::from_secs(1);
        let mut last_link_to_cdj = Instant::now() - std::time::Duration::from_secs(1);
        let mut last_playing = false;

        let mut link_poll = tokio::time::interval(tokio::time::Duration::from_millis(20));
        let mut prev_link_bpm: f64 = link.capture_app_session_state().tempo();

        loop {
            tokio::select! {
                beat_result = beats_rx.recv() => {
                    match beat_result {
                        Ok(BeatEvent::NewBeat(beat)) => {
                            // Suppress if we recently wrote from Link → CDJ
                            if is_echo_guarded(last_link_to_cdj, ECHO_GUARD_MS) {
                                continue;
                            }
                            let master_dev = pdl.virtual_cdj().tempo_master().master_device();
                            if master_dev.is_some_and(|d| d == beat.device_number) {
                                let cdj_bpm = beat.effective_tempo();
                                Self::sync_tempo_to_link(&mut link, cdj_bpm).await;

                                // Only sync phase from CDJs — not mixers.
                                // Skip when quantum != 4 (CDJ only has 4-beat bars).
                                if beat.device_type != DeviceType::Mixer && can_phase_sync(self.quantum) {
                                    Self::sync_phase_to_link(
                                        &mut link,
                                        beat.beat_within_bar,
                                        self.quantum,
                                    ).await;
                                }

                                last_cdj_to_link = Instant::now();
                                // Update prev_link_bpm to the value we just wrote,
                                // preventing it from being re-detected as a Link change
                                prev_link_bpm = cdj_bpm;
                            }
                            self.publish_state(&pdl, &link);
                        }
                        Ok(BeatEvent::PrecisePosition(_)) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "beat channel lagged (bidir)");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                status_result = status_rx.recv() => {
                    match status_result {
                        Ok(DeviceUpdate::Cdj(status)) => {
                            if is_echo_guarded(last_link_to_cdj, ECHO_GUARD_MS) {
                                continue;
                            }
                            if status.is_tempo_master() {
                                let playing = status.is_playing();
                                if playing != last_playing {
                                    last_playing = playing;
                                    let time = link.clock().micros();
                                    let mut session = link.capture_app_session_state();
                                    session.set_is_playing(playing, time);
                                    link.commit_app_session_state(session).await;
                                    last_cdj_to_link = Instant::now();
                                    debug!(playing, "bidir: CDJ play state → Link");
                                }
                            }
                            self.publish_state(&pdl, &link);
                        }
                        Ok(DeviceUpdate::Mixer(_)) => {}
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(skipped = n, "status channel lagged (bidir)");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = link_poll.tick() => {
                    // Suppress if we recently wrote from CDJ → Link
                    if is_echo_guarded(last_cdj_to_link, ECHO_GUARD_MS) {
                        continue;
                    }
                    let session = link.capture_app_session_state();
                    let link_bpm = session.tempo();

                    if should_sync_tempo(prev_link_bpm, link_bpm) {
                        debug!(bpm = link_bpm, "bidir: Link tempo → CDJs");
                        prev_link_bpm = link_bpm;
                        last_link_to_cdj = Instant::now();
                        let vcdj = pdl.virtual_cdj();
                        vcdj.set_tempo(Bpm(link_bpm));
                    }

                    self.publish_state(&pdl, &link);
                }
                _ = shutdown_rx.recv() => {
                    info!("shutdown signal received");
                    break;
                }
            }
        }

        link.disable().await;
        pdl.shutdown();
        info!("bridge engine stopped");
        Ok(())
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Push the CDJ tempo into Link if it differs beyond epsilon.
    async fn sync_tempo_to_link(link: &mut BasicLink, cdj_bpm: f64) {
        let link_bpm = link.capture_app_session_state().tempo();

        if should_sync_tempo(link_bpm, cdj_bpm) {
            let time = link.clock().micros();
            let mut session = link.capture_app_session_state();
            session.set_tempo(cdj_bpm, time);
            link.commit_app_session_state(session).await;
            debug!(cdj_bpm, link_bpm, "tempo synced CDJ → Link");
        }
    }

    /// Align Link phase to the CDJ beat position.
    /// CDJ `beat_within_bar` is 1-based (1–4); map to 0-based Link beat.
    async fn sync_phase_to_link(link: &mut BasicLink, beat_within_bar: u8, quantum: f64) {
        let Some(target_beat) = map_beat_to_phase(beat_within_bar) else {
            return; // unknown bar position
        };

        let time = link.clock().micros();
        let mut session = link.capture_app_session_state();
        session.force_beat_at_time(target_beat, time, quantum);
        link.commit_app_session_state(session).await;
        debug!(beat_within_bar, target_beat, "phase aligned CDJ → Link");
    }

    /// Snapshot the current state and publish via the watch channel.
    fn publish_state(&self, pdl: &ProDjLink, link: &BasicLink) {
        let tm = pdl.virtual_cdj().tempo_master();
        let master_bpm = tm.master_tempo().0;
        let master_device = tm.master_device().map(|d| d.0);

        let session = link.capture_app_session_state();
        let time = link.clock().micros();

        let state = BridgeState {
            prodjlink_bpm: master_bpm,
            link_bpm: session.tempo(),
            link_peers: link.num_peers(),
            beat_phase: session.phase_at_time(time, self.quantum),
            quantum: self.quantum,
            is_playing: session.is_playing(),
            sync_mode: self.sync_mode,
            master_device,
        };

        // Ignore send error — no receivers is fine
        let _ = self.state_tx.send(state);
    }
}

// ------------------------------------------------------------------
// Pure helper functions — testable without hardware
// ------------------------------------------------------------------

/// Determine whether a tempo change exceeds the epsilon threshold.
pub fn should_sync_tempo(current_bpm: f64, new_bpm: f64) -> bool {
    (current_bpm - new_bpm).abs() > TEMPO_EPSILON
}

/// Map a CDJ beat_within_bar (1-based, 1–4) to a 0-based Link beat position.
/// Returns `None` for invalid values (0 or >4).
pub fn map_beat_to_phase(beat_within_bar: u8) -> Option<f64> {
    if beat_within_bar == 0 || beat_within_bar > 4 {
        None
    } else {
        Some((beat_within_bar - 1) as f64)
    }
}

/// Whether phase sync is meaningful for the given quantum.
/// CDJs only report beat_within_bar as 1–4 (a 4-beat bar), so phase
/// alignment is only correct when the Link quantum is also 4.
pub fn can_phase_sync(quantum: f64) -> bool {
    (quantum - 4.0).abs() < f64::EPSILON
}

/// Determine if the echo guard is still active.
pub fn is_echo_guarded(last_write: Instant, guard_ms: u128) -> bool {
    last_write.elapsed().as_millis() < guard_ms
}

/// Format the master device for display.
pub fn format_master_device(device: Option<u8>) -> String {
    device
        .map(|d| format!("P{d}"))
        .unwrap_or_else(|| "---".to_string())
}

/// Compute the drift between ProDjLink and Link tempos.
pub fn compute_drift(prodjlink_bpm: f64, link_bpm: f64) -> f64 {
    prodjlink_bpm - link_bpm
}

/// Format the sync indicator based on drift.
pub fn format_sync_indicator(drift: f64) -> String {
    if drift.abs() > 0.1 {
        format!("⚠ drift {drift:.1}")
    } else {
        "✓ synced".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ================================================================
    // BridgeEngine constructor & channels
    // ================================================================

    #[test]
    fn engine_new_initial_state() {
        let engine = BridgeEngine::new(SyncMode::Master, 4.0, 128.0);
        assert_eq!(engine.quantum, 4.0);
        assert!(matches!(engine.sync_mode, SyncMode::Master));

        let state = engine.state_rx.borrow().clone();
        assert_eq!(state.prodjlink_bpm, 128.0);
        assert_eq!(state.link_bpm, 128.0);
        assert_eq!(state.quantum, 4.0);
        assert_eq!(state.link_peers, 0);
        assert_eq!(state.beat_phase, 0.0);
        assert!(!state.is_playing);
        assert!(state.master_device.is_none());
        assert!(matches!(state.sync_mode, SyncMode::Master));
    }

    #[test]
    fn engine_new_slave_mode() {
        let engine = BridgeEngine::new(SyncMode::Slave, 8.0, 140.0);
        assert!(matches!(engine.sync_mode, SyncMode::Slave));
        assert_eq!(engine.quantum, 8.0);

        let state = engine.state_rx.borrow().clone();
        assert_eq!(state.link_bpm, 140.0);
        assert_eq!(state.quantum, 8.0);
    }

    #[test]
    fn engine_new_bidirectional_mode() {
        let engine = BridgeEngine::new(SyncMode::Bidirectional, 4.0, 120.0);
        assert!(matches!(engine.sync_mode, SyncMode::Bidirectional));
    }

    #[test]
    fn state_receiver_gets_initial_state() {
        let engine = BridgeEngine::new(SyncMode::Master, 4.0, 120.0);
        let rx = engine.state_receiver();
        let state = rx.borrow().clone();
        assert_eq!(state.prodjlink_bpm, 120.0);
        assert_eq!(state.link_bpm, 120.0);
        assert_eq!(state.quantum, 4.0);
        assert!(!state.is_playing);
    }

    #[tokio::test]
    async fn state_receiver_sees_updates() {
        let engine = BridgeEngine::new(SyncMode::Master, 4.0, 120.0);
        let mut rx = engine.state_receiver();

        let new_state = BridgeState {
            prodjlink_bpm: 140.0,
            link_bpm: 140.0,
            link_peers: 2,
            beat_phase: 1.5,
            quantum: 4.0,
            is_playing: true,
            sync_mode: SyncMode::Master,
            master_device: Some(1),
        };
        engine.state_tx.send(new_state).unwrap();
        rx.changed().await.unwrap();

        let state = rx.borrow().clone();
        assert_eq!(state.prodjlink_bpm, 140.0);
        assert_eq!(state.link_peers, 2);
        assert!(state.is_playing);
        assert_eq!(state.master_device, Some(1));
    }

    #[tokio::test]
    async fn shutdown_signal_propagation() {
        let engine = BridgeEngine::new(SyncMode::Master, 4.0, 120.0);
        let shutdown = engine.shutdown_handle();
        let mut shutdown_rx = shutdown.subscribe();

        shutdown.send(()).unwrap();
        let result = shutdown_rx.recv().await;
        assert!(result.is_ok());
    }

    #[test]
    fn sync_mode_variants_distinct() {
        let master = SyncMode::Master;
        let slave = SyncMode::Slave;
        let bidir = SyncMode::Bidirectional;

        assert_ne!(master, slave);
        assert_ne!(master, bidir);
        assert_ne!(slave, bidir);
        assert_eq!(master, SyncMode::Master);
    }

    #[test]
    fn bridge_state_clone() {
        let state = BridgeState {
            prodjlink_bpm: 128.0,
            link_bpm: 128.0,
            link_peers: 1,
            beat_phase: 2.0,
            quantum: 4.0,
            is_playing: true,
            sync_mode: SyncMode::Bidirectional,
            master_device: Some(3),
        };
        let cloned = state.clone();
        assert_eq!(cloned.prodjlink_bpm, 128.0);
        assert_eq!(cloned.master_device, Some(3));
        assert!(cloned.is_playing);
    }

    #[test]
    fn bridge_state_default() {
        let state = BridgeState::default();
        assert_eq!(state.prodjlink_bpm, 0.0);
        assert_eq!(state.link_bpm, 0.0);
        assert_eq!(state.link_peers, 0);
        assert_eq!(state.beat_phase, 0.0);
        assert_eq!(state.quantum, 4.0);
        assert!(!state.is_playing);
        assert_eq!(state.sync_mode, SyncMode::Master);
        assert!(state.master_device.is_none());
    }

    // ================================================================
    // Pure helper functions — tempo sync threshold
    // ================================================================

    #[test]
    fn should_sync_tempo_above_epsilon() {
        assert!(should_sync_tempo(128.0, 128.1));
        assert!(should_sync_tempo(120.0, 130.0));
    }

    #[test]
    fn should_sync_tempo_within_epsilon() {
        assert!(!should_sync_tempo(128.0, 128.0));
        assert!(!should_sync_tempo(128.0, 128.04));
        assert!(!should_sync_tempo(128.0, 127.96));
    }

    #[test]
    fn should_sync_tempo_exactly_at_epsilon() {
        // At exactly TEMPO_EPSILON (0.05), should NOT sync (uses strict >)
        // Use a value clearly within epsilon to avoid FP rounding
        assert!(!should_sync_tempo(128.0, 128.049));
        // Just above epsilon — should sync
        assert!(should_sync_tempo(128.0, 128.06));
    }

    #[test]
    fn should_sync_tempo_negative_diff() {
        assert!(should_sync_tempo(130.0, 120.0));
    }

    #[test]
    fn should_sync_tempo_zero_bpm() {
        assert!(!should_sync_tempo(0.0, 0.0));
        assert!(should_sync_tempo(0.0, 120.0));
    }

    // ================================================================
    // Pure helper functions — beat-to-phase mapping
    // ================================================================

    #[test]
    fn map_beat_to_phase_valid_beats() {
        assert_eq!(map_beat_to_phase(1), Some(0.0));
        assert_eq!(map_beat_to_phase(2), Some(1.0));
        assert_eq!(map_beat_to_phase(3), Some(2.0));
        assert_eq!(map_beat_to_phase(4), Some(3.0));
    }

    #[test]
    fn map_beat_to_phase_zero_is_invalid() {
        assert_eq!(map_beat_to_phase(0), None);
    }

    #[test]
    fn map_beat_to_phase_above_four_is_invalid() {
        assert_eq!(map_beat_to_phase(5), None);
        assert_eq!(map_beat_to_phase(255), None);
    }

    // ================================================================
    // Pure helper functions — phase sync quantum check
    // ================================================================

    #[test]
    fn can_phase_sync_quantum_four() {
        assert!(can_phase_sync(4.0));
    }

    #[test]
    fn can_phase_sync_quantum_not_four() {
        assert!(!can_phase_sync(8.0));
        assert!(!can_phase_sync(2.0));
        assert!(!can_phase_sync(3.0));
        assert!(!can_phase_sync(1.0));
        assert!(!can_phase_sync(16.0));
    }

    // ================================================================
    // Pure helper functions — echo guard
    // ================================================================

    #[test]
    fn echo_guard_active_when_recent() {
        let now = Instant::now();
        // Just created — elapsed is ~0ms, guard at 100ms should be active
        assert!(is_echo_guarded(now, 100));
    }

    #[test]
    fn echo_guard_inactive_when_old() {
        let old = Instant::now() - std::time::Duration::from_secs(1);
        assert!(!is_echo_guarded(old, 100));
    }

    #[test]
    fn echo_guard_zero_ms_never_guards() {
        let now = Instant::now();
        // 0ms guard means nothing is ever guarded (elapsed >= 0)
        assert!(!is_echo_guarded(now, 0));
    }

    // ================================================================
    // Pure helper functions — display formatting
    // ================================================================

    #[test]
    fn format_master_device_some() {
        assert_eq!(format_master_device(Some(1)), "P1");
        assert_eq!(format_master_device(Some(3)), "P3");
    }

    #[test]
    fn format_master_device_none() {
        assert_eq!(format_master_device(None), "---");
    }

    #[test]
    fn compute_drift_synced() {
        assert_eq!(compute_drift(128.0, 128.0), 0.0);
    }

    #[test]
    fn compute_drift_positive() {
        let drift = compute_drift(130.0, 128.0);
        assert!((drift - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn compute_drift_negative() {
        let drift = compute_drift(126.0, 128.0);
        assert!((drift - (-2.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn sync_indicator_synced() {
        assert_eq!(format_sync_indicator(0.0), "✓ synced");
        assert_eq!(format_sync_indicator(0.05), "✓ synced");
        assert_eq!(format_sync_indicator(-0.1), "✓ synced");
    }

    #[test]
    fn sync_indicator_drifting() {
        assert_eq!(format_sync_indicator(0.2), "⚠ drift 0.2");
        assert_eq!(format_sync_indicator(-1.5), "⚠ drift -1.5");
    }

    #[test]
    fn sync_indicator_at_threshold() {
        // Exactly 0.1 — should show synced (> not >=)
        assert_eq!(format_sync_indicator(0.1), "✓ synced");
    }

    // ================================================================
    // Multiple state updates through watch channel
    // ================================================================

    #[tokio::test]
    async fn multiple_state_updates() {
        let engine = BridgeEngine::new(SyncMode::Master, 4.0, 120.0);
        let mut rx = engine.state_receiver();

        // Push several updates
        for bpm in [125.0, 130.0, 135.0] {
            let state = BridgeState {
                prodjlink_bpm: bpm,
                link_bpm: bpm,
                ..BridgeState::default()
            };
            engine.state_tx.send(state).unwrap();
        }

        // Watch channel only retains the latest value
        rx.changed().await.unwrap();
        let state = rx.borrow().clone();
        assert_eq!(state.prodjlink_bpm, 135.0);
    }

    #[test]
    fn multiple_shutdown_subscribers() {
        let engine = BridgeEngine::new(SyncMode::Master, 4.0, 120.0);
        let tx = engine.shutdown_handle();
        let mut rx1 = tx.subscribe();
        let mut rx2 = tx.subscribe();

        tx.send(()).unwrap();

        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }
}
