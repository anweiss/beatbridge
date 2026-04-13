use std::time::Instant;

use tokio::sync::{broadcast, watch};
use tracing::{debug, info, warn};

use ableton_link_rs::link::BasicLink;
use prodjlink_rs::{BeatEvent, DeviceUpdate, ProDjLink};

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
                                Self::sync_tempo_to_link(&mut link, cdj_bpm, self.quantum).await;
                                Self::sync_phase_to_link(
                                    &mut link,
                                    beat.beat_within_bar,
                                    self.quantum,
                                ).await;
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

                    if (link_bpm - last_link_bpm).abs() > TEMPO_EPSILON {
                        debug!(bpm = link_bpm, "link tempo changed, relaying to CDJs");
                        last_link_bpm = link_bpm;
                        // TODO: relay tempo to CDJs once prodjlink-rs exposes
                        // VirtualCdj::set_master_tempo() or similar API
                    }

                    if playing != last_playing {
                        debug!(playing, "link play state changed, relaying to CDJs");
                        last_playing = playing;
                        // TODO: relay play/stop to CDJs via fader-start commands
                        // once prodjlink-rs exposes the send-side API
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
                            if last_link_to_cdj.elapsed().as_millis() < ECHO_GUARD_MS {
                                continue;
                            }
                            let master_dev = pdl.virtual_cdj().tempo_master().master_device();
                            if master_dev.is_some_and(|d| d == beat.device_number) {
                                let cdj_bpm = beat.effective_tempo();
                                Self::sync_tempo_to_link(&mut link, cdj_bpm, self.quantum).await;
                                Self::sync_phase_to_link(
                                    &mut link,
                                    beat.beat_within_bar,
                                    self.quantum,
                                ).await;
                                last_cdj_to_link = Instant::now();
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
                            if last_link_to_cdj.elapsed().as_millis() < ECHO_GUARD_MS {
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
                    if last_cdj_to_link.elapsed().as_millis() < ECHO_GUARD_MS {
                        continue;
                    }
                    let session = link.capture_app_session_state();
                    let link_bpm = session.tempo();

                    if (link_bpm - prev_link_bpm).abs() > TEMPO_EPSILON {
                        debug!(bpm = link_bpm, "bidir: Link tempo → CDJs");
                        prev_link_bpm = link_bpm;
                        last_link_to_cdj = Instant::now();
                        // TODO: relay to CDJs once send API available
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
    async fn sync_tempo_to_link(link: &mut BasicLink, cdj_bpm: f64, quantum: f64) {
        let session = link.capture_app_session_state();
        let link_bpm = session.tempo();

        if (cdj_bpm - link_bpm).abs() > TEMPO_EPSILON {
            let time = link.clock().micros();
            let mut session = link.capture_app_session_state();
            session.set_tempo(cdj_bpm, time);
            link.commit_app_session_state(session).await;
            debug!(cdj_bpm, link_bpm, "tempo synced CDJ → Link");
        }

        // Always re-read to get fresh phase for logging
        let session = link.capture_app_session_state();
        let time = link.clock().micros();
        let _phase = session.phase_at_time(time, quantum);
    }

    /// Align Link phase to the CDJ beat position.
    /// CDJ `beat_within_bar` is 1-based (1–4); map to 0-based Link beat.
    async fn sync_phase_to_link(link: &mut BasicLink, beat_within_bar: u8, quantum: f64) {
        if beat_within_bar == 0 || beat_within_bar > 4 {
            return; // unknown bar position
        }

        let target_beat = (beat_within_bar - 1) as f64;
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

#[cfg(test)]
mod tests {
    use super::*;

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

        // Push a new state through the watch channel
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

        // Send shutdown
        shutdown.send(()).unwrap();

        // Receiver should get the signal
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
}
