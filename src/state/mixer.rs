//! Mixer channel state and controls

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// DJ section controls
#[derive(Debug, Clone)]
pub struct DjSection {
    /// Crossfader position (-1.0 = full A, 0.0 = center, 1.0 = full B)
    pub crossfader: f32,
    /// Headphone volume
    pub headphone_volume: f32,
    /// Which channel is assigned to deck A (usually 0)
    pub deck_a_channel: usize,
    /// Which channel is assigned to deck B (usually 1)
    pub deck_b_channel: usize,
    /// Which channel is assigned to deck C / CUE (usually 2)
    pub deck_c_channel: usize,
}

impl DjSection {
    pub fn new() -> Self {
        Self {
            crossfader: 0.0, // Center
            headphone_volume: 0.75,
            deck_a_channel: 0,
            deck_b_channel: 1,
            deck_c_channel: 2,
        }
    }
}

impl Default for DjSection {
    fn default() -> Self {
        Self::new()
    }
}

/// A single mixer channel with all its controls
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixerChannel {
    /// Channel name/label
    pub name: String,
    /// Channel index (0-based)
    pub index: usize,
    /// Main fader level (0.0 to 1.0, can go to 1.5 for +6dB boost)
    pub fader: f32,
    /// Mute state
    pub muted: bool,
    /// Solo state
    pub solo: bool,
    /// Pan position (-1.0 left, 0.0 center, 1.0 right)
    pub pan: f32,
    /// Filter cutoff amount (0.0 = off, 1.0 = full intensity)
    pub filter_cutoff: f32,
    /// Filter frequency position (0.0 = low/20Hz, 0.5 = center/1kHz, 1.0 = high/20kHz)
    pub filter_freq: f32,
    /// LFO shape (0.0 = square, 1.0 = sine)
    pub lfo_shape: f32,
    /// LFO speed (0.0 = slow, 1.0 = fast)
    pub lfo_speed: f32,
    /// LFO phase accumulator (in radians)
    #[serde(skip)]
    pub lfo_phase: f32,
    /// Counter to throttle lavfi sync calls (0 = sync now)
    #[serde(skip)]
    pub lfo_sync_tick: u32,
    /// Previous LFO speed for detecting activation from idle
    #[serde(skip)]
    pub prev_lfo_speed: f32,
    /// Low shelf EQ gain (-15 to +15 dB)
    pub eq_low: f32,
    /// Mid EQ gain (-15 to +15 dB)
    pub eq_mid: f32,
    /// High shelf EQ gain (-15 to +15 dB)
    pub eq_high: f32,
    /// EQ Low kill switch (true = cut to -∞)
    #[serde(default)]
    pub eq_low_kill: bool,
    /// EQ Mid kill switch (true = cut to -∞)
    #[serde(default)]
    pub eq_mid_kill: bool,
    /// EQ High kill switch (true = cut to -∞)
    #[serde(default)]
    pub eq_high_kill: bool,
    /// Pre-fader listen (PFL) / Cue
    pub pfl: bool,
    /// Channel is connected to audio source
    pub connected: bool,
    /// Source identifier (socket path, device name, etc.)
    #[serde(default)]
    pub source_id: Option<String>,
    /// Peak meter level (0.0 to 1.0)
    pub peak_level: f32,
    /// RMS level for metering (0.0 to 1.0)
    pub rms_level: f32,
    /// Left channel peak level (0.0 to 1.0)
    #[serde(default)]
    pub peak_left: f32,
    /// Right channel peak level (0.0 to 1.0)
    #[serde(default)]
    pub peak_right: f32,
    /// Left channel RMS level (0.0 to 1.0)
    #[serde(default)]
    pub rms_left: f32,
    /// Right channel RMS level (0.0 to 1.0)
    #[serde(default)]
    pub rms_right: f32,
    /// Is the source currently playing audio
    #[serde(default)]
    pub playing: bool,
    /// Playback speed multiplier (1.0 = normal, 0.0 = stopped, 2.0 = 2x speed)
    #[serde(default = "default_playback_speed")]
    pub playback_speed: f32,
    /// Detected BPM of the currently loaded track (None = not yet analyzed)
    #[serde(default)]
    pub bpm: Option<f32>,
    /// Base BPM captured at track load — used for stable speed factor display
    #[serde(default)]
    pub base_bpm: f32,
    /// Target BPM for playback speed adjustment (user-controlled)
    #[serde(default)]
    pub target_bpm: f32,
    /// Detected musical key in Camelot Wheel notation (e.g., "8A", "12B")
    #[serde(default)]
    pub key: Option<String>,
    /// Key offset in semitones from detected key (user-controlled via edit mode)
    #[serde(default)]
    pub key_offset: i32,
    /// Current playback position in seconds (polled from MPV)
    #[serde(default)]
    pub time_pos: f32,
    /// Total duration of the current track in seconds
    #[serde(default)]
    pub duration: f32,
    /// Age of last timeline update in milliseconds (0 = fresh/current tick)
    #[serde(default)]
    pub timeline_age_ms: u32,
    /// Whether playlist has a previous track available
    #[serde(default)]
    pub has_prev_track: bool,
    /// Whether playlist has a next track available
    #[serde(default)]
    pub has_next_track: bool,
    /// Last time PREV action was executed (ms since app start)
    #[serde(skip)]
    pub prev_exec_flash_ms: u64,
    /// Last time NEXT action was executed (ms since app start)
    #[serde(skip)]
    pub next_exec_flash_ms: u64,
    /// Scrub direction: -1.0 = reverse, 0.0 = stopped, 1.0 = forward
    #[serde(skip)]
    pub scrub_direction: f32,
    /// Current scrub speed multiplier (accelerates while held)
    #[serde(skip)]
    pub scrub_speed: f32,
    /// Whether the scrub was coarse (H/J/K/L) for faster acceleration
    #[serde(skip)]
    pub scrub_coarse: bool,
    /// Accumulated seek amount (seek when threshold reached)
    #[serde(skip)]
    pub scrub_accumulator: f32,
    /// Whether this channel's source is SuperCollider (disables scrub)
    #[serde(skip)]
    pub uses_supercollider: bool,
    /// Per-band spectrum peaks for EQ meter overlay (L/M/H)
    #[serde(skip)]
    pub spectrum_peaks: [f32; 3],
    /// Per-band spectrum decay state
    #[serde(skip)]
    pub spectrum_decay: [f32; 3],
}

fn default_playback_speed() -> f32 {
    1.0
}

