//! Sample pad state and configuration

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Key mapping for the 4x4 pad grid
/// Layout:
///   4 5 6 7
///   R T Y U
///   F G H J
///   V B N M
pub const PAD_KEYS: [[char; 4]; 4] = [
    ['4', '5', '6', '7'],
    ['r', 't', 'y', 'u'],
    ['f', 'g', 'h', 'j'],
    ['v', 'b', 'n', 'm'],
];

/// Per-pad DSP configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PadConfig {
    /// Volume level (0.0 to 1.0)
    pub volume: f32,
    /// Mute toggle
    pub mute: bool,
    /// High-pass filter cutoff (20.0 to 20000.0 Hz)
    pub high_pass: f32,
    /// Low-pass filter cutoff (20.0 to 20000.0 Hz)
    pub low_pass: f32,
    /// EQ low band gain (0.0 to 2.0, 1.0 = flat)
    pub eq_low: f32,
    /// EQ mid band gain (0.0 to 2.0, 1.0 = flat)
    pub eq_mid: f32,
    /// EQ high band gain (0.0 to 2.0, 1.0 = flat)
    pub eq_high: f32,
    /// Reverb send amount (0.0 to 1.0)
    pub reverb: f32,
    /// Chorus depth (0.0 to 1.0)
    pub chorus: f32,
    /// Distortion amount (0.0 to 1.0)
    pub distortion: f32,
}

impl Default for PadConfig {
    fn default() -> Self {
        Self {
            volume: 0.5,
            mute: false,
            high_pass: 20.0,
            low_pass: 20000.0,
            eq_low: 1.0,
            eq_mid: 1.0,
            eq_high: 1.0,
            reverb: 0.0,
            chorus: 0.0,
            distortion: 0.0,
        }
    }
}

/// Controls available in the pad config pane
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadControl {
    Sample,
    Volume,
    Mute,
    BpmMultiplier,
    HighPass,
    LowPass,
    EqLow,
    EqMid,
    EqHigh,
    FiltersHeader,
    Reverb,
    Chorus,
    Distortion,
}

impl PadControl {
    /// All controls in navigation order
    pub fn all() -> &'static [PadControl] {
        &[
            PadControl::Sample,
            PadControl::Volume,
            PadControl::Mute,
            PadControl::BpmMultiplier,
            PadControl::HighPass,
            PadControl::LowPass,
            PadControl::EqLow,
            PadControl::EqMid,
            PadControl::EqHigh,
            PadControl::FiltersHeader,
            PadControl::Reverb,
            PadControl::Chorus,
            PadControl::Distortion,
        ]
    }

    /// Whether this control is continuous (adjustable with hjkl)
    pub fn is_continuous(&self) -> bool {
        matches!(
            self,
            PadControl::Volume
                | PadControl::BpmMultiplier
                | PadControl::HighPass
                | PadControl::LowPass
                | PadControl::EqLow
                | PadControl::EqMid
                | PadControl::EqHigh
                | PadControl::Reverb
                | PadControl::Chorus
                | PadControl::Distortion
        )
    }

    /// Whether this control is a toggle
    pub fn is_toggle(&self) -> bool {
        matches!(self, PadControl::Mute)
    }

    /// Label for display
    pub fn label(&self) -> &'static str {
        match self {
            PadControl::Sample => "Sample",
            PadControl::Volume => "Volume",
            PadControl::Mute => "Mute",
            PadControl::BpmMultiplier => "Speed",
            PadControl::HighPass => "High Pass",
            PadControl::LowPass => "Low Pass",
            PadControl::EqLow => "EQ Low",
            PadControl::EqMid => "EQ Mid",
            PadControl::EqHigh => "EQ High",
            PadControl::FiltersHeader => "Filters",
            PadControl::Reverb => "Reverb",
            PadControl::Chorus => "Chorus",
            PadControl::Distortion => "Distort",
        }
    }
}

