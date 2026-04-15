# BeatBridge

Bridge between **Pioneer Pro DJ Link** and **Ableton Link** — sync tempo, phase, and transport from CDJs/mixers to any Link-enabled application in real time.

Built on [prodjlink-rs](https://github.com/anweiss/prodjlink-rs) and [ableton-link-rs](https://github.com/anweiss/ableton-link-rs), two native Rust implementations of their respective protocols.

## What It Does

BeatBridge joins a Pioneer DJ Link network as a virtual CDJ, listens for tempo/beat/transport events, and relays them to the Ableton Link session. This lets you sync:

- **CDJs → Ableton Live** (or any Link app) — DJs control the tempo, Link follows ✅
- **Link → CDJs** — Link apps control the tempo, CDJs follow via tempo master + fader start ✅
- **Bidirectional** — Both directions active, last writer wins with echo-guard suppression ✅

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

Link controls tempo. BeatBridge claims tempo master on the DJ Link network, then relays Link tempo changes via status broadcasts and transport changes via fader-start commands to CDJs on channels 1–4. CDJs in sync mode will follow the Link tempo.

### Bidirectional

Both directions active — last writer wins. CDJ beats push tempo into Link; Link tempo changes push back to CDJs. Uses a 100ms echo guard to prevent feedback loops.

## Status Display

BeatBridge prints a compact, single-line status that updates in place — ideal for headless operation:

```
  ▶ 128.0 BPM │ CDJ→Link │ Master: P1 │ Link: 2 peers │ Phase: [█░░░] │ ✓ synced
```

## Running on Raspberry Pi

BeatBridge is designed to run headless on a Raspberry Pi connected to the same network as your DJ equipment.

### Cross-compile for aarch64 (from macOS or Linux x86_64)

BeatBridge depends on [cpal](https://github.com/RustAudioGroup/cpal) (via `ableton-link-rs` → `rodio`), which links against ALSA on Linux. Cross-compiling requires a cross-toolchain **and** the ALSA development libraries for the target architecture.

#### 1. Install the Rust target

```bash
rustup target add aarch64-unknown-linux-gnu
```

#### 2. Install the cross-toolchain

**macOS (Homebrew):**

```bash
brew tap messense/macos-cross-toolchains
brew install aarch64-unknown-linux-gnu
```

**Linux (Debian/Ubuntu):**

```bash
sudo apt install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu
```

#### 3. Obtain aarch64 ALSA dev libraries

The `cpal` crate requires `libasound2-dev` headers and libraries for the target. You can get these from a Raspberry Pi or by downloading the arm64 package:

**Option A — Copy from the Pi:**

```bash
# On the Raspberry Pi, install the dev package if not already present
sudo apt install libasound2-dev

# From your build machine, copy the required files
mkdir -p sysroot/usr/lib/aarch64-linux-gnu sysroot/usr/include
scp pi@<pi-ip>:/usr/lib/aarch64-linux-gnu/libasound.so* sysroot/usr/lib/aarch64-linux-gnu/
scp pi@<pi-ip>:/usr/lib/aarch64-linux-gnu/libasound.a sysroot/usr/lib/aarch64-linux-gnu/
scp -r pi@<pi-ip>:/usr/include/alsa sysroot/usr/include/
```

**Option B — Download the arm64 .deb package directly:**

```bash
mkdir -p sysroot
# Download the arm64 libasound2-dev package (check the Pi's Debian version)
apt download libasound2-dev:arm64 2>/dev/null || \
  wget http://ports.ubuntu.com/pool/main/a/alsa-lib/libasound2-dev_1.2.8-1build1_arm64.deb
dpkg-deb -x libasound2-dev_*_arm64.deb sysroot/

# You'll also need the runtime library
apt download libasound2:arm64 2>/dev/null || \
  wget http://ports.ubuntu.com/pool/main/a/alsa-lib/libasound2_1.2.8-1build1_arm64.deb
dpkg-deb -x libasound2_*_arm64.deb sysroot/
```

**Option C — Use [cross](https://github.com/cross-rs/cross) (easiest, requires Docker):**

```bash
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu
```

`cross` handles the entire sysroot and toolchain automatically via Docker. Skip to [Deploy to the Pi](#deploy-to-the-pi) if using this method.

#### 4. Build

Point `pkg-config` and the linker at your sysroot and toolchain:

```bash
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export PKG_CONFIG_SYSROOT_DIR="$(pwd)/sysroot"
export PKG_CONFIG_PATH="$(pwd)/sysroot/usr/lib/aarch64-linux-gnu/pkgconfig"
export PKG_CONFIG_ALLOW_CROSS=1

cargo build --release --target aarch64-unknown-linux-gnu
```

The binary will be at `target/aarch64-unknown-linux-gnu/release/beatbridge`.

> **Tip:** Add these settings to `.cargo/config.toml` to avoid repeating them:
>
> ```toml
> [target.aarch64-unknown-linux-gnu]
> linker = "aarch64-linux-gnu-gcc"
> ```

#### Deploy to the Pi

```bash
# Copy binary and config
scp target/aarch64-unknown-linux-gnu/release/beatbridge pi@<pi-ip>:/usr/local/bin/
scp beatbridge.toml pi@<pi-ip>:/etc/beatbridge.toml

# On the Pi — install the ALSA runtime if not already present
ssh pi@<pi-ip> 'sudo apt install -y libasound2'
```

### Or build natively on the Pi

```bash
# Install build dependencies
sudo apt install build-essential libasound2-dev

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build
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