impl MixerChannel {
    pub fn new(name: impl Into<String>, index: usize) -> Self {
        Self {
            name: name.into(),
            index,
            fader: 0.5, // Unity gain at center
            muted: false,
            solo: false,
            pan: 0.0,
            filter_cutoff: 0.0, // Off
            filter_freq: 0.5,   // Center (1kHz)
            lfo_shape: 0.5,     // Blend (square/sine)
            lfo_speed: 0.0,     // Slow
            lfo_phase: 0.0,
            lfo_sync_tick: 0,
            prev_lfo_speed: 0.0,
            eq_low: 0.0,
            eq_mid: 0.0,
            eq_high: 0.0,
            eq_low_kill: false,
            eq_mid_kill: false,
            eq_high_kill: false,
            pfl: false,
            connected: false,
            source_id: None,
            peak_level: 0.0,
            rms_level: 0.0,
            peak_left: 0.0,
            peak_right: 0.0,
            rms_left: 0.0,
            rms_right: 0.0,
            playing: false,
            playback_speed: 1.0,
            bpm: None,
            base_bpm: 0.0,
            target_bpm: 120.0,
            key: None,
            key_offset: 0,
            time_pos: 0.0,
            duration: 0.0,
            timeline_age_ms: 0,
            has_prev_track: false,
            has_next_track: false,
            prev_exec_flash_ms: 0,
            next_exec_flash_ms: 0,
            scrub_direction: 0.0,
            scrub_speed: 0.0,
            scrub_coarse: false,
            scrub_accumulator: 0.0,
            uses_supercollider: false,
            spectrum_peaks: [0.0; 3],
            spectrum_decay: [0.0; 3],
        }
    }

    /// Convert fader position to dB
    pub fn fader_db(&self) -> f32 {
        if self.fader <= 0.0 {
            f32::NEG_INFINITY
        } else {
            // Logarithmic scale: 0.5 = 0dB (center), 1.0 = +6dB, 0.0 = -inf
            20.0 * (self.fader / 0.5).log10()
        }
    }

    /// Whether scrub controls should be available for this channel.
    ///
    /// MPV route-mode PCM streams often report no finite duration, so
    /// visibility can't rely on `duration > 0.0` alone.
    pub fn scrub_available(&self) -> bool {
        self.connected
            && !self.uses_supercollider
            && (self.duration > 0.0 || self.time_pos > 0.0 || self.playing)
    }
}

/// Which control is currently selected within a channel
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelControl {
    PrevTrack,
    PlayPause,
    NextTrack,
    Scrub,
    Bpm,
    Key,
    Fader,
    Pan,
    FilterCutoff,
    FilterFreq,
    LfoShape,
    LfoSpeed,
    EqLow,
    EqLowKill,
    EqMid,
    EqMidKill,
    EqHigh,
    EqHighKill,
    Mute,
    Solo,
    // CUE-specific controls (Deck C only)
    CueSendToA,
    CueSendToB,
    CueOutputSelect,
}

impl ChannelControl {
    /// Label for display in control select mode
    pub fn label(&self) -> &'static str {
        match self {
            ChannelControl::PrevTrack => "PREV",
            ChannelControl::PlayPause => "PLAY",
            ChannelControl::NextTrack => "NEXT",
            ChannelControl::Scrub => "SCRUB",
            ChannelControl::Bpm => "BPM",
            ChannelControl::Key => "KEY",
            ChannelControl::Fader => "GAIN",
            ChannelControl::Pan => "PAN",
            ChannelControl::FilterCutoff => "CUTOFF",
            ChannelControl::FilterFreq => "FREQUENCY",
            ChannelControl::LfoShape => "SHAPE",
            ChannelControl::LfoSpeed => "SPEED",
            ChannelControl::EqLow => "LOW",
            ChannelControl::EqLowKill => "L×",
            ChannelControl::EqMid => "MID",
            ChannelControl::EqMidKill => "M×",
            ChannelControl::EqHigh => "HIGH",
            ChannelControl::EqHighKill => "H×",
            ChannelControl::Mute => "MUTE",
            ChannelControl::Solo => "SOLO",
            ChannelControl::CueSendToA => " A",
            ChannelControl::CueSendToB => " B",
            ChannelControl::CueOutputSelect => "OUTPUT",
        }
    }

    /// Is this a continuous control (knob/fader) vs toggle (button)
    pub fn is_continuous(&self) -> bool {
        matches!(
            self,
            ChannelControl::Fader
                | ChannelControl::Pan
                | ChannelControl::Scrub
                | ChannelControl::Bpm
                | ChannelControl::Key
                | ChannelControl::FilterCutoff
                | ChannelControl::FilterFreq
                | ChannelControl::LfoShape
                | ChannelControl::LfoSpeed
                | ChannelControl::EqLow
                | ChannelControl::EqMid
                | ChannelControl::EqHigh
        )
    }

    /// Get the index in the vertical layout (top to bottom)
    /// PlayPause, Bpm, and Key are only available for deck channels (A/B)
    /// Kill switches share rows with their EQ controls
    /// CueControl items are rows 13-15 (Deck C only)
    pub fn row_index(&self) -> usize {
        match self {
            ChannelControl::Scrub => 0,
            ChannelControl::PrevTrack | ChannelControl::PlayPause | ChannelControl::NextTrack => 1,
            ChannelControl::Bpm => 2,
            ChannelControl::Key => 3,
            ChannelControl::EqHigh | ChannelControl::EqHighKill => 4,
            ChannelControl::EqMid | ChannelControl::EqMidKill => 5,
            ChannelControl::EqLow | ChannelControl::EqLowKill => 6,
            ChannelControl::FilterCutoff => 7,
            ChannelControl::FilterFreq => 8,
            ChannelControl::LfoShape => 9,
            ChannelControl::LfoSpeed => 10,
            ChannelControl::Pan => 11,
            ChannelControl::Fader => 12,
            ChannelControl::Mute => 13,
            ChannelControl::Solo => 14,
            ChannelControl::CueSendToA => 15,
            ChannelControl::CueSendToB => 16,
            ChannelControl::CueOutputSelect => 16,
        }
    }

    pub fn from_row_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(ChannelControl::Scrub),
            1 => Some(ChannelControl::PlayPause),
            2 => Some(ChannelControl::Bpm),
            3 => Some(ChannelControl::Key),
            4 => Some(ChannelControl::EqHigh),
            5 => Some(ChannelControl::EqMid),
            6 => Some(ChannelControl::EqLow),
            7 => Some(ChannelControl::FilterCutoff),
            8 => Some(ChannelControl::FilterFreq),
            9 => Some(ChannelControl::LfoShape),
            10 => Some(ChannelControl::LfoSpeed),
            11 => Some(ChannelControl::Pan),
            12 => Some(ChannelControl::Fader),
            13 => Some(ChannelControl::Mute),
            14 => Some(ChannelControl::Solo),
            15 => Some(ChannelControl::CueSendToA),
            16 => Some(ChannelControl::CueSendToB),
            17 => Some(ChannelControl::CueOutputSelect),
            _ => None,
        }
    }

    /// Get row index for non-deck channels (no PlayPause/Bpm/Key)
    pub fn row_index_no_deck(&self) -> usize {
        match self {
            ChannelControl::PrevTrack => 0,
            ChannelControl::NextTrack => 0,
            ChannelControl::PlayPause => 0, // Not used, but fallback
            ChannelControl::Scrub => 0,     // Not used, but fallback
            ChannelControl::Bpm => 0,       // Not used for non-deck, fallback
            ChannelControl::Key => 0,       // Not used for non-deck, fallback
            ChannelControl::EqHigh | ChannelControl::EqHighKill => 0,
            ChannelControl::EqMid | ChannelControl::EqMidKill => 1,
            ChannelControl::EqLow | ChannelControl::EqLowKill => 2,
            ChannelControl::FilterCutoff => 3,
            ChannelControl::FilterFreq => 4,
            ChannelControl::LfoShape => 5,
            ChannelControl::LfoSpeed => 6,
            ChannelControl::Pan => 7,
            ChannelControl::Fader => 8,
            ChannelControl::Mute => 9,
            ChannelControl::Solo => 10,
            // CUE controls don't apply to non-deck channels
            ChannelControl::CueSendToA => 11,
            ChannelControl::CueSendToB => 12,
            ChannelControl::CueOutputSelect => 13,
        }
    }

    pub fn from_row_index_no_deck(index: usize) -> Option<Self> {
        match index {
            0 => Some(ChannelControl::EqHigh),
            1 => Some(ChannelControl::EqMid),
            2 => Some(ChannelControl::EqLow),
            3 => Some(ChannelControl::FilterCutoff),
            4 => Some(ChannelControl::FilterFreq),
            5 => Some(ChannelControl::LfoShape),
            6 => Some(ChannelControl::LfoSpeed),
            7 => Some(ChannelControl::Pan),
            8 => Some(ChannelControl::Fader),
            9 => Some(ChannelControl::Mute),
            10 => Some(ChannelControl::Solo),
            _ => None,
        }
    }
}