/// A single sample pad
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamplePad {
    /// Display name for the pad
    pub name: String,
    /// Path to the sample file
    pub sample_path: Option<PathBuf>,
    /// Color for the pad (RGB)
    pub color: (u8, u8, u8),
    /// Volume level (0.0 to 1.0)
    pub volume: f32,
    /// Is currently playing (for loops/toggle modes)
    #[serde(skip)]
    pub playing: bool,
    /// Was just triggered (for visual flash, decays after 1 frame)
    #[serde(skip)]
    pub triggered: bool,
    /// Trigger decay counter (frames until triggered clears)
    #[serde(skip)]
    pub trigger_frames: u8,
    /// Pad index (0-15)
    pub index: usize,
    /// Per-pad DSP configuration
    pub config: PadConfig,
}

impl SamplePad {
    pub fn new(index: usize) -> Self {
        // Default colors based on row (like Launchpad)
        let color = match index / 4 {
            0 => (255, 100, 100), // Row 1: Red-ish
            1 => (100, 255, 100), // Row 2: Green-ish
            2 => (100, 100, 255), // Row 3: Blue-ish
            _ => (255, 255, 100), // Row 4: Yellow-ish
        };

        Self {
            name: format!("Pad {}", index + 1),
            sample_path: None,
            color,
            volume: 1.0,
            playing: false,
            triggered: false,
            trigger_frames: 0,
            index,
            config: PadConfig::default(),
        }
    }

    /// Get the key character for this pad
    pub fn key_char(&self) -> char {
        let row = self.index / 4;
        let col = self.index % 4;
        PAD_KEYS[row][col]
    }

    /// Check if this pad has a sample assigned
    pub fn has_sample(&self) -> bool {
        self.sample_path.is_some()
    }
}

/// The 4x4 sample pad grid
#[derive(Debug, Clone)]
pub struct SamplePadGrid {
    /// 16 pads in a 4x4 grid
    pub pads: [SamplePad; 16],
    /// Currently selected pad for editing
    pub selected_pad: usize,
    /// Is pad mode active (keys trigger pads instead of normal functions)
    pub active: bool,
    /// Is configuration mode active
    pub config_mode: bool,
    /// Currently selected control in config pane
    pub selected_control: PadControl,
    /// Is a config control being edited (level 3)
    pub editing_control: bool,
}

impl SamplePadGrid {
    pub fn new() -> Self {
        let pads = std::array::from_fn(SamplePad::new);

        Self {
            pads,
            selected_pad: 0,
            active: false,
            config_mode: false,
            selected_control: PadControl::Sample,
            editing_control: false,
        }
    }

    /// Find pad index by key character
    pub fn pad_index_for_key(&self, key: char) -> Option<usize> {
        let key_lower = key.to_ascii_lowercase();
        for (row_idx, row) in PAD_KEYS.iter().enumerate() {
            for (col_idx, &k) in row.iter().enumerate() {
                if k == key_lower {
                    return Some(row_idx * 4 + col_idx);
                }
            }
        }
        None
    }

    /// Trigger a pad by key
    pub fn trigger_by_key(&mut self, key: char) -> Option<usize> {
        if let Some(index) = self.pad_index_for_key(key) {
            self.trigger_pad(index);
            Some(index)
        } else {
            None
        }
    }

    /// Trigger a pad (momentary flash)
    pub fn trigger_pad(&mut self, index: usize) {
        if index < self.pads.len() {
            let pad = &mut self.pads[index];
            pad.triggered = true;
            pad.trigger_frames = 3;
        }
    }

    /// Release a pad (no-op, kept for API compatibility)
    pub fn release_pad(&mut self, _index: usize) {}

    /// Stop all pads
    pub fn stop_all(&mut self) {
        for pad in &mut self.pads {
            pad.playing = false;
            pad.triggered = false;
            pad.trigger_frames = 0;
        }
    }

