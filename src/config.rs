use clap::Parser;
use serde::Deserialize;
use std::net::Ipv4Addr;
use std::path::PathBuf;

/// Sync mode determines the direction of tempo/transport synchronization
#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    /// ProDJLink is master, Link follows
    #[default]
    Master,
    /// Link is master, ProDJLink follows
    Slave,
    /// Bidirectional — last change wins
    Bidirectional,
}

/// CLI arguments — all config fields are `Option` so we can distinguish
/// "user explicitly passed this" from "defaulted by clap".
#[derive(Parser, Debug)]
#[command(name = "beatbridge", version, about = "Bridge Pioneer Pro DJ Link ↔ Ableton Link")]
pub struct Cli {
    /// Network interface IP address for Pro DJ Link
    #[arg(short, long)]
    pub interface: Option<Ipv4Addr>,

    /// Virtual CDJ device name on the DJ Link network
    #[arg(long)]
    pub device_name: Option<String>,

    /// Virtual CDJ device number (1-6)
    #[arg(long)]
    pub device_number: Option<u8>,

    /// Ableton Link quantum (beats per phase, typically 4)
    #[arg(short, long)]
    pub quantum: Option<f64>,

    /// Sync mode: master (CDJ→Link), slave (Link→CDJ), bidirectional
    #[arg(short, long)]
    pub sync_mode: Option<SyncMode>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long)]
    pub log_level: Option<String>,

    /// Path to TOML config file
    #[arg(short = 'C', long)]
    pub config: Option<PathBuf>,

    /// Initial BPM for Ableton Link (used when no CDJ is connected yet)
    #[arg(long)]
    pub initial_bpm: Option<f64>,

    /// Status display refresh interval in milliseconds
    #[arg(long)]
    pub status_interval_ms: Option<u64>,
}

/// TOML config file structure (all fields optional, CLI overrides)
#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    pub interface: Option<Ipv4Addr>,
    pub device_name: Option<String>,
    pub device_number: Option<u8>,
    pub quantum: Option<f64>,
    pub sync_mode: Option<SyncMode>,
    pub log_level: Option<String>,
    pub initial_bpm: Option<f64>,
    pub status_interval_ms: Option<u64>,
}

impl std::fmt::Display for SyncMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncMode::Master => write!(f, "master"),
            SyncMode::Slave => write!(f, "slave"),
            SyncMode::Bidirectional => write!(f, "bidirectional"),
        }
    }
}

/// Resolved configuration (CLI + file merged)
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub interface: Option<Ipv4Addr>,
    pub device_name: String,
    pub device_number: u8,
    pub quantum: f64,
    pub sync_mode: SyncMode,
    pub log_level: String,
    pub initial_bpm: f64,
    pub status_interval_ms: u64,
}

/// Default values as constants for clarity and testability.
pub const DEFAULT_DEVICE_NAME: &str = "beatbridge";
pub const DEFAULT_DEVICE_NUMBER: u8 = 5;
pub const DEFAULT_QUANTUM: f64 = 4.0;
pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_INITIAL_BPM: f64 = 120.0;
pub const DEFAULT_STATUS_INTERVAL_MS: u64 = 500;

impl BridgeConfig {
    /// Load config from CLI args, optionally overlaying a TOML file.
    /// Precedence: CLI > file > built-in defaults.
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let cli = Cli::parse();

        let file_cfg = if let Some(ref path) = cli.config {
            let contents = std::fs::read_to_string(path)?;
            toml::from_str::<FileConfig>(&contents)?
        } else {
            FileConfig::default()
        };