/// Center frequencies for the 10-band master graphic EQ
pub const MASTER_EQ_FREQUENCIES: [f32; 10] = [
    32.0, 64.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// Master channel with additional controls
#[derive(Debug, Clone)]
pub struct MasterChannel {
    /// Master fader level
    pub fader: f32,
    /// Microphone fader level (0.5 is unity)
    pub mic_fader: f32,
    /// Microphone mute
    pub mic_muted: bool,
    /// Master mute
    pub muted: bool,
    /// Master play/pause (controls all decks and loops)
    pub playing: bool,
    /// Left peak level
    pub peak_left: f32,
    /// Right peak level
    pub peak_right: f32,
    /// Left RMS level (for meter fill)
    pub rms_left: f32,
    /// Right RMS level (for meter fill)
    pub rms_right: f32,
    /// 10-band master EQ gain per band (dB), range -12.0 to +12.0
    /// Bands: 32Hz, 64Hz, 125Hz, 250Hz, 500Hz, 1kHz, 2kHz, 4kHz, 8kHz, 16kHz
    pub master_eq: [f32; 10],
    /// Live spectrum peak levels per band (0.0 to 1.0), updated each tick
    pub spectrum_peaks: [f32; 10],
    /// Spectrum peak decay values for smooth falloff
    pub spectrum_decay: [f32; 10],
}

impl MasterChannel {
    pub fn new() -> Self {
        Self {
            fader: 0.5,
            mic_fader: 0.5,
            mic_muted: true,
            muted: false,
            playing: true,
            peak_left: 0.0,
            peak_right: 0.0,
            rms_left: 0.0,
            rms_right: 0.0,
            master_eq: [0.0; 10],
            spectrum_peaks: [0.0; 10],
            spectrum_decay: [0.0; 10],
        }
    }

    pub fn fader_db(&self) -> f32 {
        if self.fader <= 0.0 {
            f32::NEG_INFINITY
        } else {
            20.0 * (self.fader / 0.5).log10()
        }
    }
}

impl Default for MasterChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Which global control is selected (DJ section, master)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlobalControl {
    MasterPlayPause,
    Crossfader,
    HeadphoneVolume,
    MasterFader,
    MicFader,
    MicMute,
    MicInputSelect,
    MasterMute,
    MasterOutputSelect,
    MasterEq32,
    MasterEq64,
    MasterEq125,
    MasterEq250,
    MasterEq500,
    MasterEq1k,
    MasterEq2k,
    MasterEq4k,
    MasterEq8k,
    MasterEq16k,
}

impl GlobalControl {
    /// Label for display in control select mode
    pub fn label(&self) -> &'static str {
        match self {
            GlobalControl::MasterPlayPause => "PLAY",
            GlobalControl::Crossfader => "X-FADER",
            GlobalControl::HeadphoneVolume => "PHONES",
            GlobalControl::MasterFader => "GAIN",
            GlobalControl::MicFader => "MIC GAIN",
            GlobalControl::MicMute => "MIC MUTE",
            GlobalControl::MicInputSelect => "MIC INPUT",
            GlobalControl::MasterMute => "MUTE",
            GlobalControl::MasterOutputSelect => "OUTPUT",
            GlobalControl::MasterEq32 => "32",
            GlobalControl::MasterEq64 => "64",
            GlobalControl::MasterEq125 => "125",
            GlobalControl::MasterEq250 => "250",
            GlobalControl::MasterEq500 => "500",
            GlobalControl::MasterEq1k => "1k",
            GlobalControl::MasterEq2k => "2k",
            GlobalControl::MasterEq4k => "4k",
            GlobalControl::MasterEq8k => "8k",
            GlobalControl::MasterEq16k => "16k",
        }
    }

    /// Returns the index into `master_eq` array if this is an EQ control
    pub fn eq_band_index(&self) -> Option<usize> {
        match self {
            GlobalControl::MasterEq32 => Some(0),
            GlobalControl::MasterEq64 => Some(1),
            GlobalControl::MasterEq125 => Some(2),
            GlobalControl::MasterEq250 => Some(3),
            GlobalControl::MasterEq500 => Some(4),
            GlobalControl::MasterEq1k => Some(5),
            GlobalControl::MasterEq2k => Some(6),
            GlobalControl::MasterEq4k => Some(7),
            GlobalControl::MasterEq8k => Some(8),
            GlobalControl::MasterEq16k => Some(9),
            _ => None,
        }
    }

    /// All EQ control variants in band order
    pub fn all_eq_variants() -> [GlobalControl; 10] {
        [
            GlobalControl::MasterEq32,
            GlobalControl::MasterEq64,
            GlobalControl::MasterEq125,
            GlobalControl::MasterEq250,
            GlobalControl::MasterEq500,
            GlobalControl::MasterEq1k,
            GlobalControl::MasterEq2k,
            GlobalControl::MasterEq4k,
            GlobalControl::MasterEq8k,
            GlobalControl::MasterEq16k,
        ]
    }
}