    /// Assign sample to specific pad by index
    pub fn assign_sample_to_pad(&mut self, index: usize, path: PathBuf, name: Option<String>) {
        if index < self.pads.len() {
            let pad = &mut self.pads[index];
            pad.sample_path = Some(path.clone());
            if let Some(n) = name {
                pad.name = n;
            } else {
                // Use filename as name
                pad.name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Sample")
                    .to_string();
            }
        }
    }

    /// Move config control selection up
    pub fn config_control_up(&mut self) {
        let controls = PadControl::all();
        if let Some(pos) = controls.iter().position(|c| *c == self.selected_control)
            && pos > 0
        {
            let mut new_pos = pos - 1;
            // Skip over FiltersHeader (non-interactive)
            while new_pos > 0 && controls[new_pos] == PadControl::FiltersHeader {
                new_pos -= 1;
            }
            self.selected_control = controls[new_pos];
        }
    }

    /// Move config control selection down
    pub fn config_control_down(&mut self) {
        let controls = PadControl::all();
        if let Some(pos) = controls.iter().position(|c| *c == self.selected_control)
            && pos + 1 < controls.len()
        {
            let mut new_pos = pos + 1;
            // Skip over FiltersHeader (non-interactive)
            while new_pos < controls.len() - 1 && controls[new_pos] == PadControl::FiltersHeader {
                new_pos += 1;
            }
            self.selected_control = controls[new_pos];
        }
    }

    /// Adjust the currently selected config control by delta
    pub fn adjust_selected_config(&mut self, delta: f32) {
        if let Some(pad) = self.pads.get_mut(self.selected_pad) {
            match self.selected_control {
                PadControl::Volume => {
                    pad.config.volume = (pad.config.volume + delta).clamp(0.0, 2.0);
                }
                PadControl::HighPass => {
                    // Multiplicative: ~10% per fine step, ~40% per coarse step
                    let factor = 1.0 + delta * 2.0;
                    pad.config.high_pass = (pad.config.high_pass * factor).clamp(20.0, 20000.0);
                }
                PadControl::LowPass => {
                    let factor = 1.0 + delta * 2.0;
                    pad.config.low_pass = (pad.config.low_pass * factor).clamp(20.0, 20000.0);
                }
                PadControl::EqLow => {
                    pad.config.eq_low = (pad.config.eq_low + delta).clamp(0.0, 2.0);
                }
                PadControl::EqMid => {
                    pad.config.eq_mid = (pad.config.eq_mid + delta).clamp(0.0, 2.0);
                }
                PadControl::EqHigh => {
                    pad.config.eq_high = (pad.config.eq_high + delta).clamp(0.0, 2.0);
                }
                PadControl::Reverb => {
                    pad.config.reverb = (pad.config.reverb + delta).clamp(0.0, 1.0);
                }
                PadControl::Chorus => {
                    pad.config.chorus = (pad.config.chorus + delta).clamp(0.0, 1.0);
                }
                PadControl::Distortion => {
                    pad.config.distortion = (pad.config.distortion + delta).clamp(0.0, 1.0);
                }
                _ => {}
            }
        }
    }

    /// Toggle the currently selected config control (for Mute, EffectsHeader)
    pub fn toggle_selected_config(&mut self) {
        if let Some(pad) = self.pads.get_mut(self.selected_pad) {
            match self.selected_control {
                PadControl::Mute => {
                    pad.config.mute = !pad.config.mute;
                }
                PadControl::FiltersHeader => {
                    // Header is not toggleable, do nothing
                }
                _ => {}
            }
        }
    }

