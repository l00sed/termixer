use std::sync::Mutex;

use triple_buffer::{Input, Output};

use crate::state::SEQUENCE_STEPS;

/// Commands sent from the UI thread to the audio callback.
/// State changes (volume, EQ, filters, etc.) go via the triple-buffered
/// snapshot in `ControlState`. These commands are for thread-bound actions only.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum AudioCommand {
    Stop(usize),
    Quit,
}

/// Per-deck control state.
#[derive(Clone)]
pub struct DeckState {
    pub volume: f32,
    pub playback_rate: f32,
    pub playing: bool,
    pub muted: bool,
    pub solo: bool,
    pub filter_cutoff: f32,
    pub filter_freq: f32,
    pub lfo_speed: f32,
    pub lfo_shape: f32,
    pub eq_low: f32,
    pub eq_mid: f32,
    pub eq_high: f32,
    pub eq_low_kill: bool,
    pub eq_mid_kill: bool,
    pub eq_high_kill: bool,
    pub pan: f32,
}

impl Default for DeckState {
    fn default() -> Self {
        Self {
            volume: 0.8, playback_rate: 1.0, playing: true, filter_cutoff: 0.0, filter_freq: 0.5,
            lfo_speed: 0.0, lfo_shape: 0.5,
            eq_low: 0.0, eq_mid: 0.0, eq_high: 0.0,
            eq_low_kill: false, eq_mid_kill: false, eq_high_kill: false,
            pan: 0.0, muted: false, solo: false,
        }
    }
}

#[derive(Clone)]
pub struct MasterState {
    pub fader: f32,
    pub muted: bool,
    pub crossfader: f32,
    pub solo_active: bool,
    pub master_eq: [f32; 10],
}

impl Default for MasterState {
    fn default() -> Self {
        Self {
            fader: 0.8,
            muted: false,
            crossfader: 0.5,
            solo_active: false,
            master_eq: [0.0; 10],
        }
    }
}

/// Per-sequence state read by the audio thread for step-accurate sequencing.
#[derive(Clone)]
pub struct SequenceSnapshot {
    pub pad_idx: usize,
    pub volume: f32,
    pub mute: bool,
    pub tempo_multiplier: f32,
    pub global_bpm: f32,
    pub pattern: [bool; SEQUENCE_STEPS],
    pub playing: bool,
    /// Pad config volume (applied on top of sequence volume)
    pub pad_volume: f32,
    /// Pad config mute (applied on top of sequence mute)
    #[allow(dead_code)]
    pub pad_mute: bool,
}

impl Default for SequenceSnapshot {
    fn default() -> Self {
        Self {
            pad_idx: 0,
            volume: 0.8,
            mute: false,
            tempo_multiplier: 1.0,
            global_bpm: 120.0,
            pattern: [false; SEQUENCE_STEPS],
            playing: false,
            pad_volume: 1.0,
            pad_mute: false,
        }
    }
}

/// Snapshot read by the audio thread.
#[derive(Clone, Default)]
pub struct ControlSnapshot {
    pub decks: [DeckState; 3],
    pub master: MasterState,
    pub sequences: Vec<SequenceSnapshot>,
}

/// Thread-safe control state shared between UI and audio threads.
///
/// **Design**: SPSC triple-buffer snapshot. The UI thread owns a canonical
/// copy behind a Mutex (safe — UI is not RT). Each mutation clones the
/// canonical copy into the triple buffer's `Input` slot, which the audio
/// thread reads lockfree via `Output::read()`. Zero contention on the
/// audio thread, no priority inversion, no glitches from rapid UI updates.
pub struct ControlState {
    ui: Mutex<UiSide>,
}

struct UiSide {
    canonical: ControlSnapshot,
    input: Input<ControlSnapshot>,
}

impl ControlState {
    /// Construct the state and return (shared handle, audio-side reader).
    /// The `Output` must be moved into the audio callback closure.
    pub fn new() -> (Self, Output<ControlSnapshot>) {
        let initial = ControlSnapshot::default();
        let (input, output) = triple_buffer::triple_buffer(&initial);
        let state = Self {
            ui: Mutex::new(UiSide { canonical: initial, input }),
        };
        (state, output)
    }

