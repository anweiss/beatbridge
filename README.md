# BeatBridge

Bridge between **Pioneer Pro DJ Link** and **Ableton Link** — sync tempo, phase, and transport from CDJs/mixers to any Link-enabled application in real time.

Built on [prodjlink-rs](https://github.com/anweiss/prodjlink-rs) and [ableton-link-rs](https://github.com/anweiss/ableton-link-rs), two native Rust implementations of their respective protocols.

## What It Does

BeatBridge joins a Pioneer DJ Link network as a virtual CDJ, listens for tempo/beat/transport events, and relays them to the Ableton Link session. This lets you sync:

- **CDJs → Ableton Live** (or any Link app) — DJs control the tempo, Link follows ✅
- **Link → CDJs** — Link apps control the tempo, CDJs follow *(detection implemented; relay pending prodjlink-rs send API)*
- **Bidirectional** — CDJ→Link fully functional; Link→CDJ direction pending *(same as above)*

### Use Cases

- Sync Ableton Live sets to CDJ tempo for hybrid DJ/production performances
- Bridge DJ hardware to Link-enabled iOS/Android apps
- Clock-sync modular gear (via Link) to a DJ setup
- Run headless on a Raspberry Pi sitting on the DJ network

## Quick Start

```bash
# Build
cargo build --release

# Run with defaults (CDJ→Link, quantum 4, device #5)
./target/release/beatbridge

# Specify network interface and sync mode
beatbridge --interface 192.168.1.100 --sync-mode master

# Use a config file
beatbridge --config beatbridge.toml
```

## CLI Options

| Flag | Default | Description |
|------|---------|-------------|
| `-i, --interface` | auto-detect | Network interface IP for Pro DJ Link |
| `--device-name` | `beatbridge` | Virtual CDJ name on the DJ network |
| `--device-number` | `5` | Virtual CDJ device number (1–6) |
| `-q, --quantum` | `4.0` | Ableton Link quantum (beats per phase) |
| `-s, --sync-mode` | `master` | `master` (CDJ→Link), `slave` (Link→CDJ), `bidirectional` |
| `--initial-bpm` | `120.0` | Starting BPM when no CDJ is connected |
| `-l, --log-level` | `info` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `-C, --config` | — | Path to TOML config file |
| `--status-interval-ms` | `500` | Status display refresh interval |

## Configuration File

Copy `beatbridge.example.toml` to `beatbridge.toml` and customize:

```toml
# Network interface IP for Pro DJ Link (auto-detect if omitted)
# interface = "192.168.1.100"

device_name = "beatbridge"
device_number = 5
quantum = 4.0
sync_mode = "master"
initial_bpm = 120.0
log_level = "info"
```

CLI flags override config file values.

## Sync Modes

### Master (CDJ→Link) — default

The DJ controls tempo. BeatBridge listens for beats from the Pro DJ Link tempo master and pushes tempo + phase to the Ableton Link session. Play/stop state is forwarded as Link transport.

### Slave (Link→CDJ)

Link controls tempo. BeatBridge polls the Link session and detects tempo/transport changes. Full relay to CDJs is pending the addition of a send-side API in prodjlink-rs.

### Bidirectional

CDJ→Link direction is fully functional. Link→CDJ direction detects changes but relay is pending (same as slave). Uses a 100ms echo guard to prevent feedback loops. Last change wins.

## Status Display

BeatBridge prints a compact, single-line status that updates in place — ideal for headless operation:

```
  ▶ 128.0 BPM │ CDJ→Link │ Master: P1 │ Link: 2 peers │ Phase: [█░░░] │ ✓ synced
```

## Running on Raspberry Pi

BeatBridge is designed to run headless on a Raspberry Pi connected to the same network as your DJ equipment.

### Cross-compile for aarch64 (from macOS/Linux x86)

```bash
# Install the cross-compilation target
rustup target add aarch64-unknown-linux-gnu

# Install a linker (macOS with Homebrew)
brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu

# Build
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-unknown-linux-gnu-gcc \
  cargo build --release --target aarch64-unknown-linux-gnu
```

### Or build natively on the Pi

```bash
# On the Raspberry Pi
cargo build --release
```

### Run as a systemd service

Create `/etc/systemd/system/beatbridge.service`:

```ini
[Unit]
Description=BeatBridge Pro DJ Link ↔ Ableton Link
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/beatbridge --config /etc/beatbridge.toml
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable beatbridge
sudo systemctl start beatbridge
sudo journalctl -u beatbridge -f  # view logs
```

## Architecture

```
┌─────────────────┐         ┌──────────────┐         ┌─────────────────┐
│  CDJ-3000 / NXS2│◄───────►│              │◄───────►│  Ableton Live   │
│  DJM-A9 / V10   │ Pro DJ  │  BeatBridge  │ Ableton │  iOS/Android    │
│  Opus Quad      │  Link   │              │  Link   │  Modular gear   │
└─────────────────┘  (UDP)  └──────────────┘  (UDP)  └─────────────────┘
                              │            │
                         prodjlink-rs  ableton-link-rs
```

### Modules

- **`config`** — CLI args (clap) + TOML config file parsing
- **`bridge`** — Core sync engine with three modes (master/slave/bidirectional)
- **`status`** — Compact terminal status display with phase visualization
- **`main`** — Bootstrap, service wiring, and graceful shutdown

## Supported Hardware

Any Pioneer DJ equipment supported by [prodjlink-rs](https://github.com/anweiss/prodjlink-rs):

- CDJ-3000, CDJ-2000NXS2, CDJ-2000NXS, XDJ-XZ, XDJ-1000MK2
- DJM-A9, DJM-900NXS2, DJM-V10
- Opus Quad
- Any Ableton Link-compatible application (Live, iOS apps, etc.)

## License

MIT