    /// Reset the currently selected config control to default
    pub fn reset_selected_config(&mut self) {
        if let Some(pad) = self.pads.get_mut(self.selected_pad) {
            match self.selected_control {
                PadControl::Volume => pad.config.volume = 0.5,
                PadControl::Mute => pad.config.mute = false,
                PadControl::HighPass => pad.config.high_pass = 20.0,
                PadControl::LowPass => pad.config.low_pass = 20000.0,
                PadControl::EqLow => pad.config.eq_low = 1.0,
                PadControl::EqMid => pad.config.eq_mid = 1.0,
                PadControl::EqHigh => pad.config.eq_high = 1.0,
                PadControl::Reverb => pad.config.reverb = 0.0,
                PadControl::Chorus => pad.config.chorus = 0.0,
                PadControl::Distortion => pad.config.distortion = 0.0,
                _ => {}
            }
        }
    }

    /// Update playing states (decay triggers)
    pub fn update(&mut self) {
        for pad in &mut self.pads {
            // Decay trigger flash
            if pad.triggered && pad.trigger_frames > 0 {
                pad.trigger_frames -= 1;
                if pad.trigger_frames == 0 {
                    pad.triggered = false;
                }
            }

            // Clear playing after trigger decay
            if !pad.triggered {
                pad.playing = false;
            }
        }
    }
}

impl Default for SamplePadGrid {
    fn default() -> Self {
        Self::new()
    }
}

pub const SEQUENCE_STEPS: usize = 16;

/// A step sequencer sequence tied to a pad
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    /// Which pad this sequence triggers
    pub pad_idx: usize,
    /// Display name
    pub name: String,
    /// Volume level (0.0 to 1.0)
    pub volume: f32,
    /// Mute toggle
    pub mute: bool,
    /// Tempo in BPM
    pub tempo: f32,
    /// 16-step pattern (true = marked, plays sample)
    pub pattern: [bool; SEQUENCE_STEPS],
    /// Whether currently playing
    #[serde(skip)]
    pub playing: bool,
    /// Current step being played (for UI highlight)
    #[serde(skip)]
    pub current_step: usize,
}

impl Sequence {
    pub fn new(pad_idx: usize, _seq_number: usize) -> Self {
        let key = PAD_KEYS[pad_idx / 4][pad_idx % 4];
        Self {
            pad_idx,
            name: format!("{}", key.to_ascii_uppercase()),
            volume: 0.8,
            mute: false,
            tempo: 1.0, // multiplier relative to global BPM
            pattern: [false; SEQUENCE_STEPS],
            playing: false,
            current_step: 0,
        }
    }

    /// Returns true if any step is marked
    pub fn any_marked(&self) -> bool {
        self.pattern.iter().any(|&s| s)
    }

    /// Step interval in seconds given a global BPM
    #[allow(dead_code)]
    pub fn step_interval_secs(&self, global_bpm: f32) -> f32 {
        let actual_bpm = global_bpm * self.tempo;
        60.0 / actual_bpm.clamp(20.0, 400.0) / 4.0
    }
}

/// Horizontal cursor target within a sequence row
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    Step(usize),
    Mute,
    Gear,
}

impl EditTarget {
    /// Total number of targets: 16 steps + mute + gear
    pub fn count() -> usize {
        SEQUENCE_STEPS + 2
    }

    /// Index of this target in the flat horizontal layout
    pub fn index(&self) -> usize {
        match self {
            EditTarget::Step(s) => *s,
            EditTarget::Mute => SEQUENCE_STEPS,
            EditTarget::Gear => SEQUENCE_STEPS + 1,
        }
    }

    /// Create from index
    pub fn from_index(i: usize) -> Self {
        if i < SEQUENCE_STEPS {
            EditTarget::Step(i)
        } else if i == SEQUENCE_STEPS {
            EditTarget::Mute
        } else {
            EditTarget::Gear
        }
    }

    /// Move right (wrapping)
    pub fn right(&self) -> Self {
        let next = (self.index() + 1) % Self::count();
        Self::from_index(next)
    }

    /// Move left (wrapping)
    pub fn left(&self) -> Self {
        let next = if self.index() == 0 {
            Self::count() - 1
        } else {
            self.index() - 1
        };
        Self::from_index(next)
    }
}