    /// Mutate the canonical UI-side state and publish a snapshot to the
    /// audio thread. Bounded work; the mutex is only ever contended by UI.
    fn mutate<F: FnOnce(&mut ControlSnapshot)>(&self, f: F) {
        if let Ok(mut ui) = self.ui.lock() {
            f(&mut ui.canonical);
            let snap = ui.canonical.clone();
            ui.input.write(snap);
        }
    }

    /// Read the current UI-side canonical state (UI thread convenience).
    #[allow(dead_code)]
    pub fn read(&self) -> ControlSnapshot {
        self.ui.lock().map(|u| u.canonical.clone()).unwrap_or_default()
    }

    // --- UI thread write helpers ---

    pub fn set_volume(&self, ch: usize, v: f32) {
        let clamped = v.clamp(0.0, 1.5);
        self.mutate(|s| { if let Some(d) = s.decks.get_mut(ch) { d.volume = clamped; } });
    }
    pub fn set_playback_rate(&self, ch: usize, rate: f32) {
        let clamped = rate.clamp(0.1, 4.0);
        self.mutate(|s| { if let Some(d) = s.decks.get_mut(ch) { d.playback_rate = clamped; } });
    }
    pub fn set_muted(&self, ch: usize, m: bool) {
        self.mutate(|s| { if let Some(d) = s.decks.get_mut(ch) { d.muted = m; } });
    }
    pub fn set_playing(&self, ch: usize, p: bool) {
        self.mutate(|s| { if let Some(d) = s.decks.get_mut(ch) { d.playing = p; } });
    }
    pub fn set_solo(&self, ch: usize, s_on: bool) {
        self.mutate(|s| { if let Some(d) = s.decks.get_mut(ch) { d.solo = s_on; } });
    }
    pub fn set_filter_cutoff(&self, ch: usize, v: f32) {
        let clamped = v.clamp(0.0, 1.0);
        self.mutate(|s| { if let Some(d) = s.decks.get_mut(ch) { d.filter_cutoff = clamped; } });
    }
    pub fn set_filter_freq(&self, ch: usize, v: f32) {
        let clamped = v.clamp(0.0, 1.0);
        self.mutate(|s| { if let Some(d) = s.decks.get_mut(ch) { d.filter_freq = clamped; } });
    }
    pub fn set_lfo(&self, ch: usize, speed: f32, shape: f32) {
        let sp = speed.clamp(0.0, 1.0);
        let sh = shape.clamp(0.0, 1.0);
        self.mutate(|s| {
            if let Some(d) = s.decks.get_mut(ch) { d.lfo_speed = sp; d.lfo_shape = sh; }
        });
    }
    pub fn set_eq(&self, ch: usize, lo: f32, mi: f32, hi: f32) {
        let l = lo.clamp(-24.0, 24.0);
        let m = mi.clamp(-24.0, 24.0);
        let h = hi.clamp(-24.0, 24.0);
        self.mutate(|s| {
            if let Some(d) = s.decks.get_mut(ch) { d.eq_low = l; d.eq_mid = m; d.eq_high = h; }
        });
    }
    pub fn set_eq_kill(&self, ch: usize, lo: bool, mi: bool, hi: bool) {
        self.mutate(|s| {
            if let Some(d) = s.decks.get_mut(ch) {
                d.eq_low_kill = lo; d.eq_mid_kill = mi; d.eq_high_kill = hi;
            }
        });
    }
    pub fn set_pan(&self, ch: usize, v: f32) {
        let clamped = v.clamp(-1.0, 1.0);
        self.mutate(|s| { if let Some(d) = s.decks.get_mut(ch) { d.pan = clamped; } });
    }
    pub fn set_master_fader(&self, v: f32) {
        let clamped = v.clamp(0.0, 1.0);
        self.mutate(|s| { s.master.fader = clamped; });
    }
    pub fn set_crossfader(&self, v: f32) {
        // App uses -1.0..+1.0, engine uses 0.0..1.0 (0=full A, 1=full B)
        let clamped = ((v + 1.0) / 2.0).clamp(0.0, 1.0);
        self.mutate(|s| { s.master.crossfader = clamped; });
    }
    pub fn set_solo_active(&self, s_on: bool) {
        self.mutate(|s| { s.master.solo_active = s_on; });
    }
    pub fn set_master_eq(&self, bands: [f32; 10]) {
        self.mutate(|s| { s.master.master_eq = bands; });
    }
    pub fn set_sequences(&self, seqs: Vec<SequenceSnapshot>) {
        self.mutate(|s| { s.sequences = seqs; });
    }
}