/// Selection focus - either on a channel or on global controls
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionFocus {
    /// Focus on a channel strip
    Channel(usize),
    /// Focus on DJ/master section
    Global,
}

/// Full mixer state
#[derive(Debug, Clone)]
pub struct MixerState {
    /// Individual channels (up to 2 for this mixer)
    pub channels: Vec<MixerChannel>,
    /// CUE channel (headphone preview deck)
    pub cue_channel: MixerChannel,
    /// DJ section (crossfader, cue mix, etc.)
    pub dj: DjSection,
    /// Master output
    pub master: MasterChannel,
    /// Selection focus
    pub focus: SelectionFocus,
    /// Currently selected channel index (when focus is Channel)
    pub selected_channel: usize,
    /// Currently selected control within channel
    pub selected_control: ChannelControl,
    /// Currently selected global control (when focus is Global)
    pub selected_global: GlobalControl,
    /// Is any solo active (for solo-in-place logic)
    pub solo_active: bool,
    /// Per-channel fader levels saved before solo was activated (channel_idx -> fader).
    /// Each channel's fader is saved independently when it first enters solo mode,
    /// so switching between soloed decks preserves all their original levels.
    pub pre_solo_faders: HashMap<usize, f32>,
    /// CUE channel fader saved before solo was activated
    pub pre_solo_cue_fader: Option<f32>,
    /// Terminal height for computing fader step size
    pub terminal_height: u16,
    /// Per-channel play state saved before master pause (index 0 = deck A, 1 = deck B, 2 = CUE)
    pub previously_playing: Vec<bool>,
}

/// Target deck for sending CUE
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendTarget {
    A,
    B,
}

impl MixerState {
    pub fn new(num_channels: usize) -> Self {
        let channels = (0..num_channels)
            .map(|i| MixerChannel::new(format!("CH {}", i + 1), i))
            .collect();

        let mut cue_channel = MixerChannel::new("CUE", 2);
        cue_channel.pfl = true; // CUE channel is always in PFL mode

        Self {
            channels,
            cue_channel,
            dj: DjSection::new(),
            master: MasterChannel::new(),
            focus: SelectionFocus::Channel(0),
            selected_channel: 0,
            selected_control: ChannelControl::Fader,
            selected_global: GlobalControl::Crossfader,
            solo_active: false,
            pre_solo_faders: HashMap::new(),
            pre_solo_cue_fader: None,
            terminal_height: 24,
            previously_playing: vec![false; 3], // deck A, deck B, CUE
        }
    }

    pub fn selected_channel(&self) -> Option<&MixerChannel> {
        self.get_channel(self.selected_channel)
    }

    pub fn selected_channel_mut(&mut self) -> Option<&mut MixerChannel> {
        self.get_channel_mut(self.selected_channel)
    }

    /// Get a channel reference by index, handling Deck C's separate storage.
    pub fn get_channel(&self, idx: usize) -> Option<&MixerChannel> {
        if idx == self.dj.deck_c_channel {
            Some(&self.cue_channel)
        } else {
            self.channels.get(idx)
        }
    }

    /// Get a mutable channel reference by index, handling Deck C's separate storage.
    pub fn get_channel_mut(&mut self, idx: usize) -> Option<&mut MixerChannel> {
        if idx == self.dj.deck_c_channel {
            Some(&mut self.cue_channel)
        } else {
            self.channels.get_mut(idx)
        }
    }

    /// Check if a channel index is a deck channel (has PlayPause/BPM controls)
    pub fn is_deck_channel(&self, ch_idx: usize) -> bool {
        ch_idx == self.dj.deck_a_channel
            || ch_idx == self.dj.deck_b_channel
            || ch_idx == self.dj.deck_c_channel
    }

    /// Check if the current control is in the EQ section (High/Mid/Low + kill switches)
    pub fn is_in_eq_section(&self) -> bool {
        matches!(
            self.selected_control,
            ChannelControl::EqHigh
                | ChannelControl::EqHighKill
                | ChannelControl::EqMid
                | ChannelControl::EqMidKill
                | ChannelControl::EqLow
                | ChannelControl::EqLowKill
        )
    }

    /// Check if the current control is in the filter section (including LFO)
    pub fn is_in_filter_section(&self) -> bool {
        matches!(
            self.selected_control,
            ChannelControl::FilterCutoff
                | ChannelControl::FilterFreq
                | ChannelControl::LfoShape
                | ChannelControl::LfoSpeed
        )
    }

    /// Check if the current control is in the EQ or filter section
    pub fn is_in_eq_or_filter_section(&self) -> bool {
        self.is_in_eq_section() || self.is_in_filter_section()
    }

    /// Swap CUE with Deck A or B while keeping each channel's physical index.
    pub fn swap_cue_with_deck(&mut self, target: SendTarget) {
        let target_idx = match target {
            SendTarget::A => self.dj.deck_a_channel,
            SendTarget::B => self.dj.deck_b_channel,
        };

        if let Some(target_channel) = self.channels.get_mut(target_idx) {
            std::mem::swap(target_channel, &mut self.cue_channel);
            target_channel.index = target_idx;
            target_channel.pfl = false;
            self.cue_channel.index = self.dj.deck_c_channel;
            self.cue_channel.pfl = true;
        }

        let saved_target_fader = self.pre_solo_faders.remove(&target_idx);
        let saved_cue_fader = self.pre_solo_cue_fader.take();
        if let Some(fader) = saved_cue_fader {
            self.pre_solo_faders.insert(target_idx, fader);
        }
        self.pre_solo_cue_fader = saved_target_fader;
        self.previously_playing
            .swap(target_idx, self.dj.deck_c_channel);
    }