/// Global controls for all sequences (shown in top bar)
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GlobalSequenceControls {
    pub volume: f32,
    pub bpm: f32,
    pub mute: bool,
}

impl Default for GlobalSequenceControls {
    fn default() -> Self {
        Self {
            volume: 0.8,
            bpm: 120.0,
            mute: false,
        }
    }
}

/// Which global control is selected in the top bar
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalSequenceControl {
    Volume,
    Bpm,
    Save,
    Load,
    Mute,
}

/// State for sequence management
#[derive(Debug, Clone)]
pub struct SequenceState {
    /// All sequences
    pub sequences: Vec<Sequence>,
    /// Currently selected sequence index (None = global bar selected)
    pub selected: Option<usize>,
    /// Scroll offset for visible sequences
    pub scroll_offset: usize,
    /// Global controls for all sequences
    pub global: GlobalSequenceControls,
    /// Currently selected global control in top bar
    pub global_control: GlobalSequenceControl,
    /// Whether the global bar is focused (vs per-sequence)
    pub global_focused: bool,
    /// Current horizontal cursor target (steps/mute/multiplier)
    pub cursor: EditTarget,
    /// Per-sequence play state saved before global sequence mute
    pub previously_playing: Vec<bool>,
    /// Per-sequence play state saved before master pause
    pub master_previously_playing: Vec<bool>,
    /// Global sequence mute state saved before master pause
    pub master_previously_global_mute: bool,
}

impl SequenceState {
    pub fn new() -> Self {
        Self {
            sequences: Vec::new(),
            selected: None,
            scroll_offset: 0,
            global: GlobalSequenceControls::default(),
            global_control: GlobalSequenceControl::Volume,
            global_focused: true,
            cursor: EditTarget::Step(0),
            previously_playing: Vec::new(),
            master_previously_playing: Vec::new(),
            master_previously_global_mute: false,
        }
    }

    /// Add a new sequence for a pad and select it
    pub fn add_sequence(&mut self, pad_idx: usize) -> usize {
        let seq = Sequence::new(pad_idx, self.sequences.len() + 1);
        self.sequences.push(seq);
        self.sort_sequences();
        // Find the newly added sequence by pad_idx (first match)
        let idx = self
            .sequences
            .iter()
            .position(|s| s.pad_idx == pad_idx)
            .unwrap_or(0);
        self.selected = Some(idx);
        idx
    }

    /// Sort sequences by pad grid order: 4,5,6,7,R,T,Y,U,F,G,H,J,V,B,N,M
    fn sort_sequences(&mut self) {
        self.sequences.sort_by_key(|s| s.pad_idx);
    }

    /// Remove a sequence by index
    #[allow(dead_code)]
    pub fn remove_sequence(&mut self, idx: usize) {
        if idx < self.sequences.len() {
            self.sequences.remove(idx);
            if self.sequences.is_empty() {
                self.selected = None;
            } else if let Some(sel) = self.selected
                && sel >= self.sequences.len()
            {
                self.selected = Some(self.sequences.len() - 1);
            }
        }
    }

    /// Move selection up (round-robin)
    pub fn select_up(&mut self) {
        if self.sequences.is_empty() {
            return;
        }
        if self.global_focused {
            self.global_focused = false;
            self.selected = Some(self.sequences.len() - 1);
            return;
        }
        match self.selected {
            Some(0) => {
                // Move to global bar
                self.selected = None;
                self.global_focused = true;
            }
            Some(i) => self.selected = Some(i - 1),
            None => {
                self.selected = Some(self.sequences.len() - 1);
                self.global_focused = false;
            }
        }
    }

    /// Move selection down (round-robin)
    pub fn select_down(&mut self) {
        if self.sequences.is_empty() {
            return;
        }
        if self.global_focused {
            self.global_focused = false;
            self.selected = Some(0);
            return;
        }
        match self.selected {
            Some(i) if i + 1 < self.sequences.len() => self.selected = Some(i + 1),
            Some(_) => {
                self.selected = None;
                self.global_focused = true;
            }
            None => {
                self.selected = Some(0);
                self.global_focused = false;
            }
        }
    }

