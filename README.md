# tidal-mixer

A terminal-based DJ mixer for live performance with [TidalCycles](https://tidalcycles.org/). Built in Rust with [ratatui](https://ratatui.rs/), it provides real-time EQ, filtering, crossfading, and sample pads for mixing audio from MPV and SuperCollider.

![Terminal UI](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-blue)

## Features

- **Dual-deck mixer** with per-channel fader, pan, 3-band EQ, LPF/HPF
- **DJ center** with crossfader (4 curves: Linear, Smooth, Cut, ConstantPower), cue mix, headphone/booth outputs
- **Sample pads** — 4x4 grid with OneShot, Gate, Toggle, and Loop modes
- **Auto-discovery** of MPV sockets, SuperCollider, PulseAudio, PipeWire, JACK sources
- **SuperCollider integration** — custom SynthDefs for mixer channel processing
- **Mouse support** — click and drag faders/knobs
- **Vim navigation** — hjkl throughout, 3-level mode system

## Prerequisites

- **Rust** (edition 2021)
- **[Nerd Fonts](https://www.nerdfonts.com/)** — required for icons (rewind, fast-forward, etc.)
- **MPV** — media playback with IPC socket support
- **SuperCollider** (optional) — for TidalCycles integration

## Installation

### From crates.io

```bash
cargo install termixer
```

### From source

```bash
git clone https://github.com/l00sed/termixer.git
cd termixer
cargo install --path .
```

## Build

```bash
cargo build              # debug
cargo build --release    # optimized with LTO
```

## Usage

```bash
# Auto-discover audio sources
cargo run

# Specify MPV sources explicitly
cargo run -- -s "Deck A" /tmp/mpv-a.sock -s "Deck B" /tmp/mpv-b.sock

# With music and samples directories
cargo run -- -m ~/Music -S ~/Samples
```

### Starting MPV with IPC

```bash
mpv --input-ipc-server=/tmp/mpv-music.sock music.mp3
```

### CLI Options

| Flag | Description |
|------|-------------|
| `-s, --source NAME SOCKET` | Add an audio source (MPV IPC socket) |
| `-m, --music-dir PATH` | Directory for audio file browser |
| `-S, --samples-dir PATH` | Directory for sample pad files |
| `-d, --discover` | Auto-discover audio sources (default) |
| `-h, --help` | Show help |

## Keyboard Controls

### Navigation

| Key | Action |
|-----|--------|
| `Tab` / `h` / `l` | Switch between panes (Deck A, DJ, Deck B, Master) |
| `Enter` | Enter control select mode |
| `Esc` | Go back one level |
| `?` | Toggle help overlay |
| `q` | Quit |

### Controls

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate controls up/down |
| `h` / `l` | Adjust value / toggle EQ kill switch |
| `J` / `K` | Coarse adjustment (0.2) |
| `+` / `-` | Fine adjustment (0.05) |
| `m` / `s` | Toggle mute / solo |
| `c` | Center pan or crossfader |
| `0` | Reset control to default |

### Source & Sample Pads

| Key | Action |
|-----|--------|
| `A` | Open source picker for Deck A |
| `B` | Open source picker for Deck B |
| `P` | Toggle sample pads mode |
| `f` | Toggle fullscreen pad view |

### Mouse

- **Click** to select controls
- **Drag** to adjust faders and knobs
- **Scroll** for fine adjustment

## Architecture

```
SC/Tidal → MPV IPC → ring buffer → DSP → output → speakers
```

The app follows MVC: `state/` (model), `ui/` (view), `app.rs` (controller). The audio pipeline captures from MPV via IPC, applies per-deck biquad filters (LPF, HPF, 3-band EQ), stereo mixing, crossfading, and pan in real-time.

## License

MIT