    /// Move selection up (round-robin)
    /// `is_cue_pane` is true when the Deck C pane is active
    pub fn select_prev_control(&mut self, is_cue_pane: bool) {
        let is_deck = self.is_deck_channel(self.selected_channel);
        let current_row = if is_deck {
            self.selected_control.row_index()
        } else {
            self.selected_control.row_index_no_deck()
        };

        // Check if scrubber is visible.
        let scrub_visible = self
            .selected_channel()
            .map(MixerChannel::scrub_available)
            .unwrap_or(false);

        // Max row indices: CUE=16 (CueOutputSelect), Deck A/B=13 (Solo), Non-deck=10 (Solo)
        let max_row = if is_cue_pane {
            16
        } else if is_deck {
            13
        } else {
            10
        };
        // Min row: skip Scrub (0) when scrub is unavailable
        let min_row = if scrub_visible { 0 } else { 1 };
        let mut new_row = if current_row <= min_row {
            max_row
        } else {
            current_row - 1
        };

        // CUE deck visual layout:
        //   Row 1: CUT(6) | FREQ(7) | SHP(8) | SPD(9)
        //   Row 2: PAN(10)
        //   Row 3: Fader(11)
        //   Row 4: M(12) | ->A(14)
        //   Row 5: S(13) | ->B(15)
        //   Row 6: OUTPUT(16)
        if is_cue_pane {
            new_row = match current_row {
                16 => 15, // OUTPUT → ->B
                15 => 14, // ->B → ->A (up one row, same side)
                14 => 11, // →A → Fader
                13 => 12, // S → M (up, same side)
                12 => 11, // M → Fader
                11 => 10, // Fader → Pan
                10 => 9,  // Pan → SPD
                _ => {
                    if new_row <= min_row {
                        max_row
                    } else {
                        new_row
                    }
                }
            };
        } else if is_deck && current_row == 13 {
            // Decks A/B: S → Fader (skip M)
            new_row = 11;
        }

        // Skip Scrub when scrub is unavailable
        if !scrub_visible && new_row == 0 {
            new_row = max_row;
        }

        if is_deck || is_cue_pane {
            if let Some(ctrl) = ChannelControl::from_row_index(new_row) {
                self.selected_control = ctrl;
            }
        } else if let Some(ctrl) = ChannelControl::from_row_index_no_deck(new_row) {
            self.selected_control = ctrl;
        }
    }

    pub fn select_next_control(&mut self, is_cue_pane: bool) {
        let is_deck = self.is_deck_channel(self.selected_channel);
        let current_row = if is_deck {
            self.selected_control.row_index()
        } else {
            self.selected_control.row_index_no_deck()
        };

        // Check if scrubber is visible.
        let scrub_visible = self
            .selected_channel()
            .map(MixerChannel::scrub_available)
            .unwrap_or(false);

        // Max row indices: CUE=16 (CueOutputSelect), Deck A/B=13 (Solo), Non-deck=10 (Solo)
        let max_row = if is_cue_pane {
            16
        } else if is_deck {
            13
        } else {
            10
        };
        // Min row: skip Scrub (0) when scrub is unavailable
        let min_row = if scrub_visible { 0 } else { 1 };
        let mut new_row = if current_row >= max_row {
            min_row
        } else {
            current_row + 1
        };

        // Skip Scrub when scrub is unavailable
        if !scrub_visible && new_row == 0 {
            new_row = 1;
        }

        // CUE deck visual layout:
        //   Row 1: CUT(6) | FREQ(7) | SHP(8) | SPD(9)
        //   Row 2: PAN(10)
        //   Row 3: Fader(11)
        //   Row 4: M(12) | ->A(14)
        //   Row 5: S(13) | ->B(15)
        //   Row 6: OUTPUT(16)
        if is_cue_pane {
            new_row = match current_row {
                11 => 12,      // Fader → M
                12 => 13,      // M → S
                14 => 15,      // ->A → ->B (down one row)
                15 => 16,      // ->B → OUTPUT (down to row 6)
                13 => 16,      // S → OUTPUT (down)
                16 => min_row, // OUTPUT → wrap to top (skip Scrub if no track)
                _ => {
                    if new_row >= max_row {
                        min_row
                    } else {
                        new_row
                    }
                }
            };
        }

        if is_deck || is_cue_pane {
            if let Some(ctrl) = ChannelControl::from_row_index(new_row) {
                self.selected_control = ctrl;
            }
        } else if let Some(ctrl) = ChannelControl::from_row_index_no_deck(new_row) {
            self.selected_control = ctrl;
        }
    }

