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

#[derive(Parser, Debug)]
#[command(name = "beatbridge", version, about = "Bridge Pioneer Pro DJ Link ↔ Ableton Link")]
pub struct Cli {
    /// Network interface IP address for Pro DJ Link
    #[arg(short, long)]
    pub interface: Option<Ipv4Addr>,

    /// Virtual CDJ device name on the DJ Link network
    #[arg(long, default_value = "beatbridge")]
    pub device_name: String,

    /// Virtual CDJ device number (1-6)
    #[arg(long, default_value_t = 5)]
    pub device_number: u8,

    /// Ableton Link quantum (beats per phase, typically 4)
    #[arg(short, long, default_value_t = 4.0)]
    pub quantum: f64,

    /// Sync mode: master (CDJ→Link), slave (Link→CDJ), bidirectional
    #[arg(short, long, default_value = "master")]
    pub sync_mode: SyncMode,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info")]
    pub log_level: String,

    /// Path to TOML config file
    #[arg(short = 'C', long)]
    pub config: Option<PathBuf>,

    /// Initial BPM for Ableton Link (used when no CDJ is connected yet)
    #[arg(long, default_value_t = 120.0)]
    pub initial_bpm: f64,

    /// Status display refresh interval in milliseconds
    #[arg(long, default_value_t = 500)]
    pub status_interval_ms: u64,
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

impl BridgeConfig {
    /// Load config from CLI args, optionally overlaying a TOML file.
    /// CLI args take precedence over file config.
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let cli = Cli::parse();

        let file_cfg = if let Some(ref path) = cli.config {
            let contents = std::fs::read_to_string(path)?;
            toml::from_str::<FileConfig>(&contents)?
        } else {
            FileConfig::default()
        };

        Ok(Self {
            interface: cli.interface.or(file_cfg.interface),
            device_name: if cli.device_name != "beatbridge" {
                cli.device_name
            } else {
                file_cfg.device_name.unwrap_or(cli.device_name)
            },
            device_number: if cli.device_number != 5 {
                cli.device_number
            } else {
                file_cfg.device_number.unwrap_or(cli.device_number)
            },
            quantum: if (cli.quantum - 4.0).abs() > f64::EPSILON {
                cli.quantum
            } else {
                file_cfg.quantum.unwrap_or(cli.quantum)
            },
            sync_mode: file_cfg.sync_mode.unwrap_or(cli.sync_mode),
            log_level: if cli.log_level != "info" {
                cli.log_level
            } else {
                file_cfg.log_level.unwrap_or(cli.log_level)
            },
            initial_bpm: if (cli.initial_bpm - 120.0).abs() > f64::EPSILON {
                cli.initial_bpm
            } else {
                file_cfg.initial_bpm.unwrap_or(cli.initial_bpm)
            },
            status_interval_ms: if cli.status_interval_ms != 500 {
                cli.status_interval_ms
            } else {
                file_cfg.status_interval_ms.unwrap_or(cli.status_interval_ms)
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn file_config_invalid_sync_mode_fails() {
        let toml_str = r#"sync_mode = "unknown""#;
        let result = toml::from_str::<FileConfig>(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn file_config_empty_toml() {
        let cfg: FileConfig = toml::from_str("").unwrap();
        assert!(cfg.interface.is_none());
        assert!(cfg.device_name.is_none());
    }
}