        Ok(Self::merge(cli, file_cfg))
    }

    /// Three-way merge: CLI > file > defaults.
    /// Extracted from `load()` so it can be tested without process args.
    pub fn merge(cli: Cli, file: FileConfig) -> Self {
        Self {
            interface: cli.interface.or(file.interface),
            device_name: cli
                .device_name
                .or(file.device_name)
                .unwrap_or_else(|| DEFAULT_DEVICE_NAME.to_string()),
            device_number: cli
                .device_number
                .or(file.device_number)
                .unwrap_or(DEFAULT_DEVICE_NUMBER),
            quantum: cli.quantum.or(file.quantum).unwrap_or(DEFAULT_QUANTUM),
            sync_mode: cli
                .sync_mode
                .or(file.sync_mode)
                .unwrap_or_default(),
            log_level: cli
                .log_level
                .or(file.log_level)
                .unwrap_or_else(|| DEFAULT_LOG_LEVEL.to_string()),
            initial_bpm: cli
                .initial_bpm
                .or(file.initial_bpm)
                .unwrap_or(DEFAULT_INITIAL_BPM),
            status_interval_ms: cli
                .status_interval_ms
                .or(file.status_interval_ms)
                .unwrap_or(DEFAULT_STATUS_INTERVAL_MS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Helper to build a Cli with all None ----
    fn empty_cli() -> Cli {
        Cli {
            interface: None,
            device_name: None,
            device_number: None,
            quantum: None,
            sync_mode: None,
            log_level: None,
            config: None,
            initial_bpm: None,
            status_interval_ms: None,
        }
    }

    // ================================================================
    // FileConfig TOML deserialization
    // ================================================================

    #[test]
    fn file_config_default_all_none() {
        let cfg = FileConfig::default();
        assert!(cfg.interface.is_none());
        assert!(cfg.device_name.is_none());
        assert!(cfg.device_number.is_none());
        assert!(cfg.quantum.is_none());
        assert!(cfg.sync_mode.is_none());
        assert!(cfg.log_level.is_none());
        assert!(cfg.initial_bpm.is_none());
        assert!(cfg.status_interval_ms.is_none());
    }

    #[test]
    fn file_config_full_toml() {
        let toml_str = r#"
            interface = "192.168.1.10"
            device_name = "mybridge"
            device_number = 3
            quantum = 8.0
            sync_mode = "slave"
            log_level = "debug"
            initial_bpm = 140.0
            status_interval_ms = 250
        "#;
        let cfg: FileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.interface.unwrap(), "192.168.1.10".parse::<Ipv4Addr>().unwrap());
        assert_eq!(cfg.device_name.unwrap(), "mybridge");
        assert_eq!(cfg.device_number.unwrap(), 3);
        assert_eq!(cfg.quantum.unwrap(), 8.0);
        assert_eq!(cfg.sync_mode.unwrap(), SyncMode::Slave);
        assert_eq!(cfg.log_level.unwrap(), "debug");
        assert_eq!(cfg.initial_bpm.unwrap(), 140.0);
        assert_eq!(cfg.status_interval_ms.unwrap(), 250);
    }

    #[test]
    fn file_config_partial_toml() {
        let toml_str = r#"
            device_name = "partial"
            initial_bpm = 100.0
        "#;
        let cfg: FileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.device_name.unwrap(), "partial");
        assert_eq!(cfg.initial_bpm.unwrap(), 100.0);
        assert!(cfg.interface.is_none());
        assert!(cfg.sync_mode.is_none());
        assert!(cfg.quantum.is_none());
    }

    #[test]
    fn file_config_empty_toml() {
        let cfg: FileConfig = toml::from_str("").unwrap();
        assert!(cfg.interface.is_none());
        assert!(cfg.device_name.is_none());
    }

    #[test]
    fn file_config_invalid_sync_mode_fails() {
        let toml_str = r#"sync_mode = "unknown""#;
        let result = toml::from_str::<FileConfig>(toml_str);
        assert!(result.is_err());
    }

    // ================================================================
    // SyncMode
    // ================================================================

    #[test]
    fn sync_mode_deserialize_master() {
        let toml_str = r#"sync_mode = "master""#;
        let cfg: FileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.sync_mode.unwrap(), SyncMode::Master);
    }

    #[test]
    fn sync_mode_deserialize_slave() {
        let toml_str = r#"sync_mode = "slave""#;
        let cfg: FileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.sync_mode.unwrap(), SyncMode::Slave);
    }

    #[test]
    fn sync_mode_deserialize_bidirectional() {
        let toml_str = r#"sync_mode = "bidirectional""#;
        let cfg: FileConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.sync_mode.unwrap(), SyncMode::Bidirectional);
    }

    #[test]
    fn sync_mode_default_is_master() {
        let mode = SyncMode::default();
        assert_eq!(mode, SyncMode::Master);
    }

    #[test]
    fn sync_mode_display() {
        assert_eq!(SyncMode::Master.to_string(), "master");
        assert_eq!(SyncMode::Slave.to_string(), "slave");
        assert_eq!(SyncMode::Bidirectional.to_string(), "bidirectional");
    }

    // ================================================================
    // BridgeConfig::merge — 3-way precedence: CLI > file > default
    // ================================================================

    #[test]
    fn merge_all_defaults() {
        let cfg = BridgeConfig::merge(empty_cli(), FileConfig::default());
        assert!(cfg.interface.is_none());
        assert_eq!(cfg.device_name, DEFAULT_DEVICE_NAME);
        assert_eq!(cfg.device_number, DEFAULT_DEVICE_NUMBER);
        assert_eq!(cfg.quantum, DEFAULT_QUANTUM);
        assert_eq!(cfg.sync_mode, SyncMode::Master);
        assert_eq!(cfg.log_level, DEFAULT_LOG_LEVEL);
        assert_eq!(cfg.initial_bpm, DEFAULT_INITIAL_BPM);
        assert_eq!(cfg.status_interval_ms, DEFAULT_STATUS_INTERVAL_MS);
    }

    #[test]
    fn merge_file_overrides_defaults() {
        let file = FileConfig {
            interface: Some("10.0.0.1".parse().unwrap()),
            device_name: Some("file-bridge".to_string()),
            device_number: Some(2),
            quantum: Some(8.0),
            sync_mode: Some(SyncMode::Slave),
            log_level: Some("debug".to_string()),
            initial_bpm: Some(140.0),
            status_interval_ms: Some(250),
        };
        let cfg = BridgeConfig::merge(empty_cli(), file);
        assert_eq!(cfg.interface.unwrap(), "10.0.0.1".parse::<Ipv4Addr>().unwrap());
        assert_eq!(cfg.device_name, "file-bridge");
        assert_eq!(cfg.device_number, 2);
        assert_eq!(cfg.quantum, 8.0);
        assert_eq!(cfg.sync_mode, SyncMode::Slave);
        assert_eq!(cfg.log_level, "debug");
        assert_eq!(cfg.initial_bpm, 140.0);
        assert_eq!(cfg.status_interval_ms, 250);
    }

    #[test]
    fn merge_cli_overrides_file() {
        let cli = Cli {
            interface: Some("192.168.1.1".parse().unwrap()),
            device_name: Some("cli-bridge".to_string()),
            device_number: Some(3),
            quantum: Some(16.0),
            sync_mode: Some(SyncMode::Bidirectional),
            log_level: Some("trace".to_string()),
            config: None,
            initial_bpm: Some(160.0),
            status_interval_ms: Some(100),
        };
        let file = FileConfig {
            interface: Some("10.0.0.1".parse().unwrap()),
            device_name: Some("file-bridge".to_string()),
            device_number: Some(2),
            quantum: Some(8.0),
            sync_mode: Some(SyncMode::Slave),
            log_level: Some("debug".to_string()),
            initial_bpm: Some(140.0),
            status_interval_ms: Some(250),
        };
        let cfg = BridgeConfig::merge(cli, file);
        assert_eq!(cfg.interface.unwrap(), "192.168.1.1".parse::<Ipv4Addr>().unwrap());
        assert_eq!(cfg.device_name, "cli-bridge");
        assert_eq!(cfg.device_number, 3);
        assert_eq!(cfg.quantum, 16.0);
        assert_eq!(cfg.sync_mode, SyncMode::Bidirectional);
        assert_eq!(cfg.log_level, "trace");
        assert_eq!(cfg.initial_bpm, 160.0);
        assert_eq!(cfg.status_interval_ms, 100);
    }

    #[test]
    fn merge_cli_explicit_default_overrides_file() {
        // User explicitly passes `--sync-mode master` — CLI should win
        // even though master is also the default.
        let cli = Cli {
            sync_mode: Some(SyncMode::Master),
            device_number: Some(DEFAULT_DEVICE_NUMBER),
            quantum: Some(DEFAULT_QUANTUM),
            initial_bpm: Some(DEFAULT_INITIAL_BPM),
            ..empty_cli()
        };
        let file = FileConfig {
            sync_mode: Some(SyncMode::Slave),
            device_number: Some(2),
            quantum: Some(8.0),
            initial_bpm: Some(140.0),
            ..FileConfig::default()
        };
        let cfg = BridgeConfig::merge(cli, file);
        assert_eq!(cfg.sync_mode, SyncMode::Master); // CLI wins
        assert_eq!(cfg.device_number, DEFAULT_DEVICE_NUMBER); // CLI wins
        assert_eq!(cfg.quantum, DEFAULT_QUANTUM); // CLI wins
        assert_eq!(cfg.initial_bpm, DEFAULT_INITIAL_BPM); // CLI wins
    }

    #[test]
    fn merge_partial_cli_partial_file() {
        let cli = Cli {
            device_name: Some("cli-name".to_string()),
            quantum: Some(3.0),
            ..empty_cli()
        };
        let file = FileConfig {
            device_number: Some(6),
            sync_mode: Some(SyncMode::Bidirectional),
            initial_bpm: Some(90.0),
            ..FileConfig::default()
        };
        let cfg = BridgeConfig::merge(cli, file);
        assert_eq!(cfg.device_name, "cli-name"); // from CLI
        assert_eq!(cfg.quantum, 3.0); // from CLI
        assert_eq!(cfg.device_number, 6); // from file
        assert_eq!(cfg.sync_mode, SyncMode::Bidirectional); // from file
        assert_eq!(cfg.initial_bpm, 90.0); // from file
        assert_eq!(cfg.log_level, DEFAULT_LOG_LEVEL); // from default
        assert_eq!(cfg.status_interval_ms, DEFAULT_STATUS_INTERVAL_MS); // from default
    }

    #[test]
    fn merge_interface_none_when_neither_set() {
        let cfg = BridgeConfig::merge(empty_cli(), FileConfig::default());
        assert!(cfg.interface.is_none());
    }

    #[test]
    fn merge_interface_from_cli_only() {
        let cli = Cli {
            interface: Some("172.16.0.1".parse().unwrap()),
            ..empty_cli()
        };
        let cfg = BridgeConfig::merge(cli, FileConfig::default());
        assert_eq!(cfg.interface.unwrap(), "172.16.0.1".parse::<Ipv4Addr>().unwrap());
    }
}