    /// Move cursor left (wrapping) within a sequence row
    pub fn select_control_up(&mut self) {
        if self.global_focused {
            self.select_global_control_left();
            return;
        }
        self.cursor = self.cursor.left();
    }

    /// Move cursor right (wrapping) within a sequence row
    pub fn select_control_down(&mut self) {
        if self.global_focused {
            self.select_global_control_right();
            return;
        }
        self.cursor = self.cursor.right();
    }

    /// Move global control selection left/right
    pub fn select_global_control_left(&mut self) {
        let controls = [
            GlobalSequenceControl::Volume,
            GlobalSequenceControl::Bpm,
            GlobalSequenceControl::Save,
            GlobalSequenceControl::Load,
            GlobalSequenceControl::Mute,
        ];
        let idx = controls
            .iter()
            .position(|&c| c == self.global_control)
            .unwrap_or(0);
        let new_idx = if idx == 0 {
            controls.len() - 1
        } else {
            idx - 1
        };
        self.global_control = controls[new_idx];
    }

    pub fn select_global_control_right(&mut self) {
        let controls = [
            GlobalSequenceControl::Volume,
            GlobalSequenceControl::Bpm,
            GlobalSequenceControl::Save,
            GlobalSequenceControl::Load,
            GlobalSequenceControl::Mute,
        ];
        let idx = controls
            .iter()
            .position(|&c| c == self.global_control)
            .unwrap_or(0);
        let new_idx = if idx + 1 >= controls.len() {
            0
        } else {
            idx + 1
        };
        self.global_control = controls[new_idx];
    }

    /// Toggle a step in the selected sequence
    pub fn toggle_step(&mut self, step: usize) {
        if let Some(sel) = self.selected
            && let Some(seq) = self.sequences.get_mut(sel)
        {
            seq.pattern[step] = !seq.pattern[step];
            // Auto-play when any step is marked, auto-stop when all unmarked
            seq.playing = seq.any_marked();
        }
    }
}

impl Default for SequenceState {
    fn default() -> Self {
        Self::new()
    }
}

/// Serializable session state for saving/loading pads and sequences
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    /// All 16 sample pads with their configuration
    pub pads: [SamplePad; 16],
    /// All sequences
    pub sequences: Vec<Sequence>,
    /// Global sequence controls (volume, BPM, mute)
    pub global: GlobalSequenceControls,
}

impl SessionState {
    /// Create a session state snapshot from current app state
    pub fn from_current(pads: &[SamplePad; 16], sequence_state: &SequenceState) -> Self {
        Self {
            pads: pads.clone(),
            sequences: sequence_state.sequences.clone(),
            global: sequence_state.global,
        }
    }

    /// Save session to a JSON file
    pub fn save_to_file(&self, path: &std::path::Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize session: {}", e))?;
        std::fs::write(path, json).map_err(|e| format!("Failed to write file: {}", e))
    }

    /// Load session from a JSON file
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, String> {
        let json =
            std::fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;
        serde_json::from_str(&json).map_err(|e| format!("Failed to parse session: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pad_keys() {
        let grid = SamplePadGrid::new();

        assert_eq!(grid.pad_index_for_key('4'), Some(0));
        assert_eq!(grid.pad_index_for_key('7'), Some(3));
        assert_eq!(grid.pad_index_for_key('r'), Some(4));
        assert_eq!(grid.pad_index_for_key('R'), Some(4)); // Case insensitive
        assert_eq!(grid.pad_index_for_key('m'), Some(15));
        assert_eq!(grid.pad_index_for_key('z'), None);
    }

    #[test]
    fn test_pad_position() {
        let pad = SamplePad::new(5);
        assert_eq!(pad.key_char(), 't');
    }
}