    /// Adjust the currently selected control
    pub fn adjust_selected(&mut self, delta: f32) {
        match self.focus {
            SelectionFocus::Channel(ch_idx) => {
                let channel = if ch_idx == 2 {
                    Some(&mut self.cue_channel)
                } else {
                    self.channels.get_mut(ch_idx)
                };

                if let Some(channel) = channel {
                    match self.selected_control {
                        ChannelControl::Fader => {
                            // One row per keypress: fader track = terminal - 28 rows
                            let track_h = self.terminal_height.saturating_sub(28).max(1) as f32;
                            let step = 1.0 / track_h;
                            channel.fader = (channel.fader + delta.signum() * step).clamp(0.0, 1.0);
                        }
                        ChannelControl::Scrub => {
                            if !channel.uses_supercollider {
                                channel.playback_speed =
                                    (channel.playback_speed + delta * 0.5).clamp(0.5, 2.0);
                            }
                        }
                        ChannelControl::Bpm => {
                            // Adjust target BPM: +/- 1 BPM per keypress
                            channel.target_bpm =
                                (channel.target_bpm + delta * 20.0).clamp(10.0, 400.0);
                        }
                        ChannelControl::Key => {
                            // Keep detected key as the base label; edits adjust semitone offset.
                            channel.key_offset += delta.signum() as i32;
                        }
                        ChannelControl::Pan => {
                            channel.pan = (channel.pan + delta).clamp(-1.0, 1.0);
                        }
                        ChannelControl::FilterCutoff => {
                            channel.filter_cutoff =
                                (channel.filter_cutoff + delta * 0.5).clamp(0.0, 1.0);
                        }
                        ChannelControl::FilterFreq => {
                            channel.filter_freq = (channel.filter_freq + delta).clamp(0.0, 1.0);
                        }
                        ChannelControl::LfoShape => {
                            channel.lfo_shape = (channel.lfo_shape + delta * 0.5).clamp(0.0, 1.0);
                        }
                        ChannelControl::LfoSpeed => {
                            channel.lfo_speed = (channel.lfo_speed + delta * 0.5).clamp(0.0, 1.0);
                        }
                        ChannelControl::EqLow => {
                            channel.eq_low = (channel.eq_low + delta * 6.0).clamp(-24.0, 24.0);
                        }
                        ChannelControl::EqMid => {
                            channel.eq_mid = (channel.eq_mid + delta * 6.0).clamp(-24.0, 24.0);
                        }
                        ChannelControl::EqHigh => {
                            channel.eq_high = (channel.eq_high + delta * 6.0).clamp(-24.0, 24.0);
                        }
                        _ => {} // Buttons, kills, CUE controls - no continuous adjustment
                    }
                }
            }
            SelectionFocus::Global => {
                match self.selected_global {
                    GlobalControl::Crossfader => {
                        self.dj.crossfader = (self.dj.crossfader + delta).clamp(-1.0, 1.0);
                    }
                    GlobalControl::HeadphoneVolume => {
                        self.dj.headphone_volume =
                            (self.dj.headphone_volume + delta).clamp(0.0, 1.0);
                    }
                    GlobalControl::MasterFader => {
                        let track_h = self.terminal_height.saturating_sub(28).max(1) as f32;
                        let step = 1.0 / track_h;
                        self.master.fader =
                            (self.master.fader + delta.signum() * step).clamp(0.0, 1.0);
                    }
                    GlobalControl::MicFader => {
                        let track_h = self.terminal_height.saturating_sub(28).max(1) as f32;
                        let step = 1.0 / track_h;
                        self.master.mic_fader =
                            (self.master.mic_fader + delta.signum() * step).clamp(0.0, 1.0);
                    }
                    // Master EQ bands: +/- 6 dB per keypress, range -12 to +12
                    GlobalControl::MasterEq32 => {
                        self.master.master_eq[0] =
                            (self.master.master_eq[0] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq64 => {
                        self.master.master_eq[1] =
                            (self.master.master_eq[1] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq125 => {
                        self.master.master_eq[2] =
                            (self.master.master_eq[2] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq250 => {
                        self.master.master_eq[3] =
                            (self.master.master_eq[3] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq500 => {
                        self.master.master_eq[4] =
                            (self.master.master_eq[4] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq1k => {
                        self.master.master_eq[5] =
                            (self.master.master_eq[5] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq2k => {
                        self.master.master_eq[6] =
                            (self.master.master_eq[6] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq4k => {
                        self.master.master_eq[7] =
                            (self.master.master_eq[7] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq8k => {
                        self.master.master_eq[8] =
                            (self.master.master_eq[8] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    GlobalControl::MasterEq16k => {
                        self.master.master_eq[9] =
                            (self.master.master_eq[9] + delta * 6.0).clamp(-12.0, 12.0);
                    }
                    // Output selection controls are handled by UI, not continuous
                    GlobalControl::MasterOutputSelect | GlobalControl::MicInputSelect => {}
                    // Buttons/toggles don't need continuous adjustment
                    GlobalControl::MasterMute | GlobalControl::MicMute => {}
                    GlobalControl::MasterPlayPause => {}
                }
            }
        }
    }

    /// Toggle the currently selected control (for buttons)
    pub fn toggle_selected(&mut self) {
        // Use cue_channel for channel 2 (Deck C)
        let channel = if self.selected_channel == 2 {
            Some(&mut self.cue_channel)
        } else {
            self.channels.get_mut(self.selected_channel)
        };

        if let Some(channel) = channel {
            match self.selected_control {
                ChannelControl::PrevTrack | ChannelControl::NextTrack => {}
                ChannelControl::PlayPause => channel.playing = !channel.playing,
                ChannelControl::Mute => channel.muted = !channel.muted,
                ChannelControl::Solo => {
                    channel.solo = !channel.solo;
                    self.update_solo_state();
                }
                ChannelControl::EqLowKill => channel.eq_low_kill = !channel.eq_low_kill,
                ChannelControl::EqMidKill => channel.eq_mid_kill = !channel.eq_mid_kill,
                ChannelControl::EqHighKill => channel.eq_high_kill = !channel.eq_high_kill,
                _ => {}
            }
        }
    }

    fn update_solo_state(&mut self) {
        self.solo_active = self.channels.iter().any(|c| c.solo);
    }

    /// Check if any deck (A, B, CUE) or master is muted
    pub fn mute_active(&self) -> bool {
        // Check any deck channel is muted
        self.channels.iter().any(|c| c.muted) || self.cue_channel.muted || self.master.muted
    }

    /// Save a channel's fader level before it enters solo mode.
    /// If the channel already has a saved level, this is a no-op (preserves the original).
    pub fn save_fader_for_solo(&mut self, channel_idx: usize) {
        if channel_idx == 2 {
            // CUE channel
            if self.pre_solo_cue_fader.is_none() {
                self.pre_solo_cue_fader = Some(self.cue_channel.fader);
            }
        } else if let Some(ch) = self.channels.get(channel_idx) {
            self.pre_solo_faders.entry(channel_idx).or_insert(ch.fader);
        }
    }

    /// Restore a channel's fader level saved before solo was activated.
    /// Returns true if restoration happened.
    pub fn restore_fader_from_solo(&mut self, channel_idx: usize) -> bool {
        if channel_idx == 2 {
            if let Some(fader) = self.pre_solo_cue_fader.take() {
                self.cue_channel.fader = fader;
                return true;
            }
        } else if let Some(fader) = self.pre_solo_faders.remove(&channel_idx)
            && let Some(ch) = self.channels.get_mut(channel_idx)
        {
            ch.fader = fader;
            return true;
        }
        false
    }

    /// Update metering levels — reactive to fader, EQ, filters, pan, crossfader, and master gain.
    /// Master meters are computed by summing per-channel levels with gains applied.
    pub fn update_meters(
        &mut self,
        real_channels: &[(usize, f32, f32, f32, f32)], // (channel_idx, peak_l, peak_r, rms_l, rms_r)
    ) {
        // Precompute crossfader gains (equal-power sqrt, matching audio engine)
        let xf = self.dj.crossfader;
        let cf = ((xf + 1.0) * 0.5).clamp(0.0, 1.0);
        let xf_gain_a = (1.0 - cf).sqrt();
        let xf_gain_b = cf.sqrt();

        for (i, channel) in self.channels.iter_mut().enumerate() {
            // Decay peaks (slow hold) and RMS (faster fall)
            channel.peak_left *= 0.92;
            channel.peak_right *= 0.92;
            channel.rms_left *= 0.85;
            channel.rms_right *= 0.85;
            channel.peak_level *= 0.92;
            channel.rms_level *= 0.85;

            // Check if we have real audio levels for this channel
            if let Some(&(_, peak_l, peak_r, rms_l, rms_r)) =
                real_channels.iter().find(|(idx, _, _, _, _)| *idx == i)
            {
                let effective_muted =
                    channel.muted || (self.solo_active && !channel.solo) || !channel.playing;

                if effective_muted {
                    continue;
                }

                channel.peak_left = channel.peak_left.max(peak_l);
                channel.peak_right = channel.peak_right.max(peak_r);
                channel.peak_level = channel.peak_left.max(channel.peak_right);
                let rms_attack = 0.35;
                let rms_release = 0.06;
                channel.rms_left += if rms_l > channel.rms_left {
                    (rms_l - channel.rms_left) * rms_attack
                } else {
                    (rms_l - channel.rms_left) * rms_release
                };
                channel.rms_right += if rms_r > channel.rms_right {
                    (rms_r - channel.rms_right) * rms_attack
                } else {
                    (rms_r - channel.rms_right) * rms_release
                };
                channel.rms_level = (channel.rms_left + channel.rms_right) / 2.0;

                // Per-band spectrum peaks shaped by EQ gains
                let peak = channel.peak_level;
                let eq_gains = [
                    10f32.powf(channel.eq_low / 20.0),
                    10f32.powf(channel.eq_mid / 20.0),
                    10f32.powf(channel.eq_high / 20.0),
                ];
                let weights = [6.0, 4.0, 4.2]; // Low, Mid, High
                for b in 0..3 {
                    channel.spectrum_decay[b] *= 0.88;
                    let noise = rand_simple() * 0.5;
                    let band_peak = (peak * weights[b] * eq_gains[b] + noise * peak * 0.4).min(1.0);
                    let attack = 0.5;
                    let release = 0.12;
                    let target = band_peak;
                    let current = channel.spectrum_decay[b];
                    let new_val = if target > current {
                        current + (target - current) * attack
                    } else {
                        current + (target - current) * release
                    };
                    channel.spectrum_decay[b] = new_val;
                    channel.spectrum_peaks[b] = channel.spectrum_peaks[b].max(new_val) * 0.92;
                }

                continue;
            }

            // Effective mute: muted OR (solo active and not soloed) OR not playing
            let effective_muted =
                channel.muted || (self.solo_active && !channel.solo) || !channel.playing;

            if effective_muted || channel.fader <= 0.0 {
                continue;
            }

            // --- Compute effective gain for this channel ---
            let fader = channel.fader;

            // EQ gain: apply each band separately, then combine
            // Kill a band = -60dB (near silence for that frequency range)
            let low = if channel.eq_low_kill {
                -60.0
            } else {
                channel.eq_low
            };
            let mid = if channel.eq_mid_kill {
                -60.0
            } else {
                channel.eq_mid
            };
            let high = if channel.eq_high_kill {
                -60.0
            } else {
                channel.eq_high
            };

            // Convert each band to linear and combine
            let low_lin = 10f32.powf(low / 20.0);
            let mid_lin = 10f32.powf(mid / 20.0);
            let high_lin = 10f32.powf(high / 20.0);

            // Weighted combination (each band covers ~1/3 of spectrum)
            let eq_mult = (low_lin + mid_lin + high_lin) / 3.0;

            // Filter attenuation (simplified — full cutoff = near silence)
            let filter_mult = if channel.filter_cutoff > 0.01 {
                1.0 - channel.filter_cutoff * 0.9 // Up to 90% attenuation at full cutoff
            } else {
                1.0
            };

            // Crossfader gain
            let xf_gain = if i == self.dj.deck_a_channel {
                xf_gain_a
            } else if i == self.dj.deck_b_channel {
                xf_gain_b
            } else {
                1.0
            };

            // Total effective gain (pre-master)
            let meter_boost = if channel.uses_supercollider { 4.0 } else { 1.0 };
            let gain = (fader * eq_mult * filter_mult * xf_gain * meter_boost).clamp(0.0, 1.0);

            if gain <= 0.001 {
                continue;
            }

            // --- Pan weighting ---
            // pan = -1.0 → all L, pan = 1.0 → all R, pan = 0.0 → equal
            let pan_l = ((1.0 - channel.pan) * 0.5).max(0.0);
            let pan_r = ((1.0 + channel.pan) * 0.5).max(0.0);

            // Simulated audio activity — responds to fader, EQ, filter, crossfader
            let t = rand_simple();
            let beat = (t * std::f32::consts::TAU).sin().abs(); // smooth rhythmic pulse
            let noise = rand_simple() * 0.2;
            let base = gain * (0.6 + beat * 0.25);
            let activity_l = (base + noise) * pan_l;
            let activity_r = (base + noise) * pan_r;

            // Track RMS toward the current activity level (smooth envelope following)
            let rms_attack = 0.35;
            let rms_release = 0.06;
            channel.rms_left += if activity_l > channel.rms_left {
                (activity_l - channel.rms_left) * rms_attack
            } else {
                (activity_l - channel.rms_left) * rms_release
            };
            channel.rms_right += if activity_r > channel.rms_right {
                (activity_r - channel.rms_right) * rms_attack
            } else {
                (activity_r - channel.rms_right) * rms_release
            };
            channel.rms_level = (channel.rms_left + channel.rms_right) / 2.0;

            // Update peaks — more frequent transients above RMS
            if rand_simple() > 0.7 {
                let peak_l = (activity_l * 1.4).min(1.0);
                let peak_r = (activity_r * 1.4).min(1.0);
                channel.peak_left = channel.peak_left.max(peak_l);
                channel.peak_right = channel.peak_right.max(peak_r);
                channel.peak_level = channel.peak_left.max(channel.peak_right);
            }
            update_channel_spectrum(channel);
        }

        // --- Deck C (cue channel) meters ---
        {
            let cue = &mut self.cue_channel;
            cue.peak_left *= 0.92;
            cue.peak_right *= 0.92;
            cue.rms_left *= 0.85;
            cue.rms_right *= 0.85;
            cue.peak_level *= 0.92;
            cue.rms_level *= 0.85;

            if let Some(&(_, peak_l, peak_r, rms_l, rms_r)) = real_channels
                .iter()
                .find(|(idx, _, _, _, _)| *idx == self.dj.deck_c_channel)
            {
                let effective_muted = cue.muted || (self.solo_active && !cue.solo) || !cue.playing;

                if !effective_muted {
                    cue.peak_left = cue.peak_left.max(peak_l);
                    cue.peak_right = cue.peak_right.max(peak_r);
                    cue.peak_level = cue.peak_left.max(cue.peak_right);
                    let rms_attack = 0.35;
                    let rms_release = 0.06;
                    cue.rms_left += if rms_l > cue.rms_left {
                        (rms_l - cue.rms_left) * rms_attack
                    } else {
                        (rms_l - cue.rms_left) * rms_release
                    };
                    cue.rms_right += if rms_r > cue.rms_right {
                        (rms_r - cue.rms_right) * rms_attack
                    } else {
                        (rms_r - cue.rms_right) * rms_release
                    };
                    cue.rms_level = (cue.rms_left + cue.rms_right) / 2.0;
                    update_channel_spectrum(cue);
                }
            } else {
                let effective_muted = cue.muted || (self.solo_active && !cue.solo) || !cue.playing;

                if !effective_muted && cue.fader > 0.0 {
                    let low = if cue.eq_low_kill { -60.0 } else { cue.eq_low };
                    let mid = if cue.eq_mid_kill { -60.0 } else { cue.eq_mid };
                    let high = if cue.eq_high_kill { -60.0 } else { cue.eq_high };
                    let eq_mult =
                        (10f32.powf(low / 20.0) + 10f32.powf(mid / 20.0) + 10f32.powf(high / 20.0))
                            / 3.0;
                    let filter_mult = if cue.filter_cutoff > 0.01 {
                        1.0 - cue.filter_cutoff * 0.9
                    } else {
                        1.0
                    };
                    let meter_boost = if cue.uses_supercollider { 4.0 } else { 1.0 };
                    let gain = (cue.fader * eq_mult * filter_mult * meter_boost).clamp(0.0, 1.0);

                    if gain > 0.001 {
                        let pan_l = ((1.0 - cue.pan) * 0.5).max(0.0);
                        let pan_r = ((1.0 + cue.pan) * 0.5).max(0.0);
                        let t = rand_simple();
                        let beat = (t * std::f32::consts::TAU).sin().abs();
                        let noise = rand_simple() * 0.2;
                        let base = gain * (0.6 + beat * 0.25);
                        let activity_l = (base + noise) * pan_l;
                        let activity_r = (base + noise) * pan_r;

                        let rms_attack = 0.35;
                        let rms_release = 0.06;
                        cue.rms_left += if activity_l > cue.rms_left {
                            (activity_l - cue.rms_left) * rms_attack
                        } else {
                            (activity_l - cue.rms_left) * rms_release
                        };
                        cue.rms_right += if activity_r > cue.rms_right {
                            (activity_r - cue.rms_right) * rms_attack
                        } else {
                            (activity_r - cue.rms_right) * rms_release
                        };
                        cue.rms_level = (cue.rms_left + cue.rms_right) / 2.0;

                        if rand_simple() > 0.7 {
                            let peak_l = (activity_l * 1.4).min(1.0);
                            let peak_r = (activity_r * 1.4).min(1.0);
                            cue.peak_left = cue.peak_left.max(peak_l);
                            cue.peak_right = cue.peak_right.max(peak_r);
                            cue.peak_level = cue.peak_left.max(cue.peak_right);
                        }
                        update_channel_spectrum(cue);
                    }
                }
            }
        }

        // --- Master meters ---
        // Compute master from channel sum
        self.master.peak_left *= 0.92;
        self.master.peak_right *= 0.92;
        self.master.rms_left *= 0.85;
        self.master.rms_right *= 0.85;

        if !self.master.muted {
            let mut sum_l: f32 = 0.0;
            let mut sum_r: f32 = 0.0;
            let mut peak_sum_l: f32 = 0.0;
            let mut peak_sum_r: f32 = 0.0;

            for (i, c) in self.channels.iter().enumerate() {
                if c.muted || (self.solo_active && !c.solo) {
                    continue;
                }
                let xf_g = if i == self.dj.deck_a_channel {
                    xf_gain_a
                } else if i == self.dj.deck_b_channel {
                    xf_gain_b
                } else {
                    1.0
                };
                let pan_l = (1.0 - c.pan) * 0.5 + 0.25;
                let pan_r = (1.0 + c.pan) * 0.5 + 0.25;
                sum_l += c.rms_left * pan_l * xf_g;
                sum_r += c.rms_right * pan_r * xf_g;
                peak_sum_l += c.peak_left * pan_l * xf_g;
                peak_sum_r += c.peak_right * pan_r * xf_g;
            }

            let master_l = (sum_l * self.master.fader).min(1.0);
            let master_r = (sum_r * self.master.fader).min(1.0);
            let master_peak_l = (peak_sum_l * self.master.fader).min(1.0);
            let master_peak_r = (peak_sum_r * self.master.fader).min(1.0);

            self.master.rms_left = self.master.rms_left.max(master_l);
            self.master.rms_right = self.master.rms_right.max(master_r);
            self.master.peak_left = self.master.peak_left.max(master_peak_l);
            self.master.peak_right = self.master.peak_right.max(master_peak_r);
        }

        // --- Spectrum analyzer ---
        // Compute per-band peaks from master RMS with frequency-dependent weighting
        let master_energy = (self.master.rms_left + self.master.rms_right) * 0.5;
        let master_peak = self.master.peak_left.max(self.master.peak_right);

        // Simulated frequency distribution: boost all bands so the analyzer is lively.
        // Real FFT would give natural distribution; here we shape noise to look musical.
        let spectrum_weights: [f32; 10] = [
            6.0, // 32Hz
            7.0, // 64Hz
            5.5, // 125Hz
            4.5, // 250Hz
            3.8, // 500Hz
            4.0, // 1kHz
            4.8, // 2kHz
            5.0, // 4kHz
            4.2, // 8kHz
            3.0, // 16kHz
        ];

        for (i, weight) in spectrum_weights.iter().enumerate() {
            // Decay existing peaks
            self.master.spectrum_decay[i] *= 0.88;

            // Apply EQ gain: boost increases peak, cut decreases it
            let eq_db = self.master.master_eq[i];
            let eq_mult = 10f32.powf(eq_db / 20.0); // dB to linear

            // Compute new peak from master energy + shaped noise + EQ
            let noise = rand_simple() * 0.5;
            let band_energy = (master_energy * weight * eq_mult).min(1.0);
            let band_peak = (band_energy + noise * master_peak * 0.6 * eq_mult).min(1.0);

            // Smooth envelope following
            let attack = 0.5;
            let release = 0.12;
            let target = band_peak;
            let current = self.master.spectrum_decay[i];
            let new_val = if target > current {
                current + (target - current) * attack
            } else {
                current + (target - current) * release
            };

            self.master.spectrum_decay[i] = new_val;
            // Peak holds briefly then decays
            self.master.spectrum_peaks[i] = self.master.spectrum_peaks[i].max(new_val) * 0.92;
        }
    }
}

impl Default for MixerState {
    fn default() -> Self {
        Self::new(2) // Default to 2 channels
    }
}

/// Simple pseudo-random for meter simulation
fn rand_simple() -> f32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    ((nanos % 1000) as f32) / 1000.0
}

fn update_channel_spectrum(channel: &mut MixerChannel) {
    let peak = channel.peak_level.max(channel.rms_level);
    let eq_gains = [
        10f32.powf(channel.eq_low / 20.0),
        10f32.powf(channel.eq_mid / 20.0),
        10f32.powf(channel.eq_high / 20.0),
    ];
    let weights = [6.0, 4.0, 4.2];
    for b in 0..3 {
        channel.spectrum_decay[b] *= 0.88;
        let noise = rand_simple() * 0.5;
        let band_peak = (peak * weights[b] * eq_gains[b] + noise * peak * 0.4).min(1.0);
        let current = channel.spectrum_decay[b];
        let new_val = if band_peak > current {
            current + (band_peak - current) * 0.5
        } else {
            current + (band_peak - current) * 0.12
        };
        channel.spectrum_decay[b] = new_val;
        channel.spectrum_peaks[b] = channel.spectrum_peaks[b].max(new_val) * 0.92;
    }
}
