# AGENTS.md - termixer

This file is the fast onboard for coding agents entering this repo.
Read it before making changes.

## 1) What This Project Is

- Rust terminal DJ mixer for live performance workflows with TidalCycles/MPV/SuperCollider.
- Main capabilities: multi-deck mixing, EQ/filtering, crossfader curves, sample pads, rack recording, source discovery, output-device routing.
- Runtime model: TUI and control logic on main thread; real-time audio callback in a separate low-latency path.

## 2) Core Tech Stack (What You Must Understand)

- Language/build: Rust edition 2024 (`Cargo.toml`).
- TUI: `ratatui` + `crossterm` + custom widgets in `src/ui/`.
- Audio I/O and playback:
  - `cpal` for output device access and stream callback.
  - `rodio` for sample pad/rack playback and mixing.
  - MPV JSON IPC (Unix socket) for deck control and telemetry.
  - Optional SuperCollider OSC control via UDP.
- DSP and analysis:
  - Custom DSP in `src/audio/dsp.rs` + engine filters in `src/audio/engine.rs`.
  - BPM/key analysis via `stratum-dsp` + `symphonia`.
- Concurrency primitives used intentionally for RT safety:
  - `triple_buffer` for UI -> audio snapshot state.
  - `rtrb` for SPSC command/sample rings.
  - atomics + lock-free data structures in decoder/metering paths.

## 3) Repo Layout and Responsibilities

- `src/main.rs`: entrypoint, CLI parsing, terminal setup/teardown, render loop.
- `src/app.rs`: primary controller/state machine and all keyboard/mouse behavior.
- `src/state/`: model state (`mixer.rs`, `sampler.rs`).
- `src/ui/`: rendering/layout/widgets.
- `src/audio/`: engine, mpv/sc adapters, discovery, decoding, output devices, BPM analysis.
- `synthdefs/`: SuperCollider SynthDefs used by SC integration.
- `build.rs`: linker rpath tweak for local runtime linking.

## 4) High-Value Runtime Concepts

- Navigation model is mode-driven (`AppMode`): PaneSelect -> ControlSelect -> Edit, plus overlay/picker modes.
- Deck model is effectively A/B/C (C is CUE-focused) with shared master/crossfader logic.
- Tick cadence is ~20 FPS (`tick_rate = 50ms`) and drives meter updates/animations.
- MPV route mode conventions:
  - socket: `/tmp/termixer.sock`
  - fifo: `/tmp/termixer.pcm`
  - wildcard variants: `/tmp/termixer-*.sock` and `/tmp/termixer-*.pcm`
- Debug pane/logging is gated by `DEBUG=1`. Without it, stderr is redirected to avoid TUI corruption.

## 5) External Dependencies and Expected Environment

- Required: Rust toolchain, MPV with IPC enabled, Nerd Font in terminal.
- Optional: SuperCollider (`scsynth`/`sclang`) for synth routing.
- Platform assumptions in current code are Unix-heavy (Unix sockets, `/tmp`, FIFO handling).
- Discovery shells out to system tools (`pgrep`, `pactl`, `pw-cli`, etc.); guard graceful failure paths.

## 6) Build, Run, Validate (Agent Default Workflow)

- Build/check first:
  - `cargo check`
  - `cargo build`
- Run app:
  - `cargo run`
  - or `cargo run -- -s "Deck A" /tmp/mpv-a.sock -s "Deck B" /tmp/mpv-b.sock`
- Debug mode when touching behavior/state sync:
  - `DEBUG=1 cargo run`
- Lint pass before finishing substantial edits:
  - `cargo clippy -- -D warnings` (if existing codebase warnings allow it; otherwise report blockers clearly).

## 7) Non-Negotiable Engineering Rules

- Performance regressions in live-control paths are unacceptable.
  - Treat UI responsiveness, scrub/seek latency, and timestamp sync as critical-path behavior.
  - Prefer architectures that avoid repeated reconnects, blocking IPC in hot loops, and cross-deck resource contention.
  - If a change trades correctness for latency (or vice versa), stop and redesign rather than ship degradation.
- Keep real-time path allocation-light and lock-averse.
  - Avoid adding `Mutex`/blocking I/O/log spam in audio callback paths.
  - Prefer existing lock-free channels/snapshots already in use.
- Respect module boundaries:
  - `state` = data model, `ui` = rendering, `audio` = signal/IO backend, `app` = orchestration.
- Reuse existing control/navigation patterns before introducing new abstractions.
- Keep changes surgical; do not refactor broad surfaces unless task explicitly requires it.
- Do not hardcode ad-hoc colors in UI code; centralize through `src/ui/colors.rs`.

## 8) Safe Change Playbooks

### A) Add or modify a mixer control

- Update enum + semantics in `src/state/mixer.rs`.
- Wire key/mouse behavior and mode handling in `src/app.rs`.
- Render/edit affordance in `src/ui/channel.rs` or `src/ui/widgets.rs`.
- Sync side effects to backend(s): MPV/SC/audio engine state updates.
- Verify with `DEBUG=1` and manual interaction.

### B) Add or modify a pane/layout behavior

- Update pane selection/navigation in `src/app.rs` (`SelectedPane`, movement handlers, visibility sync).
- Update layout/rendering in `src/ui/mixer.rs`.
- Keep mouse hit-testing aligned with rendered geometry (`calculate_all_areas` in `src/main.rs`).

### C) Touch DSP/audio engine behavior

- Start in `src/audio/engine.rs` and `src/audio/dsp.rs`.
- Preserve control snapshot flow (`audio/engine/inner.rs`) and command/ring semantics.
- Validate no obvious glitch regressions (dropouts, zipper noise, unstable meters).
- Document any unavoidable RT tradeoff in PR/notes.

## 9) Known Risk Areas (Read Before Editing)

- `src/app.rs` is large and highly coupled; make focused diffs and avoid broad rewrites.
- MPV + engine hybrid routing has nuanced mute/volume interactions to avoid double-audio.
- Unix-specific assumptions exist in discovery/routing; avoid silently breaking Linux/macOS compatibility.
- `unsafe` exists in decoder/ring code (`src/audio/decoder.rs`) and stderr redirection in `src/main.rs`; do not modify casually.

## 10) Definition of Done for Agent Changes

- Change compiles (`cargo check` or better).
- Keyboard/mouse behavior for touched flows still works in app.
- Sync path updates are consistent across MPV/SC/engine where relevant.
- No new obvious warnings, panics, or mode dead-ends.
- User-facing behavior changes are reflected in `README.md` if controls/options changed.

## 11) Quick File Index

- App state machine: `src/app.rs`
- Startup loop + area hit-testing: `src/main.rs`
- Mixer model and controls: `src/state/mixer.rs`
- Sample/rack model: `src/state/sampler.rs`
- Main layout/UI orchestration: `src/ui/mixer.rs`
- Widgets: `src/ui/widgets.rs`
- Audio engine: `src/audio/engine.rs`
- MPV adapter: `src/audio/mpv.rs`
- Source discovery: `src/audio/discovery.rs`
- Sample cache/player: `src/audio/sample_cache.rs`
