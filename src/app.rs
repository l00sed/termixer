//! Application state and event handling

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::collections::{HashSet, VecDeque};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::audio::bpm::{parse_camelot, parse_key_name, pitch_class_to_camelot};
use crate::audio::engine::AudioEngine;
use crate::audio::{
    AudioOutput, AudioSource, AudioSourceManager, BpmAnalyzer, MicInputDevice, MpvClient,
    SampleEngine, SuperColliderClient,
};
use crate::state::{
    ChannelControl, GlobalControl, MixerState, PadControl, SamplePadGrid, SelectionFocus,
    SendTarget, SequenceState, SessionState,
};

type PendingBpm = Arc<Mutex<Vec<(usize, f32, Option<String>)>>>;

const SC_GAIN_BOOST: f32 = 8.0;
const SC_DEFAULT_BPM: f32 = 135.0;
const TIDAL_BPM_PATH: &str = "/tmp/termixer-bpm";
const TM_SOCKET: &str = "/tmp/termixer.sock";
const TM_FIFO: &str = "/tmp/termixer.pcm";
const ROUTE_TIMELINE_TIMEOUT_MS: u64 = 3_000;
const ROUTE_TIMELINE_STALE_MS: u64 = 5_000;
const TM_META: &str = "/tmp/termixer-meta.json";
const TM_FIFO_GLOB: &str = "/tmp/termixer-*.pcm";
const SCRUB_FINE_STEP_MIN_SECS: f32 = 0.003;
const SCRUB_FINE_STEP_MAX_SECS: f32 = 0.02;
const SCRUB_COARSE_STEP_MIN_SECS: f32 = 0.015;
const SCRUB_COARSE_STEP_MAX_SECS: f32 = 0.08;
const SCRUB_INPUT_HOLD_MS: u64 = 40;
const SCRUB_ACCEL_RAMP_MS: u64 = 650;
const SCRUB_TAP_RETURN_DELAY_MS: u64 = 85;
const SCRUB_FINE_HOLD_ARM_MS: u64 = 320;
const SCRUB_SEEK_SEND_INTERVAL_MS: u64 = 25;
const SCRUB_STEP_BASE_DT_SECS: f32 = 0.05;
const ROUTE_SEEK_TARGET_EPSILON_SECS: f32 = 0.25;
const ROUTE_SEEK_PENDING_MAX_MS: u64 = 1500;

/// Which deck is being configured
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deck {
    A,
    B,
    C,
}

impl Deck {
    pub fn index(self) -> usize {
        match self {
            Deck::A => 0,
            Deck::B => 1,
            Deck::C => 2,
        }
    }
}

/// Application mode - 3-level navigation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// Mode 1: Navigate between panes (Deck A, DJ Center, Deck B, Master)
    PaneSelect,
    /// Mode 2: Navigate controls within selected pane
    ControlSelect,
    /// Mode 3: Edit selected control value
    Edit,
    /// Help overlay
    Help,
    /// Pad keys active, trigger samples
    SamplePads,
    /// Configure pads (assign samples)
    SamplePadConfig,
    /// Source picker popup for deck A or B
    SourcePicker(Deck),
    /// Sample picker popup for a pad
    SamplePicker(usize), // pad index
    /// Confirmation dialog for destructive actions
    ConfirmAction(ConfirmAction),
    /// Config file update check dialog
    ConfigCheck,
    /// Session file picker for save/load
    SessionPicker,
}

/// Actions that require confirmation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Quit the application
    Quit,
    /// Clear a deck (disconnect source)
    ClearDeck(Deck),
    /// Reset deck controls to defaults (source stays connected)
    ResetDeck(Deck),
    /// Reset all controls to defaults
    ResetAll,
}

/// Source picker tab
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerInputMode {
    Normal,
    Insert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePickerTab {
    Queue,
    SuperCollider,
    DeckActions,
    /// Internal: used by sample picker and add-to-queue sub-view
    AudioFiles,
}

/// A source item queued for playback on a deck.
#[derive(Debug, Clone)]
pub struct QueueItem {
    pub name: String,
    pub path: PathBuf,
    pub is_socket: bool,
    pub is_pcm_fifo: bool,
    pub is_udp: bool,
}

/// Session file picker state
#[derive(Debug, Clone)]
pub struct SessionPickerState {
    pub input_mode: PickerInputMode,
    pub query: String,
    pub items: Vec<SessionPickerItem>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub current_dir: PathBuf,
    pub root_dir: PathBuf,
    pub visible_height: usize,
    pub is_save: bool,
    pub save_name: String,
    /// Cursor position within save_name when editing.
    pub save_name_cursor: usize,
    /// When selected == 0 and this is true, keystrokes edit save_name.
    /// Enter toggles this; j/k only work when false.
    pub editing_save_name: bool,
    /// When Some, shows an overwrite confirmation dialog for this path.
    pub overwrite_confirm: Option<PathBuf>,
    /// Status message displayed below the save name row (e.g., validation errors).
    pub status_message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionPickerItem {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

impl SessionPickerState {
    pub fn new() -> Self {
        Self {
            input_mode: PickerInputMode::Normal,
            query: String::new(),
            items: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            current_dir: PathBuf::new(),
            root_dir: PathBuf::new(),
            visible_height: 12,
            is_save: true,
            save_name: "session.json".to_string(),
            save_name_cursor: 0,
            editing_save_name: false,
            overwrite_confirm: None,
            status_message: None,
        }
    }

    pub fn set_root(&mut self, root: PathBuf) {
        self.root_dir = root.clone();
        self.current_dir = root;
    }

    pub fn filter(&mut self) {
        let query_lower = self.query.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                if query_lower.is_empty() {
                    true
                } else {
                    item.name.to_lowercase().contains(&query_lower)
                }
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn scan_dir(&mut self) {
        self.items.clear();

        let can_go_up = self.current_dir != self.root_dir
            && self.current_dir.parent().is_some();
        if can_go_up {
            self.items.push(SessionPickerItem {
                name: "..".to_string(),
                path: self
                    .current_dir
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.root_dir.clone()),
                is_dir: true,
            });
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.current_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                if name.starts_with('.') {
                    continue;
                }

                if path.is_dir() {
                    dirs.push(SessionPickerItem {
                        name: format!("{}/", name),
                        path,
                        is_dir: true,
                    });
                } else if path.is_file() {
                    let ext_lower = path
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    if ext_lower == "json" {
                        files.push(SessionPickerItem {
                            name,
                            path,
                            is_dir: false,
                        });
                    }
                }
            }
        }

        dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        self.items.extend(dirs);
        self.items.extend(files);
        self.filter();
    }

    pub fn move_up(&mut self) {
        if self.is_save && self.selected == 0 {
            // Wrap from save name row to last item in file list
            if !self.filtered.is_empty() {
                self.selected = self.filtered.len();
                let file_idx = self.selected.saturating_sub(1);
                if file_idx >= self.scroll_offset + self.visible_height {
                    self.scroll_offset = file_idx.saturating_sub(self.visible_height - 1);
                }
            }
            return;
        }
        if self.selected == 0 {
            if !self.is_save && !self.filtered.is_empty() {
                // Round-robin: wrap from top to last item
                self.selected = self.filtered.len().saturating_sub(1);
                let file_idx = self.selected;
                if file_idx >= self.scroll_offset + self.visible_height {
                    self.scroll_offset = file_idx.saturating_sub(self.visible_height - 1);
                }
            }
            return;
        }
        self.selected -= 1;
        if self.selected > 0 && self.selected - 1 < self.scroll_offset {
            self.scroll_offset = self.selected - 1;
        } else if self.selected == 0 && self.is_save {
            self.scroll_offset = 0;
        }
    }

    pub fn move_down(&mut self) {
        let max = if self.is_save {
            self.filtered.len() // 0=save name, 1..=len=file items
        } else {
            self.filtered.len().saturating_sub(1)
        };
        if self.selected >= max {
            if self.is_save {
                // Wrap from bottom of file list back to save name row
                self.selected = 0;
                self.scroll_offset = 0;
            } else if !self.filtered.is_empty() {
                // Round-robin: wrap from bottom to top
                self.selected = 0;
                self.scroll_offset = 0;
            }
            return;
        }
        self.selected += 1;
        self.editing_save_name = false;
        // scroll_offset indexes into the file list (items 1.. in save mode)
        let file_idx = if self.is_save {
            self.selected.saturating_sub(1)
        } else {
            self.selected
        };
        if file_idx >= self.scroll_offset + self.visible_height {
            self.scroll_offset = file_idx.saturating_sub(self.visible_height - 1);
        }
    }

    pub fn enter_dir(&mut self) -> bool {
        let file_idx = if self.is_save {
            self.selected.saturating_sub(1)
        } else {
            self.selected
        };
        if let Some(&idx) = self.filtered.get(file_idx)
            && let Some(item) = self.items.get(idx)
            && item.is_dir
        {
            self.current_dir = item.path.clone();
            self.query.clear();
            self.input_mode = PickerInputMode::Normal;
            self.scan_dir();
            return true;
        }
        false
    }

    pub fn go_up(&mut self) {
        if self.current_dir != self.root_dir
            && let Some(parent) = self.current_dir.parent()
        {
            self.current_dir = parent.to_path_buf();
            self.scan_dir();
        }
    }
}

/// Source picker state
#[derive(Debug, Clone)]
pub struct SourcePickerState {
    pub tab: SourcePickerTab,
    pub tab_focused: bool, // When true, h/l navigates tabs instead of entering dirs
    pub input_mode: PickerInputMode,
    pub query: String,
    pub items: Vec<SourcePickerItem>,
    pub filtered: Vec<usize>, // Indices into items
    pub selected: usize,      // Index into filtered
    pub scroll_offset: usize,
    pub current_dir: PathBuf,  // Current directory being browsed
    pub root_dir: PathBuf,     // Root samples directory (can't go above this)
    pub visible_height: usize, // Number of visible items (set by UI)
    /// Character offset for horizontal tab scrolling
    pub tab_scroll_offset: usize,
    /// Queue of sources for this deck
    pub queue: Vec<QueueItem>,
    /// Index of selected item in queue (-1 means none selected)
    pub queue_selected: Option<usize>,
    /// Scroll offset for the queue list
    pub queue_scroll_offset: usize,
    /// Sub-view for browsing sources when adding to queue
    pub adding_to_queue: bool,
    /// Sub-view tab (MpvSockets or AudioFiles when adding)
    pub add_tab: AddSourceTab,
    /// Sub-view items (from scanning)
    pub add_items: Vec<SourcePickerItem>,
    /// Sub-view filtered indices
    pub add_filtered: Vec<usize>,
    /// Sub-view selected index
    pub add_selected: usize,
    /// Paths highlighted for batch addition to the queue.
    pub add_marked: HashSet<PathBuf>,
    /// Starting row for a Shift+navigation range selection.
    pub add_selection_anchor: Option<usize>,
    /// Sub-view scroll offset
    pub add_scroll_offset: usize,
    /// Sub-view query for filtering
    pub add_query: String,
    /// Sub-view input mode
    pub add_input_mode: PickerInputMode,
    /// Sub-view current directory (for audio files browsing)
    pub add_current_dir: PathBuf,
}

/// Tabs visible when adding a source to the queue
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddSourceTab {
    MpvSockets,
    AudioFiles,
}

#[derive(Debug, Clone)]
pub struct SourcePickerItem {
    pub name: String,
    pub path: PathBuf,
    pub is_socket: bool,
    pub is_pcm_fifo: bool,
    pub is_udp: bool,
    pub is_dir: bool,
}

impl SourcePickerState {
    pub fn new() -> Self {
        Self {
            tab: SourcePickerTab::Queue,
            tab_focused: true,
            input_mode: PickerInputMode::Normal,
            query: String::new(),
            items: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            current_dir: PathBuf::new(),
            root_dir: PathBuf::new(),
            visible_height: 12,
            tab_scroll_offset: 0,
            queue: Vec::new(),
            queue_selected: None,
            queue_scroll_offset: 0,
            adding_to_queue: false,
            add_tab: AddSourceTab::MpvSockets,
            add_items: Vec::new(),
            add_filtered: Vec::new(),
            add_selected: 0,
            add_marked: HashSet::new(),
            add_selection_anchor: None,
            add_scroll_offset: 0,
            add_query: String::new(),
            add_input_mode: PickerInputMode::Normal,
            add_current_dir: PathBuf::new(),
        }
    }

    pub fn set_root(&mut self, root: PathBuf) {
        self.root_dir = root.clone();
        self.current_dir = root;
    }

    pub fn filter(&mut self) {
        let query_lower = self.query.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                if query_lower.is_empty() {
                    true
                } else {
                    item.name.to_lowercase().contains(&query_lower)
                }
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = self.filtered.len(); // sentinel: no item selected
        self.scroll_offset = 0;
    }

    pub fn selected_item(&self) -> Option<&SourcePickerItem> {
        self.filtered
            .get(self.selected)
            .and_then(|&i| self.items.get(i))
    }

    pub fn move_up(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        if self.selected == 0 || self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
            let vh = self.visible_height;
            if self.selected >= self.scroll_offset + vh {
                self.scroll_offset = self.selected.saturating_sub(vh - 1);
            }
        } else {
            self.selected -= 1;
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        if self.selected + 1 >= self.filtered.len() {
            self.selected = 0;
            self.scroll_offset = 0;
        } else {
            self.selected += 1;
            if self.selected >= self.scroll_offset + self.visible_height {
                self.scroll_offset = self.selected.saturating_sub(self.visible_height - 1);
            }
        }
    }

    // --- Add-to-queue sub-view methods ---

    pub fn add_filter(&mut self) {
        let query_lower = self.add_query.to_lowercase();
        self.add_filtered = self
            .add_items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                if query_lower.is_empty() {
                    true
                } else {
                    item.name.to_lowercase().contains(&query_lower)
                }
            })
            .map(|(i, _)| i)
            .collect();
        self.add_selected = self.add_filtered.len(); // sentinel: no item selected
        self.add_scroll_offset = 0;
    }

    pub fn add_selected_item(&self) -> Option<&SourcePickerItem> {
        self.add_filtered
            .get(self.add_selected)
            .and_then(|&i| self.add_items.get(i))
    }

    pub fn add_move_up(&mut self) {
        if self.add_filtered.is_empty() {
            return;
        }
        if self.add_selected == 0 || self.add_selected >= self.add_filtered.len() {
            self.add_selected = self.add_filtered.len() - 1;
            let vh = self.visible_height;
            if self.add_selected >= self.add_scroll_offset + vh {
                self.add_scroll_offset = self.add_selected.saturating_sub(vh - 1);
            }
        } else {
            self.add_selected -= 1;
            if self.add_selected < self.add_scroll_offset {
                self.add_scroll_offset = self.add_selected;
            }
        }
    }

    pub fn add_move_down(&mut self) {
        if self.add_filtered.is_empty() {
            return;
        }
        if self.add_selected + 1 >= self.add_filtered.len() {
            self.add_selected = 0;
            self.add_scroll_offset = 0;
        } else {
            self.add_selected += 1;
            if self.add_selected >= self.add_scroll_offset + self.visible_height {
                self.add_scroll_offset = self.add_selected.saturating_sub(self.visible_height - 1);
            }
        }
    }

    pub fn add_extend_selection(&mut self, down: bool) {
        if self.add_filtered.is_empty() {
            return;
        }

        if self.add_selected >= self.add_filtered.len() {
            self.add_selected = if down { 0 } else { self.add_filtered.len() - 1 };
            self.add_selection_anchor = Some(self.add_selected);
            if let Some(item) = self
                .add_filtered
                .get(self.add_selected)
                .and_then(|&item_idx| self.add_items.get(item_idx))
                && !item.is_dir
            {
                self.add_marked.insert(item.path.clone());
            }
            return;
        }
        let anchor = *self.add_selection_anchor.get_or_insert(self.add_selected);

        if down {
            if self.add_selected + 1 < self.add_filtered.len() {
                self.add_selected += 1;
            }
        } else if self.add_selected > 0 {
            self.add_selected -= 1;
        }

        self.add_marked.clear();
        let start = anchor.min(self.add_selected);
        let end = anchor.max(self.add_selected);
        for filtered_idx in start..=end {
            if let Some(item) = self
                .add_filtered
                .get(filtered_idx)
                .and_then(|&item_idx| self.add_items.get(item_idx))
                && !item.is_dir
            {
                self.add_marked.insert(item.path.clone());
            }
        }
        if self.add_selected < self.add_scroll_offset {
            self.add_scroll_offset = self.add_selected;
        } else if self.add_selected >= self.add_scroll_offset + self.visible_height {
            self.add_scroll_offset = self
                .add_selected
                .saturating_sub(self.visible_height.saturating_sub(1));
        }
    }

    pub fn add_clamp_scroll(&mut self) {
        if self.visible_height == 0 {
            return;
        }
        let max_scroll = self.add_filtered.len().saturating_sub(self.visible_height);
        self.add_scroll_offset = self.add_scroll_offset.min(max_scroll);
        if self.add_selected < self.add_scroll_offset {
            self.add_scroll_offset = self.add_selected;
        } else if self.add_selected >= self.add_scroll_offset + self.visible_height {
            self.add_selected = self.add_selected.saturating_sub(self.visible_height - 1);
        }
    }

    pub fn add_next_tab(&mut self) {
        self.add_marked.clear();
        self.add_selection_anchor = None;
        self.add_tab = match self.add_tab {
            AddSourceTab::MpvSockets => AddSourceTab::AudioFiles,
            AddSourceTab::AudioFiles => AddSourceTab::MpvSockets,
        };
        self.tab_focused = true;
    }

    pub fn add_prev_tab(&mut self) {
        self.add_marked.clear();
        self.add_selection_anchor = None;
        self.add_tab = match self.add_tab {
            AddSourceTab::MpvSockets => AddSourceTab::AudioFiles,
            AddSourceTab::AudioFiles => AddSourceTab::MpvSockets,
        };
        self.tab_focused = true;
    }

    /// Ensure scroll offset keeps selected item visible
    pub fn clamp_scroll(&mut self) {
        if self.visible_height == 0 {
            return;
        }
        let max_scroll = self.filtered.len().saturating_sub(self.visible_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        // Ensure selected is visible
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.visible_height {
            self.scroll_offset = self.selected.saturating_sub(self.visible_height - 1);
        }
    }

    /// Ensure queue scroll offset keeps the selected queue item visible.
    /// The queue has an extra "+" row at the bottom, so total count is queue.len() + 1.
    pub fn clamp_queue_scroll(&mut self) {
        if self.visible_height == 0 {
            return;
        }
        let total = self.queue.len() + 1;
        let max_scroll = total.saturating_sub(self.visible_height);
        self.queue_scroll_offset = self.queue_scroll_offset.min(max_scroll);

        match self.queue_selected {
            Some(sel) => {
                if sel < self.queue_scroll_offset {
                    self.queue_scroll_offset = sel;
                } else if sel >= self.queue_scroll_offset + self.visible_height {
                    self.queue_scroll_offset = sel.saturating_sub(self.visible_height - 1);
                }
            }
            None => {
                // The "+" item is selected (queue.len() position)
                let plus_idx = self.queue.len();
                if plus_idx < self.queue_scroll_offset {
                    self.queue_scroll_offset = plus_idx;
                } else if plus_idx >= self.queue_scroll_offset + self.visible_height {
                    self.queue_scroll_offset = plus_idx.saturating_sub(self.visible_height - 1);
                }
            }
        }
    }

    /// Cycle to the next tab (forward round-robin)
    pub fn next_tab(&mut self) {
        self.tab = match self.tab {
            SourcePickerTab::Queue => SourcePickerTab::SuperCollider,
            SourcePickerTab::SuperCollider => SourcePickerTab::DeckActions,
            SourcePickerTab::DeckActions => SourcePickerTab::Queue,
            SourcePickerTab::AudioFiles => SourcePickerTab::Queue,
        };
        self.tab_focused = true;
        self.queue_selected = None;
        self.scroll_tab_into_view(60);
    }

    /// Cycle to the previous tab (backward round-robin)
    pub fn prev_tab(&mut self) {
        self.tab = match self.tab {
            SourcePickerTab::Queue => SourcePickerTab::DeckActions,
            SourcePickerTab::DeckActions => SourcePickerTab::SuperCollider,
            SourcePickerTab::SuperCollider => SourcePickerTab::Queue,
            SourcePickerTab::AudioFiles => SourcePickerTab::Queue,
        };
        self.tab_focused = true;
        self.queue_selected = None;
        self.scroll_tab_into_view(60);
    }

    /// Ensure the selected tab is visible within the given viewport width.
    /// Adjusts tab_scroll_offset so the active tab label fits on screen.
    fn scroll_tab_into_view(&mut self, viewport_width: usize) {
        // Tab labels: active uses padding (12-14 chars), inactive is just the label (12-13 chars)
        let tab_widths: Vec<(SourcePickerTab, usize)> = vec![
            (SourcePickerTab::Queue, 10),         // " Queue "
            (SourcePickerTab::SuperCollider, 16), // " SuperCollider "
            (SourcePickerTab::DeckActions, 15),   // " Deck Actions "
        ];

        // Compute x-position of each tab label
        let mut x = 0;
        let mut active_x = 0;
        let mut active_width = 16;
        for (tab, width) in &tab_widths {
            if *tab == self.tab {
                active_x = x;
                active_width = *width;
            }
            x += width;
        }

        // If active tab is before the viewport, scroll left
        if active_x < self.tab_scroll_offset {
            self.tab_scroll_offset = active_x;
        }
        // If active tab extends past the viewport, scroll right
        else if active_x + active_width > self.tab_scroll_offset + viewport_width {
            self.tab_scroll_offset = active_x + active_width - viewport_width;
        }
    }

    /// Check if we can go up a directory (not at root)
    pub fn can_go_up(&self) -> bool {
        self.current_dir != self.root_dir && self.current_dir.starts_with(&self.root_dir)
    }

    /// Get display path relative to root
    pub fn relative_path(&self) -> String {
        self.current_dir
            .strip_prefix(&self.root_dir)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod source_picker_tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use crate::state::Sequence;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{
        App, AppMode, ConfirmAction, Deck, GlobalControl, QueueItem, SelectedPane, SelectionFocus,
        SourcePickerItem, SourcePickerState, queue_nav_available,
    };

    fn queue_item(name: &str) -> QueueItem {
        QueueItem {
            name: name.to_string(),
            path: PathBuf::from(format!("/{name}.wav")),
            is_socket: false,
            is_pcm_fifo: false,
            is_udp: false,
        }
    }

    fn source_item(name: &str) -> SourcePickerItem {
        SourcePickerItem {
            name: name.to_string(),
            path: PathBuf::from(format!("/{name}.wav")),
            is_socket: false,
            is_pcm_fifo: false,
            is_udp: false,
            is_dir: false,
        }
    }

    #[test]
    fn shifted_navigation_extends_add_source_selection() {
        let mut picker = SourcePickerState::new();
        picker.add_items = vec![source_item("one"), source_item("two"), source_item("three")];
        picker.add_filter();

        picker.add_extend_selection(true);
        assert_eq!(picker.add_selected, 0);
        assert_eq!(picker.add_marked, HashSet::from([PathBuf::from("/one.wav")]));

        picker.add_extend_selection(true);
        assert_eq!(picker.add_selected, 1);
        assert_eq!(
            picker.add_marked,
            HashSet::from([PathBuf::from("/one.wav"), PathBuf::from("/two.wav")])
        );

        picker.add_extend_selection(false);
        assert_eq!(picker.add_selected, 0);
        assert_eq!(picker.add_marked, HashSet::from([PathBuf::from("/one.wav")]));
    }

    #[test]
    fn changing_tabs_clears_queue_selection() {
        let mut picker = SourcePickerState::new();

        picker.tab_focused = false;
        picker.queue_selected = Some(0);
        picker.next_tab();
        assert!(picker.tab_focused);
        assert_eq!(picker.queue_selected, None);

        picker.tab_focused = false;
        picker.queue_selected = Some(0);
        picker.prev_tab();
        assert!(picker.tab_focused);
        assert_eq!(picker.queue_selected, None);
    }

    #[test]
    fn queue_navigation_stops_at_boundaries() {
        assert_eq!(queue_nav_available(0, 0), (false, false));
        assert_eq!(queue_nav_available(0, 3), (false, true));
        assert_eq!(queue_nav_available(1, 3), (true, true));
        assert_eq!(queue_nav_available(2, 3), (true, false));
    }

    #[test]
    fn shifted_arrows_move_queue_items() {
        let mut app = App::new(2);
        app.mode = AppMode::SourcePicker(Deck::A);
        app.source_picker.queue = vec![queue_item("one"), queue_item("two")];
        app.source_picker.queue_selected = Some(0);

        app.handle_queue_key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(app.source_picker.queue_selected, Some(1));
        assert_eq!(app.source_picker.queue[0].name, "two");

        app.handle_queue_key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.source_picker.queue_selected, Some(0));
        assert_eq!(app.source_picker.queue[0].name, "one");
    }

    #[test]
    fn master_play_pause_only_toggles_with_space_or_enter() {
        let mut app = App::new(2);
        app.mode = AppMode::ControlSelect;
        app.selected_pane = SelectedPane::Master;
        app.mixer.focus = SelectionFocus::Global;
        app.mixer.selected_global = GlobalControl::MasterPlayPause;
        app.mixer.master.playing = true;

        for key in [
            KeyCode::Char('h'),
            KeyCode::Left,
            KeyCode::Char('l'),
            KeyCode::Right,
        ] {
            app.handle_control_select_key(KeyEvent::new(key, KeyModifiers::NONE));
            assert!(app.mixer.master.playing);
        }

        app.handle_control_select_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(!app.mixer.master.playing);
        app.handle_control_select_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.mixer.master.playing);
    }

    #[test]
    fn master_controls_follow_visual_vertical_navigation() {
        let mut app = App::new(2);
        app.mode = AppMode::ControlSelect;
        app.selected_pane = SelectedPane::Master;
        app.mixer.focus = SelectionFocus::Global;

        app.mixer.selected_global = GlobalControl::MasterMute;
        app.handle_control_select_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.mixer.selected_global, GlobalControl::MasterPlayPause);

        app.mixer.selected_global = GlobalControl::MasterOutputSelect;
        app.handle_control_select_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.mixer.selected_global, GlobalControl::MasterFader);
    }

    #[test]
    fn master_pause_only_resumes_previously_playing_decks_and_sequences() {
        let mut app = App::new(2);
        app.mixer.get_channel_mut(0).unwrap().playing = true;
        app.mixer.get_channel_mut(1).unwrap().playing = false;
        app.mixer.cue_channel.playing = false;
        app.sequence_state.sequences = vec![Sequence::new(0, 0), Sequence::new(1, 1)];
        app.sequence_state.sequences[0].playing = true;
        app.sequence_state.sequences[1].playing = false;

        app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        assert!(!app.mixer.get_channel(0).unwrap().playing);
        assert!(!app.mixer.get_channel(1).unwrap().playing);
        assert!(app.sequence_state.sequences.iter().all(|seq| !seq.playing));

        app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        assert!(app.mixer.get_channel(0).unwrap().playing);
        assert!(!app.mixer.get_channel(1).unwrap().playing);
        assert!(app.sequence_state.sequences[0].playing);
        assert!(!app.sequence_state.sequences[1].playing);
    }

    #[test]
    fn master_pause_preserves_sequence_global_mute() {
        let mut app = App::new(2);
        app.sequence_state.sequences = vec![Sequence::new(0, 0)];
        app.sequence_state.sequences[0].playing = true;
        app.sequence_state.global.mute = true;

        app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE));

        assert!(app.sequence_state.global.mute);
        assert!(app.sequence_state.sequences[0].playing);
    }

    #[test]
    fn quit_requires_confirmation() {
        let mut app = App::new(2);

        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(app.mode, AppMode::ConfirmAction(ConfirmAction::Quit));
        assert!(!app.should_quit);

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(app.mode, AppMode::PaneSelect);
        assert!(!app.should_quit);

        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn removing_active_queue_item_loads_successor() {
        let mut app = App::new(2);
        app.mode = AppMode::SourcePicker(Deck::A);
        app.source_picker.queue = vec![queue_item("one"), queue_item("two"), queue_item("three")];
        app.source_picker.queue_selected = Some(1);
        app.deck_queues[0] = app.source_picker.queue.clone();
        app.deck_queue_indices[0] = 1;

        app.handle_queue_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

        assert_eq!(app.deck_queue_indices[0], 1);
        assert_eq!(
            app.deck_queues[0]
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["one", "three"]
        );
        let channel = app.mixer.get_channel(app.mixer.dj.deck_a_channel).unwrap();
        assert_eq!(channel.name, "three");
        assert!(channel.connected);
        assert!(channel.playing);
    }

    #[test]
    fn removing_active_tail_clears_deck_and_preserves_earlier_queue_items() {
        let mut app = App::new(2);
        app.mode = AppMode::SourcePicker(Deck::A);
        app.source_picker.queue = vec![queue_item("one"), queue_item("two")];
        app.source_picker.queue_selected = Some(1);
        app.deck_queues[0] = app.source_picker.queue.clone();
        app.deck_queue_indices[0] = 1;
        let channel_idx = app.mixer.dj.deck_a_channel;
        let channel = app.mixer.get_channel_mut(channel_idx).unwrap();
        channel.connected = true;
        channel.playing = true;
        channel.source_id = Some("/two.wav".to_string());

        app.handle_queue_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));

        assert_eq!(
            app.source_picker
                .queue
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["one"]
        );
        assert_eq!(
            app.deck_queues[0]
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["one"]
        );
        let channel = app.mixer.get_channel(channel_idx).unwrap();
        assert!(!channel.connected);
        assert!(!channel.playing);
        assert!(channel.source_id.is_none());
    }

    #[test]
    fn swapping_cue_with_deck_swaps_sources_and_queues() {
        let mut app = App::new(2);
        app.mixer.channels[0].name = "Deck A source".to_string();
        app.mixer.channels[0].source_id = Some("/a.wav".to_string());
        app.mixer.cue_channel.name = "Deck C source".to_string();
        app.mixer.cue_channel.source_id = Some("/c.wav".to_string());
        app.deck_queues[0] = vec![queue_item("a")];
        app.deck_queues[2] = vec![queue_item("c1"), queue_item("c2")];
        app.deck_queue_indices[2] = 1;

        app.swap_cue_with_deck(super::SendTarget::A);

        assert_eq!(app.mixer.channels[0].name, "Deck C source");
        assert_eq!(app.mixer.channels[0].source_id.as_deref(), Some("/c.wav"));
        assert_eq!(app.mixer.cue_channel.name, "Deck A source");
        assert_eq!(app.mixer.cue_channel.source_id.as_deref(), Some("/a.wav"));
        assert_eq!(
            app.deck_queues[0]
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["c1", "c2"]
        );
        assert_eq!(app.deck_queue_indices[0], 1);
        assert_eq!(app.deck_queues[2][0].name, "a");
        assert_eq!(app.deck_queue_indices[2], 0);
        assert_eq!(app.mixer.channels[0].index, 0);
        assert!(!app.mixer.channels[0].pfl);
        assert_eq!(app.mixer.cue_channel.index, 2);
        assert!(app.mixer.cue_channel.pfl);
    }
}

fn queue_nav_available(index: usize, len: usize) -> (bool, bool) {
    (index > 0 && index < len, index.saturating_add(1) < len)
}

/// Which pane is selected
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectedPane {
    DeckA,
    DjCenter,
    Loops,
    Crossfader,
    DeckB,
    DeckC,
    Master,
}

impl SelectedPane {
    pub fn next(self) -> Self {
        match self {
            SelectedPane::DeckA => SelectedPane::DjCenter,
            SelectedPane::DjCenter => SelectedPane::Loops,
            SelectedPane::Loops => SelectedPane::Crossfader,
            SelectedPane::Crossfader => SelectedPane::DeckB,
            SelectedPane::DeckB => SelectedPane::DeckC,
            SelectedPane::DeckC => SelectedPane::Master,
            SelectedPane::Master => SelectedPane::DeckA,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            SelectedPane::DeckA => SelectedPane::Master,
            SelectedPane::DjCenter => SelectedPane::DeckA,
            SelectedPane::Loops => SelectedPane::DjCenter,
            SelectedPane::Crossfader => SelectedPane::Loops,
            SelectedPane::DeckB => SelectedPane::Crossfader,
            SelectedPane::DeckC => SelectedPane::DeckB,
            SelectedPane::Master => SelectedPane::DeckC,
        }
    }
}

/// Horizontal layout for the mixer row.
///
/// The 5 columns, left to right, are: Deck A, DJ Center (PADS), Deck B,
/// Deck C, Master. When the viewport is too narrow for everything, Master
/// drops into an overflow region to the right. Navigation reveals it by
/// scrolling the viewport left, dropping the fewest leftmost columns needed
/// so the selected column (and as many neighbours as fit) stay visible.
#[derive(Debug, Clone, Copy)]
pub struct MixerLayout {
    pub deck_a: u16,
    pub dj: u16,
    pub deck_b: u16,
    pub deck_c: u16,
    pub master: u16,
    /// First visible column index (0=DeckA, 1=DJ, 2=DeckB, 3=DeckC, 4=Master).
    pub start: u8,
    /// Last visible column index (inclusive).
    pub end: u8,
}

impl MixerLayout {
    pub fn compute(
        viewport_w: u16,
        selected: SelectedPane,
        cur_start: Option<usize>,
        cur_end: Option<usize>,
    ) -> Self {
        const DECK_MAX: u16 = 21;
        const DECK_MIN: u16 = 13;
        const MASTER: u16 = 21;
        // Must fit 16 sequence toggles (32 cells) + name (9 cells) + right controls (5 cells) + borders (2)
        const DJ_MIN: u16 = 48;
        const MIN_CORE: u16 = DECK_MIN * 3 + DJ_MIN;

        // Minimum widths of the 5 columns, left to right.
        let mins = [DECK_MIN, DJ_MIN, DECK_MIN, DECK_MIN, MASTER];

        // Case 1: all 5 columns fit -> full layout, no scrolling.
        if viewport_w >= MIN_CORE + MASTER {
            let w = ((viewport_w - MASTER - DJ_MIN) / 3).clamp(DECK_MIN, DECK_MAX);
            return Self {
                deck_a: w,
                dj: viewport_w - w * 3 - MASTER,
                deck_b: w,
                deck_c: w,
                master: MASTER,
                start: 0,
                end: 4,
            };
        }

        // Case 2: overflow. Master is not in the normal 5-column layout.
        // Build the window of visible columns. Master always anchors the right
        // edge when selected. For other panes, center the window around the
        // selected column so it's always visible.
        let selected_col = match selected {
            SelectedPane::DeckA => 0usize,
            SelectedPane::DjCenter | SelectedPane::Loops | SelectedPane::Crossfader => 1,
            SelectedPane::DeckB => 2,
            SelectedPane::DeckC => 3,
            SelectedPane::Master => 4,
        };

        let (start, end) = if selected == SelectedPane::Master {
            // Master selected: anchor right edge at Master, grow left.
            let mut s = 4usize;
            let mut used = mins[4];
            while s > 0 {
                let cand = mins[s - 1];
                if used + cand <= viewport_w {
                    used += cand;
                    s -= 1;
                } else {
                    break;
                }
            }
            (s, 4usize)
        } else if let (Some(cs), Some(ce)) = (cur_start, cur_end) {
            // If the selected column is already visible and the window
            // fits the viewport, keep it. Otherwise recompute.
            let window_min: u16 = (cs..=ce).map(|i| mins[i]).sum();
            if selected_col >= cs && selected_col <= ce && window_min <= viewport_w {
                (cs, ce)
            } else {
                // Selection moved outside — shift window to include it,
                // preserving the current width where possible.
                let width = ce - cs;
                let (s, e) = if selected_col < cs {
                    // Shift left
                    (selected_col, selected_col + width)
                } else {
                    // Shift right
                    (selected_col.saturating_sub(width), selected_col)
                };
                let mut s = s;
                let mut e = e;
                // Clamp and shrink if needed
                s = s.min(4);
                e = e.min(4);
                // Shrink from the far edge if it doesn't fit
                while s < e && (s..=e).map(|i| mins[i]).sum::<u16>() > viewport_w {
                    if selected_col - s <= e - selected_col {
                        e -= 1;
                    } else {
                        s += 1;
                    }
                }
                (s, e)
            }
        } else {
            // No current window — find largest window around selected_col.
            let mut s = selected_col;
            let mut e = selected_col;
            let mut used = mins[selected_col];
            loop {
                let fit_left = s > 0 && used + mins[s - 1] <= viewport_w;
                let fit_right = e < 4 && used + mins[e + 1] <= viewport_w;
                if fit_left && fit_right {
                    if mins[s - 1] <= mins[e + 1] {
                        used += mins[s - 1];
                        s -= 1;
                    } else {
                        used += mins[e + 1];
                        e += 1;
                    }
                } else if fit_left {
                    used += mins[s - 1];
                    s -= 1;
                } else if fit_right {
                    used += mins[e + 1];
                    e += 1;
                } else {
                    break;
                }
            }
            (s, e)
        };

        let min_total: u16 = (start..=end).map(|i| mins[i]).sum();
        let extra = viewport_w.saturating_sub(min_total);

        let mut widths = [0u16; 5];
        if start == 4 && end == 4 {
            // Master is the only visible column — give it all the space.
            widths[4] = viewport_w;
        } else {
            // Master stays at its minimum (fits the full EQ); distribute the
            // extra space across the other visible columns.
            let non_master_visible: u16 = (start..=end).filter(|&i| i != 4).count() as u16;
            let per = extra / non_master_visible;
            let rem = extra % non_master_visible;

            let mut assigned = 0u16;
            for i in start..=end {
                widths[i] = if i == 4 {
                    MASTER
                } else {
                    let add = per + if assigned < rem { 1 } else { 0 };
                    assigned += 1;
                    mins[i] + add
                };
            }
        }
        // Dropped columns keep their minimum width (they sit off-screen left).
        widths[..start].copy_from_slice(&mins[..start]);

        Self {
            deck_a: widths[0],
            dj: widths[1],
            deck_b: widths[2],
            deck_c: widths[3],
            master: widths[4],
            start: start as u8,
            end: end as u8,
        }
    }
}

/// Main application state
pub struct App {
    pub mixer: MixerState,
    pub sample_pads: SamplePadGrid,
    pub audio_manager: AudioSourceManager,
    pub mode: AppMode,
    pub selected_pane: SelectedPane,
    pub should_quit: bool,
    pub last_tick: Instant,
    pub tick_rate: Duration,
    // Mouse dragging state
    drag_start_y: Option<u16>,
    drag_start_x: Option<u16>,
    drag_start_value: Option<f32>,
    // Channel strip areas for mouse hit testing
    channel_areas: Vec<ChannelArea>,
    // Sample pad areas for mouse hit testing
    pad_areas: Vec<(usize, u16, u16, u16, u16)>, // (pad_idx, x, y, w, h)
    // Pane areas for mouse hit testing
    crossfader_area: Option<PaneArea>,
    master_area: Option<PaneArea>,
    cue_area: Option<PaneArea>,
    loops_area: Option<PaneArea>,
    // Source picker
    pub source_picker: SourcePickerState,
    pub music_dir: PathBuf,
    pub samples_dir: PathBuf,
    // Currently selected pad in DJ center (when navigating pads)
    pub selected_pad_idx: Option<usize>,
    // MPV clients for each deck
    mpv_deck_a: Option<MpvClient>,
    mpv_deck_b: Option<MpvClient>,
    mpv_deck_c: Option<MpvClient>,
    // Per-deck queues for auto-advance
    pub(crate) deck_queues: [Vec<QueueItem>; 3],
    pub(crate) deck_queue_indices: [usize; 3], // Current playing index in each deck's queue
    // Per-deck route-mode command workers (FIFO control).
    route_cmd_workers: [Option<RouteCommandWorker>; 3],
    route_seek_workers: [Option<RouteSeekWorker>; 3],
    route_timeline_workers: [Option<RouteTimelineWorker>; 3],
    route_playlist_nav_workers: [Option<RoutePlaylistNavWorker>; 3],
    route_nav_cache: [Option<(bool, bool)>; 3],
    route_nav_last_ms: [u64; 3],
    route_meta_last_ms: [u64; 3],
    route_scrub_last_ms: [u64; 3],
    route_duration_last_ms: [u64; 3],
    route_seek_last_ms: [u64; 3],
    route_seek_input_last_ms: [u64; 3],
    route_seek_send_last_ms: [u64; 3],
    route_seek_target_pos: [f32; 3],
    route_seek_pending: [bool; 3],
    route_seek_pending_since_ms: [u64; 3],
    scrub_input_last_ms: [u64; 3],
    scrub_hold_start_ms: [u64; 3],
    scrub_fine_last_ms: [u64; 3],
    scrub_fine_last_dir: [i8; 3],
    scrub_pending_return_ms: [u64; 3],
    scrub_pending_return_delta: [f32; 3],
    route_scrub_lock_until_ms: [u64; 3],
    route_last_time_pos: [f32; 3],
    last_scrub_tick_ms: u64,
    route_last_seek_delta_ms: [u32; 3],
    route_last_cmd_sent_ms: [u64; 3],
    route_last_track_sig: [u64; 3],
    route_speed_cache: [f32; 3],
    route_prev_cache: [bool; 3],
    route_next_cache: [bool; 3],
    perf_trace: PerfTrace,
    // SuperCollider clients for each deck
    sc_deck_a: Option<SuperColliderClient>,
    sc_deck_b: Option<SuperColliderClient>,
    sc_deck_c: Option<SuperColliderClient>,
    // Sample playback engine (cached samples for instant playback)
    sample_engine: Option<SampleEngine>,
    // Rust-native audio engine (replaces MPV/SC for DSP)
    pub audio_engine: Option<AudioEngine>,
    // Sequence state
    pub sequence_state: SequenceState,
    // Frame counter for internal periodic tasks
    pub frame_counter: u64,
    // Elapsed time in ms since program start
    pub elapsed_ms: u64,
    boot_instant: Instant,
    // Terminal height for calculating visible rows
    pub terminal_height: u16,
    // Terminal width for horizontal pane scrolling
    pub term_width: u16,
    // Current mixer window bounds (column indices) — preserved when
    // navigating between already-visible panes.
    pub mixer_window_start: usize,
    pub mixer_window_end: usize,
    // Audio output devices
    pub master_output: AudioOutput,
    pub cue_output: AudioOutput,
    // Selected output device indices (for UI navigation)
    pub selected_master_output_idx: usize,
    pub selected_cue_output_idx: usize,
    pub mic_input_devices: Vec<MicInputDevice>,
    pub selected_mic_input_idx: usize,
    // Output device picker mode
    pub output_picker_active: bool,
    pub output_picker_target: OutputPickerTarget,
    // Debug log messages (circular buffer, last 100 messages)
    pub debug_log: VecDeque<String>,
    // Scroll offset for debug log (0 = latest, higher = older messages)
    pub debug_scroll: usize,
    // Confirm dialog selection (true = Y focused, false = N focused)
    pub confirm_selected: bool,
    // Config check diffs (files that need updating)
    pub config_diffs: Vec<crate::config::ConfigDiff>,
    // Config check message to display (e.g. PATH setup result)
    pub config_check_msg: Option<String>,
    // Help panel scroll offset
    pub help_scroll: usize,
    // Counter for MPV state polling (poll every N ticks)
    mpv_poll_counter: u8,
    source_refresh_counter: u8,
    tidal_bpm_poll_counter: u8,
    route_meta_poll_counter: u8,
    // Timestamp (elapsed_ms) of last TUI-initiated volume push per deck (0,1,2)
    last_volume_push_ms: [u64; 3],
    // Consecutive poll failures per deck (A=0, B=1, C=2) — cleared deck after threshold
    consecutive_poll_failures: [u8; 3],
    // Pending BPM+key results from background analysis (channel_idx, bpm, key)
    pending_bpm: PendingBpm,
    // Last detected key per deck (0=A, 1=B, 2=C) for stability check on mid-track changes
    last_detected_keys: [Option<String>; 3],
    // Track-end detection: was the engine playing on the previous tick?
    engine_was_playing: [bool; 3],
    // Session picker state (for save/load without native dialogs)
    pub session_picker: SessionPickerState,
    pub sessions_dir: PathBuf,
}

/// Which output device picker is active
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputPickerTarget {
    Master,
    Cue,
    Mic,
}

/// Tracks the screen areas for each channel for mouse interaction
#[derive(Debug, Clone, Default)]
pub struct ChannelArea {
    pub bounds: (u16, u16, u16, u16), // x, y, width, height
    pub control_rows: Vec<(ChannelControl, u16, u16)>, // control, y_start, y_end
}

/// Generic rectangular area for a pane (crossfader, master, CUE, etc.)
#[derive(Debug, Clone, Copy, Default)]
pub struct PaneArea {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl PaneArea {
    pub fn contains(&self, px: u16, py: u16) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Result of a mouse hit-test across all panes
#[derive(Debug, Clone)]
pub enum HitResult {
    /// Click on a channel strip control
    Channel(usize, ChannelControl),
    /// Click on a sample pad
    Pad(usize),
    /// Click/drag on the crossfader
    Crossfader,
    /// Click on master pane (specific control inferred from y position)
    Master,
    /// Click on CUE pane (specific control inferred from y position)
    Cue,
    /// Click on loops pane
    Loops,
}

struct RouteCommandWorker {
    path: String,
    tx: Sender<Vec<serde_json::Value>>,
}

struct RouteSeekWorker {
    path: String,
    tx: Sender<f32>,
}

#[derive(Clone, Copy, Default)]
struct RouteTimelineSnapshot {
    time_pos: Option<f32>,
    duration: Option<f32>,
    generated_ms: u64,
}

struct RouteTimelineWorker {
    path: String,
    rx: Receiver<RouteTimelineSnapshot>,
    latest: RouteTimelineSnapshot,
}

struct RoutePlaylistNavWorker {
    path: String,
    tx: Sender<()>,
    rx: Receiver<(bool, bool)>,
}

enum EngineDeckSource {
    None,
    File { path: String, position: f64 },
    Capture(PathBuf),
}

#[derive(Default, Clone, Copy)]
struct PerfTrace {
    route_seek_sends: u32,
    route_seek_send_failures: u32,
    route_seek_input_events: u32,
    route_seek_input_to_send_max_ms: u32,
    route_seek_send_to_apply_max_ms: u32,
    route_timepos_delta_max_ms: u32,
    route_meta_polls: u32,
    route_meta_selected_polls: u32,
    route_meta_updates: u32,
    route_meta_failures: u32,
    route_meta_age_max_ms: u32,
    timeline_updates: u32,
    timeline_age_max_ms: u32,
}

impl PerfTrace {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

impl App {
    fn spawn_route_command_worker(ch_idx: usize, sock_str: String) -> Option<RouteCommandWorker> {
        let (tx, rx) = mpsc::channel::<Vec<serde_json::Value>>();
        let path_for_thread = sock_str.clone();
        let thread_name = format!("route-cmd-{}", ch_idx);

        let spawn = thread::Builder::new().name(thread_name).spawn(move || {
            let mut client = MpvClient::new(path_for_thread.clone());
            if let Err(e) = client.connect() {
                eprintln!(
                    "Route CMD ch{}: connection failed ({}): {}",
                    ch_idx, path_for_thread, e
                );
                let path = std::path::Path::new(&path_for_thread);
                if path.exists() {
                    eprintln!(
                        "Route CMD ch{}: stale socket detected, removing {}",
                        ch_idx, path_for_thread
                    );
                    let _ = std::fs::remove_file(path);
                }
                if let Some(alt) = Self::find_alternative_route_socket(&path_for_thread) {
                    eprintln!(
                        "Route CMD ch{}: trying alternative socket {}",
                        ch_idx,
                        alt.display()
                    );
                    client = MpvClient::new(alt.to_string_lossy().to_string());
                    if client.connect().is_err() {
                        eprintln!("Route CMD ch{}: alternative socket also failed", ch_idx);
                        return;
                    }
                } else {
                    return;
                }
            }
            client.set_timeouts(10, 10);

            while let Ok(command) = rx.recv() {
                if client.send_command(command.clone()).is_ok() {
                    continue;
                }

                if client.connect().is_ok() {
                    client.set_timeouts(10, 10);
                    let _ = client.send_command(command);
                } else {
                    eprintln!(
                        "Route CMD ch{}: command failed and reconnection failed, worker exiting",
                        ch_idx
                    );
                    break;
                }
            }
        });

        if spawn.is_err() {
            return None;
        }

        Some(RouteCommandWorker { path: sock_str, tx })
    }

    fn ensure_route_command_worker(&mut self, ch_idx: usize) -> Option<&RouteCommandWorker> {
        let sock = self.route_socket_for_channel(ch_idx)?;
        let sock_str = sock.to_string_lossy().to_string();
        if self.route_cmd_workers[ch_idx]
            .as_ref()
            .map(|w| w.path.as_str())
            != Some(sock_str.as_str())
        {
            self.log_debug(format!("Route CMD ch{} -> {}", ch_idx, sock_str));
            self.route_cmd_workers[ch_idx] = None;
        }

        if self.route_cmd_workers[ch_idx].is_none() {
            self.route_cmd_workers[ch_idx] =
                Self::spawn_route_command_worker(ch_idx, sock_str.clone());
            if self.route_cmd_workers[ch_idx].is_none() {
                eprintln!(
                    "Route CMD ch{}: spawn failed for socket {}",
                    ch_idx, sock_str
                );
                self.log_debug(format!("Route CMD spawn failed for ch{}", ch_idx));
                return None;
            }
        }

        self.route_cmd_workers[ch_idx].as_ref()
    }

    fn spawn_route_seek_worker(ch_idx: usize, sock_str: String) -> Option<RouteSeekWorker> {
        let (tx, rx) = mpsc::channel::<f32>();
        let path_for_thread = sock_str.clone();
        let thread_name = format!("route-seek-{}", ch_idx);

        let spawn = thread::Builder::new().name(thread_name).spawn(move || {
            let mut client = MpvClient::new(path_for_thread.clone());
            if client.connect().is_err() {
                return;
            }
            client.set_timeouts(10, 10);

            while let Ok(mut target) = rx.recv() {
                while let Ok(next) = rx.try_recv() {
                    target = next;
                }

                let cmd = vec![
                    serde_json::json!("seek"),
                    serde_json::json!(target),
                    serde_json::json!("absolute+exact"),
                ];

                if client.send_command(cmd.clone()).is_ok() {
                    continue;
                }

                if client.connect().is_err() {
                    break;
                }
                client.set_timeouts(10, 10);
                let _ = client.send_command(cmd);
            }
        });

        if spawn.is_err() {
            return None;
        }

        Some(RouteSeekWorker { path: sock_str, tx })
    }

    fn ensure_route_seek_worker(&mut self, ch_idx: usize) -> Option<&RouteSeekWorker> {
        let sock = self.route_socket_for_channel(ch_idx)?;
        let sock_str = sock.to_string_lossy().to_string();
        if self.route_seek_workers[ch_idx]
            .as_ref()
            .map(|w| w.path.as_str())
            != Some(sock_str.as_str())
        {
            self.log_debug(format!("Route SEEK ch{} -> {}", ch_idx, sock_str));
            self.route_seek_workers[ch_idx] = None;
        }

        if self.route_seek_workers[ch_idx].is_none() {
            self.route_seek_workers[ch_idx] = Self::spawn_route_seek_worker(ch_idx, sock_str);
            if self.route_seek_workers[ch_idx].is_none() {
                self.log_debug(format!("Route SEEK spawn failed for ch{}", ch_idx));
                return None;
            }
        }

        self.route_seek_workers[ch_idx].as_ref()
    }

    fn spawn_route_timeline_worker(
        ch_idx: usize,
        sock_str: String,
        start: Instant,
    ) -> Option<RouteTimelineWorker> {
        let (tx, rx) = mpsc::channel::<RouteTimelineSnapshot>();
        let path_for_thread = sock_str.clone();
        let thread_name = format!("route-time-{}", ch_idx);

        let spawn = thread::Builder::new().name(thread_name).spawn(move || {
            let mut client = MpvClient::new(path_for_thread.clone());
            let mut connected = false;

            let mut duration_cache: Option<f32> = None;
            let mut prev_time_pos: Option<f32> = None;
            let mut miss_count: u32 = 0;

            loop {
                if !connected {
                    if client.connect().is_ok() {
                        client.set_timeouts(ROUTE_TIMELINE_TIMEOUT_MS, 100);
                        connected = true;
                    } else {
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                }

                let mut time_pos = client.get_time_pos().ok();

                // Only poll duration when we don't have it yet, or when
                // time_pos jumps backward >5s (track change / seek-to-start),
                // or when time_pos reappears after being unavailable (gap
                // between playlist tracks where get_time_pos returns None).
                let need_duration = duration_cache.is_none()
                    || matches!(
                        (prev_time_pos, time_pos),
                        (Some(prev), Some(cur)) if cur + 5.0 < prev
                    )
                    || (prev_time_pos.is_none() && time_pos.is_some());
                if need_duration && let Ok(dur) = client.get_duration() {
                    duration_cache = Some(dur);
                    time_pos = client.get_time_pos().ok().or(time_pos);
                }
                prev_time_pos = time_pos;

                let snapshot = RouteTimelineSnapshot {
                    time_pos,
                    duration: duration_cache,
                    generated_ms: start.elapsed().as_millis() as u64,
                };

                if tx.send(snapshot).is_err() {
                    break;
                }

                if snapshot.time_pos.is_none() {
                    miss_count = miss_count.saturating_add(1);
                } else {
                    miss_count = 0;
                }

                if miss_count >= 20 {
                    if client.connect().is_ok() {
                        client.set_timeouts(ROUTE_TIMELINE_TIMEOUT_MS, 100);
                        miss_count = 0;
                        connected = true;
                    } else {
                        connected = false;
                        miss_count = 0;
                        thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                }

                thread::sleep(Duration::from_millis(10));
            }
        });

        if spawn.is_err() {
            return None;
        }

        Some(RouteTimelineWorker {
            path: sock_str,
            rx,
            latest: RouteTimelineSnapshot::default(),
        })
    }

    fn spawn_route_playlist_nav_worker(
        ch_idx: usize,
        sock_str: String,
    ) -> Option<RoutePlaylistNavWorker> {
        let (req_tx, req_rx) = mpsc::channel::<()>();
        let (resp_tx, resp_rx) = mpsc::channel::<(bool, bool)>();
        let path_for_thread = sock_str.clone();
        let thread_name = format!("route-nav-{}", ch_idx);

        let spawn = thread::Builder::new().name(thread_name).spawn(move || {
            let mut client = MpvClient::new(path_for_thread.clone());
            if client.connect().is_err() {
                return;
            }
            client.set_timeouts(20, 20);

            while req_rx.recv().is_ok() {
                let nav = client.get_playlist_nav_available().ok().or_else(|| {
                    if client.connect().is_ok() {
                        client.set_timeouts(20, 20);
                        client.get_playlist_nav_available().ok()
                    } else {
                        None
                    }
                });

                if let Some(pair) = nav
                    && resp_tx.send(pair).is_err()
                {
                    break;
                }
            }
        });

        if spawn.is_err() {
            return None;
        }

        Some(RoutePlaylistNavWorker {
            path: sock_str,
            tx: req_tx,
            rx: resp_rx,
        })
    }

    fn ensure_route_timeline_worker(&mut self, ch_idx: usize) -> Option<&mut RouteTimelineWorker> {
        let sock = self.route_socket_for_channel(ch_idx)?;
        let sock_str = sock.to_string_lossy().to_string();
        if self.route_timeline_workers[ch_idx]
            .as_ref()
            .map(|w| w.path.as_str())
            != Some(sock_str.as_str())
        {
            self.log_debug(format!("Route TIME ch{} -> {}", ch_idx, sock_str));
            self.route_timeline_workers[ch_idx] = None;
        }

        if self.route_timeline_workers[ch_idx].is_none() {
            self.route_timeline_workers[ch_idx] =
                Self::spawn_route_timeline_worker(ch_idx, sock_str, self.boot_instant);
            if self.route_timeline_workers[ch_idx].is_none() {
                self.log_debug(format!("Route TIME spawn failed for ch{}", ch_idx));
                return None;
            }
        }

        self.route_timeline_workers[ch_idx].as_mut()
    }

    fn flush_route_timeline_updates(&mut self) {
        let deck_channels = [
            self.mixer.dj.deck_a_channel,
            self.mixer.dj.deck_b_channel,
            self.mixer.dj.deck_c_channel,
        ];

        for ch_idx in deck_channels {
            let has_capture = self
                .audio_engine
                .as_ref()
                .map(|engine| engine.has_capture(ch_idx))
                .unwrap_or(false);
            if !has_capture {
                continue;
            }

            let (latest, update_count, disconnected) = {
                let Some(worker) = self.ensure_route_timeline_worker(ch_idx) else {
                    continue;
                };
                let mut update_count = 0u32;
                let mut disconnected = false;
                loop {
                    match worker.rx.try_recv() {
                        Ok(snapshot) => {
                            update_count = update_count.saturating_add(1);
                            worker.latest = snapshot;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                (worker.latest, update_count, disconnected)
            };

            if disconnected {
                self.route_timeline_workers[ch_idx] = None;
                let _ = self.ensure_route_timeline_worker(ch_idx);
                continue;
            }

            if update_count == 0 {
                let age_ms = if latest.generated_ms > 0 {
                    self.elapsed_ms.saturating_sub(latest.generated_ms) as u32
                } else {
                    0
                };
                if age_ms > ROUTE_TIMELINE_STALE_MS as u32 {
                    self.route_timeline_workers[ch_idx] = None;
                    let _ = self.ensure_route_timeline_worker(ch_idx);
                    continue;
                }
            }

            self.perf_trace.timeline_updates = self
                .perf_trace
                .timeline_updates
                .saturating_add(update_count);
            if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
                if let Some(dur) = latest.duration {
                    ch.duration = dur.max(0.0);
                    self.route_duration_last_ms[ch_idx] = self.elapsed_ms;
                }
                let age = if latest.generated_ms > 0 {
                    self.elapsed_ms.saturating_sub(latest.generated_ms) as u32
                } else {
                    0
                };
                ch.timeline_age_ms = age;
                self.perf_trace.timeline_age_max_ms = self.perf_trace.timeline_age_max_ms.max(age);
            }
        }
    }

    fn ensure_route_playlist_nav_worker(
        &mut self,
        ch_idx: usize,
    ) -> Option<&mut RoutePlaylistNavWorker> {
        let sock = self.route_socket_for_channel(ch_idx)?;
        let sock_str = sock.to_string_lossy().to_string();
        if self.route_playlist_nav_workers[ch_idx]
            .as_ref()
            .map(|w| w.path.as_str())
            != Some(sock_str.as_str())
        {
            self.log_debug(format!("Route NAV ch{} -> {}", ch_idx, sock_str));
            self.route_playlist_nav_workers[ch_idx] = None;
        }

        if self.route_playlist_nav_workers[ch_idx].is_none() {
            self.route_playlist_nav_workers[ch_idx] =
                Self::spawn_route_playlist_nav_worker(ch_idx, sock_str);
            if self.route_playlist_nav_workers[ch_idx].is_none() {
                self.log_debug(format!("Route NAV spawn failed for ch{}", ch_idx));
                return None;
            }
        }

        self.route_playlist_nav_workers[ch_idx].as_mut()
    }

    fn reset_route_clients(&mut self, ch_idx: usize) {
        if ch_idx >= self.route_cmd_workers.len() {
            return;
        }
        self.route_cmd_workers[ch_idx] = None;
        self.route_seek_workers[ch_idx] = None;
        self.route_timeline_workers[ch_idx] = None;
        self.route_playlist_nav_workers[ch_idx] = None;
        self.route_seek_last_ms[ch_idx] = 0;
        self.route_seek_input_last_ms[ch_idx] = 0;
        self.route_seek_send_last_ms[ch_idx] = 0;
        self.route_seek_target_pos[ch_idx] = 0.0;
        self.route_seek_pending[ch_idx] = false;
        self.route_seek_pending_since_ms[ch_idx] = 0;
        self.scrub_input_last_ms[ch_idx] = 0;
        self.scrub_hold_start_ms[ch_idx] = 0;
        self.scrub_fine_last_ms[ch_idx] = 0;
        self.scrub_fine_last_dir[ch_idx] = 0;
        self.scrub_pending_return_ms[ch_idx] = 0;
        self.scrub_pending_return_delta[ch_idx] = 0.0;
        self.route_scrub_lock_until_ms[ch_idx] = 0;
        self.route_last_time_pos[ch_idx] = 0.0;
        self.last_scrub_tick_ms = self.elapsed_ms;
        self.route_meta_last_ms[ch_idx] = 0;
        self.route_last_seek_delta_ms[ch_idx] = 0;
        self.route_last_cmd_sent_ms[ch_idx] = 0;
        self.route_last_track_sig[ch_idx] = 0;
        self.route_speed_cache[ch_idx] = 1.0;
        self.route_prev_cache[ch_idx] = false;
        self.route_next_cache[ch_idx] = false;
    }

    fn send_route_command_for_channel(
        &mut self,
        ch_idx: usize,
        command: Vec<serde_json::Value>,
    ) -> bool {
        let cmd_label = command
            .first()
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let send_result = self
            .ensure_route_command_worker(ch_idx)
            .map(|worker| worker.tx.clone())
            .map(|tx| tx.send(command.clone()).is_ok())
            .unwrap_or(false);

        let is_seek = command
            .first()
            .and_then(|v| v.as_str())
            .map(|s| s == "seek")
            .unwrap_or(false);

        if send_result {
            if ch_idx < self.route_last_cmd_sent_ms.len() {
                self.route_last_cmd_sent_ms[ch_idx] = self.elapsed_ms;
            }
            if ch_idx < self.route_nav_cache.len() {
                self.route_nav_cache[ch_idx] = None;
                self.route_nav_last_ms[ch_idx] = 0;
            }
            return true;
        }

        if is_seek {
            eprintln!(
                "Route CMD ch{}: seek command {:?} dropped (send failed, no retry for seeks)",
                ch_idx, cmd_label
            );
            return false;
        }

        if ch_idx < self.route_cmd_workers.len() {
            self.route_cmd_workers[ch_idx] = None;
        }
        let retry_result = self
            .ensure_route_command_worker(ch_idx)
            .map(|worker| worker.tx.clone())
            .map(|tx| tx.send(command).is_ok())
            .unwrap_or(false);

        if retry_result {
            if ch_idx < self.route_last_cmd_sent_ms.len() {
                self.route_last_cmd_sent_ms[ch_idx] = self.elapsed_ms;
            }
            if ch_idx < self.route_nav_cache.len() {
                self.route_nav_cache[ch_idx] = None;
                self.route_nav_last_ms[ch_idx] = 0;
            }
            return true;
        }
        eprintln!(
            "Route CMD ch{}: failed to send command {:?} (worker dead, retry also failed)",
            ch_idx, cmd_label
        );
        false
    }

    fn send_route_seek_relative(&mut self, ch_idx: usize, delta_secs: f32) -> bool {
        if ch_idx >= self.route_seek_workers.len() {
            return false;
        }

        if ch_idx < self.route_seek_input_last_ms.len() && self.route_seek_input_last_ms[ch_idx] > 0
        {
            let lag = self
                .elapsed_ms
                .saturating_sub(self.route_seek_input_last_ms[ch_idx]) as u32;
            self.perf_trace.route_seek_input_to_send_max_ms =
                self.perf_trace.route_seek_input_to_send_max_ms.max(lag);
        }

        let target_pos = self
            .mixer
            .get_channel(ch_idx)
            .map(|ch| {
                let current = ch.time_pos.max(0.0);
                let last_target = self.route_seek_target_pos[ch_idx].max(0.0);
                let base = if delta_secs >= 0.0 {
                    current.max(last_target)
                } else {
                    current.min(last_target)
                };
                let proposed = base + delta_secs;
                if ch.duration > 0.0 {
                    proposed.clamp(0.0, ch.duration)
                } else {
                    proposed.max(0.0)
                }
            })
            .unwrap_or_else(|| delta_secs.max(0.0));

        let send_result = self
            .ensure_route_seek_worker(ch_idx)
            .map(|worker| worker.tx.clone())
            .map(|tx| tx.send(target_pos).is_ok())
            .unwrap_or(false);

        if send_result {
            self.perf_trace.route_seek_sends = self.perf_trace.route_seek_sends.saturating_add(1);
            self.route_last_cmd_sent_ms[ch_idx] = self.elapsed_ms;
            if ch_idx < self.route_seek_send_last_ms.len() {
                self.route_seek_send_last_ms[ch_idx] = self.elapsed_ms;
                self.route_seek_target_pos[ch_idx] = target_pos;
                self.route_seek_pending[ch_idx] = true;
                self.route_seek_pending_since_ms[ch_idx] = self.elapsed_ms;
            }
            return true;
        }

        self.perf_trace.route_seek_send_failures =
            self.perf_trace.route_seek_send_failures.saturating_add(1);
        if ch_idx < self.route_seek_workers.len() {
            self.route_seek_workers[ch_idx] = None;
        }
        let retry_result = self
            .ensure_route_seek_worker(ch_idx)
            .map(|worker| worker.tx.clone())
            .map(|tx| tx.send(target_pos).is_ok())
            .unwrap_or(false);
        if retry_result {
            self.perf_trace.route_seek_sends = self.perf_trace.route_seek_sends.saturating_add(1);
            self.route_last_cmd_sent_ms[ch_idx] = self.elapsed_ms;
            if ch_idx < self.route_seek_send_last_ms.len() {
                self.route_seek_send_last_ms[ch_idx] = self.elapsed_ms;
                self.route_seek_target_pos[ch_idx] = target_pos;
                self.route_seek_pending[ch_idx] = true;
                self.route_seek_pending_since_ms[ch_idx] = self.elapsed_ms;
            }
            return true;
        }

        self.perf_trace.route_seek_send_failures =
            self.perf_trace.route_seek_send_failures.saturating_add(1);
        false
    }

    fn route_socket_for_channel(&self, ch_idx: usize) -> Option<PathBuf> {
        let source_id = self
            .mixer
            .get_channel(ch_idx)
            .and_then(|c| c.source_id.as_deref())?;
        let fifo = std::path::Path::new(source_id);
        Self::route_socket_candidates_for_fifo(fifo)
            .into_iter()
            .find(|p| p.exists())
    }

    fn trigger_playlist_nav(&mut self, deck: Deck, next: bool) {
        let ch_idx = match deck {
            Deck::A => self.mixer.dj.deck_a_channel,
            Deck::B => self.mixer.dj.deck_b_channel,
            Deck::C => self.mixer.dj.deck_c_channel,
        };
        if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
            if next {
                ch.next_exec_flash_ms = self.elapsed_ms;
            } else {
                ch.prev_exec_flash_ms = self.elapsed_ms;
            }
        }

        if self
            .audio_engine
            .as_ref()
            .is_some_and(|engine| engine.has_capture(ch_idx))
        {
            let can_navigate = self
                .route_playlist_nav(ch_idx)
                .map(|(has_prev, has_next)| if next { has_next } else { has_prev })
                .unwrap_or(false);
            if can_navigate {
                self.route_nav_cache[ch_idx] = None;
                self.route_nav_last_ms[ch_idx] = 0;
                let cmd = if next {
                    "playlist-next"
                } else {
                    "playlist-prev"
                };
                let sent = self.send_route_command_for_channel(
                    ch_idx,
                    vec![serde_json::json!(cmd), serde_json::json!("force")],
                );
                if sent {
                    self.reset_route_track_timeline_state(ch_idx);
                    if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
                        if next {
                            ch.has_prev_track = true;
                        } else {
                            ch.has_next_track = true;
                        }
                    }
                }
                return;
            }
            if next {
                self.navigate_queue_next(deck);
            } else {
                self.navigate_queue_prev(deck);
            }
            return;
        }

        let navigated = if next {
            self.try_mpv_next(deck)
        } else {
            self.try_mpv_prev(deck)
        };
        if navigated {
            if let Some(channel) = self.mixer.get_channel_mut(ch_idx) {
                channel.time_pos = 0.0;
                channel.duration = 0.0;
            }
            return;
        }

        if next {
            self.navigate_queue_next(deck);
        } else {
            self.navigate_queue_prev(deck);
        }
    }

    fn prioritize_selected_route_timeline(&mut self) {
        let SelectionFocus::Channel(ch_idx) = self.mixer.focus else {
            return;
        };

        let has_capture = self
            .audio_engine
            .as_ref()
            .map(|engine| engine.has_capture(ch_idx))
            .unwrap_or(false);
        if !has_capture {
            return;
        }

        let (latest, update_count, disconnected) = {
            let Some(worker) = self.ensure_route_timeline_worker(ch_idx) else {
                return;
            };
            let mut update_count = 0u32;
            let mut disconnected = false;
            loop {
                match worker.rx.try_recv() {
                    Ok(snapshot) => {
                        update_count = update_count.saturating_add(1);
                        worker.latest = snapshot;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            (worker.latest, update_count, disconnected)
        };

        if disconnected {
            self.route_timeline_workers[ch_idx] = None;
            let _ = self.ensure_route_timeline_worker(ch_idx);
            return;
        }

        self.perf_trace.timeline_updates = self
            .perf_trace
            .timeline_updates
            .saturating_add(update_count);

        if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
            if let Some(dur) = latest.duration {
                ch.duration = dur.max(0.0);
                self.route_duration_last_ms[ch_idx] = self.elapsed_ms;
            }

            let rate = self.route_speed_cache[ch_idx].clamp(0.1, 4.0);
            ch.playback_speed = rate;

            let age = if latest.generated_ms > 0 {
                self.elapsed_ms.saturating_sub(latest.generated_ms) as u32
            } else {
                0
            };
            ch.timeline_age_ms = age;
            self.perf_trace.timeline_age_max_ms = self.perf_trace.timeline_age_max_ms.max(age);
        }
    }

    fn route_socket_candidates_for_fifo(fifo: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        out.push(fifo.with_extension("sock"));
        if fifo == std::path::Path::new(TM_FIFO) {
            out.push(PathBuf::from(TM_SOCKET));
        }
        out
    }

    fn route_meta_candidates_for_fifo(fifo: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        out.push(fifo.with_extension("json"));
        if let Some(stem) = fifo.file_stem().and_then(|s| s.to_str()) {
            out.push(fifo.with_file_name(format!("{}-meta.json", stem)));
        }
        if fifo == std::path::Path::new(TM_FIFO) {
            out.push(PathBuf::from(TM_META));
        }
        out
    }

    fn is_termixer_socket(path: &std::path::Path) -> bool {
        let p = path.to_string_lossy();
        p == TM_SOCKET || (p.starts_with("/tmp/termixer-") && p.ends_with(".sock"))
    }

    fn find_alternative_route_socket(stale_path: &str) -> Option<PathBuf> {
        let stale = std::path::Path::new(stale_path);
        let parent = stale.parent()?;
        let pattern = format!("{}/*.sock", parent.display());
        if let Ok(paths) = glob::glob(&pattern) {
            for entry in paths.flatten() {
                if entry != stale && Self::is_termixer_socket(&entry) {
                    return Some(entry);
                }
            }
        }
        None
    }

    fn find_termixer_fifo() -> Option<PathBuf> {
        let canonical = PathBuf::from(TM_FIFO);
        if let Ok(meta) = canonical.metadata() {
            use std::os::unix::fs::FileTypeExt;
            if meta.file_type().is_fifo() {
                return Some(canonical);
            }
        }

        let entries = std::fs::read_dir("/tmp").ok()?;
        for entry in entries.flatten() {
            let p = entry.path();
            let name = p.file_name().and_then(|n| n.to_str())?;
            if !(name.starts_with("termixer-") && name.ends_with(".pcm")) {
                continue;
            }
            if let Ok(meta) = p.metadata() {
                use std::os::unix::fs::FileTypeExt;
                if meta.file_type().is_fifo() {
                    return Some(p);
                }
            }
        }
        None
    }

    fn attach_fifo_capture_to_deck(
        &mut self,
        deck: Deck,
        fifo: &std::path::Path,
        label: Option<String>,
    ) -> Result<(), String> {
        let ch_idx = match deck {
            Deck::A => self.mixer.dj.deck_a_channel,
            Deck::B => self.mixer.dj.deck_b_channel,
            Deck::C => self.mixer.dj.deck_c_channel,
        };

        match deck {
            Deck::A => self.mpv_deck_a = None,
            Deck::B => self.mpv_deck_b = None,
            Deck::C => self.mpv_deck_c = None,
        }

        if let Some(ref engine) = self.audio_engine {
            engine.stop_decoder(ch_idx);
            engine.attach_capture(ch_idx, fifo).map_err(|e| {
                eprintln!("Audio: attach_capture failed ({}): {}", fifo.display(), e);
                e
            })?;
            if deck == Deck::C {
                self.mixer.cue_channel.connected = true;
                self.mixer.cue_channel.playing = true;
                self.mixer.cue_channel.uses_supercollider = false;
                self.mixer.cue_channel.source_id = Some(fifo.to_string_lossy().to_string());
                if let Some(name) = label {
                    self.mixer.cue_channel.name = name;
                }
                if self.mixer.cue_channel.fader < 0.01 {
                    self.mixer.cue_channel.fader = 0.5;
                }
            } else if let Some(channel) = self.mixer.channels.get_mut(ch_idx) {
                channel.connected = true;
                channel.playing = true;
                channel.uses_supercollider = false;
                channel.source_id = Some(fifo.to_string_lossy().to_string());
                if let Some(name) = label {
                    channel.name = name;
                }
                if channel.fader < 0.01 {
                    channel.fader = 0.5;
                }
            }
            self.sync_volume_to_mpv(ch_idx);
            self.route_meta_poll_counter = 0;
            Ok(())
        } else {
            Err("no audio engine".to_string())
        }
    }

    pub fn new(num_channels: usize) -> Self {
        let mut mixer = MixerState::new(num_channels);

        // Set up channel names
        for (i, channel) in mixer.channels.iter_mut().enumerate() {
            channel.name = format!("INPUT {}", i + 1);
        }

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Default samples directory: SuperCollider Dirt-Samples
        let default_samples_dir = std::env::var("HOME")
            .map(|home| {
                let base = PathBuf::from(&home);
                if cfg!(target_os = "linux") {
                    base.join(".local/share/SuperCollider/downloaded-quarks/Dirt-Samples")
                } else {
                    base.join(
                        "Library/Application Support/SuperCollider/downloaded-quarks/Dirt-Samples",
                    )
                }
            })
            .unwrap_or_else(|_| cwd.clone());

        // Default session directory: ~/Documents
        let default_session_dir = std::env::var("HOME")
            .map(|home| PathBuf::from(home).join("Documents"))
            .unwrap_or_else(|_| cwd.clone());

        // Initialize sample engine for instant playback
        let sample_engine = SampleEngine::new().ok();

        Self {
            mixer,
            sample_pads: SamplePadGrid::new(),
            audio_manager: AudioSourceManager::new(),
            mode: AppMode::PaneSelect,
            selected_pane: SelectedPane::DeckA,
            should_quit: false,
            last_tick: Instant::now(),
            tick_rate: Duration::from_millis(20), // 50 FPS — sufficient for meter updates and control
            drag_start_y: None,
            drag_start_x: None,
            drag_start_value: None,
            channel_areas: Vec::new(),
            pad_areas: Vec::new(),
            crossfader_area: None,
            master_area: None,
            cue_area: None,
            loops_area: None,
            source_picker: SourcePickerState::new(),
            music_dir: cwd,
            samples_dir: default_samples_dir,
            selected_pad_idx: None,
            mpv_deck_a: None,
            mpv_deck_b: None,
            mpv_deck_c: None,
            deck_queues: [Vec::new(), Vec::new(), Vec::new()],
            deck_queue_indices: [0; 3],
            route_cmd_workers: [None, None, None],
            route_seek_workers: [None, None, None],
            route_timeline_workers: [None, None, None],
            route_playlist_nav_workers: [None, None, None],
            route_nav_cache: [None, None, None],
            route_nav_last_ms: [0; 3],
            route_meta_last_ms: [0; 3],
            route_scrub_last_ms: [0; 3],
            route_duration_last_ms: [0; 3],
            route_seek_last_ms: [0; 3],
            route_seek_input_last_ms: [0; 3],
            route_seek_send_last_ms: [0; 3],
            route_seek_target_pos: [0.0; 3],
            route_seek_pending: [false; 3],
            route_seek_pending_since_ms: [0; 3],
            scrub_input_last_ms: [0; 3],
            scrub_hold_start_ms: [0; 3],
            scrub_fine_last_ms: [0; 3],
            scrub_fine_last_dir: [0; 3],
            scrub_pending_return_ms: [0; 3],
            scrub_pending_return_delta: [0.0; 3],
            route_scrub_lock_until_ms: [0; 3],
            route_last_time_pos: [0.0; 3],
            last_scrub_tick_ms: 0,
            route_last_seek_delta_ms: [0; 3],
            route_last_cmd_sent_ms: [0; 3],
            route_last_track_sig: [0; 3],
            route_speed_cache: [1.0; 3],
            route_prev_cache: [false; 3],
            route_next_cache: [false; 3],
            perf_trace: PerfTrace::default(),
            sc_deck_a: None,
            sc_deck_b: None,
            sc_deck_c: None,
            sample_engine,
            audio_engine: None,
            sequence_state: SequenceState::new(),
            frame_counter: 0,
            elapsed_ms: 0,
            boot_instant: Instant::now(),
            terminal_height: 24,
            term_width: 0,
            mixer_window_start: 0,
            mixer_window_end: 4,
            master_output: AudioOutput::new(),
            cue_output: AudioOutput::new(),
            selected_master_output_idx: 0,
            selected_cue_output_idx: 0,
            mic_input_devices: Vec::new(),
            selected_mic_input_idx: 0,
            output_picker_active: false,
            output_picker_target: OutputPickerTarget::Master,
            debug_log: VecDeque::new(),
            debug_scroll: 0,
            confirm_selected: false,
            config_diffs: Vec::new(),
            config_check_msg: None,
            help_scroll: 0,
            mpv_poll_counter: 0,
            source_refresh_counter: 0,
            tidal_bpm_poll_counter: 0,
            route_meta_poll_counter: 0,
            last_volume_push_ms: [0; 3],
            consecutive_poll_failures: [0; 3],
            pending_bpm: Arc::new(Mutex::new(Vec::new())),
            last_detected_keys: [None, None, None],
            engine_was_playing: [false; 3],
            session_picker: SessionPickerState::new(),
            sessions_dir: default_session_dir,
        }
    }

    pub fn set_music_dir(&mut self, dir: PathBuf) {
        self.music_dir = dir;
    }

    pub fn set_samples_dir(&mut self, dir: PathBuf) {
        self.samples_dir = dir;
    }

    pub fn set_session_dir(&mut self, dir: PathBuf) {
        self.sessions_dir = dir;
    }

    /// Configure audio sources from socket paths
    pub fn configure_sources(&mut self, sources: Vec<(String, String)>) {
        // First pass: add all sources to manager
        for (name, socket_path) in &sources {
            let source = AudioSource::new(name.clone(), socket_path.clone());
            self.audio_manager.add_source(source);
        }

        // Second pass: connect to MPV and load files into engine decoders.
        //
        // CRITICAL: connecting to MPV's IPC socket causes it to reinit
        // audio, which zeroes ao=pcm output. FIFO capture scan runs
        // FIRST, before any socket connection. If a FIFO is found, we
        // skip IPC entirely for that deck.
        let mut loaded_channels = Vec::new();

        // Pre-scan: check for a FIFO capture source (from mpv-mixer shell fn).
        let fifo_attached = if self.audio_engine.is_some() {
            let fifo_opt = Self::find_termixer_fifo();
            if let Some(ref fifo) = fifo_opt {
                let route_name = sources.first().map(|(name, _)| name.clone());
                if self
                    .attach_fifo_capture_to_deck(Deck::A, fifo, route_name)
                    .is_ok()
                {
                    loaded_channels.push(0);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        // Socket-based sources (IPC). Skip entirely when FIFO capture is
        // active — ANY IPC connection to MPV causes it to reinit audio
        // and zero the ao=pcm output.
        eprintln!(
            "configure_sources: fifo_attached={}, sources.len()={}",
            fifo_attached,
            sources.len()
        );
        if !fifo_attached {
            for (i, (name, socket_path)) in sources.iter().enumerate() {
                if let Some(channel) = self.mixer.channels.get_mut(i) {
                    channel.name = name.clone();
                }
                if i >= 3 {
                    continue;
                }

                let mut client = crate::audio::MpvClient::new(socket_path);
                if client.connect().is_err() {
                    continue;
                }

                let file_path = client.get_path().ok().map(PathBuf::from);
                if let Some(ref path) = file_path {
                    if let Some(ref engine) = self.audio_engine {
                        let path_str = path.to_string_lossy().to_string();
                        engine.load_file(i, path_str);
                        loaded_channels.push(i);
                    }
                } else {
                    eprintln!("Audio: get_path failed for {}", name);
                }

                let _ = client.ensure_astats();
                client.start_metering();

                if let Some(channel) = self.mixer.channels.get_mut(i) {
                    channel.connected = true;
                    channel.source_id = Some(socket_path.clone());
                    channel.uses_supercollider = false;
                    channel.base_bpm = 0.0;

                    if let Some(key) = client.get_key_from_metadata() {
                        channel.key = Some(key);
                        channel.key_offset = 0;
                    }
                }

                if let Some(path) = file_path
                    && path.exists()
                {
                    let pending = self.pending_bpm.clone();
                    let channel_idx = i;
                    let on_result = Arc::new(Mutex::new(
                        move |result: Result<crate::audio::BpmResult, String>| match result {
                            Ok(r) => {
                                if let Ok(mut queue) = pending.lock() {
                                    queue.push((channel_idx, r.bpm, r.key));
                                }
                            }
                            Err(e) => {
                                if let Ok(mut queue) = pending.lock() {
                                    queue.push((usize::MAX, 0.0, Some(e)));
                                }
                            }
                        },
                    ));
                    BpmAnalyzer::analyze_file(&path, on_result);
                }

                match i {
                    0 => self.mpv_deck_a = Some(client),
                    1 => self.mpv_deck_b = Some(client),
                    2 => self.mpv_deck_c = Some(client),
                    _ => {}
                }
            }
        }
        // Sync crossfader/volume state to engine for all loaded channels
        for ch in &loaded_channels {
            self.sync_volume_to_mpv(*ch);
        }

        // Ensure fader is at unity gain (+0 dB) for immediate sound
        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;
        if let Some(ch) = self.mixer.channels.get_mut(deck_a_ch) {
            ch.fader = 0.5; // Unity gain (+0 dB)
        }
        if let Some(ch) = self.mixer.channels.get_mut(deck_b_ch) {
            ch.fader = 0.5; // Unity gain (+0 dB)
        }
        // Push state to engine
        for ch in &loaded_channels {
            self.sync_volume_to_mpv(*ch);
        }
    }

    /// Main tick - update meters, etc.
    pub fn tick(&mut self) {
        self.elapsed_ms = self.boot_instant.elapsed().as_millis() as u64;

        // Advance MPV filter smoother — sends any pending af-command
        // updates in small stepped increments to avoid ffmpeg biquad
        // transients (main source of cutoff crackle on MPV sources).
        for client in [
            self.mpv_deck_a.as_mut(),
            self.mpv_deck_b.as_mut(),
            self.mpv_deck_c.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            client.tick_smooth_filters();
        }

        // Poll per-deck audio levels from MPV astats filters
        let mut real_channels = Vec::new();
        for (i, client_opt) in [
            self.mpv_deck_a.as_mut(),
            self.mpv_deck_b.as_mut(),
            self.mpv_deck_c.as_mut(),
        ]
        .iter_mut()
        .enumerate()
        {
            if let Some(client) = client_opt {
                let (peak_l, peak_r, rms_l, rms_r) = client.get_audio_levels();
                if peak_l > 0.0 || peak_r > 0.0 {
                    real_channels.push((i, peak_l, peak_r, rms_l, rms_r));
                }
            }
        }

        // Also read meters from Rust audio engine (preferred when available)
        if let Some(ref engine) = self.audio_engine {
            for i in 0..3 {
                let (pl, pr, rl, rr) = engine.meters[i].load();
                if pl > 0.0 || pr > 0.0 {
                    real_channels.push((i, pl, pr, rl, rr));
                }
            }
            // Poll LFO debug from audio callback
            let lfo_line: Option<String> = engine
                .lfo_debug
                .lock()
                .ok()
                .map(|mut s| {
                    if s.is_empty() {
                        None
                    } else {
                        Some(std::mem::take(&mut *s))
                    }
                })
                .unwrap_or(None);
            if let Some(line) = lfo_line {
                self.log_debug(line);
            }
        }

        self.mixer.update_meters(&real_channels);
        self.sample_pads.update();

        // Poll MPV state every 250ms for bidirectional sync
        self.mpv_poll_counter = self.mpv_poll_counter.wrapping_add(1);
        if self.mpv_poll_counter % 25 == 0 {
            self.poll_mpv_state();
            self.poll_engine_positions();
        }

        self.tidal_bpm_poll_counter = self.tidal_bpm_poll_counter.wrapping_add(1);
        if self.tidal_bpm_poll_counter % 100 == 0 {
            self.poll_tidal_bpm();
        }

        self.route_meta_poll_counter = self.route_meta_poll_counter.wrapping_add(1);
        if self.route_meta_poll_counter % 2 == 0 {
            self.poll_route_bpm_key();
        }

        // Refresh source picker every 500ms when add-to-queue sub-view is open on MPV Sockets tab
        if matches!(self.mode, AppMode::SourcePicker(_))
            && self.source_picker.adding_to_queue
            && self.source_picker.add_tab == AddSourceTab::MpvSockets
        {
            self.source_refresh_counter = self.source_refresh_counter.wrapping_add(1);
            if self.source_refresh_counter % 50 == 0 {
                // Save current selection to restore after refresh
                let prev_path = self
                    .source_picker
                    .add_filtered
                    .get(self.source_picker.add_selected)
                    .and_then(|&idx| self.source_picker.add_items.get(idx))
                    .map(|item| item.path.clone());

                self.scan_add_sources();

                // Restore selection if the previously selected item still exists
                if let Some(path) = prev_path
                    && let Some(new_idx) = self
                        .source_picker
                        .add_items
                        .iter()
                        .position(|item| item.path == path)
                {
                    self.source_picker.add_filter();
                    if let Some(filtered_idx) = self
                        .source_picker
                        .add_filtered
                        .iter()
                        .position(|&idx| idx == new_idx)
                    {
                        self.source_picker.add_selected = filtered_idx;
                    }
                }
            }
        }

        // Scrub: tick accumulation, decay speed, and poll positions
        self.tick_scrub();
        self.decay_scrub_speed();
        self.flush_route_timeline_updates();
        self.prioritize_selected_route_timeline();
        self.poll_scrub_positions();

        // Re-apply route metadata after scrub position updates so duration
        // from the Lua metadata file is not clobbered by the worker's stale
        // duration_cache.
        self.refresh_route_title();

        // Apply any pending BPM results from background analysis
        self.apply_pending_bpm();

        // Read real-time onset-detected BPM from MPV metering thread
        self.poll_onset_bpm();

        // Advance LFO phase and count sync ticks for all channels
        for ch in &mut self.mixer.channels {
            if ch.lfo_speed > 0.001 {
                // Start at peak when LFO activates from idle
                if ch.prev_lfo_speed <= 0.001 {
                    ch.lfo_phase = 0.25;
                }
                let freq_hz = 0.05 + ch.lfo_speed.powf(3.0) * 29.95;
                ch.lfo_phase = (ch.lfo_phase + freq_hz / 20.0) % 1.0;
            } else {
                ch.lfo_phase = 0.0;
            }
            ch.prev_lfo_speed = ch.lfo_speed;
            ch.lfo_sync_tick = ch.lfo_sync_tick.wrapping_add(1);
        }
        // Sync filters every tick — af-command updates are cheap (no graph rebuild)
        for ch_idx in 0..self.mixer.channels.len() {
            self.sync_filter_to_mpv(ch_idx);
        }
    }

    /// Poll each connected MPV deck for state changes and sync back to mixer.
    /// Reads play/pause and volume. Volume readback reverses the full formula
    /// (fader * gain * master * 2.0 * 200.0) to extract the per-channel fader.
    /// Skips volume readback for 1s after TUI pushes a change to avoid race conditions.
    /// Clears deck if socket disappears or track ends.
    fn poll_mpv_state(&mut self) {
        let deck_a_channel = self.mixer.dj.deck_a_channel;
        let deck_b_channel = self.mixer.dj.deck_b_channel;
        let (gain_a, gain_b) = self.calculate_crossfader_gains();
        let master = self.mixer.master.fader;
        let now = self.elapsed_ms;
        let cooldown_ms = 1000;

        // Skip volume readback while solo is active — MPV volumes are muted
        // by solo logic and don't reflect true fader positions.
        let solo_active = self.mixer.solo_active;

        // Collect poll results to avoid borrow conflicts with clear_deck()
        struct PollResult {
            deck: Deck,
            pause_ok: bool,
            playing: bool,
            volume_ok: bool,
            fader: f32,
            time_pos: Option<f32>,
            duration: Option<f32>,
            has_prev_track: Option<bool>,
            has_next_track: Option<bool>,
        }

        let mut results: Vec<PollResult> = Vec::new();

        // Read deck A
        if let Some(ref mut client) = self.mpv_deck_a {
            let mut pause_ok = false;
            let mut playing = false;
            let mut volume_ok = false;
            let mut fader = 0.0;
            if let Ok(paused) = client.get_pause() {
                playing = !paused;
                pause_ok = true;
            }
            if !solo_active
                && now.saturating_sub(self.last_volume_push_ms[0]) >= cooldown_ms
                && let Ok(vol) = client.get_volume()
            {
                let divisor = gain_a * master * 2.0 * 200.0;
                if divisor > 0.0 {
                    fader = (vol / divisor).clamp(0.0, 1.0);
                    volume_ok = true;
                }
            }
            let time_pos = client.get_time_pos().ok();
            let duration = client.get_duration().ok();
            let (has_prev_track, has_next_track) = client
                .get_playlist_nav_available()
                .ok()
                .map(|(p, n)| (Some(p), Some(n)))
                .unwrap_or((None, None));
            results.push(PollResult {
                deck: Deck::A,
                pause_ok,
                playing,
                volume_ok,
                fader,
                time_pos,
                duration,
                has_prev_track,
                has_next_track,
            });
        }

        // Read deck B
        if let Some(ref mut client) = self.mpv_deck_b {
            let mut pause_ok = false;
            let mut playing = false;
            let mut volume_ok = false;
            let mut fader = 0.0;
            if let Ok(paused) = client.get_pause() {
                playing = !paused;
                pause_ok = true;
            }
            if !solo_active
                && now.saturating_sub(self.last_volume_push_ms[1]) >= cooldown_ms
                && let Ok(vol) = client.get_volume()
            {
                let divisor = gain_b * master * 2.0 * 200.0;
                if divisor > 0.0 {
                    fader = (vol / divisor).clamp(0.0, 1.0);
                    volume_ok = true;
                }
            }
            let time_pos = client.get_time_pos().ok();
            let duration = client.get_duration().ok();
            let (has_prev_track, has_next_track) = client
                .get_playlist_nav_available()
                .ok()
                .map(|(p, n)| (Some(p), Some(n)))
                .unwrap_or((None, None));
            results.push(PollResult {
                deck: Deck::B,
                pause_ok,
                playing,
                volume_ok,
                fader,
                time_pos,
                duration,
                has_prev_track,
                has_next_track,
            });
        }

        // Read CUE (no crossfader, gain=1.0)
        if let Some(ref mut client) = self.mpv_deck_c {
            let mut pause_ok = false;
            let mut playing = false;
            let mut volume_ok = false;
            let mut fader = 0.0;
            if let Ok(paused) = client.get_pause() {
                playing = !paused;
                pause_ok = true;
            }
            if !solo_active
                && now.saturating_sub(self.last_volume_push_ms[2]) >= cooldown_ms
                && let Ok(vol) = client.get_volume()
            {
                let divisor = master * 2.0 * 200.0;
                if divisor > 0.0 {
                    fader = (vol / divisor).clamp(0.0, 1.0);
                    volume_ok = true;
                }
            }
            let time_pos = client.get_time_pos().ok();
            let duration = client.get_duration().ok();
            let (has_prev_track, has_next_track) = client
                .get_playlist_nav_available()
                .ok()
                .map(|(p, n)| (Some(p), Some(n)))
                .unwrap_or((None, None));
            results.push(PollResult {
                deck: Deck::C,
                pause_ok,
                playing,
                volume_ok,
                fader,
                time_pos,
                duration,
                has_prev_track,
                has_next_track,
            });
        }

        // Apply results and detect failures / track end
        let mut decks_to_clear: Vec<Deck> = Vec::new();

        self.refresh_deck_titles();
        self.refresh_route_title();

        for result in &results {
            let deck_idx = match result.deck {
                Deck::A => 0,
                Deck::B => 1,
                Deck::C => 2,
            };

            // Socket failure detection: if get_pause() failed, the socket is dead
            if !result.pause_ok {
                self.consecutive_poll_failures[deck_idx] += 1;
                if self.consecutive_poll_failures[deck_idx] >= 2 {
                    decks_to_clear.push(result.deck);
                }
                continue;
            }

            // Reset failure counter on success
            self.consecutive_poll_failures[deck_idx] = 0;

            // Apply volume/fader state
            let ch_idx = match result.deck {
                Deck::A => deck_a_channel,
                Deck::B => deck_b_channel,
                Deck::C => self.mixer.dj.deck_c_channel,
            };
            let (queue_prev, queue_next) = queue_nav_available(
                self.deck_queue_indices[deck_idx],
                self.deck_queues[deck_idx].len(),
            );

            if result.deck == Deck::C {
                let deck_c_ch = self.mixer.dj.deck_c_channel;
                let decoder_owned = self
                    .audio_engine
                    .as_ref()
                    .map(|engine| {
                        engine.has_decoder(deck_c_ch) && !engine.has_capture(deck_c_ch)
                    })
                    .unwrap_or(false);
                if !decoder_owned {
                    self.mixer.cue_channel.playing = result.playing;
                }
                if result.volume_ok {
                    self.mixer.cue_channel.fader = result.fader;
                }
                if let Some(tp) = result.time_pos {
                    self.mixer.cue_channel.time_pos = tp;
                }
                if let Some(dur) = result.duration {
                    self.mixer.cue_channel.duration = dur;
                }
                if let Some(has_prev) = result.has_prev_track {
                    self.mixer.cue_channel.has_prev_track = has_prev || queue_prev;
                }
                if let Some(has_next) = result.has_next_track {
                    self.mixer.cue_channel.has_next_track = has_next || queue_next;
                }
            } else if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
                let has_capture = self
                    .audio_engine
                    .as_ref()
                    .map(|engine| engine.has_capture(ch_idx))
                    .unwrap_or(false);
                let decoder_owned = self
                    .audio_engine
                    .as_ref()
                    .map(|engine| engine.has_decoder(ch_idx) && !engine.has_capture(ch_idx))
                    .unwrap_or(false);

                if !decoder_owned {
                    ch.playing = result.playing;
                }
                if result.volume_ok && !ch.muted {
                    ch.fader = result.fader;
                }
                if !has_capture {
                    if let Some(tp) = result.time_pos {
                        ch.time_pos = tp;
                    }
                    if let Some(dur) = result.duration {
                        ch.duration = dur;
                    }
                }
                if let Some(has_prev) = result.has_prev_track {
                    ch.has_prev_track = has_prev || queue_prev;
                }
                if let Some(has_next) = result.has_next_track {
                    ch.has_next_track = has_next || queue_next;
                }

                // Track-end detection: playback stopped and position is at/near end
                // Only clear if we have a known duration and are very close to it.
                // This avoids false positives from playlists (MPV advances to next track).
                if !result.playing
                    && ch.duration > 0.0
                    && ch.time_pos >= ch.duration - 1.0
                    && ch.connected
                    && !has_capture
                {
                    decks_to_clear.push(result.deck);
                }
            }
        }

        // Clear dead/ended decks (outside borrow scope)
        for deck in decks_to_clear {
            self.clear_deck(deck);
        }
    }

    /// Sync time_pos and duration from the Rust audio engine into mixer channels.
    /// This covers files loaded directly via engine.load_file() (no MPV client).
    fn poll_engine_positions(&mut self) {
        let engine = match self.audio_engine.as_ref() {
            Some(e) => e,
            None => return,
        };
        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;
        let deck_c_ch = self.mixer.dj.deck_c_channel;

        // Deck A
        if self.mpv_deck_a.is_none() {
            let pos = engine.time_pos[0].load() as f32;
            let dur = engine.duration[0].load() as f32;
            if let Some(ch) = self.mixer.get_channel_mut(deck_a_ch)
                && (pos > 0.0 || dur > 0.0)
            {
                ch.time_pos = pos;
                ch.duration = dur;
            }
        }
        // Deck B
        if self.mpv_deck_b.is_none() {
            let pos = engine.time_pos[1].load() as f32;
            let dur = engine.duration[1].load() as f32;
            if let Some(ch) = self.mixer.get_channel_mut(deck_b_ch)
                && (pos > 0.0 || dur > 0.0)
            {
                ch.time_pos = pos;
                ch.duration = dur;
            }
        }
        // Deck C
        if self.mpv_deck_c.is_none() {
            let pos = engine.time_pos[2].load() as f32;
            let dur = engine.duration[2].load() as f32;
            if pos > 0.0 || dur > 0.0 {
                self.mixer.cue_channel.time_pos = pos;
                self.mixer.cue_channel.duration = dur;
            }
        }

        // Track-end detection for engine-backed decks: when the decoder's ring
        // buffer drains empty and time_pos has reached duration, the track is done.
        // Collect data first to avoid borrow conflicts with self.clear_deck().
        let deck_indices = [
            (Deck::A, deck_a_ch),
            (Deck::B, deck_b_ch),
            (Deck::C, deck_c_ch),
        ];
        let mut finished_decks: Vec<(Deck, bool)> = Vec::new(); // (deck, has_next)
        for (deck, ch_idx) in deck_indices {
            let deck_idx = deck.index();
            let has_mpv = match deck {
                Deck::A => self.mpv_deck_a.is_some(),
                Deck::B => self.mpv_deck_b.is_some(),
                Deck::C => self.mpv_deck_c.is_some(),
            };
            let has_dec = engine.has_decoder(ch_idx);
            let pos = engine.time_pos[deck_idx].load() as f32;
            let dur = engine.duration[deck_idx].load() as f32;
            let was_playing = self.engine_was_playing[deck_idx];
            let buffer_empty = !engine.buffer_has_data(deck_idx);
            let is_eof = engine.decoder_is_eof(ch_idx);
            // Check if there's a next item in the queue
            let current_queue_idx = self.deck_queue_indices[deck_idx];
            let has_next = current_queue_idx + 1 < self.deck_queues[deck_idx].len();

            if has_mpv || !has_dec {
                self.engine_was_playing[deck_idx] = false;
                continue;
            }
            // Track has finished: decoder hit EOF and ring buffer drained,
            // or position reached reported duration and buffer drained
            if was_playing && (is_eof || (dur > 0.0 && pos >= dur - 0.5)) && buffer_empty {
                self.engine_was_playing[deck_idx] = false;
                finished_decks.push((deck, has_next));
            } else if pos > 0.0 {
                self.engine_was_playing[deck_idx] = true;
            }
        }

        // Act on finished tracks (outside engine borrow scope)
        for (deck, has_next) in finished_decks {
            if has_next {
                self.advance_to_next_track(deck);
            } else {
                self.clear_deck(deck);
            }
        }
    }

    /// Load the next track from a deck's directory playlist.
    fn advance_to_next_track(&mut self, deck: Deck) {
        let deck_idx = deck.index();
        let current_idx = self.deck_queue_indices[deck_idx];
        let next_idx = current_idx + 1;

        if next_idx >= self.deck_queues[deck_idx].len() {
            return; // No next item in queue
        }

        self.deck_queue_indices[deck_idx] = next_idx;
        let item = self.deck_queues[deck_idx][next_idx].clone();
        self.load_queue_item(deck, &item);
    }

    /// Refresh deck labels from MPV media titles.
    /// This keeps route-mode sockets (`/tmp/termixer.sock`) showing the
    /// active track title instead of a generic source name.
    fn refresh_deck_titles(&mut self) {
        let try_update =
            |client_opt: &mut Option<MpvClient>,
             channel_opt: Option<&mut crate::state::MixerChannel>| {
                if let (Some(client), Some(channel)) = (client_opt.as_mut(), channel_opt)
                    && let Some(title) = client.get_media_title()
                    && channel.name != title
                {
                    channel.name = title;
                }
            };

        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;

        try_update(&mut self.mpv_deck_a, self.mixer.get_channel_mut(deck_a_ch));
        try_update(&mut self.mpv_deck_b, self.mixer.get_channel_mut(deck_b_ch));

        if let Some(client) = self.mpv_deck_c.as_mut()
            && let Some(title) = client.get_media_title()
            && self.mixer.cue_channel.name != title
        {
            self.mixer.cue_channel.name = title;
        }
    }

    /// Refresh route-mode label from metadata file emitted by mpv Lua.
    /// Used when audio is attached through FIFO and we intentionally avoid
    /// IPC connections to keep mpv `ao=pcm` stable.
    fn refresh_route_title(&mut self) {
        // Apply route metadata/nav for every connected capture-backed deck,
        // including Deck C (stored separately as cue_channel).
        let deck_indices = [
            self.mixer.dj.deck_a_channel,
            self.mixer.dj.deck_b_channel,
            self.mixer.dj.deck_c_channel,
        ];
        let selected_channel = match self.mixer.focus {
            SelectionFocus::Channel(idx) => Some(idx),
            _ => None,
        };

        for ch_idx in deck_indices {
            let connected = self
                .mixer
                .get_channel(ch_idx)
                .map(|c| c.connected)
                .unwrap_or(false);
            if !connected {
                continue;
            }
            let has_capture = self
                .audio_engine
                .as_ref()
                .map(|e| e.has_capture(ch_idx))
                .unwrap_or(false);
            if !has_capture {
                continue;
            }

            let source_id = self
                .mixer
                .get_channel(ch_idx)
                .and_then(|c| c.source_id.clone());
            let Some(source_id) = source_id else {
                continue;
            };
            let source_path = Some(std::path::Path::new(source_id.as_str()));
            // Avoid polling playlist nav over IPC for every capture deck on every
            // refresh. In route mode this can churn sockets and produce noisy MPV
            // broken-pipe logs. Poll only for the actively selected deck, and
            // prefer metadata-derived nav when available.
            let route_nav = if selected_channel == Some(ch_idx) {
                self.route_playlist_nav(ch_idx)
            } else {
                None
            };

            let mut should_sync_speed = false;
            let mut meta_duration: Option<f32> = None;
            if let Some(meta_path) = Self::resolve_route_meta_path(source_path)
                && let Ok(raw) = std::fs::read_to_string(meta_path)
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw)
                && let Some(obj) = data.as_object()
            {
                let track_sig = Self::route_track_signature(obj);
                if track_sig != 0 {
                    let prev_sig = self.route_last_track_sig[ch_idx];
                    let track_changed = prev_sig != 0 && prev_sig != track_sig;
                    self.route_last_track_sig[ch_idx] = track_sig;
                    if track_changed {
                        self.reset_route_track_timeline_state(ch_idx);
                        if let Some(dur) = obj.get("duration").and_then(|v| v.as_f64()) {
                            meta_duration = Some(dur as f32);
                        }
                    }
                }
            }

            let deck_idx = if ch_idx == self.mixer.dj.deck_a_channel {
                Some(Deck::A.index())
            } else if ch_idx == self.mixer.dj.deck_b_channel {
                Some(Deck::B.index())
            } else if ch_idx == self.mixer.dj.deck_c_channel {
                Some(Deck::C.index())
            } else {
                None
            };
            let queue_nav = deck_idx.map(|deck_idx| {
                queue_nav_available(
                    self.deck_queue_indices[deck_idx],
                    self.deck_queues[deck_idx].len(),
                )
            });

            let Some(channel) = self.mixer.get_channel_mut(ch_idx) else {
                continue;
            };

            if let Some(label) = Self::route_meta_label(source_path)
                && channel.name != label
            {
                channel.name = label;
            }

            if let Some(dur) = meta_duration {
                channel.duration = dur.max(0.0);
            }

            let mut nav_from_meta = false;
            if let Some(meta_path) = Self::resolve_route_meta_path(source_path)
                && let Ok(raw) = std::fs::read_to_string(meta_path)
                && let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw)
                && let Some(obj) = data.as_object()
            {
                let pick_num = |keys: &[&str]| -> Option<f32> {
                    keys.iter().find_map(|k| {
                        let v = obj.get(*k)?;
                        if let Some(n) = v.as_f64() {
                            Some(n as f32)
                        } else if let Some(s) = v.as_str() {
                            s.trim().parse::<f32>().ok()
                        } else {
                            None
                        }
                    })
                };

                let pick_text = |keys: &[&str]| -> Option<String> {
                    keys.iter()
                        .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned)
                };

                if let Some(raw_key) = pick_text(&[
                    "key",
                    "initialkey",
                    "initial_key",
                    "camelot",
                    "camelot_key",
                    "tkey",
                    "KEY",
                    "INITIALKEY",
                    "TKEY",
                ]) {
                    let parsed = parse_camelot(&raw_key)
                        .map(|(pc, is_major)| pitch_class_to_camelot(pc, is_major))
                        .or_else(|| parse_key_name(&raw_key));
                    if let Some(camelot) = parsed {
                        channel.key = Some(camelot);
                    }
                }

                if let Some(mut bpm) =
                    pick_num(&["bpm", "tempo", "initial_bpm", "initial-bpm", "BPM", "TBPM"])
                {
                    while bpm > 400.0 {
                        bpm *= 0.5;
                    }
                    while bpm > 0.0 && bpm < 40.0 {
                        bpm *= 2.0;
                    }
                    if (10.0..=400.0).contains(&bpm) {
                        channel.bpm = Some(bpm);
                        if channel.base_bpm <= 0.0 {
                            channel.base_bpm = bpm;
                            channel.target_bpm = bpm;
                            should_sync_speed = true;
                        }
                    }
                }

                let pick_int = |keys: &[&str]| -> Option<i64> {
                    keys.iter().find_map(|k| {
                        let v = obj.get(*k)?;
                        if let Some(n) = v.as_i64() {
                            Some(n)
                        } else if let Some(n) = v.as_u64() {
                            Some(n as i64)
                        } else if let Some(s) = v.as_str() {
                            s.trim().parse::<i64>().ok()
                        } else {
                            None
                        }
                    })
                };

                if let (Some(pos), Some(count)) = (
                    pick_int(&["playlist_pos", "playlist-pos"]),
                    pick_int(&["playlist_count", "playlist-count"]),
                ) {
                    let has_prev = pos > 0;
                    let has_next = count > 0 && pos >= 0 && pos < count - 1;
                    channel.has_prev_track = has_prev;
                    channel.has_next_track = has_next;
                    self.route_prev_cache[ch_idx] = has_prev;
                    self.route_next_cache[ch_idx] = has_next;
                    self.route_nav_cache[ch_idx] = Some((has_prev, has_next));
                    self.route_nav_last_ms[ch_idx] = self.elapsed_ms;
                    nav_from_meta = true;
                }
            }

            if !nav_from_meta && let Some((has_prev, has_next)) = route_nav {
                channel.has_prev_track = has_prev;
                channel.has_next_track = has_next;
                self.route_prev_cache[ch_idx] = has_prev;
                self.route_next_cache[ch_idx] = has_next;
            }

            if let Some((queue_prev, queue_next)) = queue_nav {
                channel.has_prev_track |= queue_prev;
                channel.has_next_track |= queue_next;
            }

            if should_sync_speed {
                self.sync_bpm_to_mpv(ch_idx);
            }
        }
    }

    fn resolve_route_meta_path(source_path: Option<&std::path::Path>) -> Option<PathBuf> {
        if let Some(path) = source_path {
            // Direct metadata file adjacent to FIFO (e.g., /tmp/termixer-A.json)
            let direct_json = path.with_extension("json");
            if direct_json.exists() {
                return Some(direct_json);
            }
            for candidate in Self::route_meta_candidates_for_fifo(path) {
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
        if std::path::Path::new(TM_META).exists() {
            return Some(PathBuf::from(TM_META));
        }
        None
    }

    fn route_track_signature(obj: &serde_json::Map<String, serde_json::Value>) -> u64 {
        let pick_text = |keys: &[&str]| -> Option<String> {
            keys.iter()
                .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        };
        let pick_int = |keys: &[&str]| -> Option<i64> {
            keys.iter().find_map(|k| {
                let v = obj.get(*k)?;
                if let Some(n) = v.as_i64() {
                    Some(n)
                } else if let Some(n) = v.as_u64() {
                    Some(n as i64)
                } else if let Some(s) = v.as_str() {
                    s.trim().parse::<i64>().ok()
                } else {
                    None
                }
            })
        };

        let path = pick_text(&["path", "filename", "file", "file_path", "url"]);
        let title = pick_text(&["title", "media_title", "TITLE"]);
        let artist = pick_text(&["artist", "ARTIST"]);
        let album = pick_text(&["album", "ALBUM"]);
        let playlist_pos = pick_int(&["playlist_pos", "playlist-pos"]);
        let playlist_count = pick_int(&["playlist_count", "playlist-count"]);

        if path.is_none()
            && title.is_none()
            && artist.is_none()
            && album.is_none()
            && playlist_pos.is_none()
            && playlist_count.is_none()
        {
            return 0;
        }

        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        title.hash(&mut hasher);
        artist.hash(&mut hasher);
        album.hash(&mut hasher);
        playlist_pos.hash(&mut hasher);
        playlist_count.hash(&mut hasher);
        hasher.finish()
    }

    fn reset_route_track_timeline_state(&mut self, ch_idx: usize) {
        if let Some(engine) = self.audio_engine.as_ref() {
            engine.reset_capture_time_pos(ch_idx);
        }
        // The worker caches duration to avoid polling MPV continuously. Restart
        // it so playlist navigation cannot restore the previous track's value.
        self.route_timeline_workers[ch_idx] = None;
        self.route_seek_last_ms[ch_idx] = 0;
        self.route_seek_input_last_ms[ch_idx] = 0;
        self.route_seek_send_last_ms[ch_idx] = 0;
        self.route_seek_target_pos[ch_idx] = 0.0;
        self.route_seek_pending[ch_idx] = false;
        self.route_seek_pending_since_ms[ch_idx] = 0;
        self.route_scrub_last_ms[ch_idx] = self.elapsed_ms;
        self.route_duration_last_ms[ch_idx] = self.elapsed_ms;
        self.route_last_time_pos[ch_idx] = 0.0;
        self.scrub_pending_return_ms[ch_idx] = 0;
        self.scrub_pending_return_delta[ch_idx] = 0.0;
        if let Some(channel) = self.mixer.get_channel_mut(ch_idx) {
            channel.time_pos = 0.0;
            channel.duration = 0.0;
        }
    }

    fn route_meta_label(source_path: Option<&std::path::Path>) -> Option<String> {
        let meta_path = Self::resolve_route_meta_path(source_path)?;
        let raw = std::fs::read_to_string(meta_path).ok()?;
        let data = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
        let obj = data.as_object()?;

        let pick = |keys: &[&str]| -> Option<String> {
            keys.iter()
                .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
        };

        let title = pick(&["title", "media_title", "TITLE"]);
        let artist = pick(&["artist", "ARTIST"]);
        let album = pick(&["album", "ALBUM"]);

        match (title, artist, album) {
            (Some(t), Some(a), Some(al)) => Some(format!("{} - {} [{}]", t, a, al)),
            (Some(t), Some(a), None) => Some(format!("{} - {}", t, a)),
            (Some(t), None, Some(al)) => Some(format!("{} [{}]", t, al)),
            (Some(t), None, None) => Some(t),
            (None, Some(a), Some(al)) => Some(format!("{} [{}]", a, al)),
            (None, Some(a), None) => Some(a),
            (None, None, Some(al)) => Some(al),
            (None, None, None) => None,
        }
    }

    fn route_playlist_nav(&mut self, ch_idx: usize) -> Option<(bool, bool)> {
        if ch_idx >= self.route_nav_cache.len() {
            return None;
        }

        let now = self.elapsed_ms;
        if let Some(cached) = self.route_nav_cache[ch_idx]
            && now.saturating_sub(self.route_nav_last_ms[ch_idx]) < 250
        {
            return Some(cached);
        }

        let mut nav: Option<(bool, bool)> = None;
        if let Some(worker) = self.ensure_route_playlist_nav_worker(ch_idx) {
            let _ = worker.tx.send(());
            while let Ok(pair) = worker.rx.try_recv() {
                nav = Some(pair);
            }
        }

        if let Some((has_prev, has_next)) = nav {
            self.route_prev_cache[ch_idx] = has_prev;
            self.route_next_cache[ch_idx] = has_next;
        }

        if nav.is_none() {
            nav = Some((self.route_prev_cache[ch_idx], self.route_next_cache[ch_idx]));
        }

        self.route_nav_cache[ch_idx] = nav;
        self.route_nav_last_ms[ch_idx] = now;
        nav
    }

    /// Check if we're in edit mode
    pub fn is_editing(&self) -> bool {
        self.mode == AppMode::Edit
    }

    /// Handle keyboard input
    pub fn handle_key(&mut self, key: KeyEvent) {
        // Insert-mode text input takes precedence over global shortcuts.
        if matches!(self.mode, AppMode::SourcePicker(_))
            && if self.source_picker.adding_to_queue {
                self.source_picker.add_input_mode == PickerInputMode::Insert
            } else {
                self.source_picker.input_mode == PickerInputMode::Insert
            }
        {
            self.handle_source_picker_key(key);
            return;
        }

        // Global shortcuts available from any mode
        match key.code {
            // Confirm quit from anywhere
            KeyCode::Char('q') => {
                self.confirm_selected = false;
                self.mode = AppMode::ConfirmAction(ConfirmAction::Quit);
                return;
            }
            // Copy visible debug log to clipboard
            KeyCode::Char('y')
                if key.modifiers.contains(KeyModifiers::CONTROL) && !self.debug_log.is_empty() =>
            {
                self.copy_debug_log_to_clipboard();
                return;
            }
            // Debug log scrolling (only when log is non-empty)
            KeyCode::Char('[') if !self.debug_log.is_empty() => {
                if self.debug_scroll == 0 {
                    // Entering scroll mode from follow: start at current top line
                    let inner_height = 8usize;
                    self.debug_scroll = self.debug_log.len().saturating_sub(inner_height).max(1);
                } else {
                    let max_top = self.debug_log.len().saturating_sub(8);
                    self.debug_scroll = (self.debug_scroll + 1).min(max_top);
                }
                return;
            }
            KeyCode::Char(']') if !self.debug_log.is_empty() => {
                self.debug_scroll = self.debug_scroll.saturating_sub(1);
                return;
            }
            KeyCode::PageUp if !self.debug_log.is_empty() => {
                let page = 10usize;
                if self.debug_scroll == 0 {
                    let inner_height = 8usize;
                    self.debug_scroll = self
                        .debug_log
                        .len()
                        .saturating_sub(inner_height + page)
                        .max(1);
                } else {
                    let max_top = self.debug_log.len().saturating_sub(8);
                    self.debug_scroll = (self.debug_scroll + page).min(max_top);
                }
                return;
            }
            KeyCode::PageDown if !self.debug_log.is_empty() => {
                let page = 10usize;
                self.debug_scroll = self.debug_scroll.saturating_sub(page);
                return;
            }
            KeyCode::Home if !self.debug_log.is_empty() => {
                self.debug_scroll = 1;
                return;
            }
            KeyCode::End if !self.debug_log.is_empty() => {
                self.debug_scroll = 0;
                return;
            }
            // Source picker for Deck A from anywhere
            KeyCode::Char('A') => {
                self.open_source_picker(Deck::A);
                return;
            }
            // Source picker for Deck B from anywhere
            KeyCode::Char('B') => {
                self.open_source_picker(Deck::B);
                return;
            }
            // Source picker for Deck C (CUE) from anywhere
            KeyCode::Char('C') => {
                self.open_source_picker(Deck::C);
                return;
            }
            // F8 toggles master play/pause from any mode
            KeyCode::F(8) => {
                self.mixer.master.playing = !self.mixer.master.playing;
                self.sync_all_playpause();
                return;
            }
            _ => {}
        }

        // Handle recording commit (removed - no more recording)
        // Recording-related handling removed in SEQUENCES rework

        // Handle output picker navigation if active
        if self.output_picker_active {
            match key.code {
                KeyCode::Esc => {
                    self.output_picker_active = false;
                    return;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let max_idx = match self.output_picker_target {
                        OutputPickerTarget::Master => self.master_output.devices().len(),
                        OutputPickerTarget::Cue => self.cue_output.devices().len(),
                        OutputPickerTarget::Mic => self.mic_input_devices.len(),
                    };
                    if max_idx > 0 {
                        match self.output_picker_target {
                            OutputPickerTarget::Master => {
                                self.selected_master_output_idx =
                                    (self.selected_master_output_idx + 1).min(max_idx - 1);
                            }
                            OutputPickerTarget::Cue => {
                                self.selected_cue_output_idx =
                                    (self.selected_cue_output_idx + 1).min(max_idx - 1);
                            }
                            OutputPickerTarget::Mic => {
                                self.selected_mic_input_idx =
                                    (self.selected_mic_input_idx + 1).min(max_idx - 1);
                            }
                        }
                    }
                    return;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    match self.output_picker_target {
                        OutputPickerTarget::Master => {
                            self.selected_master_output_idx =
                                self.selected_master_output_idx.saturating_sub(1);
                        }
                        OutputPickerTarget::Cue => {
                            self.selected_cue_output_idx =
                                self.selected_cue_output_idx.saturating_sub(1);
                        }
                        OutputPickerTarget::Mic => {
                            self.selected_mic_input_idx =
                                self.selected_mic_input_idx.saturating_sub(1);
                        }
                    }
                    return;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    self.select_output_device();
                    return;
                }
                _ => {}
            }
            return;
        }

        // Mode-specific handling
        self.log_debug(format!("Key {:?} in mode {:?}", key.code, self.mode));
        match self.mode {
            AppMode::Help => self.handle_help_key(key),
            AppMode::PaneSelect => self.handle_pane_select_key(key),
            AppMode::ControlSelect => self.handle_control_select_key(key),
            AppMode::Edit => self.handle_edit_key(key),
            AppMode::SamplePads => self.handle_pad_key(key),
            AppMode::SamplePadConfig => self.handle_pad_config_key(key),
            AppMode::SourcePicker(_) => self.handle_source_picker_key(key),
            AppMode::SamplePicker(_) => self.handle_sample_picker_key(key),
            AppMode::ConfirmAction(action) => self.handle_confirm_key(key, action),
            AppMode::ConfigCheck => self.handle_config_check_key(key),
            AppMode::SessionPicker => self.handle_session_picker_key(key),
        }
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => {
                self.mode = AppMode::PaneSelect;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.help_scroll = self.help_scroll.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            _ => {}
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent, action: ConfirmAction) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                match action {
                    ConfirmAction::Quit => self.should_quit = true,
                    ConfirmAction::ClearDeck(deck) => self.clear_deck_no_advance(deck),
                    ConfirmAction::ResetDeck(_deck) => self.reset_deck_to_defaults(),
                    ConfirmAction::ResetAll => self.reset_all_controls(),
                }
                self.mode = AppMode::PaneSelect;
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.mode = AppMode::PaneSelect;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.confirm_selected = false;
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.confirm_selected = true;
            }
            _ => {}
        }
    }

    pub fn handle_config_check_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let errors = crate::config::apply_config_files(&self.config_diffs);
                if errors.is_empty() {
                    let path_msg = crate::config::ensure_local_bin_in_path();
                    self.config_check_msg = path_msg;
                } else {
                    self.config_check_msg = Some(errors.join("; "));
                }
                self.mode = AppMode::PaneSelect;
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.mode = AppMode::PaneSelect;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.confirm_selected = true;
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.confirm_selected = false;
            }
            _ => {}
        }
    }

    /// Handle session picker key events
    fn handle_session_picker_key(&mut self, key: KeyEvent) {
        let is_save = self.session_picker.is_save;

        if is_save {
            self.handle_session_picker_key_save(key);
        } else {
            self.handle_session_picker_key_load(key);
        }
    }

    /// Save mode: "Save as:" row is index 0, file list items are 1+.
    ///
    /// selected=0          → save name row focused, typing edits, Enter saves
    /// selected=1..        → file list navigation, Enter opens dir / triggers overwrite
    /// overwrite_confirm   → overwrite confirmation dialog, y/n/Esc
    fn handle_session_picker_key_save(&mut self, key: KeyEvent) {
        // Overwrite confirmation takes priority
        if let Some(ref path) = self.session_picker.overwrite_confirm.clone() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    let path = path.clone();
                    self.session_picker.overwrite_confirm = None;
                    self.save_session_to_path(&path);
                    self.focus_sequences_global();
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.session_picker.overwrite_confirm = None;
                }
                _ => {}
            }
            return;
        }

        if self.session_picker.selected == 0 && self.session_picker.input_mode == PickerInputMode::Normal {
            // Save name row (only in Normal mode; Insert mode with selected==0 stays in file list)
            if self.session_picker.editing_save_name {
                // Editing mode: type to edit save name
                match key.code {
                    KeyCode::Esc => {
                        self.session_picker.editing_save_name = false;
                    }
                    KeyCode::Left => {
                        let mut cur = self.session_picker.save_name_cursor.saturating_sub(1);
                        while !self.session_picker.save_name.is_char_boundary(cur) {
                            cur -= 1;
                        }
                        self.session_picker.save_name_cursor = cur;
                    }
                    KeyCode::Right => {
                        let mut cur = self.session_picker.save_name_cursor;
                        if cur < self.session_picker.save_name.len() {
                            cur += 1;
                            while !self.session_picker.save_name.is_char_boundary(cur) {
                                cur += 1;
                            }
                            self.session_picker.save_name_cursor = cur;
                        }
                    }
                    KeyCode::Enter => {
                        let save_name = self.session_picker.save_name.clone();
                        if !save_name.ends_with(".json") {
                            self.session_picker.status_message =
                                Some("Filename must end with .json".to_string());
                            return;
                        }
                        if save_name
                            .strip_suffix(".json")
                            .is_none_or(|name| name.trim().is_empty())
                        {
                            self.session_picker.status_message =
                                Some("Filename must include a name".to_string());
                            return;
                        }
                        let current_dir = self.session_picker.current_dir.clone();
                        let path = current_dir.join(&save_name);
                        self.session_picker.status_message = None;
                        if path.exists() {
                            self.session_picker.overwrite_confirm = Some(path);
                        } else {
                            self.save_session_to_path(&path);
                            self.focus_sequences_global();
                        }
                    }
                    KeyCode::Backspace => {
                        let cur = self.session_picker.save_name_cursor;
                        if cur > 0 {
                            let mut previous = cur - 1;
                            while !self.session_picker.save_name.is_char_boundary(previous) {
                                previous -= 1;
                            }
                            self.session_picker.save_name.drain(previous..cur);
                            self.session_picker.save_name_cursor = previous;
                        }
                    }
                    KeyCode::Char(c) => {
                        self.session_picker.status_message = None;
                        let cur = self.session_picker.save_name_cursor;
                        self.session_picker.save_name.insert(cur, c);
                        self.session_picker.save_name_cursor += c.len_utf8();
                    }
                    _ => {}
                }
            } else {
                // Normal mode on save name row: navigate or toggle editing
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.focus_sequences_global();
                    }
                    KeyCode::Enter => {
                        self.session_picker.save_name_cursor = self.session_picker.save_name.len();
                        self.session_picker.editing_save_name = true;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.session_picker.move_down();
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.session_picker.move_up();
                    }
                    KeyCode::Char('i') => {
                        self.session_picker.input_mode = PickerInputMode::Insert;
                    }
                    _ => {}
                }
            }
        } else {
            // File list zone
            match self.session_picker.input_mode {
                PickerInputMode::Normal => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.focus_sequences_global();
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        self.session_picker.move_down();
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        self.session_picker.move_up();
                    }
                    KeyCode::Char('l') | KeyCode::Right => {
                        self.session_picker.enter_dir();
                    }
                    KeyCode::Enter => {
                        // selected >= 1, so file_idx = selected - 1
                        let file_idx = self.session_picker.selected - 1;
                        if let Some(&idx) = self.session_picker.filtered.get(file_idx)
                            && let Some(item) = self.session_picker.items.get(idx)
                        {
                            if item.is_dir {
                                self.session_picker.enter_dir();
                                self.session_picker.selected = 0;
                            } else if item.path.extension().is_some_and(|e| e == "json") {
                                self.session_picker.save_name = item
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "session.json".to_string());
                                self.session_picker.overwrite_confirm = Some(item.path.clone());
                            }
                        }
                    }
                    KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                        self.session_picker.go_up();
                    }
                    KeyCode::Char('i') => {
                        self.session_picker.input_mode = PickerInputMode::Insert;
                    }
                    KeyCode::Char('g') => {
                        self.session_picker.selected = 0;
                        self.session_picker.scroll_offset = 0;
                    }
                    KeyCode::Char('G') => {
                        let max = self.session_picker.filtered.len(); // 0=save, 1..=len=file
                        self.session_picker.selected = max;
                        let file_idx = max.saturating_sub(1);
                        if file_idx >= self.session_picker.scroll_offset + self.session_picker.visible_height {
                            self.session_picker.scroll_offset =
                                file_idx.saturating_sub(self.session_picker.visible_height - 1);
                        }
                    }
                    _ => {}
                },
                PickerInputMode::Insert => match key.code {
                    KeyCode::Esc => {
                        self.session_picker.input_mode = PickerInputMode::Normal;
                    }
                    KeyCode::Backspace => {
                        self.session_picker.query.pop();
                        self.session_picker.filter();
                    }
                    KeyCode::Enter => {
                        self.session_picker.input_mode = PickerInputMode::Normal;
                    }
                    KeyCode::Down => {
                        self.session_picker.move_down();
                    }
                    KeyCode::Up => {
                        self.session_picker.move_up();
                    }
                    KeyCode::Char(c) => {
                        self.session_picker.query.push(c);
                        self.session_picker.filter();
                    }
                    _ => {}
                },
            }
        }
    }

    /// Load mode: file list navigation with fuzzy filter in insert mode.
    fn handle_session_picker_key_load(&mut self, key: KeyEvent) {
        match self.session_picker.input_mode {
            PickerInputMode::Normal => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.focus_sequences_global();
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.session_picker.move_down();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.session_picker.move_up();
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    self.session_picker.enter_dir();
                }
                KeyCode::Enter => {
                    if let Some(&idx) = self
                        .session_picker
                        .filtered
                        .get(self.session_picker.selected)
                        && let Some(item) = self.session_picker.items.get(idx)
                    {
                        if item.is_dir {
                            self.session_picker.enter_dir();
                        } else {
                            let path = item.path.clone();
                            self.load_session_from_path(&path);
                            self.focus_sequences_global();
                        }
                    }
                }
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                    self.session_picker.go_up();
                }
                KeyCode::Char('i') => {
                    self.session_picker.input_mode = PickerInputMode::Insert;
                }
                KeyCode::Char('g') => {
                    self.session_picker.selected = 0;
                    self.session_picker.scroll_offset = 0;
                }
                KeyCode::Char('G') => {
                    self.session_picker.selected =
                        self.session_picker.filtered.len().saturating_sub(1);
                    if self.session_picker.selected
                        >= self.session_picker.scroll_offset + self.session_picker.visible_height
                    {
                        self.session_picker.scroll_offset = self
                            .session_picker
                            .selected
                            .saturating_sub(self.session_picker.visible_height - 1);
                    }
                }
                _ => {}
            },
            PickerInputMode::Insert => match key.code {
                KeyCode::Esc => {
                    self.session_picker.input_mode = PickerInputMode::Normal;
                }
                KeyCode::Enter => {
                    self.session_picker.input_mode = PickerInputMode::Normal;
                }
                KeyCode::Backspace => {
                    self.session_picker.query.pop();
                    self.session_picker.filter();
                }
                KeyCode::Down => {
                    self.session_picker.move_down();
                }
                KeyCode::Up => {
                    self.session_picker.move_up();
                }
                KeyCode::Char(c) => {
                    self.session_picker.query.push(c);
                    self.session_picker.filter();
                }
                _ => {}
            },
        }
    }

    fn save_session_to_path(&mut self, path: &std::path::Path) {
        let session = SessionState::from_current(&self.sample_pads.pads, &self.sequence_state);
        match session.save_to_file(path) {
            Ok(()) => self.log_debug(format!("Session saved to {}", path.display())),
            Err(e) => self.log_debug(format!("Save failed: {}", e)),
        }
    }

    fn load_session_from_path(&mut self, path: &std::path::Path) {
        match SessionState::load_from_file(path) {
            Ok(session) => {
                self.sample_pads.pads = session.pads;
                self.sequence_state.sequences = session.sequences;
                for seq in &mut self.sequence_state.sequences {
                    seq.playing = seq.any_marked();
                }
                self.sequence_state.global = session.global;
                self.sequence_state.selected = None;
                self.sequence_state.global_focused = true;
                self.sequence_state.scroll_offset = 0;
                self.sequence_state.previously_playing = self
                    .sequence_state
                    .sequences
                    .iter()
                    .map(|seq| seq.playing)
                    .collect();
                self.sequence_state.global.mute = true;
                for seq in &mut self.sequence_state.sequences {
                    seq.playing = false;
                }
                // Reload pad samples into the audio engine
                for (pad_idx, pad) in self.sample_pads.pads.iter().enumerate() {
                    if let Some(ref sample_path) = pad.sample_path
                        && sample_path.exists()
                    {
                        if let Some(ref mut engine) = self.sample_engine {
                            let _ = engine.preload(sample_path);
                        }
                        let pad_cfg = pad.config.clone();
                        if let Some(ref engine) = self.audio_engine {
                            let mut loaded = false;
                            if let Some(ref sample_eng) = self.sample_engine
                                && let Some(cached) = sample_eng.cache.get(sample_path)
                            {
                                let processed =
                                    crate::audio::sample_cache::apply_dsp_to_buffer(
                                        &cached.samples,
                                        &pad_cfg,
                                    );
                                engine.set_pad_sample(
                                    pad_idx,
                                    processed,
                                    cached.sample_rate,
                                    cached.channels,
                                );
                                loaded = true;
                            }
                            if !loaded
                                && let Ok(cached) =
                                    crate::audio::sample_cache::CachedSample::load(sample_path)
                            {
                                let processed =
                                    crate::audio::sample_cache::apply_dsp_to_buffer(
                                        &cached.samples,
                                        &pad_cfg,
                                    );
                                engine.set_pad_sample(
                                    pad_idx,
                                    processed,
                                    cached.sample_rate,
                                    cached.channels,
                                );
                            }
                        }
                    }
                }
                self.log_debug(format!("Session loaded from {}", path.display()));
            }
            Err(e) => {
                self.log_debug(format!("Load failed: {}", e));
            }
        }
    }

    /// Return focus to the SEQUENCES pane with sub-navigation active.
    /// Used when closing the session picker to land back on the global bar.
    fn focus_sequences_global(&mut self) {
        self.selected_pane = SelectedPane::Loops;
        self.sequence_state.global_focused = true;
        self.mode = AppMode::ControlSelect;
    }

    /// Open the session picker to save session state
    fn save_session(&mut self) {
        self.session_picker.set_root(self.sessions_dir.clone());
        self.session_picker.is_save = true;
        self.session_picker.input_mode = PickerInputMode::Normal;
        self.session_picker.query.clear();
        self.session_picker.save_name = "session.json".to_string();
        self.session_picker.save_name_cursor = self.session_picker.save_name.len();
        self.session_picker.selected = 0;
        self.session_picker.editing_save_name = false;
        self.session_picker.status_message = None;
        self.session_picker.scan_dir();
        self.mode = AppMode::SessionPicker;
    }

    /// Open the session picker to load session state
    fn load_session(&mut self) {
        self.session_picker.set_root(self.sessions_dir.clone());
        self.session_picker.is_save = false;
        self.session_picker.input_mode = PickerInputMode::Normal;
        self.session_picker.query.clear();
        self.session_picker.selected = 0;
        self.session_picker.scan_dir();
        self.mode = AppMode::SessionPicker;
    }

    /// Mode 1: Pane Select - navigate between panes with Tab/hjkl
    fn handle_pane_select_key(&mut self, key: KeyEvent) {
        match key.code {
            // Esc does nothing in PaneSelect (already at top level)
            KeyCode::Esc => {}

            // Help
            KeyCode::Char('?') => {
                self.help_scroll = 0;
                self.mode = AppMode::Help;
            }

            // Toggle sample pad mode
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.mode = AppMode::SamplePads;
                self.sample_pads.active = true;
            }

            // Tab: next pane (round-robin)
            KeyCode::Tab => {
                self.selected_pad_idx = None;
                self.selected_pane = self.selected_pane.next();
                self.sync_pane_to_mixer();
                self.ensure_mixer_pane_visible();
            }

            // Shift+Tab: previous pane (round-robin)
            KeyCode::BackTab => {
                self.selected_pad_idx = None;
                self.selected_pane = self.selected_pane.prev();
                self.sync_pane_to_mixer();
                self.ensure_mixer_pane_visible();
            }

            // h/l: horizontal navigation across mixer layout
            // DeckA ↔ Crossfader ↔ DeckB ↔ DeckC ↔ Master
            KeyCode::Char('l') | KeyCode::Right => {
                self.selected_pad_idx = None;
                self.selected_pane = match self.selected_pane {
                    SelectedPane::DeckA => SelectedPane::Crossfader,
                    SelectedPane::DjCenter | SelectedPane::Loops => SelectedPane::DeckB,
                    SelectedPane::DeckB => SelectedPane::DeckC,
                    SelectedPane::DeckC => SelectedPane::Master,
                    SelectedPane::Master => SelectedPane::DeckA,
                    SelectedPane::Crossfader => SelectedPane::DeckB,
                };
                self.sync_pane_to_mixer();
                self.ensure_mixer_pane_visible();
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.selected_pad_idx = None;
                self.selected_pane = match self.selected_pane {
                    SelectedPane::DeckA => SelectedPane::Master,
                    SelectedPane::DjCenter | SelectedPane::Loops => SelectedPane::DeckA,
                    SelectedPane::DeckB => SelectedPane::Crossfader,
                    SelectedPane::DeckC => SelectedPane::DeckB,
                    SelectedPane::Master => SelectedPane::DeckC,
                    SelectedPane::Crossfader => SelectedPane::DeckA,
                };
                self.sync_pane_to_mixer();
                self.ensure_mixer_pane_visible();
            }

            // j/k: vertical pane navigation
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected_pad_idx = None;
                self.selected_pane = match self.selected_pane {
                    SelectedPane::DjCenter => SelectedPane::Loops,
                    SelectedPane::Loops => SelectedPane::Crossfader,
                    SelectedPane::Crossfader => SelectedPane::DeckB,
                    SelectedPane::DeckB => SelectedPane::DeckC,
                    SelectedPane::DeckC => SelectedPane::Master,
                    SelectedPane::Master => SelectedPane::DeckA,
                    SelectedPane::DeckA => SelectedPane::DjCenter,
                };
                self.sync_pane_to_mixer();
                self.ensure_mixer_pane_visible();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected_pad_idx = None;
                self.selected_pane = match self.selected_pane {
                    SelectedPane::DjCenter => SelectedPane::DeckA,
                    SelectedPane::Loops => SelectedPane::DjCenter,
                    SelectedPane::Crossfader => SelectedPane::Loops,
                    SelectedPane::DeckB => SelectedPane::Crossfader,
                    SelectedPane::DeckC => SelectedPane::DeckB,
                    SelectedPane::Master => SelectedPane::DeckC,
                    SelectedPane::DeckA => SelectedPane::Master,
                };
                self.sync_pane_to_mixer();
                self.ensure_mixer_pane_visible();
            }

            // Crossfader slam shortcuts (when Crossfader pane is selected)
            KeyCode::Char('a') => {
                if self.selected_pane == SelectedPane::Crossfader {
                    self.mixer.dj.crossfader = -1.0; // Slam to full A
                    self.sync_current_control_to_mpv();
                }
            }
            KeyCode::Char('b') => {
                if self.selected_pane == SelectedPane::Crossfader {
                    self.mixer.dj.crossfader = 1.0; // Slam to full B
                    self.sync_current_control_to_mpv();
                }
            }

            // Enter: activate control select mode for this pane
            KeyCode::Enter | KeyCode::Char(' ') => {
                if self.selected_pane == SelectedPane::Crossfader {
                    // Crossfader has one control - go straight to edit
                    self.mixer.focus = SelectionFocus::Global;
                    self.mixer.selected_global = GlobalControl::Crossfader;
                    self.mode = AppMode::Edit;
                } else {
                    self.mode = AppMode::ControlSelect;
                    if self.selected_pane == SelectedPane::DjCenter {
                        self.selected_pad_idx = Some(0);
                    }
                    self.sync_pane_to_mixer();
                }
            }

            // c: open pad config if in DJ center with a pad selected
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if self.selected_pane == SelectedPane::DjCenter {
                    if self.selected_pad_idx.is_none() {
                        self.selected_pad_idx = Some(0);
                    }
                    self.mode = AppMode::SamplePadConfig;
                    self.sample_pads.config_mode = true;
                    self.sample_pads.selected_pad = self.selected_pad_idx.unwrap_or(0);
                    // Always focus the first control (Sample) when opening pad config
                    self.sample_pads.selected_control = PadControl::Sample;
                }
            }

            // X (shift-x): clear the focused deck playlist (with confirmation)
            KeyCode::Char('X') => {
                let deck = match self.selected_pane {
                    SelectedPane::DeckA => Some(Deck::A),
                    SelectedPane::DeckB => Some(Deck::B),
                    SelectedPane::DeckC => Some(Deck::C),
                    _ => None,
                };
                if let Some(deck) = deck {
                    self.confirm_selected = false;
                    self.mode = AppMode::ConfirmAction(ConfirmAction::ClearDeck(deck));
                }
            }

            // R (shift-r): reset all controls to defaults (with confirmation)
            KeyCode::Char('R') => {
                self.confirm_selected = false;
                self.mode = AppMode::ConfirmAction(ConfirmAction::ResetAll);
            }

            // Quick toggle shortcuts (work in PaneSelect too)
            KeyCode::Char('m') => {
                if let SelectionFocus::Channel(ch_idx) = self.mixer.focus {
                    if let Some(channel) = self.mixer.selected_channel_mut() {
                        channel.muted = !channel.muted;
                    }
                    self.sync_mute_to_mpv(ch_idx);
                } else if self.selected_pane == SelectedPane::Master {
                    if matches!(
                        self.mixer.selected_global,
                        GlobalControl::MicFader
                            | GlobalControl::MicMute
                            | GlobalControl::MicInputSelect
                    ) {
                        self.mixer.master.mic_muted = !self.mixer.master.mic_muted;
                        self.sync_mic_to_engine();
                    } else {
                        self.mixer.master.muted = !self.mixer.master.muted;
                        self.sync_deck_volume(true);
                        self.sync_deck_volume(false);
                    }
                }
            }
            KeyCode::Char('s') => {
                if let SelectionFocus::Channel(ch_idx) = self.mixer.focus {
                    let was_solo = self
                        .mixer
                        .selected_channel_mut()
                        .map(|c| c.solo)
                        .unwrap_or(false);
                    if !was_solo {
                        // Turning solo ON: save this channel's fader, restore any other soloed decks
                        self.mixer.save_fader_for_solo(ch_idx);
                        for i in 0..self.mixer.channels.len() {
                            if i != ch_idx && self.mixer.channels[i].solo {
                                self.mixer.channels[i].solo = false;
                                self.mixer.restore_fader_from_solo(i);
                            }
                        }
                        if self.mixer.cue_channel.solo {
                            self.mixer.cue_channel.solo = false;
                            self.mixer.restore_fader_from_solo(2);
                        }
                        if let Some(channel) = self.mixer.selected_channel_mut() {
                            channel.solo = true;
                        }
                    } else {
                        // Turning solo OFF: restore this channel's fader
                        if let Some(channel) = self.mixer.selected_channel_mut() {
                            channel.solo = false;
                        }
                        self.mixer.restore_fader_from_solo(ch_idx);
                    }
                    self.mixer.solo_active =
                        self.mixer.channels.iter().any(|c| c.solo) || self.mixer.cue_channel.solo;
                    self.sync_solo_to_all_mpv();
                }
            }

            // K: next track — MPV playlist next, then queue next
            KeyCode::Char('K') => {
                if let Some(deck) = self.selected_pane_deck() {
                    self.trigger_playlist_nav(deck, true);
                }
            }
            // J: prev track — MPV playlist prev, then queue prev
            KeyCode::Char('J') => {
                if let Some(deck) = self.selected_pane_deck() {
                    self.trigger_playlist_nav(deck, false);
                }
            }

            _ => {}
        }
    }

    fn selected_pane_deck(&self) -> Option<Deck> {
        match self.selected_pane {
            SelectedPane::DeckA => Some(Deck::A),
            SelectedPane::DeckB => Some(Deck::B),
            SelectedPane::DeckC => Some(Deck::C),
            _ => None,
        }
    }

    /// Attempt MPV playlist next for a deck. Returns true if MPV handled it.
    fn try_mpv_next(&mut self, deck: Deck) -> bool {
        let path = {
            let mpv = match deck {
                Deck::A => self.mpv_deck_a.as_mut(),
                Deck::B => self.mpv_deck_b.as_mut(),
                Deck::C => self.mpv_deck_c.as_mut(),
            };
            let Some(client) = mpv else { return false };
            let Ok((_, has_next)) = client.get_playlist_nav_available() else {
                return false;
            };
            if !has_next || client.playlist_next().is_err() {
                return false;
            }
            client.get_path().ok()
        };
        self.reload_mpv_playlist_track(deck, path);
        true
    }

    fn try_mpv_prev(&mut self, deck: Deck) -> bool {
        let path = {
            let mpv = match deck {
                Deck::A => self.mpv_deck_a.as_mut(),
                Deck::B => self.mpv_deck_b.as_mut(),
                Deck::C => self.mpv_deck_c.as_mut(),
            };
            let Some(client) = mpv else { return false };
            let Ok((has_prev, _)) = client.get_playlist_nav_available() else {
                return false;
            };
            if !has_prev || client.playlist_prev().is_err() {
                return false;
            }
            client.get_path().ok()
        };
        self.reload_mpv_playlist_track(deck, path);
        true
    }

    fn reload_mpv_playlist_track(&mut self, deck: Deck, path: Option<String>) {
        let Some(path) = path else { return };
        let ch_idx = match deck {
            Deck::A => self.mixer.dj.deck_a_channel,
            Deck::B => self.mixer.dj.deck_b_channel,
            Deck::C => self.mixer.dj.deck_c_channel,
        };
        if let Some(engine) = self.audio_engine.as_ref()
            && !engine.has_capture(ch_idx)
        {
            engine.load_file(ch_idx, path);
            self.sync_volume_to_mpv(ch_idx);
        }
    }

    fn navigate_queue_next(&mut self, deck: Deck) {
        let deck_idx = deck.index();
        let next_idx = self.deck_queue_indices[deck_idx] + 1;
        if next_idx < self.deck_queues[deck_idx].len() {
            self.deck_queue_indices[deck_idx] = next_idx;
            let item = self.deck_queues[deck_idx][next_idx].clone();
            self.load_queue_item(deck, &item);
        }
    }

    fn navigate_queue_prev(&mut self, deck: Deck) {
        let deck_idx = deck.index();
        if self.deck_queue_indices[deck_idx] > 0 {
            let prev_idx = self.deck_queue_indices[deck_idx] - 1;
            self.deck_queue_indices[deck_idx] = prev_idx;
            let item = self.deck_queues[deck_idx][prev_idx].clone();
            self.load_queue_item(deck, &item);
        }
    }

    /// Sync selected_pane to mixer focus/channel
    fn sync_pane_to_mixer(&mut self) {
        match self.selected_pane {
            SelectedPane::DeckA => {
                self.mixer.focus = SelectionFocus::Channel(self.mixer.dj.deck_a_channel);
                self.mixer.selected_channel = self.mixer.dj.deck_a_channel;
                self.mixer.selected_control = ChannelControl::Fader;
            }
            SelectedPane::DeckB => {
                self.mixer.focus = SelectionFocus::Channel(self.mixer.dj.deck_b_channel);
                self.mixer.selected_channel = self.mixer.dj.deck_b_channel;
                self.mixer.selected_control = ChannelControl::Fader;
            }
            SelectedPane::DjCenter => {
                self.mixer.focus = SelectionFocus::Global;
                self.mixer.selected_global = GlobalControl::Crossfader;
            }
            SelectedPane::Loops => {
                // Loops pane uses its own rack selection, keep global focus
                self.mixer.focus = SelectionFocus::Global;
            }
            SelectedPane::Crossfader => {
                self.mixer.focus = SelectionFocus::Global;
                self.mixer.selected_global = GlobalControl::Crossfader;
            }
            SelectedPane::Master => {
                self.mixer.focus = SelectionFocus::Global;
                self.mixer.selected_global = GlobalControl::MasterPlayPause;
            }
            SelectedPane::DeckC => {
                self.mixer.focus = SelectionFocus::Channel(2);
                self.mixer.selected_channel = 2;
                self.mixer.selected_control = ChannelControl::Fader;
            }
        }
    }

    /// Scroll mixer viewport so the currently selected pane snaps to the
    /// Recompute the mixer window to ensure the selected pane is visible.
    /// Called after rendering to keep the window in sync with viewport size.
    pub fn ensure_mixer_pane_visible(&mut self) {
        let viewport_w = self.term_width.max(1);
        let layout = MixerLayout::compute(
            viewport_w,
            self.selected_pane,
            Some(self.mixer_window_start),
            Some(self.mixer_window_end),
        );
        self.mixer_window_start = layout.start as usize;
        self.mixer_window_end = layout.end as usize;
    }

    /// Mode 2: Control Select - navigate controls within pane
    fn handle_control_select_key(&mut self, key: KeyEvent) {
        match key.code {
            // Esc: back to pane select
            KeyCode::Esc => {
                self.selected_pad_idx = None;
                self.mode = AppMode::PaneSelect;
            }

            // Help
            KeyCode::Char('?') => {
                self.help_scroll = 0;
                self.mode = AppMode::Help;
            }

            // Toggle sample pad mode
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.mode = AppMode::SamplePads;
                self.sample_pads.active = true;
            }

            // Tab: next pane (quick switch, round-robin)
            KeyCode::Tab => {
                self.selected_pad_idx = None;
                self.selected_pane = self.selected_pane.next();
                self.sync_pane_to_mixer();
            }

            // Shift+Tab: previous pane (quick switch, round-robin)
            KeyCode::BackTab => {
                self.selected_pad_idx = None;
                self.selected_pane = self.selected_pane.prev();
                self.sync_pane_to_mixer();
            }

            // Navigation within pane
            KeyCode::Char('j') | KeyCode::Down => {
                if self.selected_pane == SelectedPane::Crossfader {
                    self.mixer.dj.crossfader = (self.mixer.dj.crossfader - 0.05).clamp(-1.0, 1.0);
                    self.sync_current_control_to_mpv();
                } else {
                    self.navigate_control_down();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.selected_pane == SelectedPane::Crossfader {
                    self.mixer.dj.crossfader = (self.mixer.dj.crossfader + 0.05).clamp(-1.0, 1.0);
                    self.sync_current_control_to_mpv();
                } else {
                    self.navigate_control_up();
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if self.selected_pane == SelectedPane::Crossfader {
                    self.mixer.dj.crossfader = (self.mixer.dj.crossfader - 0.05).clamp(-1.0, 1.0);
                    self.sync_current_control_to_mpv();
                } else {
                    self.navigate_control_left();
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if self.selected_pane == SelectedPane::Crossfader {
                    self.mixer.dj.crossfader = (self.mixer.dj.crossfader + 0.05).clamp(-1.0, 1.0);
                    self.sync_current_control_to_mpv();
                } else {
                    self.navigate_control_right();
                }
            }

            // Enter/Space: context-dependent action
            KeyCode::Enter | KeyCode::Char(' ') => {
                // Handle output picker first if active
                if self.output_picker_active {
                    self.select_output_device();
                    return;
                }

                if self.selected_pane == SelectedPane::DjCenter {
                    // Pad selected → open pad config
                    if let Some(pad_idx) = self.selected_pad_idx {
                        self.sample_pads.selected_pad = pad_idx;
                        self.mode = AppMode::SamplePadConfig;
                        self.sample_pads.config_mode = true;
                        // Always focus the first control (Sample) when opening pad config
                        self.sample_pads.selected_control = PadControl::Sample;
                        return;
                    }
                }
                if self.selected_pane == SelectedPane::Loops {
                    if self.sequence_state.global_focused {
                        // Global bar selected — handle global controls
                        match self.sequence_state.global_control {
                            crate::state::GlobalSequenceControl::Volume
                            | crate::state::GlobalSequenceControl::Bpm => {
                                self.mode = AppMode::Edit;
                            }
                            crate::state::GlobalSequenceControl::Mute => {
                                let was_muted = self.sequence_state.global.mute;
                                self.sequence_state.global.mute = !was_muted;
                                if was_muted {
                                    // Unmuting: restore per-sequence play states
                                    for (i, seq) in
                                        self.sequence_state.sequences.iter_mut().enumerate()
                                    {
                                        if i < self.sequence_state.previously_playing.len() {
                                            seq.playing = self.sequence_state.previously_playing[i];
                                        }
                                    }
                                } else {
                                    // Muting: save play states so unmute can restore them
                                    self.sequence_state.previously_playing = self
                                        .sequence_state
                                        .sequences
                                        .iter()
                                        .map(|seq| seq.playing)
                                        .collect();
                                }
                            }
                            crate::state::GlobalSequenceControl::Save => {
                                self.save_session();
                            }
                            crate::state::GlobalSequenceControl::Load => {
                                self.load_session();
                            }
                        }
                        return;
                    }
                    if let Some(seq_idx) = self.sequence_state.selected {
                        // Always act on the current cursor target
                        match self.sequence_state.cursor {
                            crate::state::EditTarget::Step(step) => {
                                self.sequence_state.toggle_step(step);
                            }
                            crate::state::EditTarget::Mute => {
                                if let Some(seq) = self.sequence_state.sequences.get_mut(seq_idx) {
                                    seq.mute = !seq.mute;
                                }
                            }
                            crate::state::EditTarget::Gear => {
                                // Open pad config for this sequence's pad
                                if let Some(seq_idx) = self.sequence_state.selected
                                    && let Some(seq) = self.sequence_state.sequences.get(seq_idx)
                                {
                                    let pad_idx = seq.pad_idx;
                                    self.selected_pad_idx = Some(pad_idx);
                                    self.sample_pads.selected_pad = pad_idx;
                                    self.mode = AppMode::SamplePadConfig;
                                    self.sample_pads.config_mode = true;
                                    self.sample_pads.selected_control = PadControl::Sample;
                                }
                            }
                        }
                        return;
                    }
                }
                if self.selected_pane == SelectedPane::DeckC {
                    // CUE deck controls
                    match self.mixer.selected_control {
                        ChannelControl::CueSendToA => {
                            self.swap_cue_with_deck(SendTarget::A);
                            return;
                        }
                        ChannelControl::CueSendToB => {
                            self.swap_cue_with_deck(SendTarget::B);
                            return;
                        }
                        ChannelControl::CueOutputSelect => {
                            self.open_output_picker(OutputPickerTarget::Cue);
                            return;
                        }
                        _ => {} // Fall through to normal deck control handling
                    }
                }
                if self.selected_pane == SelectedPane::Master {
                    match self.mixer.selected_global {
                        GlobalControl::MasterOutputSelect => {
                            self.open_output_picker(OutputPickerTarget::Master);
                            return;
                        }
                        GlobalControl::MicInputSelect => {
                            self.open_output_picker(OutputPickerTarget::Mic);
                            return;
                        }
                        _ => {}
                    }
                }

                if self.is_current_control_continuous() {
                    self.mode = AppMode::Edit;
                } else {
                    if let SelectionFocus::Channel(ch_idx) = self.mixer.focus {
                        match self.mixer.selected_control {
                            ChannelControl::PrevTrack => {
                                let deck = if ch_idx == self.mixer.dj.deck_a_channel {
                                    Deck::A
                                } else if ch_idx == self.mixer.dj.deck_b_channel {
                                    Deck::B
                                } else {
                                    Deck::C
                                };
                                self.trigger_playlist_nav(deck, false);
                                return;
                            }
                            ChannelControl::NextTrack => {
                                let deck = if ch_idx == self.mixer.dj.deck_a_channel {
                                    Deck::A
                                } else if ch_idx == self.mixer.dj.deck_b_channel {
                                    Deck::B
                                } else {
                                    Deck::C
                                };
                                self.trigger_playlist_nav(deck, true);
                                return;
                            }
                            _ => {}
                        }
                    }

                    // If PlayPause on an empty deck, open source picker instead of toggling
                    if self.mixer.selected_control == ChannelControl::PlayPause
                        && let SelectionFocus::Channel(ch_idx) = self.mixer.focus
                    {
                        let is_empty = self
                            .mixer
                            .get_channel(ch_idx)
                            .map(|c| !c.connected)
                            .unwrap_or(true);
                        if is_empty {
                            let deck = if ch_idx == self.mixer.dj.deck_a_channel {
                                Deck::A
                            } else if ch_idx == self.mixer.dj.deck_b_channel {
                                Deck::B
                            } else if ch_idx == self.mixer.dj.deck_c_channel {
                                Deck::C
                            } else {
                                Deck::A // fallback
                            };
                            self.open_source_picker(deck);
                            return;
                        }
                    }
                    self.toggle_current_control();
                }
            }

            // r/R: reset deck (with confirmation) — no more recording
            KeyCode::Char('r') | KeyCode::Char('R') => {
                if (self.selected_pane == SelectedPane::DeckA
                    || self.selected_pane == SelectedPane::DeckB
                    || self.selected_pane == SelectedPane::DeckC)
                    && key.code == KeyCode::Char('R')
                {
                    let deck = match self.selected_pane {
                        SelectedPane::DeckA => Deck::A,
                        SelectedPane::DeckB => Deck::B,
                        SelectedPane::DeckC => Deck::C,
                        _ => unreachable!(),
                    };
                    self.confirm_selected = false;
                    self.mode = AppMode::ConfirmAction(ConfirmAction::ResetDeck(deck));
                }
            }

            // (a and x shortcuts disabled — sequences auto-created on pad sample load)

            // Quick toggles
            KeyCode::Char('m') => {
                if let SelectionFocus::Channel(ch_idx) = self.mixer.focus {
                    if let Some(channel) = self.mixer.selected_channel_mut() {
                        channel.muted = !channel.muted;
                    }
                    self.sync_mute_to_mpv(ch_idx);
                } else if self.selected_pane == SelectedPane::Master {
                    if matches!(
                        self.mixer.selected_global,
                        GlobalControl::MicFader
                            | GlobalControl::MicMute
                            | GlobalControl::MicInputSelect
                    ) {
                        self.mixer.master.mic_muted = !self.mixer.master.mic_muted;
                        self.sync_mic_to_engine();
                    } else {
                        self.mixer.master.muted = !self.mixer.master.muted;
                        self.sync_deck_volume(true);
                        self.sync_deck_volume(false);
                    }
                }
            }
            KeyCode::Char('s') => {
                if let SelectionFocus::Channel(ch_idx) = self.mixer.focus {
                    let was_solo = self
                        .mixer
                        .selected_channel_mut()
                        .map(|c| c.solo)
                        .unwrap_or(false);
                    if !was_solo {
                        // Turning solo ON: save this channel's fader, restore any other soloed decks
                        self.mixer.save_fader_for_solo(ch_idx);
                        for i in 0..self.mixer.channels.len() {
                            if i != ch_idx && self.mixer.channels[i].solo {
                                self.mixer.channels[i].solo = false;
                                self.mixer.restore_fader_from_solo(i);
                            }
                        }
                        if self.mixer.cue_channel.solo {
                            self.mixer.cue_channel.solo = false;
                            self.mixer.restore_fader_from_solo(2);
                        }
                        if let Some(channel) = self.mixer.selected_channel_mut() {
                            channel.solo = true;
                        }
                    } else {
                        // Turning solo OFF: restore this channel's fader
                        if let Some(channel) = self.mixer.selected_channel_mut() {
                            channel.solo = false;
                        }
                        self.mixer.restore_fader_from_solo(ch_idx);
                    }
                    self.mixer.solo_active =
                        self.mixer.channels.iter().any(|c| c.solo) || self.mixer.cue_channel.solo;
                    // Sync all channels to apply solo logic
                    self.sync_solo_to_all_mpv();
                }
            }

            // Reset to default
            KeyCode::Char('0') => {
                self.reset_current_control();
                self.sync_current_control_to_mpv();
            }

            // c: open pad config if pad selected, else center pan/crossfader
            KeyCode::Char('c') | KeyCode::Char('C') => {
                if self.selected_pane == SelectedPane::DjCenter
                    && let Some(pad_idx) = self.selected_pad_idx
                {
                    self.sample_pads.selected_pad = pad_idx;
                    self.mode = AppMode::SamplePadConfig;
                    self.sample_pads.config_mode = true;
                    // Always focus the first control (Sample) when opening pad config
                    self.sample_pads.selected_control = PadControl::Sample;
                    return;
                }
                match self.mixer.focus {
                    SelectionFocus::Channel(_) => {
                        if self.mixer.selected_control == ChannelControl::Pan
                            && let Some(channel) = self.mixer.selected_channel_mut()
                        {
                            channel.pan = 0.0;
                        }
                    }
                    SelectionFocus::Global => {
                        if self.mixer.selected_global == GlobalControl::Crossfader {
                            self.mixer.dj.crossfader = 0.0;
                        }
                    }
                }
                self.sync_current_control_to_mpv();
            }

            _ => {}
        }
    }

    /// Navigate to next control down within current pane
    fn navigate_control_down(&mut self) {
        match self.selected_pane {
            SelectedPane::DeckA | SelectedPane::DeckB | SelectedPane::DeckC => {
                if self.mixer.is_in_eq_or_filter_section() {
                    match self.mixer.selected_control {
                        // EQ bars: navigate down to Pan
                        ChannelControl::EqLow | ChannelControl::EqMid | ChannelControl::EqHigh => {
                            self.mixer.selected_control = ChannelControl::Pan;
                        }
                        // Filter/LFO knobs: cycle down through 4 controls
                        ChannelControl::FilterCutoff => {
                            self.mixer.selected_control = ChannelControl::FilterFreq;
                        }
                        ChannelControl::FilterFreq => {
                            self.mixer.selected_control = ChannelControl::LfoShape;
                        }
                        ChannelControl::LfoShape => {
                            self.mixer.selected_control = ChannelControl::LfoSpeed;
                        }
                        // LFO speed: navigate down to Pan
                        ChannelControl::LfoSpeed => {
                            self.mixer.selected_control = ChannelControl::Pan;
                        }
                        _ => {}
                    }
                } else {
                    match self.mixer.selected_control {
                        ChannelControl::Bpm => {
                            self.mixer.selected_control = ChannelControl::FilterCutoff;
                        }
                        ChannelControl::Key => {
                            self.mixer.selected_control = ChannelControl::FilterCutoff;
                        }
                        ChannelControl::Mute if self.selected_pane == SelectedPane::DeckC => {
                            self.mixer.selected_control = ChannelControl::Solo;
                        }
                        ChannelControl::Solo if self.selected_pane == SelectedPane::DeckC => {
                            self.mixer.selected_control = ChannelControl::CueOutputSelect;
                        }
                        ChannelControl::CueSendToB if self.selected_pane == SelectedPane::DeckC => {
                            self.mixer.selected_control = ChannelControl::CueOutputSelect;
                        }
                        _ => {
                            self.mixer
                                .select_next_control(self.selected_pane == SelectedPane::DeckC);
                        }
                    }
                }
            }
            SelectedPane::DjCenter => {
                if let Some(pad_idx) = self.selected_pad_idx {
                    let row = pad_idx / 4;
                    let col = pad_idx % 4;
                    // Round-robin: wrap from bottom row to top row
                    let new_row = if row == 3 { 0 } else { row + 1 };
                    self.selected_pad_idx = Some(new_row * 4 + col);
                } else if self.mixer.selected_global == GlobalControl::Crossfader {
                    // Crossfader → top controls
                    self.mixer.selected_global = GlobalControl::HeadphoneVolume;
                } else {
                    // Top controls → pad grid
                    self.selected_pad_idx = Some(0);
                }
            }
            SelectedPane::Loops => {
                if self.sequence_state.selected.is_some() || self.sequence_state.global_focused {
                    self.sequence_state.select_down();
                    self.ensure_sequence_selected_visible();
                } else if !self.sequence_state.sequences.is_empty() {
                    self.sequence_state.selected = Some(0);
                    self.sequence_state.global_focused = false;
                    self.ensure_sequence_selected_visible();
                }
            }
            SelectedPane::Crossfader => {} // Single control, nothing to navigate
            SelectedPane::Master => {
                if self.mixer.selected_global.eq_band_index().is_some() {
                    // From any EQ band: jump to MicMute.
                    self.mixer.selected_global = GlobalControl::MicMute;
                } else {
                    self.mixer.selected_global = match self.mixer.selected_global {
                        GlobalControl::MasterPlayPause => GlobalControl::MasterEq32,
                        GlobalControl::MicMute => GlobalControl::MicInputSelect,
                        GlobalControl::MicFader => GlobalControl::MicInputSelect,
                        GlobalControl::MicInputSelect => GlobalControl::MasterFader,
                        GlobalControl::MasterFader => GlobalControl::MasterMute,
                        GlobalControl::MasterMute => GlobalControl::MasterPlayPause,
                        GlobalControl::MasterOutputSelect => GlobalControl::MasterPlayPause,
                        other => other,
                    };
                }
            }
        }
    }

    /// Navigate to previous control up within current pane (round-robin)
    fn navigate_control_up(&mut self) {
        match self.selected_pane {
            SelectedPane::DeckA | SelectedPane::DeckB | SelectedPane::DeckC => {
                if self.mixer.is_in_eq_or_filter_section() {
                    match self.mixer.selected_control {
                        ChannelControl::Bpm if self.selected_pane == SelectedPane::DeckC => {
                            self.mixer.selected_control = ChannelControl::PlayPause;
                        }
                        // EQ bars: navigate up to BPM
                        ChannelControl::EqLow | ChannelControl::EqMid | ChannelControl::EqHigh => {
                            self.mixer.selected_control = ChannelControl::Key;
                        }
                        // Key: navigate up to BPM
                        ChannelControl::Key => {
                            self.mixer.selected_control = ChannelControl::Bpm;
                        }
                        // Filter cutoff: navigate up to Key
                        ChannelControl::FilterCutoff => {
                            self.mixer.selected_control = ChannelControl::Key;
                        }
                        // Other filter/LFO knobs: cycle up (reverse)
                        ChannelControl::FilterFreq => {
                            self.mixer.selected_control = ChannelControl::FilterCutoff;
                        }
                        ChannelControl::LfoShape => {
                            self.mixer.selected_control = ChannelControl::FilterFreq;
                        }
                        ChannelControl::LfoSpeed => {
                            self.mixer.selected_control = ChannelControl::LfoShape;
                        }
                        _ => {}
                    }
                } else {
                    match self.selected_pane {
                        SelectedPane::DeckA | SelectedPane::DeckB => {
                            match self.mixer.selected_control {
                                ChannelControl::Bpm | ChannelControl::Key => {
                                    self.mixer.selected_control = ChannelControl::PlayPause;
                                }
                                ChannelControl::Mute | ChannelControl::Solo => {
                                    self.mixer.selected_control = ChannelControl::Fader;
                                }
                                _ => self.mixer.select_prev_control(false),
                            }
                        }
                        SelectedPane::DeckC => match self.mixer.selected_control {
                            ChannelControl::Bpm | ChannelControl::Key => {
                                self.mixer.selected_control = ChannelControl::PlayPause;
                            }
                            ChannelControl::PlayPause
                            | ChannelControl::PrevTrack
                            | ChannelControl::NextTrack => {
                                let scrub_visible = self
                                    .mixer
                                    .selected_channel()
                                    .map(|ch| ch.scrub_available())
                                    .unwrap_or(false);
                                self.mixer.selected_control = if scrub_visible {
                                    ChannelControl::Scrub
                                } else {
                                    ChannelControl::CueOutputSelect
                                };
                            }
                            ChannelControl::Scrub => {
                                self.mixer.selected_control = ChannelControl::CueOutputSelect;
                            }
                            ChannelControl::CueOutputSelect => {
                                self.mixer.selected_control = ChannelControl::Solo;
                            }
                            ChannelControl::Solo => {
                                self.mixer.selected_control = ChannelControl::Mute;
                            }
                            ChannelControl::Mute => {
                                self.mixer.selected_control = ChannelControl::Fader;
                            }
                            ChannelControl::CueSendToA => {
                                self.mixer.selected_control = ChannelControl::Fader;
                            }
                            _ => self.mixer.select_prev_control(true),
                        },
                        _ => {}
                    }
                }
            }
            SelectedPane::DjCenter => {
                if let Some(pad_idx) = self.selected_pad_idx {
                    let row = pad_idx / 4;
                    let col = pad_idx % 4;
                    // Round-robin: wrap from top row to bottom row
                    let new_row = if row == 0 { 3 } else { row - 1 };
                    self.selected_pad_idx = Some(new_row * 4 + col);
                } else if self.mixer.selected_global == GlobalControl::Crossfader {
                    // Crossfader → bottom pad row
                    self.selected_pad_idx = Some(12);
                } else {
                    // Top controls → crossfader
                    self.mixer.selected_global = GlobalControl::Crossfader;
                }
            }
            SelectedPane::Loops => {
                if self.sequence_state.selected.is_some() || self.sequence_state.global_focused {
                    self.sequence_state.select_up();
                    self.ensure_sequence_selected_visible();
                } else if !self.sequence_state.sequences.is_empty() {
                    self.sequence_state.selected = Some(self.sequence_state.sequences.len() - 1);
                    self.sequence_state.global_focused = false;
                    self.ensure_sequence_selected_visible();
                }
            }
            SelectedPane::Crossfader => {} // Single control, nothing to navigate
            SelectedPane::Master => {
                if self.mixer.selected_global.eq_band_index().is_some() {
                    // From any EQ band: jump directly to master play/pause
                    self.mixer.selected_global = GlobalControl::MasterPlayPause;
                } else {
                    self.mixer.selected_global = match self.mixer.selected_global {
                        GlobalControl::MasterPlayPause => GlobalControl::MasterOutputSelect,
                        GlobalControl::MicMute => GlobalControl::MasterEq16k,
                        GlobalControl::MicFader => GlobalControl::MasterEq16k,
                        GlobalControl::MicInputSelect => GlobalControl::MicMute,
                        GlobalControl::MasterFader => GlobalControl::MicInputSelect,
                        GlobalControl::MasterMute => GlobalControl::MasterFader,
                        GlobalControl::MasterOutputSelect => GlobalControl::MasterFader,
                        other => other,
                    };
                }
            }
        }
    }

    /// Navigate left within DJ center (CUE/PH/BT are horizontal, pads too)
    fn navigate_control_left(&mut self) {
        if self.selected_pane == SelectedPane::Loops
            && (self.sequence_state.selected.is_some() || self.sequence_state.global_focused)
        {
            if !self.sequence_state.global_focused {
                self.sequence_state.cursor = self.sequence_state.cursor.left();
            } else {
                self.sequence_state.select_control_up();
            }
            return;
        }
        if self.selected_pane == SelectedPane::Master {
            if self.mixer.selected_global.eq_band_index().is_some() {
                let eq_variants = GlobalControl::all_eq_variants();
                let current_idx = eq_variants
                    .iter()
                    .position(|v| *v == self.mixer.selected_global)
                    .unwrap_or(0);
                let prev_idx = if current_idx == 0 { 9 } else { current_idx - 1 };
                self.mixer.selected_global = eq_variants[prev_idx];
                return;
            }
            match self.mixer.selected_global {
                GlobalControl::MasterFader => {
                    self.mixer.selected_global = GlobalControl::MicFader;
                }
                GlobalControl::MicFader => {
                    self.mixer.selected_global = GlobalControl::MicMute;
                }
                GlobalControl::MicMute => {
                    self.mixer.selected_global = GlobalControl::MicFader;
                }
                // Button row: M | OUT (left navigation)
                GlobalControl::MasterMute => {
                    self.mixer.selected_global = GlobalControl::MasterOutputSelect;
                }
                GlobalControl::MasterOutputSelect => {
                    self.mixer.selected_global = GlobalControl::MasterMute;
                }
                GlobalControl::MasterPlayPause => {}
                GlobalControl::MicInputSelect => {} // INPUT is alone, no h/l nav
                _ => {}
            }
        } else if self.selected_pane == SelectedPane::DjCenter {
            if let Some(pad_idx) = self.selected_pad_idx {
                let row = pad_idx / 4;
                let col = pad_idx % 4;
                // Round-robin: wrap from col 0 to col 3 on same row
                let new_col = if col == 0 { 3 } else { col - 1 };
                self.selected_pad_idx = Some(row * 4 + new_col);
            } else {
                // Round-robin for DJ center top controls
                self.mixer.selected_global = match self.mixer.selected_global {
                    GlobalControl::HeadphoneVolume => GlobalControl::HeadphoneVolume,
                    other => other,
                };
            }
        } else if self.selected_pane == SelectedPane::DeckC {
            // CUE deck: special navigation for swapped layout
            if self.mixer.is_in_eq_or_filter_section() {
                // EQ/Filter section: move left
                self.mixer.selected_control = match self.mixer.selected_control {
                    // L/M/H: move left between bands
                    ChannelControl::EqLow => ChannelControl::FilterCutoff,
                    ChannelControl::EqLowKill => ChannelControl::EqLowKill,
                    ChannelControl::EqMid => ChannelControl::EqLow,
                    ChannelControl::EqMidKill => ChannelControl::EqLowKill,
                    ChannelControl::EqHigh => ChannelControl::EqMid,
                    ChannelControl::EqHighKill => ChannelControl::EqMidKill,
                    // HPF/LPF: move left to H
                    ChannelControl::FilterCutoff => ChannelControl::EqHigh,
                    ChannelControl::FilterFreq => ChannelControl::EqHigh,
                    ChannelControl::LfoShape => ChannelControl::EqHigh,
                    ChannelControl::LfoSpeed => ChannelControl::EqHigh,
                    _ => self.mixer.selected_control,
                };
            } else {
                match self.mixer.selected_control {
                    ChannelControl::Mute => {
                        // M → swap A (same visual row)
                        self.mixer.selected_control = ChannelControl::CueSendToA;
                    }
                    ChannelControl::CueSendToA => {
                        // swap A → M (same visual row)
                        self.mixer.selected_control = ChannelControl::Mute;
                    }
                    ChannelControl::Solo => {
                        // S → swap B (same visual row)
                        self.mixer.selected_control = ChannelControl::CueSendToB;
                    }
                    ChannelControl::CueSendToB => {
                        // swap B → S (same visual row)
                        self.mixer.selected_control = ChannelControl::Solo;
                    }
                    _ => {}
                }
            }
        } else if self.selected_pane == SelectedPane::DeckA
            || self.selected_pane == SelectedPane::DeckB
        {
            if self.mixer.selected_control == ChannelControl::PlayPause {
                let can_nav = self
                    .mixer
                    .selected_channel()
                    .map(|c| c.connected && !c.uses_supercollider)
                    .unwrap_or(false);
                if can_nav {
                    self.mixer.selected_control = ChannelControl::PrevTrack;
                    return;
                }
            } else if self.mixer.selected_control == ChannelControl::NextTrack {
                self.mixer.selected_control = ChannelControl::PlayPause;
                return;
            }

            if self.mixer.is_in_eq_or_filter_section() {
                // EQ/Filter section: move left
                self.mixer.selected_control = match self.mixer.selected_control {
                    // L/M/H: move left between bands
                    ChannelControl::EqLow => ChannelControl::FilterCutoff,
                    ChannelControl::EqLowKill => ChannelControl::EqLowKill,
                    ChannelControl::EqMid => ChannelControl::EqLow,
                    ChannelControl::EqMidKill => ChannelControl::EqLowKill,
                    ChannelControl::EqHigh => ChannelControl::EqMid,
                    ChannelControl::EqHighKill => ChannelControl::EqMidKill,
                    // Filter: move left to H
                    ChannelControl::FilterCutoff => ChannelControl::EqHigh,
                    ChannelControl::FilterFreq => ChannelControl::EqHigh,
                    ChannelControl::LfoShape => ChannelControl::EqHigh,
                    ChannelControl::LfoSpeed => ChannelControl::EqHigh,
                    _ => self.mixer.selected_control,
                };
            } else {
                match self.mixer.selected_control {
                    ChannelControl::Bpm => {
                        self.mixer.selected_control = ChannelControl::Key;
                    }
                    ChannelControl::Key => {
                        self.mixer.selected_control = ChannelControl::Bpm;
                    }
                    ChannelControl::Mute | ChannelControl::Solo => {
                        // Toggle between Mute and Solo
                        self.mixer.selected_control =
                            if self.mixer.selected_control == ChannelControl::Mute {
                                ChannelControl::Solo
                            } else {
                                ChannelControl::Mute
                            };
                    }
                    _ => {}
                }
            }
        }
    }

    /// Navigate right within DJ center
    fn navigate_control_right(&mut self) {
        if self.selected_pane == SelectedPane::Loops
            && (self.sequence_state.selected.is_some() || self.sequence_state.global_focused)
        {
            if !self.sequence_state.global_focused {
                self.sequence_state.cursor = self.sequence_state.cursor.right();
            } else {
                self.sequence_state.select_control_down();
            }
            return;
        }
        if self.selected_pane == SelectedPane::Master {
            if self.mixer.selected_global.eq_band_index().is_some() {
                let eq_variants = GlobalControl::all_eq_variants();
                let current_idx = eq_variants
                    .iter()
                    .position(|v| *v == self.mixer.selected_global)
                    .unwrap_or(0);
                let next_idx = (current_idx + 1) % 10;
                self.mixer.selected_global = eq_variants[next_idx];
                return;
            }
            match self.mixer.selected_global {
                GlobalControl::MasterFader => {
                    self.mixer.selected_global = GlobalControl::MicMute;
                }
                GlobalControl::MicFader => {
                    self.mixer.selected_global = GlobalControl::MicMute;
                }
                GlobalControl::MicMute => {
                    self.mixer.selected_global = GlobalControl::MicFader;
                }
                // Button row: M | OUT (right navigation)
                GlobalControl::MasterMute => {
                    self.mixer.selected_global = GlobalControl::MasterOutputSelect;
                }
                GlobalControl::MasterOutputSelect => {
                    self.mixer.selected_global = GlobalControl::MasterMute;
                }
                GlobalControl::MasterPlayPause => {}
                GlobalControl::MicInputSelect => {} // INPUT is alone, no h/l nav
                _ => {}
            }
        } else if self.selected_pane == SelectedPane::DjCenter {
            if let Some(pad_idx) = self.selected_pad_idx {
                let row = pad_idx / 4;
                let col = pad_idx % 4;
                let new_col = if col == 3 { 0 } else { col + 1 };
                self.selected_pad_idx = Some(row * 4 + new_col);
            } else {
                self.mixer.selected_global = match self.mixer.selected_global {
                    GlobalControl::HeadphoneVolume => GlobalControl::HeadphoneVolume,
                    other => other,
                };
            }
        } else if self.selected_pane == SelectedPane::DeckC {
            // CUE deck: special navigation for swapped layout
            if self.mixer.is_in_eq_or_filter_section() {
                // EQ/Filter section: move right
                self.mixer.selected_control = match self.mixer.selected_control {
                    // L/M/H: move right between bands
                    ChannelControl::EqLow => ChannelControl::EqMid,
                    ChannelControl::EqLowKill => ChannelControl::EqMidKill,
                    ChannelControl::EqMid => ChannelControl::EqHigh,
                    ChannelControl::EqMidKill => ChannelControl::EqHighKill,
                    ChannelControl::EqHigh => ChannelControl::FilterCutoff,
                    ChannelControl::EqHighKill => ChannelControl::EqHighKill,
                    // Filter: move right to L
                    ChannelControl::FilterCutoff => ChannelControl::EqLow,
                    ChannelControl::FilterFreq => ChannelControl::EqLow,
                    ChannelControl::LfoShape => ChannelControl::EqLow,
                    ChannelControl::LfoSpeed => ChannelControl::EqLow,
                    _ => self.mixer.selected_control,
                };
            } else {
                match self.mixer.selected_control {
                    ChannelControl::Mute => {
                        // M → swap A (same visual row)
                        self.mixer.selected_control = ChannelControl::CueSendToA;
                    }
                    ChannelControl::CueSendToA => {
                        // swap A → M (same visual row)
                        self.mixer.selected_control = ChannelControl::Mute;
                    }
                    ChannelControl::Solo => {
                        // S → swap B (same visual row)
                        self.mixer.selected_control = ChannelControl::CueSendToB;
                    }
                    ChannelControl::CueSendToB => {
                        // swap B → S (same visual row)
                        self.mixer.selected_control = ChannelControl::Solo;
                    }
                    _ => {}
                }
            }
        } else if self.selected_pane == SelectedPane::DeckA
            || self.selected_pane == SelectedPane::DeckB
        {
            if self.mixer.selected_control == ChannelControl::PlayPause {
                let can_nav = self
                    .mixer
                    .selected_channel()
                    .map(|c| c.connected && !c.uses_supercollider)
                    .unwrap_or(false);
                if can_nav {
                    self.mixer.selected_control = ChannelControl::NextTrack;
                    return;
                }
            } else if self.mixer.selected_control == ChannelControl::PrevTrack {
                self.mixer.selected_control = ChannelControl::PlayPause;
                return;
            }

            if self.mixer.is_in_eq_or_filter_section() {
                // EQ/Filter section: move right
                self.mixer.selected_control = match self.mixer.selected_control {
                    // L/M/H: move right between bands
                    ChannelControl::EqLow => ChannelControl::EqMid,
                    ChannelControl::EqLowKill => ChannelControl::EqMidKill,
                    ChannelControl::EqMid => ChannelControl::EqHigh,
                    ChannelControl::EqMidKill => ChannelControl::EqHighKill,
                    ChannelControl::EqHigh => ChannelControl::FilterCutoff,
                    ChannelControl::EqHighKill => ChannelControl::EqHighKill,
                    // Filter: move right to L
                    ChannelControl::FilterCutoff => ChannelControl::EqLow,
                    ChannelControl::FilterFreq => ChannelControl::EqLow,
                    ChannelControl::LfoShape => ChannelControl::EqLow,
                    ChannelControl::LfoSpeed => ChannelControl::EqLow,
                    _ => self.mixer.selected_control,
                };
            } else {
                match self.mixer.selected_control {
                    ChannelControl::Bpm => {
                        self.mixer.selected_control = ChannelControl::Key;
                    }
                    ChannelControl::Key => {
                        self.mixer.selected_control = ChannelControl::Bpm;
                    }
                    ChannelControl::Mute | ChannelControl::Solo => {
                        self.mixer.selected_control =
                            if self.mixer.selected_control == ChannelControl::Mute {
                                ChannelControl::Solo
                            } else {
                                ChannelControl::Mute
                            };
                    }
                    _ => {}
                }
            }
        }
    }

    /// Check if current control is continuous (vs toggle)
    fn is_current_control_continuous(&self) -> bool {
        match self.mixer.focus {
            SelectionFocus::Channel(_) => self.mixer.selected_control.is_continuous(),
            SelectionFocus::Global => {
                self.mixer.selected_global.eq_band_index().is_some()
                    || matches!(
                        self.mixer.selected_global,
                        GlobalControl::Crossfader
                            | GlobalControl::HeadphoneVolume
                            | GlobalControl::MasterFader
                            | GlobalControl::MicFader
                    )
            }
        }
    }

    /// Toggle current control (for buttons)
    fn toggle_current_control(&mut self) {
        match self.mixer.focus {
            SelectionFocus::Channel(ch_idx) => {
                let control = self.mixer.selected_control;
                if control == ChannelControl::Solo {
                    let was_solo = self
                        .mixer
                        .get_channel(ch_idx)
                        .map(|c| c.solo)
                        .unwrap_or(false);
                    if !was_solo {
                        // Turning solo ON: save this channel's fader, restore any other soloed decks
                        self.mixer.save_fader_for_solo(ch_idx);
                        for i in 0..self.mixer.channels.len() {
                            if i != ch_idx && self.mixer.channels[i].solo {
                                self.mixer.channels[i].solo = false;
                                self.mixer.restore_fader_from_solo(i);
                            }
                        }
                        if self.mixer.cue_channel.solo {
                            self.mixer.cue_channel.solo = false;
                            self.mixer.restore_fader_from_solo(2);
                        }
                        self.mixer.toggle_selected();
                    } else {
                        // Turning solo OFF: restore this channel's fader
                        self.mixer.toggle_selected();
                        self.mixer.restore_fader_from_solo(ch_idx);
                    }
                    self.mixer.solo_active =
                        self.mixer.channels.iter().any(|c| c.solo) || self.mixer.cue_channel.solo;
                } else {
                    self.mixer.toggle_selected();
                }

                // Sync to MPV after toggle
                match control {
                    ChannelControl::PlayPause => {
                        self.sync_playpause_to_mpv(ch_idx);
                    }
                    ChannelControl::Mute => {
                        self.sync_mute_to_mpv(ch_idx);
                    }
                    ChannelControl::EqLowKill
                    | ChannelControl::EqMidKill
                    | ChannelControl::EqHighKill => {
                        self.sync_eq_to_mpv(ch_idx);
                    }
                    ChannelControl::Solo => {
                        self.mixer.solo_active = self.mixer.channels.iter().any(|c| c.solo)
                            || self.mixer.cue_channel.solo;
                        self.sync_solo_to_all_mpv();
                    }
                    _ => {}
                }
            }
            SelectionFocus::Global => match self.mixer.selected_global {
                GlobalControl::MasterMute => {
                    self.mixer.master.muted = !self.mixer.master.muted;
                    self.sync_deck_volume(true);
                    self.sync_deck_volume(false);
                }
                GlobalControl::MasterPlayPause => {
                    self.mixer.master.playing = !self.mixer.master.playing;
                    self.sync_all_playpause();
                }
                GlobalControl::MicMute => {
                    self.mixer.master.mic_muted = !self.mixer.master.mic_muted;
                    self.sync_mic_to_engine();
                }
                _ => {}
            },
        }
    }

    /// Mode 3: Edit - hjkl adjusts values, Esc returns to ControlSelect
    fn handle_edit_key(&mut self, key: KeyEvent) {
        match key.code {
            // Exit edit mode -> back to control select (or pane select for Crossfader)
            KeyCode::Esc | KeyCode::Enter => {
                if self.selected_pane == SelectedPane::Crossfader {
                    // Crossfader skipped ControlSelect on entry, skip it on exit too
                    self.mode = AppMode::PaneSelect;
                } else {
                    self.mode = AppMode::ControlSelect;
                }
            }

            // Tab: next pane (quick switch while editing, round-robin)
            KeyCode::Tab => {
                self.selected_pad_idx = None;
                self.selected_pane = self.selected_pane.next();
                self.sync_pane_to_mixer();
            }

            // Shift+Tab: previous pane (quick switch while editing, round-robin)
            KeyCode::BackTab => {
                self.selected_pad_idx = None;
                self.selected_pane = self.selected_pane.prev();
                self.sync_pane_to_mixer();
            }

            // Adjust values with hjkl
            KeyCode::Char('h') | KeyCode::Left => {
                if self.selected_pane == SelectedPane::Loops {
                    self.adjust_sequence_control(-0.05, -1.0);
                } else if self.mixer.selected_control == ChannelControl::Scrub {
                    self.scrub_tap(-1.0);
                } else {
                    self.mixer.adjust_selected(-0.05);
                    self.sync_current_control_to_mpv();
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if self.selected_pane == SelectedPane::Loops {
                    self.adjust_sequence_control(0.05, 1.0);
                } else if self.mixer.selected_control == ChannelControl::Scrub {
                    self.scrub_tap(1.0);
                } else {
                    self.mixer.adjust_selected(0.05);
                    self.sync_current_control_to_mpv();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.selected_pane == SelectedPane::Loops {
                    self.adjust_sequence_control(0.05, 1.0);
                } else if self.mixer.selected_control == ChannelControl::Scrub {
                    self.scrub_tap(1.0);
                } else {
                    self.mixer.adjust_selected(0.05);
                    self.sync_current_control_to_mpv();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.selected_pane == SelectedPane::Loops {
                    self.adjust_sequence_control(-0.05, -1.0);
                } else if self.mixer.selected_control == ChannelControl::Scrub {
                    self.scrub_tap(-1.0);
                } else {
                    self.mixer.adjust_selected(-0.05);
                    self.sync_current_control_to_mpv();
                }
            }

            // Coarse adjustment with Shift
            KeyCode::Char('H') => {
                if self.selected_pane == SelectedPane::Loops {
                    self.adjust_sequence_control(-0.05, -5.0);
                } else if self.mixer.selected_control == ChannelControl::Scrub {
                    self.start_scrub(-1.0, true);
                } else if self.is_filter_selected() {
                    match self.mixer.selected_control {
                        ChannelControl::FilterCutoff => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.filter_cutoff = 0.0;
                            }
                        }
                        ChannelControl::FilterFreq => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.filter_freq = 0.0;
                            }
                        }
                        ChannelControl::LfoShape => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.lfo_shape = 0.0;
                            }
                        }
                        ChannelControl::LfoSpeed => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.lfo_speed = 0.0;
                            }
                        }
                        _ => {}
                    }
                    self.sync_current_control_to_mpv();
                } else if self.is_pan_selected() {
                    if let Some(channel) = self.mixer.selected_channel_mut() {
                        channel.pan = -1.0;
                    }
                    self.sync_current_control_to_mpv();
                } else {
                    self.mixer.adjust_selected(-0.2);
                    self.sync_current_control_to_mpv();
                }
            }
            KeyCode::Char('L') => {
                if self.selected_pane == SelectedPane::Loops {
                    self.adjust_sequence_control(0.05, 5.0);
                } else if self.mixer.selected_control == ChannelControl::Scrub {
                    self.start_scrub(1.0, true);
                } else if self.is_filter_selected() {
                    match self.mixer.selected_control {
                        ChannelControl::FilterCutoff => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.filter_cutoff = 1.0;
                            }
                        }
                        ChannelControl::FilterFreq => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.filter_freq = 1.0;
                            }
                        }
                        ChannelControl::LfoShape => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.lfo_shape = 1.0;
                            }
                        }
                        ChannelControl::LfoSpeed => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.lfo_speed = 1.0;
                            }
                        }
                        _ => {}
                    }
                    self.sync_current_control_to_mpv();
                } else if self.is_pan_selected() {
                    if let Some(channel) = self.mixer.selected_channel_mut() {
                        channel.pan = 1.0;
                    }
                    self.sync_current_control_to_mpv();
                } else {
                    self.mixer.adjust_selected(0.2);
                    self.sync_current_control_to_mpv();
                }
            }
            KeyCode::Char('K') => {
                if self.selected_pane == SelectedPane::Loops {
                    self.adjust_sequence_control(0.05, 5.0);
                } else if self.mixer.selected_control == ChannelControl::Scrub {
                    self.start_scrub(1.0, true);
                } else if self.is_volume_fader_selected() {
                    self.mixer.adjust_selected(0.25);
                } else {
                    self.mixer.adjust_selected(0.2);
                }
                self.sync_current_control_to_mpv();
            }
            KeyCode::Char('J') => {
                if self.selected_pane == SelectedPane::Loops {
                    self.adjust_sequence_control(-0.05, -5.0);
                } else if self.mixer.selected_control == ChannelControl::Scrub {
                    self.start_scrub(-1.0, true);
                } else if self.is_volume_fader_selected() {
                    self.mixer.adjust_selected(-0.25);
                } else {
                    self.mixer.adjust_selected(-0.2);
                }
                self.sync_current_control_to_mpv();
            }

            // Crossfader slam shortcuts (when crossfader is active in Edit mode)
            KeyCode::Char('a') => {
                if matches!(self.mixer.focus, SelectionFocus::Global)
                    && matches!(self.mixer.selected_global, GlobalControl::Crossfader)
                {
                    self.mixer.dj.crossfader = -1.0; // Slam to full A
                    self.sync_current_control_to_mpv();
                }
            }
            KeyCode::Char('b') => {
                if matches!(self.mixer.focus, SelectionFocus::Global)
                    && matches!(self.mixer.selected_global, GlobalControl::Crossfader)
                {
                    self.mixer.dj.crossfader = 1.0; // Slam to full B
                    self.sync_current_control_to_mpv();
                }
            }

            // Reset
            KeyCode::Char('0') => {
                if self.selected_pane == SelectedPane::Loops {
                    self.reset_sequence_control();
                } else {
                    self.reset_current_control();
                    self.sync_current_control_to_mpv();
                }
            }

            // Center (for pan/crossfader) or reset to default
            KeyCode::Char('c') | KeyCode::Char('C') => {
                match self.mixer.focus {
                    SelectionFocus::Channel(_) => match self.mixer.selected_control {
                        ChannelControl::Pan => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.pan = 0.0;
                            }
                        }
                        ChannelControl::Scrub => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.playback_speed = 1.0;
                                channel.scrub_direction = 0.0;
                                channel.scrub_speed = 0.0;
                                channel.scrub_accumulator = 0.0;
                            }
                        }
                        ChannelControl::Bpm => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.target_bpm = if channel.base_bpm > 0.0 {
                                    channel.base_bpm
                                } else {
                                    120.0
                                };
                            }
                        }
                        ChannelControl::Key => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.key_offset = 0;
                                let semitone_factor = 1.0;
                                channel.playback_speed = semitone_factor;
                            }
                        }
                        ChannelControl::Fader => {
                            if let Some(channel) = self.mixer.selected_channel_mut() {
                                channel.fader = 0.5;
                            }
                        }
                        _ => {}
                    },
                    SelectionFocus::Global => match self.mixer.selected_global {
                        GlobalControl::Crossfader => {
                            self.mixer.dj.crossfader = 0.0;
                        }
                        GlobalControl::MasterFader => {
                            self.mixer.master.fader = 0.5;
                        }
                        GlobalControl::MicFader => {
                            self.mixer.master.mic_fader = 0.5;
                        }
                        _ => {}
                    },
                }
                self.sync_current_control_to_mpv();
            }

            _ => {}
        }
    }

    fn reset_current_control(&mut self) {
        match self.mixer.focus {
            SelectionFocus::Channel(_) => {
                let control = self.mixer.selected_control;
                if let Some(channel) = self.mixer.selected_channel_mut() {
                    match control {
                        ChannelControl::Fader => channel.fader = 0.5,
                        ChannelControl::Pan => channel.pan = 0.0,
                        ChannelControl::Scrub => {
                            channel.playback_speed = 1.0;
                            channel.scrub_direction = 0.0;
                            channel.scrub_speed = 0.0;
                            channel.scrub_accumulator = 0.0;
                        }
                        ChannelControl::Bpm => {
                            // Reset to x1.00: target_bpm = base BPM from first detection
                            channel.target_bpm = if channel.base_bpm > 0.0 {
                                channel.base_bpm
                            } else {
                                120.0
                            };
                        }
                        ChannelControl::Key => {
                            // Reset key to unknown (detection will re-populate)
                            channel.key = None;
                            channel.key_offset = 0;
                            channel.playback_speed = 1.0;
                        }
                        ChannelControl::FilterCutoff => channel.filter_cutoff = 0.0,
                        ChannelControl::FilterFreq => channel.filter_freq = 0.5,
                        ChannelControl::LfoShape => channel.lfo_shape = 0.5,
                        ChannelControl::LfoSpeed => channel.lfo_speed = 0.0,
                        ChannelControl::EqLow => channel.eq_low = 0.0,
                        ChannelControl::EqMid => channel.eq_mid = 0.0,
                        ChannelControl::EqHigh => channel.eq_high = 0.0,
                        _ => {}
                    }
                }
            }
            SelectionFocus::Global => match self.mixer.selected_global {
                GlobalControl::HeadphoneVolume => self.mixer.dj.headphone_volume = 1.0,
                GlobalControl::MasterFader => self.mixer.master.fader = 0.5,
                GlobalControl::MicFader => self.mixer.master.mic_fader = 0.5,
                GlobalControl::Crossfader => self.mixer.dj.crossfader = 0.0,
                ctrl if ctrl.eq_band_index().is_some() => {
                    let idx = ctrl.eq_band_index().unwrap();
                    self.mixer.master.master_eq[idx] = 0.0;
                }
                _ => {}
            },
        }
    }

    /// Reset all controls on the currently selected deck to defaults
    fn reset_deck_to_defaults(&mut self) {
        let ch_idx = match self.mixer.focus {
            SelectionFocus::Channel(ch) => ch,
            _ => return,
        };
        if let Some(channel) = self.mixer.get_channel_mut(ch_idx) {
            channel.fader = 0.5;
            channel.pan = 0.0;
            channel.eq_low = 0.0;
            channel.eq_mid = 0.0;
            channel.eq_high = 0.0;
            channel.eq_low_kill = false;
            channel.eq_mid_kill = false;
            channel.eq_high_kill = false;
            channel.filter_cutoff = 0.0;
            channel.filter_freq = 0.5;
            channel.muted = false;
            channel.solo = false;
            channel.target_bpm = if channel.base_bpm > 0.0 {
                channel.base_bpm
            } else {
                120.0
            };
            channel.scrub_direction = 0.0;
            channel.scrub_speed = 0.0;
            channel.scrub_accumulator = 0.0;
            channel.playback_speed = 1.0;
        }
        // Sync all controls to MPV/SC
        self.sync_volume_to_mpv(ch_idx);
        self.sync_pan_to_mpv(ch_idx);
        self.sync_eq_to_mpv(ch_idx);
        self.sync_filter_to_mpv(ch_idx);
        self.sync_mute_to_mpv(ch_idx);
        self.sync_bpm_to_mpv(ch_idx);
        self.mixer.solo_active = false;
    }

    /// Reset all controls across all decks, master, and DJ section to defaults.
    /// Sources remain connected.
    fn reset_all_controls(&mut self) {
        // Reset all deck channels (preserve connection state)
        for i in 0..self.mixer.channels.len() {
            let ch = &mut self.mixer.channels[i];
            // Save connection state
            let connected = ch.connected;
            let source_id = ch.source_id.clone();
            let playing = ch.playing;
            let bpm = ch.bpm;
            let key = ch.key.clone();
            let key_offset = ch.key_offset;
            let name = ch.name.clone();
            let index = ch.index;
            let uses_supercollider = ch.uses_supercollider;
            let duration = ch.duration;
            let time_pos = ch.time_pos;
            let base_bpm = ch.base_bpm;
            let target_bpm = ch.target_bpm;
            let playback_speed = ch.playback_speed;
            let spectrum_peaks = ch.spectrum_peaks;
            let spectrum_decay = ch.spectrum_decay;

            *ch = crate::state::MixerChannel::new(name, index);

            // Restore connection state
            ch.connected = connected;
            ch.source_id = source_id;
            ch.playing = playing;
            ch.bpm = bpm;
            ch.key = key;
            ch.key_offset = key_offset;
            ch.uses_supercollider = uses_supercollider;
            ch.duration = duration;
            ch.time_pos = time_pos;
            ch.base_bpm = base_bpm;
            ch.target_bpm = target_bpm;
            ch.playback_speed = playback_speed;
            ch.spectrum_peaks = spectrum_peaks;
            ch.spectrum_decay = spectrum_decay;
        }
        // Reset CUE channel
        let mut cue = crate::state::MixerChannel::new("CUE", 2);
        cue.pfl = true;
        self.mixer.cue_channel = cue;

        // Reset DJ section
        self.mixer.dj.crossfader = 0.0;
        self.mixer.dj.headphone_volume = 0.75;
        self.mixer.solo_active = false;
        self.mixer.pre_solo_faders.clear();
        self.mixer.pre_solo_cue_fader = None;

        // Reset master
        self.mixer.master.fader = 0.5;
        self.mixer.master.mic_fader = 0.5;
        self.mixer.master.mic_muted = true;
        self.mixer.master.muted = false;
        self.mixer.master.playing = true;
        self.mixer.master.master_eq = [0.0; 10];

        self.sequence_state.global.mute = self.sequence_state.master_previously_global_mute;
        for (i, seq) in self.sequence_state.sequences.iter_mut().enumerate() {
            if i < self.sequence_state.master_previously_playing.len() {
                seq.playing = self.sequence_state.master_previously_playing[i];
            }
        }

        // Sync all controls to MPV instances
        for i in 0..self.mixer.channels.len() {
            self.sync_volume_to_mpv(i);
            self.sync_pan_to_mpv(i);
            self.sync_eq_to_mpv(i);
            self.sync_filter_to_mpv(i);
            self.sync_mute_to_mpv(i);
        }
        // Reset MPV instances fully
        for c in [
            &mut self.mpv_deck_a,
            &mut self.mpv_deck_b,
            &mut self.mpv_deck_c,
        ]
        .into_iter()
        .flatten()
        {
            c.reset_all();
            let _ = c.ensure_astats();
        }
        // Reset Rust audio engine state
        if let Some(ref engine) = self.audio_engine {
            for i in 0..self.mixer.channels.len() {
                engine.state.set_volume(i, 0.5);
                engine.state.set_muted(i, false);
            }
            engine.set_mic_gain(1.0);
            engine.set_mic_muted(true);
        }
    }

    /// Check if the currently selected control is a volume fader
    fn is_volume_fader_selected(&self) -> bool {
        match self.mixer.focus {
            SelectionFocus::Channel(_) => {
                matches!(self.mixer.selected_control, ChannelControl::Fader)
            }
            SelectionFocus::Global => {
                matches!(
                    self.mixer.selected_global,
                    GlobalControl::MasterFader | GlobalControl::MicFader
                )
            }
        }
    }

    /// Check if the currently selected control is a filter or LFO
    fn is_filter_selected(&self) -> bool {
        matches!(self.mixer.focus, SelectionFocus::Channel(_))
            && matches!(
                self.mixer.selected_control,
                ChannelControl::FilterCutoff
                    | ChannelControl::FilterFreq
                    | ChannelControl::LfoShape
                    | ChannelControl::LfoSpeed
            )
    }

    /// Check if the currently selected control is pan
    fn is_pan_selected(&self) -> bool {
        matches!(self.mixer.focus, SelectionFocus::Channel(_))
            && matches!(self.mixer.selected_control, ChannelControl::Pan)
    }

    /// Handle keys when sample pad mode is active
    fn handle_pad_key(&mut self, key: KeyEvent) {
        match key.code {
            // Exit pad mode (recording already committed by global handler if active)
            KeyCode::Esc | KeyCode::Enter => {
                self.mode = AppMode::PaneSelect;
                self.sample_pads.active = false;
            }

            // Toggle pad mode off
            KeyCode::Char('p') | KeyCode::Char('P') => {
                self.mode = AppMode::PaneSelect;
                self.sample_pads.active = false;
            }

            // Enter config mode
            KeyCode::Char('c') | KeyCode::Char('C') => {
                self.mode = AppMode::SamplePadConfig;
                self.sample_pads.config_mode = true;
                // Always focus the first control (Sample) when opening pad config
                self.sample_pads.selected_control = PadControl::Sample;
            }

            // Stop all pads
            KeyCode::Char(' ') => {
                self.stop_all_samples();
            }

            // Pad trigger keys: 4567 / RTYU / FGHJ / VBNM
            KeyCode::Char(c) => {
                if let Some(pad_idx) = self.sample_pads.trigger_by_key(c) {
                    self.play_sample(pad_idx);
                }
            }

            _ => {}
        }
    }

    /// Play a sample using the audio engine's pad voice system (routes through cpal output)
    fn play_sample(&mut self, pad_idx: usize) {
        if let Some(pad) = self.sample_pads.pads.get(pad_idx)
            && pad.sample_path.is_some()
        {
            if let Some(ref engine) = self.audio_engine
                && pad_idx < engine.pad_triggers.len()
            {
                engine.pad_triggers[pad_idx].store(true, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            // Fallback to mpv if audio engine unavailable
            if let Some(sample_path) = &pad.sample_path
                && sample_path.exists()
            {
                let _ = std::process::Command::new("mpv")
                    .arg("--no-video")
                    .arg("--really-quiet")
                    .arg("--no-terminal")
                    .arg(sample_path)
                    .spawn();
            }
        }
    }

    /// Stop all playing samples
    fn stop_all_samples(&mut self) {
        self.sample_pads.stop_all();
        if let Some(ref mut engine) = self.sample_engine {
            engine.stop_all();
        }
    }
    /// Ensure the selected sequence row is visible within the scroll viewport
    fn ensure_sequence_selected_visible(&mut self) {
        let visible_rows = if let Some(area) = &self.loops_area {
            // inner height = area.h - 2 (borders), minus 1 for separator, minus 1 for top bar
            area.h.saturating_sub(4) as usize
        } else {
            8
        };
        if visible_rows == 0 {
            return;
        }
        if let Some(sel) = self.sequence_state.selected {
            if sel < self.sequence_state.scroll_offset {
                self.sequence_state.scroll_offset = sel;
            } else if sel >= self.sequence_state.scroll_offset + visible_rows {
                self.sequence_state.scroll_offset = sel.saturating_sub(visible_rows - 1);
            }
        }
    }

    fn adjust_sequence_control(&mut self, delta: f32, coarse_delta: f32) {
        if self.sequence_state.global_focused {
            match self.sequence_state.global_control {
                crate::state::GlobalSequenceControl::Volume => {
                    self.sequence_state.global.volume =
                        (self.sequence_state.global.volume + delta).clamp(0.0, 1.0);
                }
                crate::state::GlobalSequenceControl::Bpm => {
                    self.sequence_state.global.bpm =
                        (self.sequence_state.global.bpm + coarse_delta).clamp(20.0, 400.0);
                }
                _ => {}
            }
        }
    }

    /// Reset the currently selected sequence control to its default value
    fn reset_sequence_control(&mut self) {
        if self.sequence_state.global_focused {
            match self.sequence_state.global_control {
                crate::state::GlobalSequenceControl::Volume => {
                    self.sequence_state.global.volume = 0.8;
                }
                crate::state::GlobalSequenceControl::Bpm => {
                    self.sequence_state.global.bpm = 120.0;
                }
                _ => {}
            }
        }
    }

    /// Adjust the tempo multiplier for the sequence associated with the selected pad
    fn adjust_sequence_tempo(&mut self, delta: f32) {
        let pad_idx = self.sample_pads.selected_pad;
        if let Some(seq) = self
            .sequence_state
            .sequences
            .iter_mut()
            .find(|s| s.pad_idx == pad_idx)
        {
            seq.tempo = (seq.tempo + delta).clamp(0.25, 4.0);
        }
    }

    /// Reset the tempo multiplier for the sequence associated with the selected pad
    fn reset_sequence_tempo(&mut self) {
        let pad_idx = self.sample_pads.selected_pad;
        if let Some(seq) = self
            .sequence_state
            .sequences
            .iter_mut()
            .find(|s| s.pad_idx == pad_idx)
        {
            seq.tempo = 1.0;
        }
    }

    /// Re-apply DSP to a pad's cached samples in the audio engine
    /// Called when pad config changes (filter, EQ, distortion)
    fn refresh_pad_dsp(&self, pad_idx: usize) {
        let pad_cfg = self.sample_pads.pads[pad_idx].config.clone();
        if let Some(ref engine) = self.audio_engine
            && let Some(ref sample_eng) = self.sample_engine
            && let Some(path) = &self.sample_pads.pads[pad_idx].sample_path
            && let Some(cached) = sample_eng.cache.get(path)
        {
            let processed =
                crate::audio::sample_cache::apply_dsp_to_buffer(&cached.samples, &pad_cfg);
            engine.set_pad_sample(pad_idx, processed, cached.sample_rate, cached.channels);
        }
    }

    /// Update sequence state (called from tick)
    pub fn update_sequences(&mut self) {
        self.frame_counter = self.frame_counter.wrapping_add(1);

        // Sync sequence state to audio engine
        if let Some(ref engine) = self.audio_engine {
            use crate::audio::engine::SequenceSnapshot;
            let global_bpm = self.sequence_state.global.bpm;
            let global_vol = self.sequence_state.global.volume;
            let snapshots: Vec<SequenceSnapshot> = self
                .sequence_state
                .sequences
                .iter()
                .map(|seq| {
                    let pad_config = &self.sample_pads.pads[seq.pad_idx].config;
                    SequenceSnapshot {
                        pad_idx: seq.pad_idx,
                        volume: seq.volume * global_vol,
                        mute: seq.mute || self.sequence_state.global.mute || pad_config.mute,
                        tempo_multiplier: seq.tempo,
                        global_bpm,
                        pattern: seq.pattern,
                        playing: seq.playing
                            && !self.sequence_state.global.mute
                            && !pad_config.mute,
                        pad_volume: pad_config.volume,
                        pad_mute: pad_config.mute,
                    }
                })
                .collect();
            engine.sync_sequences(snapshots);

            // Read current steps from audio callback and update UI
            let steps = engine.read_sequence_steps();
            for (i, seq) in self.sequence_state.sequences.iter_mut().enumerate() {
                if i < steps.len() {
                    seq.current_step = steps[i];
                }
            }
        }

        if self.frame_counter % 100 == 0 {
            let perf_line = format!(
                "perf: seek_in={} seek_ok={} seek_fail={} seek_in2send_max={}ms seek_send2apply_max={}ms time_delta_max={}ms tl_updates={} tl_age_max={}ms meta_polls={} meta_sel={} meta_updates={} meta_fail={} meta_age_max={}ms",
                self.perf_trace.route_seek_input_events,
                self.perf_trace.route_seek_sends,
                self.perf_trace.route_seek_send_failures,
                self.perf_trace.route_seek_input_to_send_max_ms,
                self.perf_trace.route_seek_send_to_apply_max_ms,
                self.perf_trace.route_timepos_delta_max_ms,
                self.perf_trace.timeline_updates,
                self.perf_trace.timeline_age_max_ms,
                self.perf_trace.route_meta_polls,
                self.perf_trace.route_meta_selected_polls,
                self.perf_trace.route_meta_updates,
                self.perf_trace.route_meta_failures,
                self.perf_trace.route_meta_age_max_ms,
            );
            self.log_debug(perf_line.clone());
            if std::env::var("DEBUG").is_ok() {
                eprintln!("{}", perf_line);
            }
            self.perf_trace.reset();
        }
    }

    /// Handle keys when sample pad config mode is active (3-level nav)
    fn handle_pad_config_key(&mut self, key: KeyEvent) {
        // SPACE always previews the selected pad, at any level
        if key.code == KeyCode::Char(' ') {
            let pad_idx = self.sample_pads.selected_pad;
            self.play_sample(pad_idx);
            return;
        }

        if self.sample_pads.editing_control {
            // Level 3: editing a control value
            let is_bpm_mult = self.sample_pads.selected_control == PadControl::BpmMultiplier;
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.sample_pads.editing_control = false;
                    if !is_bpm_mult {
                        self.refresh_pad_dsp(self.sample_pads.selected_pad);
                    }
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    if is_bpm_mult {
                        self.adjust_sequence_tempo(-0.05);
                    } else {
                        self.sample_pads.adjust_selected_config(-0.05);
                    }
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    if is_bpm_mult {
                        self.adjust_sequence_tempo(0.05);
                    } else {
                        self.sample_pads.adjust_selected_config(0.05);
                    }
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if is_bpm_mult {
                        self.adjust_sequence_tempo(-0.05);
                    } else {
                        self.sample_pads.adjust_selected_config(-0.05);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if is_bpm_mult {
                        self.adjust_sequence_tempo(0.05);
                    } else {
                        self.sample_pads.adjust_selected_config(0.05);
                    }
                }
                // Coarse adjustment
                KeyCode::Char('H') => {
                    if is_bpm_mult {
                        self.adjust_sequence_tempo(-0.2);
                    } else {
                        self.sample_pads.adjust_selected_config(-0.2);
                    }
                }
                KeyCode::Char('L') => {
                    if is_bpm_mult {
                        self.adjust_sequence_tempo(0.2);
                    } else {
                        self.sample_pads.adjust_selected_config(0.2);
                    }
                }
                KeyCode::Char('K') => {
                    if is_bpm_mult {
                        self.adjust_sequence_tempo(0.2);
                    } else {
                        self.sample_pads.adjust_selected_config(0.2);
                    }
                }
                KeyCode::Char('J') => {
                    if is_bpm_mult {
                        self.adjust_sequence_tempo(-0.2);
                    } else {
                        self.sample_pads.adjust_selected_config(-0.2);
                    }
                }
                KeyCode::Char('0') => {
                    if is_bpm_mult {
                        self.reset_sequence_tempo();
                    } else {
                        self.sample_pads.reset_selected_config();
                    }
                }
                _ => {}
            }
        } else {
            // Level 2: navigating controls
            match key.code {
                KeyCode::Esc => {
                    // Exit config mode - return to the mode we came from
                    if self.sample_pads.active {
                        // Came from SamplePads (trigger) mode
                        self.mode = AppMode::SamplePads;
                    } else {
                        // Came from ControlSelect mode (navigating pads in DjCenter pane)
                        self.mode = AppMode::ControlSelect;
                    }
                    self.sample_pads.config_mode = false;
                    self.sample_pads.editing_control = false;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.sample_pads.config_control_down();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.sample_pads.config_control_up();
                }
                KeyCode::Enter => {
                    let control = self.sample_pads.selected_control;
                    match control {
                        PadControl::Sample => {
                            let pad_idx = self.sample_pads.selected_pad;
                            self.open_sample_picker(pad_idx);
                        }
                        PadControl::FiltersHeader => {
                            // Non-interactive header, skip
                        }
                        _ if control.is_continuous() => {
                            self.sample_pads.editing_control = true;
                        }
                        _ if control.is_toggle() => {
                            self.sample_pads.toggle_selected_config();
                        }
                        _ => {}
                    }
                }
                KeyCode::Char('0') => {
                    self.sample_pads.reset_selected_config();
                }
                KeyCode::Char(c) => {
                    if let Some(idx) = self.sample_pads.pad_index_for_key(c) {
                        self.sample_pads.selected_pad = idx;
                    }
                }
                _ => {}
            }
        }
    }

    /// Handle mouse events
    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        match self.mode {
            AppMode::SamplePads | AppMode::SamplePadConfig => {
                self.handle_pad_mouse(mouse);
            }
            _ => {
                self.handle_mixer_mouse(mouse);
            }
        }
    }

    fn handle_mixer_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(hit) = self.hit_test_all(mouse.column, mouse.row) {
                    match hit {
                        HitResult::Channel(channel_idx, control) => {
                            self.mixer.selected_channel = channel_idx;
                            self.mixer.selected_control = control;
                            self.mixer.focus = SelectionFocus::Channel(channel_idx);
                            self.selected_pane = match channel_idx {
                                0 => SelectedPane::DeckA,
                                _ => SelectedPane::DeckB,
                            };

                            if control.is_continuous() {
                                self.drag_start_y = Some(mouse.row);
                                self.drag_start_value = self.get_current_control_value();
                            } else {
                                self.mixer.toggle_selected();
                            }
                        }
                        HitResult::Pad(pad_idx) => {
                            // Switch to DJ center pane and trigger pad
                            self.selected_pane = SelectedPane::DjCenter;
                            self.sample_pads.trigger_pad(pad_idx);
                            self.play_sample(pad_idx);
                        }
                        HitResult::Crossfader => {
                            self.selected_pane = SelectedPane::Crossfader;
                            self.mixer.focus = SelectionFocus::Global;
                            self.mixer.selected_global = GlobalControl::Crossfader;
                            // Click-to-position: calculate crossfader value from x
                            if let Some(area) = &self.crossfader_area {
                                let inner_x = mouse.column.saturating_sub(area.x);
                                let pos = (inner_x as f32 / area.w as f32) * 2.0 - 1.0;
                                self.mixer.dj.crossfader = pos.clamp(-1.0, 1.0);
                                self.drag_start_x = Some(mouse.row);
                                self.drag_start_value = Some(self.mixer.dj.crossfader);
                            }
                        }
                        HitResult::Master => {
                            self.selected_pane = SelectedPane::Master;
                            self.mixer.focus = SelectionFocus::Global;
                            // Determine which control based on y position within master area
                            if let Some(area) = &self.master_area {
                                let rel_y = mouse.row.saturating_sub(area.y);
                                let h = area.h;
                                if rel_y <= 4 {
                                    // Play/pause area
                                    self.mixer.selected_global = GlobalControl::MasterPlayPause;
                                    self.mixer.master.playing = !self.mixer.master.playing;
                                    self.sync_all_playpause();
                                } else if rel_y >= h.saturating_sub(3) {
                                    // Two button rows: MIC M/IN on the left, MASTER M/OUT right.
                                    let mid_x = area.x + area.w / 2;
                                    if mouse.column < mid_x {
                                        if rel_y == h.saturating_sub(3) {
                                            self.mixer.selected_global = GlobalControl::MicMute;
                                            self.mixer.master.mic_muted =
                                                !self.mixer.master.mic_muted;
                                            self.sync_mic_to_engine();
                                        } else {
                                            self.mixer.selected_global =
                                                GlobalControl::MicInputSelect;
                                            self.open_output_picker(OutputPickerTarget::Mic);
                                        }
                                    } else if rel_y == h.saturating_sub(3) {
                                        self.mixer.selected_global = GlobalControl::MasterMute;
                                        self.mixer.master.muted = !self.mixer.master.muted;
                                        self.sync_deck_volume(true);
                                        self.sync_deck_volume(false);
                                    } else {
                                        self.mixer.selected_global =
                                            GlobalControl::MasterOutputSelect;
                                        self.open_output_picker(OutputPickerTarget::Master);
                                    }
                                } else {
                                    // Fader area
                                    let mid_x = area.x + area.w / 2;
                                    self.mixer.selected_global = if mouse.column < mid_x {
                                        GlobalControl::MicFader
                                    } else {
                                        GlobalControl::MasterFader
                                    };
                                    self.drag_start_y = Some(mouse.row);
                                    self.drag_start_value = Some(if mouse.column < mid_x {
                                        self.mixer.master.mic_fader
                                    } else {
                                        self.mixer.master.fader
                                    });
                                }
                            }
                        }
                        HitResult::Cue => {
                            self.selected_pane = SelectedPane::DeckC;
                            self.mixer.focus = SelectionFocus::Channel(2);
                            // Determine control from y position
                            if let Some(area) = &self.cue_area {
                                let rel_y = mouse.row.saturating_sub(area.y);
                                let h = area.h;
                                let controls_h = h.saturating_sub(4); // approximate
                                if rel_y > controls_h {
                                    // Button area at bottom
                                    let mid_x = area.x + area.w / 2;
                                    if mouse.column < mid_x {
                                        self.mixer.selected_control = ChannelControl::CueSendToA;
                                    } else {
                                        self.mixer.selected_control = ChannelControl::CueSendToB;
                                    }
                                    self.mixer.toggle_selected();
                                } else {
                                    // Fader area
                                    self.mixer.selected_control = ChannelControl::Fader;
                                    self.drag_start_y = Some(mouse.row);
                                    self.drag_start_value = self.get_current_control_value();
                                }
                            }
                        }
                        HitResult::Loops => {
                            self.selected_pane = SelectedPane::Loops;
                        }
                    }
                }
            }

            MouseEventKind::Drag(MouseButton::Left) => {
                // Handle crossfader horizontal drag
                if self.selected_pane == SelectedPane::Crossfader
                    && let (Some(start_x), Some(start_value), Some(area)) = (
                        self.drag_start_x,
                        self.drag_start_value,
                        &self.crossfader_area,
                    )
                {
                    let delta = (mouse.column as i16 - start_x as i16) as f32;
                    let sensitivity = 2.0 / area.w as f32;
                    self.mixer.dj.crossfader = (start_value + delta * sensitivity).clamp(-1.0, 1.0);
                    return;
                }

                // Handle master fader vertical drag
                if self.selected_pane == SelectedPane::Master
                    && matches!(
                        self.mixer.selected_global,
                        GlobalControl::MasterFader | GlobalControl::MicFader
                    )
                    && let (Some(start_y), Some(start_value)) =
                        (self.drag_start_y, self.drag_start_value)
                {
                    let delta = (start_y as i16 - mouse.row as i16) as f32;
                    let sensitivity = 0.02;
                    if self.mixer.selected_global == GlobalControl::MicFader {
                        self.mixer.master.mic_fader =
                            (start_value + delta * sensitivity).clamp(0.0, 1.0);
                        self.sync_mic_to_engine();
                    } else {
                        self.mixer.master.fader =
                            (start_value + delta * sensitivity).clamp(0.0, 1.0);
                        self.sync_deck_volume(true);
                        self.sync_deck_volume(false);
                    }
                    return;
                }

                // Handle CUE fader vertical drag
                if self.selected_pane == SelectedPane::DeckC
                    && self.mixer.selected_control == ChannelControl::Fader
                    && let (Some(start_y), Some(start_value)) =
                        (self.drag_start_y, self.drag_start_value)
                {
                    let delta = (start_y as i16 - mouse.row as i16) as f32;
                    let sensitivity = 0.02;
                    if let Some(ch) = self.mixer.channels.get_mut(2) {
                        ch.fader = (start_value + delta * sensitivity).clamp(0.0, 1.0);
                    }
                    return;
                }

                // Channel strip drag
                if let (Some(start_y), Some(start_value)) =
                    (self.drag_start_y, self.drag_start_value)
                {
                    let delta = (start_y as i16 - mouse.row as i16) as f32;
                    let sensitivity = 0.02;
                    let control = self.mixer.selected_control;

                    if let Some(channel) = self.mixer.selected_channel_mut() {
                        match control {
                            ChannelControl::Fader => {
                                channel.fader = (start_value + delta * sensitivity).clamp(0.0, 1.0);
                            }
                            ChannelControl::Pan => {
                                channel.pan =
                                    (start_value + delta * sensitivity * 0.5).clamp(-1.0, 1.0);
                            }
                            ChannelControl::FilterCutoff => {
                                channel.filter_cutoff =
                                    (start_value + delta * sensitivity * 0.5).clamp(0.0, 1.0);
                            }
                            ChannelControl::FilterFreq => {
                                channel.filter_freq =
                                    (start_value + delta * sensitivity * 0.5).clamp(0.0, 1.0);
                            }
                            ChannelControl::EqLow => {
                                channel.eq_low = (start_value + delta * 0.5).clamp(-15.0, 15.0);
                            }
                            ChannelControl::EqMid => {
                                channel.eq_mid = (start_value + delta * 0.5).clamp(-15.0, 15.0);
                            }
                            ChannelControl::EqHigh => {
                                channel.eq_high = (start_value + delta * 0.5).clamp(-15.0, 15.0);
                            }
                            _ => {}
                        }
                    }
                }
            }

            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_start_y = None;
                self.drag_start_x = None;
                self.drag_start_value = None;
            }

            MouseEventKind::ScrollUp => {
                self.mixer.adjust_selected(0.5);
            }

            MouseEventKind::ScrollDown => {
                self.mixer.adjust_selected(-0.5);
            }

            _ => {}
        }
    }

    fn handle_pad_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Hit test against pad areas
                for &(pad_idx, x, y, w, h) in &self.pad_areas {
                    if mouse.column >= x
                        && mouse.column < x + w
                        && mouse.row >= y
                        && mouse.row < y + h
                    {
                        if self.mode == AppMode::SamplePadConfig {
                            self.sample_pads.selected_pad = pad_idx;
                        } else {
                            self.sample_pads.trigger_pad(pad_idx);
                            self.play_sample(pad_idx);
                        }
                        break;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Release gate-mode pads (areas are in screen coordinates)
                for &(pad_idx, x, y, w, h) in &self.pad_areas {
                    if mouse.column >= x
                        && mouse.column < x + w
                        && mouse.row >= y
                        && mouse.row < y + h
                    {
                        self.sample_pads.release_pad(pad_idx);
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    /// Update channel areas for mouse hit testing
    pub fn update_channel_areas(&mut self, areas: Vec<ChannelArea>) {
        self.channel_areas = areas;
    }

    /// Update pane areas for mouse hit testing
    pub fn update_pane_areas(
        &mut self,
        crossfader: Option<PaneArea>,
        master: Option<PaneArea>,
        cue: Option<PaneArea>,
        loops: Option<PaneArea>,
        pads: Vec<(usize, u16, u16, u16, u16)>,
    ) {
        self.crossfader_area = crossfader;
        self.master_area = master;
        self.cue_area = cue;
        self.loops_area = loops;
        self.pad_areas = pads;
    }

    /// General hit-test across all panes
    pub fn hit_test_all(&self, x: u16, y: u16) -> Option<HitResult> {
        // Areas are in screen coordinates (matching the renderer), so use
        // mouse position directly — no scroll offset needed.
        // Check channel strips first
        if let Some((idx, ctrl)) = self.hit_test(x, y) {
            return Some(HitResult::Channel(idx, ctrl));
        }
        // Check pads
        for &(pad_idx, px, py, pw, ph) in &self.pad_areas {
            if x >= px && x < px + pw && y >= py && y < py + ph {
                return Some(HitResult::Pad(pad_idx));
            }
        }
        // Check crossfader
        if let Some(area) = &self.crossfader_area
            && area.contains(x, y)
        {
            return Some(HitResult::Crossfader);
        }
        // Check master
        if let Some(area) = &self.master_area
            && area.contains(x, y)
        {
            return Some(HitResult::Master);
        }
        // Check CUE
        if let Some(area) = &self.cue_area
            && area.contains(x, y)
        {
            return Some(HitResult::Cue);
        }
        // Check loops
        if let Some(area) = &self.loops_area
            && area.contains(x, y)
        {
            return Some(HitResult::Loops);
        }
        None
    }

    /// Hit test to find which channel/control is at a screen position
    fn hit_test(&self, x: u16, y: u16) -> Option<(usize, ChannelControl)> {
        for (idx, area) in self.channel_areas.iter().enumerate() {
            let (ax, ay, aw, ah) = area.bounds;
            if x >= ax && x < ax + aw && y >= ay && y < ay + ah {
                for &(control, y_start, y_end) in &area.control_rows {
                    if y >= y_start && y < y_end {
                        return Some((idx, control));
                    }
                }
                return Some((idx, ChannelControl::Fader));
            }
        }
        None
    }

    /// Get current value of selected control for drag operations
    fn get_current_control_value(&self) -> Option<f32> {
        self.mixer
            .selected_channel()
            .map(|ch| match self.mixer.selected_control {
                ChannelControl::Fader => ch.fader,
                ChannelControl::Pan => ch.pan,
                ChannelControl::FilterCutoff => ch.filter_cutoff,
                ChannelControl::FilterFreq => ch.filter_freq,
                ChannelControl::EqLow => ch.eq_low,
                ChannelControl::EqMid => ch.eq_mid,
                ChannelControl::EqHigh => ch.eq_high,
                _ => 0.0,
            })
    }

    pub fn show_help(&self) -> bool {
        self.mode == AppMode::Help
    }

    /// Open source picker for specified deck
    fn open_source_picker(&mut self, deck: Deck) {
        self.source_picker = SourcePickerState::new();
        self.source_picker.tab_focused = true;
        self.source_picker.set_root(self.music_dir.clone());
        // Load the deck's queue into the source picker
        let deck_idx = deck.index();
        self.source_picker.queue = self.deck_queues[deck_idx].clone();
        self.scan_sources();
        self.mode = AppMode::SourcePicker(deck);
    }

    /// Open an audio device picker.
    fn open_output_picker(&mut self, target: OutputPickerTarget) {
        self.output_picker_active = true;
        self.output_picker_target = target;

        if target == OutputPickerTarget::Mic {
            self.mic_input_devices = self
                .audio_engine
                .as_ref()
                .and_then(|engine| engine.list_mic_inputs().ok())
                .unwrap_or_default();
            let selected_device = self
                .audio_engine
                .as_ref()
                .and_then(|engine| engine.selected_mic_input());
            self.selected_mic_input_idx = selected_device
                .and_then(|index| {
                    self.mic_input_devices
                        .iter()
                        .position(|device| device.index == index)
                })
                .or_else(|| {
                    self.mic_input_devices
                        .iter()
                        .position(|device| device.is_default)
                })
                .unwrap_or(0);
            return;
        }

        // Try to get device list from a connected MPV client first.
        // Falls back to cpal enumeration if no clients are available.
        let mpv_devices = self
            .mpv_deck_a
            .as_mut()
            .or(self.mpv_deck_b.as_mut())
            .or(self.mpv_deck_c.as_mut())
            .and_then(|c| c.get_audio_device_list().ok());

        match target {
            OutputPickerTarget::Master => {
                if let Some(devices) = &mpv_devices {
                    let pairs: Vec<_> = devices
                        .iter()
                        .map(|d| (d.name.clone(), d.description.clone()))
                        .collect();
                    self.master_output.set_devices_from_mpv(&pairs);
                } else {
                    self.master_output.refresh_devices();
                }
                self.selected_master_output_idx = 0;
            }
            OutputPickerTarget::Cue => {
                if let Some(devices) = &mpv_devices {
                    let pairs: Vec<_> = devices
                        .iter()
                        .map(|d| (d.name.clone(), d.description.clone()))
                        .collect();
                    self.cue_output.set_devices_from_mpv(&pairs);
                } else {
                    self.cue_output.refresh_devices();
                }
                self.selected_cue_output_idx = 0;
            }
            OutputPickerTarget::Mic => unreachable!(),
        }
    }

    /// Select the currently highlighted output device
    fn select_output_device(&mut self) {
        if self.output_picker_target == OutputPickerTarget::Mic {
            if let Some(device) = self.mic_input_devices.get(self.selected_mic_input_idx)
                && let Some(engine) = &self.audio_engine
                && let Err(error) = engine.select_mic_input(device.index)
            {
                tracing::warn!("Failed to select microphone input: {}", error);
            }
            self.sync_mic_to_engine();
            self.output_picker_active = false;
            return;
        }

        let (devices, selected_idx) = match self.output_picker_target {
            OutputPickerTarget::Master => (
                self.master_output.devices().to_vec(),
                self.selected_master_output_idx,
            ),
            OutputPickerTarget::Cue => (
                self.cue_output.devices().to_vec(),
                self.selected_cue_output_idx,
            ),
            OutputPickerTarget::Mic => unreachable!(),
        };

        tracing::debug!(
            "select_output_device: target={:?}, idx={}, total_devices={}, mpv_deck_c={}",
            self.output_picker_target,
            selected_idx,
            devices.len(),
            self.mpv_deck_c.is_some()
        );

        if let Some(display_name) = devices.get(selected_idx) {
            let mpv_name = match self.output_picker_target {
                OutputPickerTarget::Master => self
                    .master_output
                    .select_device(display_name)
                    .ok()
                    .flatten(),
                OutputPickerTarget::Cue => {
                    self.cue_output.select_device(display_name).ok().flatten()
                }
                OutputPickerTarget::Mic => unreachable!(),
            };

            tracing::debug!(
                "select_output_device: display_name='{}', mpv_name={:?}",
                display_name,
                mpv_name
            );

            // Route audio to the selected device.
            // Master → Deck A + Deck B (main speakers)
            // CUE → Deck C only (headphone preview)
            // Always route cpal headphone stream for CUE target (works with or without MPV)
            if self.output_picker_target == OutputPickerTarget::Cue
                && let Some(ref engine) = self.audio_engine
            {
                engine.set_headphone_device(display_name);
            }
            // Route cpal master output stream for Master target (works with or without MPV)
            if self.output_picker_target == OutputPickerTarget::Master
                && let Some(ref engine) = self.audio_engine
            {
                engine.set_master_device(display_name);
            }

            if let Some(ref mpv_dev) = mpv_name {
                match self.output_picker_target {
                    OutputPickerTarget::Master => {
                        if let Some(client) = self.mpv_deck_a.as_mut()
                            && let Err(e) = client.set_audio_device(mpv_dev)
                        {
                            tracing::warn!("Failed to set master audio device on deck_a: {}", e);
                        }
                        if let Some(client) = self.mpv_deck_b.as_mut()
                            && let Err(e) = client.set_audio_device(mpv_dev)
                        {
                            tracing::warn!("Failed to set master audio device on deck_b: {}", e);
                        }
                    }
                    OutputPickerTarget::Cue => {
                        // Also tell MPV directly (for socket mode)
                        if let Some(client) = self.mpv_deck_c.as_mut() {
                            tracing::debug!(
                                "Setting CUE audio device to '{}' on mpv_deck_c",
                                mpv_dev
                            );
                            match client.set_audio_device(mpv_dev) {
                                Ok(()) => {
                                    // Verify the change took effect
                                    match client.get_audio_device() {
                                        Ok(current) => tracing::debug!(
                                            "CUE audio device now set to: '{}'",
                                            current
                                        ),
                                        Err(e) => tracing::warn!(
                                            "Could not verify CUE audio device: {}",
                                            e
                                        ),
                                    }
                                }
                                Err(e) => tracing::warn!("Failed to set CUE audio device: {}", e),
                            }
                        } else {
                            tracing::warn!(
                                "CUE output picker: mpv_deck_c is None, cannot set audio device"
                            );
                        }
                    }
                    OutputPickerTarget::Mic => unreachable!(),
                }
            } else {
                tracing::debug!(
                    "select_output_device: mpv_name is None for '{}' (cpal-only device, MPV routing skipped)",
                    display_name
                );
            }
        }

        self.output_picker_active = false;
    }

    /// Scan for MPV sockets and audio files
    fn scan_sources(&mut self) {
        self.source_picker.items.clear();

        match self.source_picker.tab {
            SourcePickerTab::Queue => {
                // Queue tab: items come from the queue, not from scanning
                // The "+" item is always present at the end
            }
            SourcePickerTab::SuperCollider => {
                self.scan_supercollider_sources();
            }
            SourcePickerTab::DeckActions => {
                self.scan_deck_actions();
            }
            SourcePickerTab::AudioFiles => {
                // Used by sample picker
                self.scan_audio_files();
            }
        }

        self.source_picker.filter();
    }

    /// Scan sources for the "add to queue" sub-view
    fn scan_add_sources(&mut self) {
        self.source_picker.add_items.clear();

        match self.source_picker.add_tab {
            AddSourceTab::MpvSockets => {
                self.scan_mpv_sockets_for_add();
            }
            AddSourceTab::AudioFiles => {
                self.scan_audio_files_for_add();
            }
        }

        self.source_picker.add_filter();
    }

    fn scan_audio_files(&mut self) {
        let extensions = [
            "mp3", "wav", "flac", "ogg", "m4a", "aac", "opus", "aiff", "aif",
        ];

        self.source_picker.items.clear();

        // Add ".." entry if not at root
        if self.source_picker.can_go_up() {
            self.source_picker.items.push(SourcePickerItem {
                name: "..".to_string(),
                path: self
                    .source_picker
                    .current_dir
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.source_picker.root_dir.clone()),
                is_socket: false,
                is_pcm_fifo: false,
                is_udp: false,
                is_dir: true,
            });
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.source_picker.current_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Skip hidden files/folders
                if name.starts_with('.') {
                    continue;
                }

                if path.is_dir() {
                    dirs.push(SourcePickerItem {
                        name: format!("{}/", name),
                        path,
                        is_socket: false,
                        is_pcm_fifo: false,
                        is_udp: false,
                        is_dir: true,
                    });
                } else if path.is_file()
                    && let Some(ext) = path.extension()
                {
                    let ext_lower = ext.to_string_lossy().to_lowercase();
                    if extensions.contains(&ext_lower.as_str()) {
                        files.push(SourcePickerItem {
                            name,
                            path,
                            is_socket: false,
                            is_pcm_fifo: false,
                            is_udp: false,
                            is_dir: false,
                        });
                    }
                }
            }
        }

        // Sort directories and files alphabetically
        dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        // Add directories first, then files
        self.source_picker.items.extend(dirs);
        self.source_picker.items.extend(files);
    }

    // --- Scan methods for add-to-queue sub-view ---

    fn scan_mpv_sockets_for_add(&mut self) {
        // Route FIFOs
        if let Ok(paths) = glob::glob(TM_FIFO_GLOB) {
            for entry in paths.flatten() {
                if entry.exists() {
                    let name = Self::route_meta_label(Some(&entry)).unwrap_or_else(|| {
                        entry
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "termixer route".to_string())
                    });
                    self.source_picker.add_items.push(SourcePickerItem {
                        name,
                        path: entry,
                        is_socket: false,
                        is_pcm_fifo: true,
                        is_udp: false,
                        is_dir: false,
                    });
                }
            }
        }

        // Canonical FIFO
        let canonical_fifo = PathBuf::from(TM_FIFO);
        if canonical_fifo.exists() {
            self.source_picker.add_items.push(SourcePickerItem {
                name: Self::route_meta_label(Some(&canonical_fifo))
                    .unwrap_or_else(|| "termixer route".to_string()),
                path: canonical_fifo.clone(),
                is_socket: false,
                is_pcm_fifo: true,
                is_udp: false,
                is_dir: false,
            });
        }

        // De-duplicate
        {
            let mut seen = std::collections::HashSet::<PathBuf>::new();
            self.source_picker
                .add_items
                .retain(|item| seen.insert(item.path.clone()));
        }

        // Common MPV socket locations
        let socket_patterns = ["/tmp/mpv*", "/tmp/mpvsocket*"];
        for pattern in &socket_patterns {
            if let Ok(paths) = glob::glob(pattern) {
                for entry in paths.flatten() {
                    if entry.exists() && !Self::is_termixer_socket(&entry) {
                        let name = entry
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "mpv".to_string());
                        self.source_picker.add_items.push(SourcePickerItem {
                            name,
                            path: entry,
                            is_socket: true,
                            is_pcm_fifo: false,
                            is_udp: false,
                            is_dir: false,
                        });
                    }
                }
            }
        }

        // XDG runtime dir
        if let Ok(uid) = std::env::var("UID").or_else(|_| {
            std::process::Command::new("id")
                .arg("-u")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        }) {
            let runtime_pattern = format!("/run/user/{}/mpv*", uid);
            if let Ok(paths) = glob::glob(&runtime_pattern) {
                for entry in paths.flatten() {
                    if entry.exists() && !Self::is_termixer_socket(&entry) {
                        let name = entry
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "mpv".to_string());
                        self.source_picker.add_items.push(SourcePickerItem {
                            name,
                            path: entry,
                            is_socket: true,
                            is_pcm_fifo: false,
                            is_udp: false,
                            is_dir: false,
                        });
                    }
                }
            }
        }
    }

    fn scan_audio_files_for_add(&mut self) {
        let extensions = [
            "mp3", "wav", "flac", "ogg", "m4a", "aac", "opus", "aiff", "aif",
        ];

        self.source_picker.add_items.clear();

        // Add ".." entry if not at root
        let can_go_up = self.source_picker.add_current_dir != self.source_picker.root_dir
            && self.source_picker.add_current_dir.parent().is_some();
        if can_go_up {
            self.source_picker.add_items.push(SourcePickerItem {
                name: "..".to_string(),
                path: self
                    .source_picker
                    .add_current_dir
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.source_picker.root_dir.clone()),
                is_socket: false,
                is_pcm_fifo: false,
                is_udp: false,
                is_dir: true,
            });
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.source_picker.add_current_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                if name.starts_with('.') {
                    continue;
                }

                if path.is_dir() {
                    dirs.push(SourcePickerItem {
                        name: format!("{}/", name),
                        path,
                        is_socket: false,
                        is_pcm_fifo: false,
                        is_udp: false,
                        is_dir: true,
                    });
                } else if path.is_file()
                    && let Some(ext) = path.extension()
                {
                    let ext_lower = ext.to_string_lossy().to_lowercase();
                    if extensions.contains(&ext_lower.as_str()) {
                        files.push(SourcePickerItem {
                            name,
                            path,
                            is_socket: false,
                            is_pcm_fifo: false,
                            is_udp: false,
                            is_dir: false,
                        });
                    }
                }
            }
        }

        dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        self.source_picker.add_items.extend(dirs);
        self.source_picker.add_items.extend(files);
    }

    fn scan_supercollider_sources(&mut self) {
        self.source_picker.items.push(SourcePickerItem {
            name: "SuperCollider UDP (127.0.0.1:57110)".to_string(),
            path: PathBuf::from("udp://127.0.0.1:57110"),
            is_socket: false,
            is_pcm_fifo: false,
            is_udp: true,
            is_dir: false,
        });
    }

    fn scan_deck_actions(&mut self) {
        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;
        let deck_c_ch = self.mixer.dj.deck_c_channel;
        let decks = [
            (
                Deck::A,
                "A",
                self.mixer
                    .get_channel(deck_a_ch)
                    .map(|ch| ch.connected)
                    .unwrap_or(false),
            ),
            (
                Deck::B,
                "B",
                self.mixer
                    .get_channel(deck_b_ch)
                    .map(|ch| ch.connected)
                    .unwrap_or(false),
            ),
            (
                Deck::C,
                "C",
                self.mixer
                    .get_channel(deck_c_ch)
                    .map(|ch| ch.connected)
                    .unwrap_or(false),
            ),
        ];
        for (_deck, label, connected) in &decks {
            let status = if *connected { "●" } else { "○" };
            self.source_picker.items.push(SourcePickerItem {
                name: format!("{} Clear Playlist {}", status, label),
                path: PathBuf::new(),
                is_socket: false,
                is_pcm_fifo: false,
                is_udp: false,
                is_dir: false,
            });
        }
    }

    /// Open sample picker for a pad
    fn open_sample_picker(&mut self, pad_idx: usize) {
        self.source_picker = SourcePickerState::new();
        self.source_picker.tab = SourcePickerTab::AudioFiles;
        self.source_picker.set_root(self.samples_dir.clone());
        self.scan_sample_files();
        self.source_picker.selected = 0;
        self.mode = AppMode::SamplePicker(pad_idx);
    }

    /// Scan for sample files and folders in current directory
    fn scan_sample_files(&mut self) {
        let extensions = [
            "mp3", "wav", "flac", "ogg", "m4a", "aac", "opus", "aiff", "aif",
        ];
        self.source_picker.items.clear();

        // Add ".." entry if not at root
        if self.source_picker.can_go_up() {
            self.source_picker.items.push(SourcePickerItem {
                name: "..".to_string(),
                path: self
                    .source_picker
                    .current_dir
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| self.source_picker.root_dir.clone()),
                is_socket: false,
                is_pcm_fifo: false,
                is_udp: false,
                is_dir: true,
            });
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&self.source_picker.current_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                // Skip hidden files/folders
                if name.starts_with('.') {
                    continue;
                }

                if path.is_dir() {
                    dirs.push(SourcePickerItem {
                        name: format!("{}/", name),
                        path,
                        is_socket: false,
                        is_pcm_fifo: false,
                        is_udp: false,
                        is_dir: true,
                    });
                } else if path.is_file()
                    && let Some(ext) = path.extension()
                {
                    let ext_lower = ext.to_string_lossy().to_lowercase();
                    if extensions.contains(&ext_lower.as_str()) {
                        files.push(SourcePickerItem {
                            name,
                            path,
                            is_socket: false,
                            is_pcm_fifo: false,
                            is_udp: false,
                            is_dir: false,
                        });
                    }
                }
            }
        }

        // Sort directories and files alphabetically
        dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        // Add directories first, then files
        self.source_picker.items.extend(dirs);
        self.source_picker.items.extend(files);

        self.source_picker.filter();
    }

    /// Navigate into a directory in sample picker
    fn enter_sample_directory(&mut self, path: PathBuf) {
        self.source_picker.current_dir = path;
        self.source_picker.query.clear();
        self.scan_sample_files();
    }

    /// Navigate into a directory in source picker
    /// Preview (play) the currently selected sample without assigning it
    fn preview_sample(&mut self) {
        if let Some(item) = self.source_picker.selected_item()
            && !item.is_dir
            && item.path.exists()
        {
            if let Some(ref mut engine) = self.sample_engine {
                // Use cached engine for instant preview
                let _ = engine.play(&item.path);
            } else {
                // Fallback to mpv
                let _ = std::process::Command::new("mpv")
                    .arg("--no-video")
                    .arg("--really-quiet")
                    .arg("--no-terminal")
                    .arg(&item.path)
                    .spawn();
            }
        }
    }

    /// Handle keys in sample picker mode
    fn handle_sample_picker_key(&mut self, key: KeyEvent) {
        // Set visible height for scrolling (popup height minus header rows)
        // Popup is 20 rows, minus 2 for border, 1 for path, 1 for search, 1 for hint = 15
        self.source_picker.visible_height = 15;

        match self.source_picker.input_mode {
            PickerInputMode::Normal => match key.code {
                KeyCode::Esc => {
                    self.mode = AppMode::SamplePadConfig;
                }
                KeyCode::Char('i') => {
                    self.source_picker.input_mode = PickerInputMode::Insert;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.source_picker.move_down();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.source_picker.move_up();
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    if self.source_picker.can_go_up()
                        && let Some(parent) = self.source_picker.current_dir.parent()
                    {
                        self.enter_sample_directory(parent.to_path_buf());
                    }
                }
                KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                    if let Some(item) = self.source_picker.selected_item().cloned() {
                        if item.is_dir {
                            self.enter_sample_directory(item.path);
                        } else if let AppMode::SamplePicker(pad_idx) = self.mode {
                            self.assign_sample_to_pad(pad_idx);
                            self.mode = AppMode::SamplePadConfig;
                        }
                    }
                }
                KeyCode::Char(' ') => {
                    self.preview_sample();
                }
                KeyCode::Char('g') => {
                    self.source_picker.selected = 0;
                    self.source_picker.scroll_offset = 0;
                }
                KeyCode::Char('G') => {
                    let last = self.source_picker.filtered.len().saturating_sub(1);
                    self.source_picker.selected = last;
                    self.source_picker.clamp_scroll();
                }
                _ => {}
            },
            PickerInputMode::Insert => match key.code {
                KeyCode::Esc => {
                    self.source_picker.input_mode = PickerInputMode::Normal;
                }
                KeyCode::Enter => {
                    if let Some(item) = self.source_picker.selected_item().cloned() {
                        if item.is_dir {
                            self.enter_sample_directory(item.path);
                        } else if let AppMode::SamplePicker(pad_idx) = self.mode {
                            self.assign_sample_to_pad(pad_idx);
                            self.mode = AppMode::SamplePadConfig;
                        }
                    }
                }
                KeyCode::Backspace => {
                    if self.source_picker.query.is_empty() {
                        if self.source_picker.can_go_up()
                            && let Some(parent) = self.source_picker.current_dir.parent()
                        {
                            self.enter_sample_directory(parent.to_path_buf());
                        }
                    } else {
                        self.source_picker.query.pop();
                        self.source_picker.filter();
                    }
                }
                KeyCode::Up => {
                    self.source_picker.move_up();
                }
                KeyCode::Down => {
                    self.source_picker.move_down();
                }
                KeyCode::Char(c) => {
                    self.source_picker.query.push(c);
                    self.source_picker.filter();
                }
                _ => {}
            },
        }
    }

    /// Assign selected sample to pad and preload it
    fn assign_sample_to_pad(&mut self, pad_idx: usize) {
        if let Some(item) = self.source_picker.selected_item().cloned() {
            // Preload into cache for instant playback
            if let Some(ref mut engine) = self.sample_engine {
                let _ = engine.preload(&item.path);
            }
            self.sample_pads
                .assign_sample_to_pad(pad_idx, item.path.clone(), Some(item.name));

            // Populate audio engine's pad sample cache for sequencer playback
            // Apply DSP (HP, LP, EQ, Distortion) to match pad playback
            let pad_cfg = self.sample_pads.pads[pad_idx].config.clone();
            if let Some(ref engine) = self.audio_engine {
                let mut loaded = false;
                if let Some(ref sample_eng) = self.sample_engine
                    && let Some(cached) = sample_eng.cache.get(&item.path)
                {
                    let processed =
                        crate::audio::sample_cache::apply_dsp_to_buffer(&cached.samples, &pad_cfg);
                    engine.set_pad_sample(pad_idx, processed, cached.sample_rate, cached.channels);
                    loaded = true;
                }
                if !loaded
                    && let Ok(cached) = crate::audio::sample_cache::CachedSample::load(&item.path)
                {
                    let processed =
                        crate::audio::sample_cache::apply_dsp_to_buffer(&cached.samples, &pad_cfg);
                    engine.set_pad_sample(pad_idx, processed, cached.sample_rate, cached.channels);
                }
            }

            // Auto-create a sequence for this pad if one doesn't already exist
            if !self
                .sequence_state
                .sequences
                .iter()
                .any(|s| s.pad_idx == pad_idx)
            {
                self.sequence_state.add_sequence(pad_idx);
            }
        }
    }

    /// Handle keys in source picker mode
    fn handle_source_picker_key(&mut self, key: KeyEvent) {
        self.source_picker.visible_height = 13;

        // Handle add-to-queue sub-view separately
        if self.source_picker.adding_to_queue {
            self.handle_add_source_key(key);
            return;
        }

        match self.source_picker.tab {
            SourcePickerTab::Queue => self.handle_queue_key(key),
            SourcePickerTab::SuperCollider => self.handle_supercollider_key(key),
            SourcePickerTab::DeckActions => self.handle_deck_actions_key(key),
            SourcePickerTab::AudioFiles => {
                // Should not happen in source picker mode (used by sample picker)
                self.mode = AppMode::PaneSelect;
            }
        }
    }

    /// Handle key events in the Queue tab
    fn handle_queue_key(&mut self, key: KeyEvent) {
        match self.source_picker.input_mode {
            PickerInputMode::Normal => match key.code {
                KeyCode::Esc => {
                    self.save_queue_to_deck();
                    self.mode = AppMode::PaneSelect;
                }
                KeyCode::Char('j') | KeyCode::Down
                    if !key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        if self.source_picker.queue.is_empty() {
                            self.source_picker.queue_selected = None;
                        } else {
                            self.source_picker.queue_selected = Some(0);
                        }
                    } else if let Some(sel) = self.source_picker.queue_selected {
                        if sel + 1 < self.source_picker.queue.len() {
                            self.source_picker.queue_selected = Some(sel + 1);
                        } else {
                            self.source_picker.queue_selected = None;
                        }
                    } else {
                        self.source_picker.tab_focused = true;
                    }
                    self.source_picker.clamp_queue_scroll();
                }
                KeyCode::Char('k') | KeyCode::Up
                    if !key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        self.source_picker.queue_selected = None;
                    } else if let Some(sel) = self.source_picker.queue_selected {
                        if sel > 0 {
                            self.source_picker.queue_selected = Some(sel - 1);
                        } else {
                            self.source_picker.tab_focused = true;
                            self.source_picker.queue_selected = None;
                        }
                    } else if !self.source_picker.queue.is_empty() {
                        self.source_picker.queue_selected =
                            Some(self.source_picker.queue.len() - 1);
                    } else {
                        self.source_picker.tab_focused = true;
                    }
                    self.source_picker.clamp_queue_scroll();
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    self.source_picker.tab_focused = true;
                    self.source_picker.prev_tab();
                    self.scan_sources();
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    self.source_picker.tab_focused = true;
                    self.source_picker.next_tab();
                    self.scan_sources();
                }
                KeyCode::Tab => {
                    self.source_picker.next_tab();
                    self.scan_sources();
                }
                KeyCode::BackTab => {
                    self.source_picker.prev_tab();
                    self.scan_sources();
                }
                KeyCode::Char('g') => {
                    self.source_picker.tab_focused = false;
                    if self.source_picker.queue.is_empty() {
                        self.source_picker.queue_selected = None;
                    } else {
                        self.source_picker.queue_selected = Some(0);
                    }
                    self.source_picker.clamp_queue_scroll();
                }
                KeyCode::Char('G') => {
                    self.source_picker.tab_focused = false;
                    if self.source_picker.queue.is_empty() {
                        self.source_picker.queue_selected = None;
                    } else {
                        self.source_picker.queue_selected =
                            Some(self.source_picker.queue.len() - 1);
                    }
                    self.source_picker.clamp_queue_scroll();
                }
                KeyCode::Char('a') => {
                    // Shortcut to add: open add-to-queue sub-view
                    self.enter_add_source_mode();
                }
                KeyCode::Char('x') | KeyCode::Char('d') => {
                    // Delete selected item from queue
                    if let Some(sel) = self.source_picker.queue_selected
                        && sel < self.source_picker.queue.len()
                    {
                        let deck = match self.mode {
                            AppMode::SourcePicker(deck) => deck,
                            _ => return,
                        };
                        let deck_idx = deck.index();
                        let removed_active = sel == self.deck_queue_indices[deck_idx];
                        self.source_picker.queue.remove(sel);
                        // Adjust selection
                        if self.source_picker.queue.is_empty() {
                            self.source_picker.queue_selected = None;
                        } else if sel >= self.source_picker.queue.len() {
                            self.source_picker.queue_selected =
                                Some(self.source_picker.queue.len() - 1);
                        }

                        if removed_active {
                            if let Some(item) = self.source_picker.queue.get(sel).cloned() {
                                self.deck_queue_indices[deck_idx] = sel;
                                self.save_queue_to_deck();
                                self.clear_deck_impl(deck, false, false);
                                self.load_queue_item(deck, &item);
                            } else {
                                self.deck_queue_indices[deck_idx] =
                                    self.source_picker.queue.len().saturating_sub(1);
                                self.save_queue_to_deck();
                                self.clear_deck_impl(deck, false, false);
                            }
                        } else {
                            // Keep play icon aligned with the actual playing track
                            if sel < self.deck_queue_indices[deck_idx] {
                                self.deck_queue_indices[deck_idx] -= 1;
                            }
                            self.save_queue_to_deck();
                        }
                        self.source_picker.clamp_queue_scroll();
                    }
                }
                KeyCode::Char('J') | KeyCode::Down
                    if matches!(key.code, KeyCode::Char('J'))
                        || key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    // Move selected item down
                    if let Some(sel) = self.source_picker.queue_selected
                        && sel + 1 < self.source_picker.queue.len()
                    {
                        self.source_picker.queue.swap(sel, sel + 1);
                        self.source_picker.queue_selected = Some(sel + 1);
                        // Keep play icon aligned with the actual playing track
                        if let AppMode::SourcePicker(deck) = self.mode {
                            let idx = &mut self.deck_queue_indices[deck.index()];
                            if *idx == sel {
                                *idx = sel + 1;
                            } else if *idx == sel + 1 {
                                *idx = sel;
                            }
                        }
                        self.save_queue_to_deck();
                        self.source_picker.clamp_queue_scroll();
                    }
                }
                KeyCode::Char('K') | KeyCode::Up
                    if matches!(key.code, KeyCode::Char('K'))
                        || key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    // Move selected item up
                    if let Some(sel) = self.source_picker.queue_selected
                        && sel > 0
                    {
                        self.source_picker.queue.swap(sel, sel - 1);
                        self.source_picker.queue_selected = Some(sel - 1);
                        // Keep play icon aligned with the actual playing track
                        if let AppMode::SourcePicker(deck) = self.mode {
                            let idx = &mut self.deck_queue_indices[deck.index()];
                            if *idx == sel {
                                *idx = sel - 1;
                            } else if *idx == sel - 1 {
                                *idx = sel;
                            }
                        }
                        self.save_queue_to_deck();
                        self.source_picker.clamp_queue_scroll();
                    }
                }
                KeyCode::Enter => {
                    if self.source_picker.tab_focused {
                        // Unfocus tabs, select first item or "+" (last)
                        self.source_picker.tab_focused = false;
                        if self.source_picker.queue.is_empty() {
                            // Queue is empty, "+" is implicitly selected
                            self.source_picker.queue_selected = None;
                        } else {
                            self.source_picker.queue_selected = Some(0);
                        }
                    } else if let Some(sel) = self.source_picker.queue_selected {
                        // Load selected item to deck and start playing
                        if sel < self.source_picker.queue.len() {
                            let item = self.source_picker.queue[sel].clone();
                            if let AppMode::SourcePicker(deck) = self.mode {
                                self.deck_queue_indices[deck.index()] = sel;
                                self.load_queue_item(deck, &item);
                            }
                        }
                    } else {
                        // "+" is selected (no explicit selection) — open add view
                        self.enter_add_source_mode();
                    }
                    self.source_picker.clamp_queue_scroll();
                }
                _ => {}
            },
            PickerInputMode::Insert => {
                // Insert mode not used in Queue tab
                self.source_picker.input_mode = PickerInputMode::Normal;
            }
        }
    }

    /// Handle key events in the SuperCollider tab
    fn handle_supercollider_key(&mut self, key: KeyEvent) {
        match self.source_picker.input_mode {
            PickerInputMode::Normal => match key.code {
                KeyCode::Esc => {
                    self.mode = AppMode::PaneSelect;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        self.source_picker.selected = 0;
                        self.source_picker.scroll_offset = 0;
                    } else if self.source_picker.selected + 1 >= self.source_picker.filtered.len() {
                        self.source_picker.tab_focused = true;
                        self.source_picker.selected = self.source_picker.filtered.len();
                    } else {
                        self.source_picker.move_down();
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        if self.source_picker.filtered.is_empty() {
                            self.source_picker.selected = 0;
                        } else {
                            self.source_picker.selected = self.source_picker.filtered.len() - 1;
                        }
                        self.source_picker.clamp_scroll();
                    } else if self.source_picker.selected == 0 {
                        self.source_picker.tab_focused = true;
                        self.source_picker.selected = self.source_picker.filtered.len();
                    } else {
                        self.source_picker.move_up();
                    }
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    self.source_picker.tab_focused = true;
                    self.source_picker.prev_tab();
                    self.scan_sources();
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    self.source_picker.tab_focused = true;
                    self.source_picker.next_tab();
                    self.scan_sources();
                }
                KeyCode::Tab => {
                    self.source_picker.next_tab();
                    self.scan_sources();
                }
                KeyCode::BackTab => {
                    self.source_picker.prev_tab();
                    self.scan_sources();
                }
                KeyCode::Enter => {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        self.source_picker.selected = 0;
                        self.source_picker.scroll_offset = 0;
                    } else if self.source_picker.selected < self.source_picker.filtered.len()
                        && let AppMode::SourcePicker(deck) = self.mode
                    {
                        self.select_source_for_deck(deck);
                        self.mode = AppMode::PaneSelect;
                    }
                }
                _ => {}
            },
            PickerInputMode::Insert => {
                self.source_picker.input_mode = PickerInputMode::Normal;
            }
        }
    }

    /// Handle key events in the DeckActions tab
    fn handle_deck_actions_key(&mut self, key: KeyEvent) {
        match self.source_picker.input_mode {
            PickerInputMode::Normal => match key.code {
                KeyCode::Esc => {
                    self.mode = AppMode::PaneSelect;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        self.source_picker.selected = 0;
                        self.source_picker.scroll_offset = 0;
                    } else if self.source_picker.selected + 1 >= self.source_picker.filtered.len() {
                        self.source_picker.tab_focused = true;
                        self.source_picker.selected = self.source_picker.filtered.len();
                    } else {
                        self.source_picker.move_down();
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        if self.source_picker.filtered.is_empty() {
                            self.source_picker.selected = 0;
                        } else {
                            self.source_picker.selected = self.source_picker.filtered.len() - 1;
                        }
                        self.source_picker.clamp_scroll();
                    } else if self.source_picker.selected == 0 {
                        self.source_picker.tab_focused = true;
                        self.source_picker.selected = self.source_picker.filtered.len();
                    } else {
                        self.source_picker.move_up();
                    }
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    self.source_picker.tab_focused = true;
                    self.source_picker.prev_tab();
                    self.scan_sources();
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    self.source_picker.tab_focused = true;
                    self.source_picker.next_tab();
                    self.scan_sources();
                }
                KeyCode::Tab => {
                    self.source_picker.next_tab();
                    self.scan_sources();
                }
                KeyCode::BackTab => {
                    self.source_picker.prev_tab();
                    self.scan_sources();
                }
                KeyCode::Enter => {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        self.source_picker.selected = 0;
                        self.source_picker.scroll_offset = 0;
                    } else if self.source_picker.selected < self.source_picker.filtered.len()
                        && let AppMode::SourcePicker(deck) = self.mode
                    {
                        self.select_source_for_deck(deck);
                        self.mode = AppMode::PaneSelect;
                    }
                }
                _ => {}
            },
            PickerInputMode::Insert => {
                self.source_picker.input_mode = PickerInputMode::Normal;
            }
        }
    }

    /// Enter the add-to-queue sub-view
    fn enter_add_source_mode(&mut self) {
        self.source_picker.adding_to_queue = true;
        self.source_picker.add_tab = AddSourceTab::MpvSockets;
        self.source_picker.tab_focused = true;
        self.source_picker.add_query.clear();
        self.source_picker.add_input_mode = PickerInputMode::Normal;
        self.source_picker.add_marked.clear();
        self.source_picker.add_selection_anchor = None;
        self.source_picker.add_current_dir = self.source_picker.root_dir.clone();
        self.scan_add_sources();
    }

    fn add_sources_to_queue(&mut self, items: Vec<SourcePickerItem>) {
        let items: Vec<_> = items.into_iter().filter(|item| !item.is_dir).collect();
        if items.is_empty() {
            return;
        }

        let was_empty = self.source_picker.queue.is_empty();
        let first_new_idx = self.source_picker.queue.len();
        self.source_picker.queue.extend(items.into_iter().map(|item| QueueItem {
            name: item.name,
            path: item.path,
            is_socket: item.is_socket,
            is_pcm_fifo: item.is_pcm_fifo,
            is_udp: item.is_udp,
        }));
        self.save_queue_to_deck();
        self.source_picker.adding_to_queue = false;
        self.source_picker.tab = SourcePickerTab::Queue;
        self.source_picker.queue_selected = Some(self.source_picker.queue.len() - 1);
        self.source_picker.clamp_queue_scroll();

        if was_empty {
            let loaded_item = self.source_picker.queue[first_new_idx].clone();
            if let AppMode::SourcePicker(deck) = self.mode {
                self.deck_queue_indices[deck.index()] = first_new_idx;
                self.load_queue_item(deck, &loaded_item);
            }
        }
    }

    /// Handle key events in the add-to-queue sub-view
    fn handle_add_source_key(&mut self, key: KeyEvent) {
        match self.source_picker.add_input_mode {
            PickerInputMode::Normal => match key.code {
                KeyCode::Esc => {
                    // Return to Queue tab without adding
                    self.source_picker.adding_to_queue = false;
                    self.source_picker.tab = SourcePickerTab::Queue;
                }
                KeyCode::Char('i') => {
                    self.source_picker.add_input_mode = PickerInputMode::Insert;
                }
                KeyCode::Char('J') | KeyCode::Down
                    if matches!(key.code, KeyCode::Char('J'))
                        || key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    self.source_picker.tab_focused = false;
                    self.source_picker.add_extend_selection(true);
                }
                KeyCode::Char('K') | KeyCode::Up
                    if matches!(key.code, KeyCode::Char('K'))
                        || key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    self.source_picker.tab_focused = false;
                    self.source_picker.add_extend_selection(false);
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.source_picker.add_marked.clear();
                    self.source_picker.add_selection_anchor = None;
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        self.source_picker.add_selected = 0;
                        self.source_picker.add_scroll_offset = 0;
                    } else if self.source_picker.add_selected + 1
                        >= self.source_picker.add_filtered.len()
                    {
                        self.source_picker.tab_focused = true;
                        self.source_picker.add_selected = self.source_picker.add_filtered.len();
                    } else {
                        self.source_picker.add_move_down();
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.source_picker.add_marked.clear();
                    self.source_picker.add_selection_anchor = None;
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        self.source_picker.add_selected =
                            self.source_picker.add_filtered.len().saturating_sub(1);
                        self.source_picker.add_clamp_scroll();
                    } else if self.source_picker.add_selected == 0 {
                        self.source_picker.tab_focused = true;
                        self.source_picker.add_selected = self.source_picker.add_filtered.len();
                    } else {
                        self.source_picker.add_move_up();
                    }
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    if self.source_picker.add_tab == AddSourceTab::AudioFiles {
                        // Go up a directory
                        let can_go_up =
                            self.source_picker.add_current_dir != self.source_picker.root_dir;
                        if can_go_up {
                            if let Some(parent) = self.source_picker.add_current_dir.parent() {
                                self.source_picker.add_current_dir = parent.to_path_buf();
                                self.scan_add_sources();
                            }
                        } else {
                            self.source_picker.add_prev_tab();
                            self.scan_add_sources();
                        }
                    } else {
                        self.source_picker.add_prev_tab();
                        self.scan_add_sources();
                    }
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    if self.source_picker.add_tab == AddSourceTab::AudioFiles {
                        // Enter directory or move to next tab
                        if let Some(item) = self.source_picker.add_selected_item().cloned() {
                            if item.is_dir {
                                self.source_picker.add_current_dir = item.path;
                                self.source_picker.add_marked.clear();
                                self.source_picker.add_selection_anchor = None;
                                self.scan_add_sources();
                            } else {
                                self.source_picker.add_next_tab();
                                self.scan_add_sources();
                            }
                        } else {
                            self.source_picker.add_next_tab();
                            self.scan_add_sources();
                        }
                    } else {
                        self.source_picker.add_next_tab();
                        self.scan_add_sources();
                    }
                }
                KeyCode::Tab => {
                    self.source_picker.add_next_tab();
                    self.scan_add_sources();
                }
                KeyCode::BackTab => {
                    self.source_picker.add_prev_tab();
                    self.scan_add_sources();
                }
                KeyCode::Char('g') => {
                    self.source_picker.tab_focused = false;
                    self.source_picker.add_marked.clear();
                    self.source_picker.add_selection_anchor = None;
                    self.source_picker.add_selected = 0;
                    self.source_picker.add_scroll_offset = 0;
                }
                KeyCode::Char('G') => {
                    self.source_picker.tab_focused = false;
                    self.source_picker.add_marked.clear();
                    self.source_picker.add_selection_anchor = None;
                    if self.source_picker.add_filtered.is_empty() {
                        self.source_picker.add_selected = 0;
                    } else {
                        self.source_picker.add_selected = self.source_picker.add_filtered.len() - 1;
                    }
                    self.source_picker.add_clamp_scroll();
                }
                KeyCode::Enter => {
                    if self.source_picker.tab_focused {
                        self.source_picker.tab_focused = false;
                        self.source_picker.add_selected = 0;
                        self.source_picker.add_scroll_offset = 0;
                    } else if let Some(item) = self.source_picker.add_selected_item().cloned() {
                        if item.is_dir {
                            // Enter directory
                            self.source_picker.add_current_dir = item.path;
                            self.source_picker.add_query.clear();
                            self.source_picker.add_marked.clear();
                            self.source_picker.add_selection_anchor = None;
                            self.scan_add_sources();
                        } else {
                            let items = if self.source_picker.add_marked.is_empty() {
                                vec![item]
                            } else {
                                self.source_picker
                                    .add_filtered
                                    .iter()
                                    .filter_map(|&idx| self.source_picker.add_items.get(idx))
                                    .filter(|item| {
                                        self.source_picker.add_marked.contains(&item.path)
                                    })
                                    .cloned()
                                    .collect()
                            };
                            self.add_sources_to_queue(items);
                        }
                    }
                }
                _ => {}
            },
            PickerInputMode::Insert => match key.code {
                KeyCode::Esc => {
                    self.source_picker.add_input_mode = PickerInputMode::Normal;
                }
                KeyCode::Enter => {
                    if let Some(item) = self.source_picker.add_selected_item().cloned() {
                        if item.is_dir {
                            self.source_picker.add_current_dir = item.path;
                            self.source_picker.add_query.clear();
                            self.source_picker.add_input_mode = PickerInputMode::Normal;
                            self.source_picker.add_marked.clear();
                            self.source_picker.add_selection_anchor = None;
                            self.scan_add_sources();
                        } else {
                            let items = if self.source_picker.add_marked.is_empty() {
                                vec![item]
                            } else {
                                self.source_picker
                                    .add_filtered
                                    .iter()
                                    .filter_map(|&idx| self.source_picker.add_items.get(idx))
                                    .filter(|item| {
                                        self.source_picker.add_marked.contains(&item.path)
                                    })
                                    .cloned()
                                    .collect()
                            };
                            self.add_sources_to_queue(items);
                        }
                    }
                }
                KeyCode::Backspace => {
                    self.source_picker.add_query.pop();
                    self.source_picker.add_filter();
                }
                KeyCode::Tab => {
                    self.source_picker.add_next_tab();
                    self.scan_add_sources();
                }
                KeyCode::BackTab => {
                    self.source_picker.add_prev_tab();
                    self.scan_add_sources();
                }
                KeyCode::Up => {
                    self.source_picker.add_move_up();
                }
                KeyCode::Down => {
                    self.source_picker.add_move_down();
                }
                KeyCode::Char(c) => {
                    self.source_picker.add_query.push(c);
                    self.source_picker.add_filter();
                }
                _ => {}
            },
        }
    }

    /// Save the source picker's queue back to the deck's queue storage
    fn save_queue_to_deck(&mut self) {
        if let AppMode::SourcePicker(deck) = self.mode {
            let deck_idx = deck.index();
            self.deck_queues[deck_idx] = self.source_picker.queue.clone();
        }
    }

    /// Fully reset a deck: drop MPV/SC client, reset channel state to defaults.
    fn clear_deck(&mut self, deck: Deck) {
        self.clear_deck_impl(deck, true, true);
    }

    /// Clear a deck without advancing to the next queue item
    fn clear_deck_no_advance(&mut self, deck: Deck) {
        self.clear_deck_impl(deck, false, true);
    }

    fn clear_deck_impl(&mut self, deck: Deck, advance_if_queued: bool, clear_queue: bool) {
        let ch_idx = match deck {
            Deck::A => self.mixer.dj.deck_a_channel,
            Deck::B => self.mixer.dj.deck_b_channel,
            Deck::C => self.mixer.dj.deck_c_channel,
        };

        // Check if there's a next item in the queue before clearing
        let deck_idx = deck.index();
        let next_queue_item = if advance_if_queued {
            let current_idx = self.deck_queue_indices[deck_idx];
            let next_idx = current_idx + 1;
            if next_idx < self.deck_queues[deck_idx].len() {
                self.deck_queue_indices[deck_idx] = next_idx;
                Some(self.deck_queues[deck_idx][next_idx].clone())
            } else {
                None
            }
        } else {
            None
        };

        let was_capture = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_capture(ch_idx))
            .unwrap_or(false);

        let source_id = self
            .mixer
            .get_channel(ch_idx)
            .and_then(|c| c.source_id.clone());

        if was_capture
            && let Some(sock) = source_id
                .as_deref()
                .map(std::path::Path::new)
                .and_then(|fifo| {
                    Self::route_socket_candidates_for_fifo(fifo)
                        .into_iter()
                        .find(|p| p.exists())
                })
        {
            let sock_str = sock.to_string_lossy().to_string();
            let mut client = MpvClient::new(sock_str);
            if client.connect().is_ok() {
                let _ = client.send_command(vec![
                    serde_json::json!("set_property"),
                    serde_json::json!("pause"),
                    serde_json::json!(true),
                ]);
            }
        }

        // Mute and drop MPV client
        match deck {
            Deck::A => {
                if let Some(ref mut client) = self.mpv_deck_a {
                    let _ = client.set_mute(true);
                }
                self.mpv_deck_a = None;
                self.reset_route_clients(ch_idx);
                self.route_meta_poll_counter = 0;
            }
            Deck::B => {
                if let Some(ref mut client) = self.mpv_deck_b {
                    let _ = client.set_mute(true);
                }
                self.mpv_deck_b = None;
                self.reset_route_clients(ch_idx);
                self.route_meta_poll_counter = 0;
            }
            Deck::C => {
                if let Some(ref mut client) = self.mpv_deck_c {
                    let _ = client.set_mute(true);
                }
                self.mpv_deck_c = None;
                self.reset_route_clients(ch_idx);
                self.route_meta_poll_counter = 0;
            }
        }

        // Clear engine state
        self.engine_was_playing[deck.index()] = false;

        // Free SC synths
        match deck {
            Deck::A => {
                if let Some(ref old) = self.sc_deck_a {
                    let _ = old.free_all();
                }
                self.sc_deck_a = None;
            }
            Deck::B => {
                if let Some(ref old) = self.sc_deck_b {
                    let _ = old.free_all();
                }
                self.sc_deck_b = None;
            }
            Deck::C => {
                if let Some(ref old) = self.sc_deck_c {
                    let _ = old.free_all();
                }
                self.sc_deck_c = None;
            }
        }

        // Stop Rust audio engine decoder
        if let Some(ref engine) = self.audio_engine {
            engine.stop_decoder(ch_idx);
        }
        // Reset channel to defaults (preserves name and index)
        let deck_c_ch = self.mixer.dj.deck_c_channel;
        if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
            let name = ch.name.clone();
            let index = ch.index;
            *ch = crate::state::MixerChannel::new(name, index);
            if ch_idx == deck_c_ch {
                ch.pfl = true;
            }
        }

        if clear_queue {
            self.deck_queues[deck_idx].clear();
            self.deck_queue_indices[deck_idx] = 0;
        }

        // Advance to next queue item if available
        if let Some(item) = next_queue_item {
            self.load_queue_item(deck, &item);
        }
    }

    /// Load a queue item onto a deck
    fn load_queue_item(&mut self, deck: Deck, item: &QueueItem) {
        let channel_idx = match deck {
            Deck::A => self.mixer.dj.deck_a_channel,
            Deck::B => self.mixer.dj.deck_b_channel,
            Deck::C => self.mixer.dj.deck_c_channel,
        };
        if !item.is_socket {
            let old_mpv = match deck {
                Deck::A => self.mpv_deck_a.take(),
                Deck::B => self.mpv_deck_b.take(),
                Deck::C => self.mpv_deck_c.take(),
            };
            if let Some(mut client) = old_mpv {
                let _ = client.set_mute(true);
            }
        }
        if !item.is_pcm_fifo {
            self.reset_route_clients(channel_idx);
        }
        if let Some(engine) = self.audio_engine.as_ref() {
            engine.stop_decoder(channel_idx);
        }
        let deck_idx = deck.index();
        let (has_prev, has_next) = queue_nav_available(
            self.deck_queue_indices[deck_idx],
            self.deck_queues[deck_idx].len(),
        );
        if let Some(channel) = self.mixer.get_channel_mut(channel_idx) {
            channel.has_prev_track = has_prev;
            channel.has_next_track = has_next;
        }

        if item.is_socket {
            // MPV socket
            let mut client = crate::audio::MpvClient::new(item.path.to_string_lossy().to_string());
            if client.connect().is_err() {
                return;
            }
            let file_path = client.get_path().ok().map(std::path::PathBuf::from);
            if let Some(ref path) = file_path
                && let Some(ref engine) = self.audio_engine
            {
                let path_str = path.to_string_lossy().to_string();
                engine.load_file(channel_idx, path_str);
            }
            let _ = client.ensure_astats();
            client.start_metering();

            if let Some(channel) = self.mixer.get_channel_mut(channel_idx) {
                channel.name = item.name.clone();
                channel.connected = true;
                channel.source_id = Some(item.path.to_string_lossy().to_string());
            }

            match deck {
                Deck::A => self.mpv_deck_a = Some(client),
                Deck::B => self.mpv_deck_b = Some(client),
                Deck::C => self.mpv_deck_c = Some(client),
            }
        } else if item.is_pcm_fifo {
            // FIFO capture
            let route_name = Some(item.name.clone());
            if self
                .attach_fifo_capture_to_deck(deck, &item.path, route_name)
                .is_ok()
                && let Some(channel) = self.mixer.get_channel_mut(channel_idx)
            {
                channel.connected = true;
                channel.source_id = Some(item.path.to_string_lossy().to_string());
            }
        } else {
            // Audio file
            let path_str = item.path.to_string_lossy().to_string();
            if let Some(channel) = self.mixer.get_channel_mut(channel_idx) {
                channel.name = item.name.clone();
                channel.connected = true;
                channel.playing = true;
                channel.source_id = Some(path_str.clone());
            }
            if let Some(ref engine) = self.audio_engine {
                engine.load_file(channel_idx, path_str);
            }
        }
    }

    /// Assign selected source to deck
    fn select_source_for_deck(&mut self, deck: Deck) {
        // Handle DeckActions tab — clear the selected deck
        if self.source_picker.tab == SourcePickerTab::DeckActions {
            if let Some(item) = self.source_picker.selected_item().cloned() {
                if item.name.contains("Clear Playlist A") {
                    self.clear_deck_no_advance(Deck::A);
                } else if item.name.contains("Clear Playlist B") {
                    self.clear_deck_no_advance(Deck::B);
                } else if item.name.contains("Clear Playlist C") {
                    self.clear_deck_no_advance(Deck::C);
                }
            }
            self.mode = AppMode::PaneSelect;
            return;
        }

        if let Some(item) = self.source_picker.selected_item().cloned() {
            self.log_debug(format!(
                "select_source_for_deck: deck={:?} item_name='{}' path={} is_socket={} is_pcm_fifo={} is_udp={} is_dir={}",
                deck, item.name, item.path.display(), item.is_socket, item.is_pcm_fifo, item.is_udp, item.is_dir
            ));
            // Deck::C uses cue_channel which is not in the channels Vec
            if deck == Deck::C {
                self.select_source_for_deck_c(&item);
                return;
            }

            let channel_idx = match deck {
                Deck::A => self.mixer.dj.deck_a_channel,
                Deck::B => self.mixer.dj.deck_b_channel,
                Deck::C => unreachable!(),
            };

            // Free old SC synths if switching away from SC source
            // Also stop MPV decoder and mute MPV client if switching away from MPV
            match deck {
                Deck::A => {
                    if let Some(ref old) = self.sc_deck_a {
                        let _ = old.free_all();
                    }
                    self.sc_deck_a = None;
                    self.reset_route_clients(channel_idx);
                    if let Some(ref mut client) = self.mpv_deck_a {
                        let _ = client.set_mute(true);
                    }
                    self.mpv_deck_a = None;
                    if let Some(ref engine) = self.audio_engine {
                        engine.stop_decoder(channel_idx);
                    }
                    self.route_nav_cache[channel_idx] = None;
                    self.route_nav_last_ms[channel_idx] = 0;
                    self.route_scrub_last_ms[channel_idx] = 0;
                    self.route_duration_last_ms[channel_idx] = 0;
                }
                Deck::B => {
                    if let Some(ref old) = self.sc_deck_b {
                        let _ = old.free_all();
                    }
                    self.sc_deck_b = None;
                    self.reset_route_clients(channel_idx);
                    if let Some(ref mut client) = self.mpv_deck_b {
                        let _ = client.set_mute(true);
                    }
                    self.mpv_deck_b = None;
                    if let Some(ref engine) = self.audio_engine {
                        engine.stop_decoder(channel_idx);
                    }
                    self.route_nav_cache[channel_idx] = None;
                    self.route_nav_last_ms[channel_idx] = 0;
                    self.route_scrub_last_ms[channel_idx] = 0;
                    self.route_duration_last_ms[channel_idx] = 0;
                }
                Deck::C => {
                    if let Some(ref old) = self.sc_deck_c {
                        let _ = old.free_all();
                    }
                    self.sc_deck_c = None;
                    self.reset_route_clients(channel_idx);
                    if let Some(ref mut client) = self.mpv_deck_c {
                        let _ = client.set_mute(true);
                    }
                    self.mpv_deck_c = None;
                    if let Some(ref engine) = self.audio_engine {
                        engine.stop_decoder(channel_idx);
                    }
                    self.route_nav_cache[channel_idx] = None;
                    self.route_nav_last_ms[channel_idx] = 0;
                    self.route_scrub_last_ms[channel_idx] = 0;
                    self.route_duration_last_ms[channel_idx] = 0;
                }
            }

            if item.is_pcm_fifo {
                match self.attach_fifo_capture_to_deck(deck, &item.path, Some(item.name.clone())) {
                    Ok(()) => {
                        self.route_scrub_last_ms[channel_idx] = 0;
                        self.route_duration_last_ms[channel_idx] = 0;
                    }
                    Err(e) => {
                        self.log_debug(format!(
                            "Failed to attach PCM FIFO: {} — {}",
                            item.path.display(),
                            e
                        ));
                    }
                }
            } else if item.is_socket {
                // MPV socket - create and connect client
                let socket_path = item.path.to_string_lossy().to_string();
                let mut client = MpvClient::new(&socket_path);

                let connect_result = client.connect();
                let connected = connect_result.is_ok();
                if !connected {
                    self.log_debug(format!(
                        "MPV connect failed for {}: {:?}",
                        socket_path,
                        connect_result.err()
                    ));
                }

                // Update channel state
                if let Some(channel) = self.mixer.channels.get_mut(channel_idx) {
                    channel.name = item.name.clone();
                    channel.connected = connected;
                    channel.source_id = Some(socket_path.clone());
                    channel.base_bpm = 0.0; // Reset for new track detection
                    channel.uses_supercollider = false;

                    if connected {
                        if let Ok(paused) = client.get_pause() {
                            channel.playing = !paused;
                        }
                        // Add astats filter for real-time metering
                        let _ = client.ensure_astats();
                        client.start_metering();

                        // Query MPV metadata for key info (fast, no file decode needed)
                        match client.get_key_from_metadata() {
                            Some(key) => {
                                tracing::debug!(
                                    "Got key from MPV metadata for ch{}: {}",
                                    channel_idx,
                                    key
                                );
                                channel.key = Some(key);
                                channel.key_offset = 0;
                            }
                            None => {
                                tracing::debug!(
                                    "No key found in MPV metadata for ch{}",
                                    channel_idx
                                );
                            }
                        }

                        // Query BPM from metadata tags (TBPM/bpm/tempo) — instant, no decode
                        if let Some(meta_bpm) = client.get_bpm_from_metadata() {
                            tracing::debug!(
                                "Got BPM from MPV metadata for ch{}: {:.1}",
                                channel_idx,
                                meta_bpm
                            );
                            channel.bpm = Some(meta_bpm);
                            channel.base_bpm = meta_bpm;
                            channel.target_bpm = meta_bpm;
                        }
                    }
                }

                // Get file path for BPM analysis before storing client
                let file_path = if connected {
                    match client.get_path() {
                        Ok(p) => {
                            self.log_debug(format!("MPV path ch{}: {}", channel_idx, p));
                            Some(PathBuf::from(p))
                        }
                        Err(e) => {
                            self.log_debug(format!("MPV path error ch{}: {}", channel_idx, e));
                            None
                        }
                    }
                } else {
                    None
                };

                // Store client for this deck
                match deck {
                    Deck::A => self.mpv_deck_a = Some(client),
                    Deck::B => self.mpv_deck_b = Some(client),
                    Deck::C => self.mpv_deck_c = Some(client),
                }

                // Also add to legacy manager
                let source = AudioSource::new(item.name, socket_path);
                self.audio_manager.add_source(source);

                // Load file into Rust audio engine decoder (always want engine processing)
                if file_path.is_some() {
                    self.log_debug(format!(
                        "MPV loading into engine ch{}: {:?}",
                        channel_idx, file_path
                    ));
                } else {
                    self.log_debug(format!(
                        "MPV no file_path for ch{}, skipping engine load",
                        channel_idx
                    ));
                }
                if let Some(ref engine) = self.audio_engine {
                    if let Some(ref path) = file_path {
                        let path_str = path.to_string_lossy().to_string();
                        engine.load_file(channel_idx, path_str);
                    }
                } else {
                    self.log_debug(format!("MPV no audio_engine for ch{}", channel_idx));
                }

                // Sync crossfader/volume state to engine for new source
                self.sync_volume_to_mpv(channel_idx);
                self.sync_mute_to_mpv(channel_idx);
                self.sync_playpause_to_mpv(channel_idx);

                // Trigger BPM+key analysis if we have a file path
                if let Some(ref path) = file_path {
                    if path.exists() {
                        self.log_debug(format!(
                            "BPM analysis: analyzing ch{} ({})",
                            channel_idx,
                            path.display()
                        ));
                        let pending = self.pending_bpm.clone();
                        let on_result = Arc::new(Mutex::new(
                            move |result: Result<crate::audio::BpmResult, String>| match result {
                                Ok(r) => {
                                    if let Ok(mut queue) = pending.lock() {
                                        queue.push((channel_idx, r.bpm, r.key));
                                    }
                                }
                                Err(e) => {
                                    if let Ok(mut queue) = pending.lock() {
                                        queue.push((usize::MAX, 0.0, Some(e)));
                                    }
                                }
                            },
                        ));
                        BpmAnalyzer::analyze_file(path, on_result);
                    } else {
                        self.log_debug(format!(
                            "BPM analysis: file not found for ch{}: {}",
                            channel_idx,
                            path.display()
                        ));
                    }
                }
            } else if item.is_udp {
                // UDP source (e.g., SuperCollider) - create client and connect
                let addr = item.path.to_string_lossy().to_string();
                // Strip "udp://" prefix if present
                let addr = addr.strip_prefix("udp://").unwrap_or(&addr);
                let base_node_id = match deck {
                    Deck::A => 1000,
                    Deck::B => 2000,
                    Deck::C => 3000,
                };
                let input_bus = 4;
                let mut client = SuperColliderClient::new(addr, base_node_id, input_bus);
                let connected = client.connect().is_ok();

                if let Some(channel) = self.mixer.get_channel_mut(channel_idx) {
                    channel.name = item.name.clone();
                    channel.connected = connected;
                    channel.playing = connected;
                    channel.bpm = Some(SC_DEFAULT_BPM);
                    channel.base_bpm = SC_DEFAULT_BPM;
                    channel.target_bpm = SC_DEFAULT_BPM;
                    channel.source_id = Some(addr.to_string());
                    channel.uses_supercollider = true;
                    channel.scrub_direction = 0.0;
                    channel.scrub_speed = 0.0;
                    channel.scrub_accumulator = 0.0;
                }

                // SuperDirt composite output routes to bus 4; whichever deck selects SC controls it.
                if connected {
                    let _ = client.send_synth_def();
                    let _ = client.create_group();
                    let _ = client.create_synth();

                    // Sync current mixer settings to the new synth
                    if let Some(channel) = self.mixer.get_channel(channel_idx) {
                        let xf_gain = if deck == Deck::A {
                            self.calculate_crossfader_gains().0
                        } else {
                            self.calculate_crossfader_gains().1
                        };
                        let vol = (channel.fader
                            * xf_gain
                            * self.mixer.master.fader
                            * 2.0
                            * SC_GAIN_BOOST)
                            .clamp(0.0, 8.0);
                        let _ = client.set_volume(vol);
                        // Apply unified filter (crossfade between LPF and HPF)
                        let cutoff = channel.filter_cutoff;
                        let freq_pos = channel.filter_freq;
                        let intensity = cutoff;
                        let log_min = 20f32.log10();
                        let log_max = 20000f32.log10();
                        let actual_freq = 10f32.powf(log_min + freq_pos * (log_max - log_min));

                        let blend = if actual_freq <= 300.0 {
                            0.0
                        } else if actual_freq >= 3000.0 {
                            1.0
                        } else {
                            let t = (actual_freq - 300.0) / (3000.0 - 300.0);
                            t * t * (3.0 - 2.0 * t)
                        };

                        let lpf_target = actual_freq + (20000.0 - actual_freq) * blend;
                        let hpf_target = 20.0 + (actual_freq - 20.0) * blend;
                        let effective_lpf = 20000.0 - (20000.0 - lpf_target) * intensity;
                        let soft_lpf = if effective_lpf < 1000.0 {
                            let norm = ((effective_lpf - 200.0) / 800.0).clamp(0.0, 1.0);
                            200.0 + 800.0 * norm.sqrt()
                        } else {
                            effective_lpf
                        };
                        let effective_hpf = 20.0 + (hpf_target - 20.0) * intensity;
                        let _ = client.set_lpf(soft_lpf.clamp(200.0, 20000.0));
                        let _ = client.set_hpf(effective_hpf.clamp(20.0, 8000.0));
                        // Apply kill switches: extreme cut values
                        let effective_low = if channel.eq_low_kill {
                            -24.0
                        } else {
                            channel.eq_low
                        };
                        let effective_mid = if channel.eq_mid_kill {
                            -24.0
                        } else {
                            channel.eq_mid
                        };
                        let effective_high = if channel.eq_high_kill {
                            -24.0
                        } else {
                            channel.eq_high
                        };
                        let _ = client.set_eq(effective_low, effective_mid, effective_high);
                        let _ = client.set_pan(channel.pan);
                    }
                }

                // Store client for this deck
                match deck {
                    Deck::A => self.sc_deck_a = Some(client),
                    Deck::B => self.sc_deck_b = Some(client),
                    Deck::C => self.sc_deck_c = Some(client),
                }
            } else {
                // Audio file — load directly into the Rust audio engine
                let path_str = item.path.to_string_lossy().to_string();
                self.log_debug(format!(
                    "Audio file branch: ch{} path='{}' exists={}",
                    channel_idx,
                    path_str,
                    item.path.exists()
                ));
                if let Some(channel) = self.mixer.channels.get_mut(channel_idx) {
                    channel.name = item.name.clone();
                    channel.connected = true;
                    channel.playing = true;
                    channel.uses_supercollider = false;
                    channel.source_id = Some(path_str.clone());
                }
                if let Some(ref engine) = self.audio_engine {
                    engine.load_file(channel_idx, path_str.clone());
                    self.log_debug(format!(
                        "Audio file loaded: ch{} has_decoder={} duration={:.1}",
                        channel_idx,
                        engine.has_decoder(channel_idx),
                        engine.duration[channel_idx].load()
                    ));
                } else {
                    self.log_debug(format!("Audio file: no audio_engine for ch{}", channel_idx));
                }

                self.engine_was_playing[deck.index()] = true;
                // Sync crossfader/volume state to engine for new source
                self.sync_volume_to_mpv(channel_idx);
                self.sync_mute_to_mpv(channel_idx);
                self.sync_playpause_to_mpv(channel_idx);
                self.log_debug(format!(
                    "Audio file sync done: ch{} playing={}",
                    channel_idx,
                    self.mixer
                        .get_channel(channel_idx)
                        .map(|c| c.playing)
                        .unwrap_or(false)
                ));
                // Trigger BPM+key analysis
                if item.path.exists() {
                    let pending = self.pending_bpm.clone();
                    let on_result = Arc::new(Mutex::new(
                        move |result: Result<crate::audio::BpmResult, String>| match result {
                            Ok(r) => {
                                if let Ok(mut queue) = pending.lock() {
                                    queue.push((channel_idx, r.bpm, r.key));
                                }
                            }
                            Err(e) => {
                                if let Ok(mut queue) = pending.lock() {
                                    queue.push((usize::MAX, 0.0, Some(e)));
                                }
                            }
                        },
                    ));
                    BpmAnalyzer::analyze_file(&item.path, on_result);
                }
            }
        }
    }

    /// Assign selected source to Deck C (CUE channel)
    fn select_source_for_deck_c(&mut self, item: &SourcePickerItem) {
        let channel_idx = self.mixer.dj.deck_c_channel;
        // Free old SC synths if switching away from SC source
        if let Some(ref old) = self.sc_deck_c {
            let _ = old.free_all();
        }
        self.sc_deck_c = None;
        // Stop MPV decoder and mute MPV client if switching sources
        if let Some(ref mut client) = self.mpv_deck_c {
            let _ = client.set_mute(true);
        }
        self.mpv_deck_c = None;
        self.reset_route_clients(channel_idx);
        if let Some(ref engine) = self.audio_engine {
            engine.stop_decoder(channel_idx);
        }

        if item.is_pcm_fifo {
            match self.attach_fifo_capture_to_deck(Deck::C, &item.path, Some(item.name.clone())) {
                Ok(()) => {
                    self.route_nav_cache[channel_idx] = None;
                    self.route_nav_last_ms[channel_idx] = 0;
                    self.route_scrub_last_ms[channel_idx] = 0;
                    self.route_duration_last_ms[channel_idx] = 0;
                }
                Err(e) => {
                    self.log_debug(format!(
                        "Failed to attach PCM FIFO: {} — {}",
                        item.path.display(),
                        e
                    ));
                }
            }
        } else if item.is_socket {
            let socket_path = item.path.to_string_lossy().to_string();
            let mut client = MpvClient::new(&socket_path);
            let connected = client.connect().is_ok();

            self.mixer.cue_channel.name = item.name.clone();
            self.mixer.cue_channel.connected = connected;
            self.mixer.cue_channel.source_id = Some(socket_path.clone());
            self.mixer.cue_channel.base_bpm = 0.0; // Reset for new track
            self.mixer.cue_channel.uses_supercollider = false;

            if connected {
                if let Ok(paused) = client.get_pause() {
                    self.mixer.cue_channel.playing = !paused;
                }
                // Add astats filter for real-time metering
                let _ = client.ensure_astats();
                client.start_metering();

                // Query BPM from metadata tags — instant, no decode
                if let Some(meta_bpm) = client.get_bpm_from_metadata() {
                    tracing::debug!("Got BPM from MPV metadata for CUE ch: {:.1}", meta_bpm);
                    self.mixer.cue_channel.bpm = Some(meta_bpm);
                    self.mixer.cue_channel.base_bpm = meta_bpm;
                    self.mixer.cue_channel.target_bpm = meta_bpm;
                }
            }

            // Get file path for BPM analysis before storing client
            let file_path = if connected {
                match client.get_path() {
                    Ok(p) => {
                        self.log_debug(format!("MPV path CUE: {}", p));
                        Some(PathBuf::from(p))
                    }
                    Err(e) => {
                        self.log_debug(format!("MPV path error CUE: {}", e));
                        None
                    }
                }
            } else {
                None
            };

            self.mpv_deck_c = Some(client);

            // Re-apply stored CUE output device to the new MPV client
            if let Some(ref mpv_dev) = self.cue_output.selected_mpv_name().map(|s| s.to_string())
                && let Some(ref mut client) = self.mpv_deck_c
            {
                client.set_audio_device(mpv_dev).ok();
            }

            self.sync_volume_to_mpv(channel_idx);
            self.sync_mute_to_mpv(channel_idx);
            self.sync_playpause_to_mpv(channel_idx);

            let source = AudioSource::new(item.name.clone(), socket_path);
            self.audio_manager.add_source(source);

            // Trigger BPM+key analysis if we have a file path
            if let Some(path) = file_path {
                if path.exists() {
                    self.log_debug(format!(
                        "BPM analysis: analyzing CUE ch ({})",
                        path.display()
                    ));
                    let pending = self.pending_bpm.clone();
                    let channel_idx = self.mixer.dj.deck_c_channel;
                    let on_result = Arc::new(Mutex::new(
                        move |result: Result<crate::audio::BpmResult, String>| match result {
                            Ok(r) => {
                                if let Ok(mut queue) = pending.lock() {
                                    queue.push((channel_idx, r.bpm, r.key));
                                }
                            }
                            Err(e) => {
                                if let Ok(mut queue) = pending.lock() {
                                    queue.push((usize::MAX, 0.0, Some(e)));
                                }
                            }
                        },
                    ));
                    BpmAnalyzer::analyze_file(&path, on_result);
                } else {
                    self.log_debug(format!(
                        "BPM analysis: file not found for CUE: {}",
                        path.display()
                    ));
                }
            }
        } else if item.is_udp {
            let addr = item.path.to_string_lossy().to_string();
            let addr = addr.strip_prefix("udp://").unwrap_or(&addr);
            let mut client = SuperColliderClient::new(addr, 3000, 4);
            let connected = client.connect().is_ok();

            self.mixer.cue_channel.name = item.name.clone();
            self.mixer.cue_channel.connected = connected;
            self.mixer.cue_channel.playing = connected;
            self.mixer.cue_channel.bpm = Some(SC_DEFAULT_BPM);
            self.mixer.cue_channel.base_bpm = SC_DEFAULT_BPM;
            self.mixer.cue_channel.target_bpm = SC_DEFAULT_BPM;
            self.mixer.cue_channel.source_id = Some(addr.to_string());
            self.mixer.cue_channel.uses_supercollider = true;
            self.mixer.cue_channel.scrub_direction = 0.0;
            self.mixer.cue_channel.scrub_speed = 0.0;
            self.mixer.cue_channel.scrub_accumulator = 0.0;

            if connected {
                let _ = client.send_synth_def();
                let _ = client.create_group();
                let _ = client.create_synth();

                let vol = (self.mixer.cue_channel.fader
                    * self.mixer.master.fader
                    * 2.0
                    * 6.0
                    * SC_GAIN_BOOST)
                    .clamp(0.0, 8.0);
                let _ = client.set_volume(vol);
                // Apply unified filter for CUE channel (crossfade between LPF and HPF)
                let cutoff = self.mixer.cue_channel.filter_cutoff;
                let freq_pos = self.mixer.cue_channel.filter_freq;
                let intensity = cutoff;
                let log_min = 20f32.log10();
                let log_max = 20000f32.log10();
                let actual_freq = 10f32.powf(log_min + freq_pos * (log_max - log_min));

                let blend = if actual_freq <= 300.0 {
                    0.0
                } else if actual_freq >= 3000.0 {
                    1.0
                } else {
                    let t = (actual_freq - 300.0) / (3000.0 - 300.0);
                    t * t * (3.0 - 2.0 * t)
                };

                let lpf_target = actual_freq + (20000.0 - actual_freq) * blend;
                let hpf_target = 20.0 + (actual_freq - 20.0) * blend;
                let effective_lpf = 20000.0 - (20000.0 - lpf_target) * intensity;
                let soft_lpf = if effective_lpf < 1000.0 {
                    let norm = ((effective_lpf - 200.0) / 800.0).clamp(0.0, 1.0);
                    200.0 + 800.0 * norm.sqrt()
                } else {
                    effective_lpf
                };
                let effective_hpf = 20.0 + (hpf_target - 20.0) * intensity;
                let _ = client.set_lpf(soft_lpf.clamp(200.0, 20000.0));
                let _ = client.set_hpf(effective_hpf.clamp(20.0, 8000.0));
                // Apply kill switches: extreme cut values
                let effective_low = if self.mixer.cue_channel.eq_low_kill {
                    -24.0
                } else {
                    self.mixer.cue_channel.eq_low
                };
                let effective_mid = if self.mixer.cue_channel.eq_mid_kill {
                    -24.0
                } else {
                    self.mixer.cue_channel.eq_mid
                };
                let effective_high = if self.mixer.cue_channel.eq_high_kill {
                    -24.0
                } else {
                    self.mixer.cue_channel.eq_high
                };
                let _ = client.set_eq(effective_low, effective_mid, effective_high);
                let _ = client.set_pan(self.mixer.cue_channel.pan);
            }

            self.sc_deck_c = Some(client);
        } else {
            // Audio file — load directly into the Rust audio engine
            let path_str = item.path.to_string_lossy().to_string();
            self.mixer.cue_channel.name = item.name.clone();
            self.mixer.cue_channel.connected = true;
            self.mixer.cue_channel.playing = true;
            self.mixer.cue_channel.uses_supercollider = false;
            self.mixer.cue_channel.source_id = Some(path_str.clone());
            if let Some(ref engine) = self.audio_engine {
                engine.load_file(channel_idx, path_str);
            }
            // Trigger BPM+key analysis
            if item.path.exists() {
                let pending = self.pending_bpm.clone();
                let on_result = Arc::new(Mutex::new(
                    move |result: Result<crate::audio::BpmResult, String>| match result {
                        Ok(r) => {
                            if let Ok(mut queue) = pending.lock() {
                                queue.push((channel_idx, r.bpm, r.key));
                            }
                        }
                        Err(e) => {
                            if let Ok(mut queue) = pending.lock() {
                                queue.push((usize::MAX, 0.0, Some(e)));
                            }
                        }
                    },
                ));
                BpmAnalyzer::analyze_file(&item.path, on_result);
            }
        }
    }

    /// Calculate crossfader gains for deck A and B based on current position and curve
    /// Crossfader position: -1.0 = full A, 0.0 = center (both 100%), 1.0 = full B
    /// Uses equal-power (sqrt) curve matching the audio engine.
    fn calculate_crossfader_gains(&self) -> (f32, f32) {
        let xf = self.mixer.dj.crossfader; // -1.0 to 1.0
        // Remap to engine space: 0.0 = full A, 1.0 = full B
        let cf = ((xf + 1.0) * 0.5).clamp(0.0, 1.0);
        let a = (1.0 - cf).sqrt();
        let b = cf.sqrt();
        (a, b)
    }

    /// Get the MPV client for a channel index.
    fn mpv_for_channel(&mut self, ch_idx: usize) -> Option<&mut MpvClient> {
        if ch_idx == self.mixer.dj.deck_a_channel {
            self.mpv_deck_a.as_mut()
        } else if ch_idx == self.mixer.dj.deck_b_channel {
            self.mpv_deck_b.as_mut()
        } else if ch_idx == self.mixer.dj.deck_c_channel {
            self.mpv_deck_c.as_mut()
        } else {
            None
        }
    }

    /// Get the SuperCollider client for a channel index.
    fn sc_for_channel(&mut self, ch_idx: usize) -> Option<&mut SuperColliderClient> {
        if ch_idx == self.mixer.dj.deck_a_channel {
            self.sc_deck_a.as_mut()
        } else if ch_idx == self.mixer.dj.deck_b_channel {
            self.sc_deck_b.as_mut()
        } else if ch_idx == self.mixer.dj.deck_c_channel {
            self.sc_deck_c.as_mut()
        } else {
            None
        }
    }

    fn swap_cue_with_deck(&mut self, target: SendTarget) {
        let target_idx = match target {
            SendTarget::A => self.mixer.dj.deck_a_channel,
            SendTarget::B => self.mixer.dj.deck_b_channel,
        };
        let cue_idx = self.mixer.dj.deck_c_channel;

        let target_mpv_path = match target {
            SendTarget::A => self.mpv_deck_a.as_mut(),
            SendTarget::B => self.mpv_deck_b.as_mut(),
        }
        .and_then(|client| client.get_path().ok());
        let cue_mpv_path = self
            .mpv_deck_c
            .as_mut()
            .and_then(|client| client.get_path().ok());

        let engine_source = |ch_idx: usize, mpv_path: Option<String>| {
            let Some(engine) = self.audio_engine.as_ref() else {
                return EngineDeckSource::None;
            };
            if engine.has_capture(ch_idx) {
                return engine.captures[ch_idx]
                    .lock()
                    .ok()
                    .and_then(|capture| capture.as_ref().map(|capture| capture.path.clone()))
                    .map(EngineDeckSource::Capture)
                    .unwrap_or(EngineDeckSource::None);
            }
            if engine.has_decoder(ch_idx) {
                let path = mpv_path.or_else(|| {
                    self.mixer
                        .get_channel(ch_idx)
                        .and_then(|channel| channel.source_id.clone())
                });
                return path
                    .map(|path| EngineDeckSource::File {
                        path,
                        position: engine.time_pos[ch_idx].load(),
                    })
                    .unwrap_or(EngineDeckSource::None);
            }
            EngineDeckSource::None
        };
        let target_engine_source = engine_source(target_idx, target_mpv_path);
        let cue_engine_source = engine_source(cue_idx, cue_mpv_path);

        self.mixer.swap_cue_with_deck(target);
        self.deck_queues.swap(target_idx, cue_idx);
        self.deck_queue_indices.swap(target_idx, cue_idx);
        self.last_detected_keys.swap(target_idx, cue_idx);
        self.engine_was_playing.swap(target_idx, cue_idx);
        self.consecutive_poll_failures.swap(target_idx, cue_idx);

        match target {
            SendTarget::A => {
                std::mem::swap(&mut self.mpv_deck_a, &mut self.mpv_deck_c);
                std::mem::swap(&mut self.sc_deck_a, &mut self.sc_deck_c);
            }
            SendTarget::B => {
                std::mem::swap(&mut self.mpv_deck_b, &mut self.mpv_deck_c);
                std::mem::swap(&mut self.sc_deck_b, &mut self.sc_deck_c);
            }
        }

        if let Some(engine) = self.audio_engine.as_ref() {
            engine.stop_decoder(target_idx);
            engine.stop_decoder(cue_idx);
            Self::restore_engine_source(engine, target_idx, cue_engine_source);
            Self::restore_engine_source(engine, cue_idx, target_engine_source);
        }

        self.reset_route_clients(target_idx);
        self.reset_route_clients(cue_idx);
        self.reroute_audio_devices_after_cue_send(target == SendTarget::A);
        self.sync_volume_to_mpv(target_idx);
        self.sync_volume_to_mpv(cue_idx);
    }

    fn restore_engine_source(engine: &AudioEngine, ch_idx: usize, source: EngineDeckSource) {
        match source {
            EngineDeckSource::None => {}
            EngineDeckSource::File { path, position } => {
                engine.load_file_at(ch_idx, path, position);
            }
            EngineDeckSource::Capture(path) => {
                if let Err(error) = engine.attach_capture(ch_idx, &path) {
                    eprintln!(
                        "Deck swap: capture attach failed for ch{}: {}",
                        ch_idx, error
                    );
                }
            }
        }
    }

    /// Re-route audio devices after swapping CUE with Deck A or B.
    ///
    /// After swapping MPV clients, the new Deck A/B (old CUE) process is still
    /// outputting to the CUE device, and the new CUE (old Deck A/B) is on Master.
    /// This swaps the audio outputs so each process outputs to the correct bus.
    ///
    /// Falls back to querying the opposite MPV process's current device when the
    /// user hasn't explicitly picked one from the output picker.
    fn reroute_audio_devices_after_cue_send(&mut self, to_deck_a: bool) {
        // Prefer explicit picker selection; fall back to querying the OPPOSITE process.
        // After the swap: mpv_deck_a/c = old CUE (was on CUE device), mpv_deck_c = old Deck A/B (was on Master).
        // So query the old Deck A/B process (now mpv_deck_c) to discover the Master device,
        // and query the old CUE process (now mpv_deck_a/b) to discover the CUE device.
        let master_dev = self
            .master_output
            .selected_mpv_name()
            .map(|s| s.to_string())
            .or_else(|| {
                // mpv_deck_c is the old Deck A/B process — it was on Master
                self.mpv_deck_c
                    .as_mut()
                    .and_then(|c| c.get_audio_device().ok())
            });
        let cue_dev = self
            .cue_output
            .selected_mpv_name()
            .map(|s| s.to_string())
            .or_else(|| {
                // mpv_deck_a/b is the old CUE process — it was on CUE
                let client = if to_deck_a {
                    self.mpv_deck_a.as_mut()
                } else {
                    self.mpv_deck_b.as_mut()
                };
                client.and_then(|c| c.get_audio_device().ok())
            });

        tracing::debug!(
            "reroute_audio_devices: master_dev={:?}, cue_dev={:?}, to_deck_a={}",
            master_dev,
            cue_dev,
            to_deck_a
        );

        // Set the new Deck A/B to Master output
        if let Some(dev) = &master_dev {
            let client = if to_deck_a {
                self.mpv_deck_a.as_mut()
            } else {
                self.mpv_deck_b.as_mut()
            };
            if let Some(c) = client {
                tracing::debug!(
                    "Setting deck {} audio device to master: {}",
                    if to_deck_a { "A" } else { "B" },
                    dev
                );
                c.set_audio_device(dev).ok();
            }
        }
        // Set the new CUE to CUE output
        if let Some(dev) = &cue_dev
            && let Some(c) = self.mpv_deck_c.as_mut()
        {
            tracing::debug!("Setting deck C audio device to cue: {}", dev);
            c.set_audio_device(dev).ok();
        }
    }

    /// Sync volume to MPV/SC for a specific deck, combining fader, crossfader, and master
    fn sync_deck_volume(&mut self, deck_a: bool) {
        let ch_idx = if deck_a {
            self.mixer.dj.deck_a_channel
        } else {
            self.mixer.dj.deck_b_channel
        };
        self.sync_volume_to_mpv(ch_idx);
    }

    /// Sync volume change to MPV for a channel (applies crossfader gain)
    pub fn sync_volume_to_mpv(&mut self, channel_idx: usize) {
        let (gain_a, gain_b) = self.calculate_crossfader_gains();
        let gain = if channel_idx == self.mixer.dj.deck_b_channel {
            gain_b
        } else if channel_idx == self.mixer.dj.deck_c_channel {
            1.0
        } else {
            gain_a
        };
        let master = self.mixer.master.fader;
        let solo_active = self.mixer.solo_active;

        let ch = self.mixer.get_channel(channel_idx);
        let fader = ch.map(|c| c.fader).unwrap_or(0.5);
        let muted = ch.map(|c| c.muted).unwrap_or(false);
        let solo = ch.map(|c| c.solo).unwrap_or(false);
        let effective_muted = self.mixer.master.muted || muted || (solo_active && !solo);

        // Keep MPV volume in sync even when engine owns playback. For
        // decoder-backed decks we suppress MPV with mute instead of pinning
        // volume to 0, so reconnecting sources doesn't leave external MPV
        // processes stuck at 0% volume.
        let engine_active = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(channel_idx))
            .unwrap_or(false);
        let has_capture = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_capture(channel_idx))
            .unwrap_or(false);
        let decoder_owned = engine_active && !has_capture;
        let deck_c_offset = if channel_idx == self.mixer.dj.deck_c_channel {
            6.0
        } else {
            1.0
        };
        let base_vol = (fader * gain * master * 2.0 * deck_c_offset * 200.0).clamp(0.0, 200.0);
        let vol = if effective_muted { 0.0 } else { base_vol };
        if let Some(client) = self.mpv_for_channel(channel_idx) {
            if decoder_owned {
                let _ = client.set_mute(true);
                let _ = client.set_volume(base_vol);
            } else {
                let _ = client.set_mute(effective_muted);
                let _ = client.set_volume(vol);
            }
        }
        let sc_vol = if effective_muted {
            0.0
        } else {
            (fader * gain * master * 2.0 * deck_c_offset * SC_GAIN_BOOST).clamp(0.0, 8.0)
        };
        if let Some(client) = self.sc_for_channel(channel_idx) {
            let _ = client.set_volume(sc_vol);
        }
        // Send to Rust audio engine (direct state write — no command channel)
        if let Some(ref engine) = self.audio_engine {
            engine.state.set_volume(channel_idx, fader);
            engine.state.set_muted(channel_idx, effective_muted);
            engine.state.set_solo_active(self.mixer.solo_active);
            engine.state.set_master_fader(self.mixer.master.fader);
            engine.state.set_crossfader(self.mixer.dj.crossfader);
        }
        if channel_idx < 3 {
            self.last_volume_push_ms[channel_idx] = self.elapsed_ms;
        }
        self.sync_capture_dsp_params();
    }

    /// Apply solo logic and sync all channels to MPV.
    /// When any deck has solo active, non-soloed decks are muted.
    /// When no deck has solo, individual mute states are used.
    pub fn sync_solo_to_all_mpv(&mut self) {
        let solo_active = self.mixer.solo_active;

        let (gain_a, gain_b) = self.calculate_crossfader_gains();
        let master = self.mixer.master.fader;
        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;

        // Compute all values before mutable borrows
        let a_solo = self
            .mixer
            .get_channel(deck_a_ch)
            .map(|c| c.solo)
            .unwrap_or(false);
        let a_muted = if solo_active {
            !a_solo
        } else {
            self.mixer
                .get_channel(deck_a_ch)
                .map(|c| c.muted)
                .unwrap_or(false)
        };
        let a_fader = self
            .mixer
            .get_channel(deck_a_ch)
            .map(|c| c.fader)
            .unwrap_or(0.5);
        let a_vol = if a_muted {
            0.0
        } else {
            (a_fader * gain_a * master * 2.0 * 200.0).clamp(0.0, 200.0)
        };

        let b_solo = self
            .mixer
            .get_channel(deck_b_ch)
            .map(|c| c.solo)
            .unwrap_or(false);
        let b_muted = if solo_active {
            !b_solo
        } else {
            self.mixer
                .get_channel(deck_b_ch)
                .map(|c| c.muted)
                .unwrap_or(false)
        };
        let b_fader = self
            .mixer
            .get_channel(deck_b_ch)
            .map(|c| c.fader)
            .unwrap_or(0.5);
        let b_vol = if b_muted {
            0.0
        } else {
            (b_fader * gain_b * master * 2.0 * 200.0).clamp(0.0, 200.0)
        };

        let c_solo = self.mixer.cue_channel.solo;
        let c_muted = if solo_active {
            !c_solo
        } else {
            self.mixer.cue_channel.muted
        };
        let c_fader = self.mixer.cue_channel.fader;
        let c_vol = if c_muted {
            0.0
        } else {
            (c_fader * 1.0 * master * 2.0 * 6.0 * 200.0).clamp(0.0, 200.0)
        };

        // Skip MPV/SC when engine has a decoder — engine handles playback
        let engine_a = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(deck_a_ch))
            .unwrap_or(false);
        let engine_b = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(deck_b_ch))
            .unwrap_or(false);
        let engine_c = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(2))
            .unwrap_or(false);

        if !engine_a && let Some(ref mut client) = self.mpv_deck_a {
            let _ = client.set_mute(a_muted);
            let _ = client.set_volume(a_vol);
        }

        if !engine_b && let Some(ref mut client) = self.mpv_deck_b {
            let _ = client.set_mute(b_muted);
            let _ = client.set_volume(b_vol);
        }

        if !engine_c && let Some(ref mut client) = self.mpv_deck_c {
            let _ = client.set_mute(c_muted);
            let _ = client.set_volume(c_vol);
        }

        if !engine_a && let Some(ref client) = self.sc_deck_a {
            let sc_vol = if a_muted {
                0.0
            } else {
                (a_fader * gain_a * master * 2.0 * SC_GAIN_BOOST).clamp(0.0, 8.0)
            };
            let _ = client.set_volume(sc_vol);
        }
        if !engine_b && let Some(ref client) = self.sc_deck_b {
            let sc_vol = if b_muted {
                0.0
            } else {
                (b_fader * gain_b * master * 2.0 * SC_GAIN_BOOST).clamp(0.0, 8.0)
            };
            let _ = client.set_volume(sc_vol);
        }

        // Update Rust audio engine solo state
        if let Some(ref engine) = self.audio_engine {
            engine.state.set_solo(deck_a_ch, a_solo);
            engine.state.set_solo(deck_b_ch, b_solo);
            engine.state.set_solo(2, c_solo);
            engine.state.set_solo_active(solo_active);
        }
    }

    /// Start or accelerate a scrub on the currently selected deck channel.
    /// direction: -1.0 = reverse, 1.0 = forward
    /// coarse: true for H/J/K/L (faster acceleration)
    fn start_scrub(&mut self, direction: f32, coarse: bool) {
        if let SelectionFocus::Channel(ch_idx) = self.mixer.focus
            && let Some(ch) = self.mixer.get_channel_mut(ch_idx)
        {
            if ch.uses_supercollider {
                return;
            }

            ch.scrub_direction = direction;
            ch.scrub_coarse = coarse;
            if ch_idx < self.scrub_input_last_ms.len() {
                let repeated = self.scrub_input_last_ms[ch_idx] > 0
                    && self
                        .elapsed_ms
                        .saturating_sub(self.scrub_input_last_ms[ch_idx])
                        <= SCRUB_INPUT_HOLD_MS;
                self.scrub_input_last_ms[ch_idx] = self.elapsed_ms;
                if ch_idx < self.scrub_hold_start_ms.len() && !repeated {
                    self.scrub_hold_start_ms[ch_idx] = self.elapsed_ms;
                }
            }
            if ch_idx < self.scrub_pending_return_ms.len() {
                self.scrub_pending_return_ms[ch_idx] = 0;
                self.scrub_pending_return_delta[ch_idx] = 0.0;
            }
            ch.scrub_speed = 1.0;
        }
    }

    fn scrub_tap(&mut self, direction: f32) {
        let SelectionFocus::Channel(ch_idx) = self.mixer.focus else {
            return;
        };
        let Some(ch) = self.mixer.get_channel(ch_idx) else {
            return;
        };
        if ch.uses_supercollider {
            return;
        }
        let allow_tap_return = !ch.playing;

        let dir_sign = if direction < 0.0 { -1 } else { 1 };
        let now = self.elapsed_ms;
        if ch_idx < self.scrub_fine_last_ms.len() {
            let last_ms = self.scrub_fine_last_ms[ch_idx];
            let last_dir = self.scrub_fine_last_dir[ch_idx];
            let repeated_same_dir =
                last_dir == dir_sign && now.saturating_sub(last_ms) <= SCRUB_FINE_HOLD_ARM_MS;
            self.scrub_fine_last_ms[ch_idx] = now;
            self.scrub_fine_last_dir[ch_idx] = dir_sign;

            if repeated_same_dir {
                self.start_scrub(direction, false);
                return;
            }
        }

        let delta = direction * SCRUB_FINE_STEP_MIN_SECS;
        if let Some(engine) = self.audio_engine.as_ref() {
            engine.scrub_relative(ch_idx, delta as f64);
        }

        if self.send_route_seek_relative(ch_idx, delta) {
            self.route_seek_last_ms[ch_idx] = self.elapsed_ms;
            self.route_scrub_lock_until_ms[ch_idx] = self.elapsed_ms.saturating_add(700);
            if allow_tap_return && ch_idx < self.scrub_pending_return_ms.len() {
                self.scrub_pending_return_ms[ch_idx] =
                    self.elapsed_ms.saturating_add(SCRUB_TAP_RETURN_DELAY_MS);
                self.scrub_pending_return_delta[ch_idx] = -delta;
            }
        }

        if let Some(ch_live) = self.mixer.get_channel_mut(ch_idx) {
            let new_pos = (ch_live.time_pos + delta).max(0.0);
            ch_live.time_pos = if ch_live.duration > 0.0 {
                new_pos.min(ch_live.duration)
            } else {
                new_pos
            };
            self.route_seek_target_pos[ch_idx] = ch_live.time_pos;
            self.route_scrub_last_ms[ch_idx] = self.elapsed_ms;
        }
    }

    fn process_scrub_tap_returns(&mut self) {
        let now = self.elapsed_ms;
        let deck_channels = [
            self.mixer.dj.deck_a_channel,
            self.mixer.dj.deck_b_channel,
            self.mixer.dj.deck_c_channel,
        ];

        for ch_idx in deck_channels {
            if ch_idx >= self.scrub_pending_return_ms.len() {
                continue;
            }
            let due = self.scrub_pending_return_ms[ch_idx];
            if due == 0 || now < due {
                continue;
            }

            if now.saturating_sub(self.scrub_input_last_ms[ch_idx]) <= SCRUB_INPUT_HOLD_MS {
                self.scrub_pending_return_ms[ch_idx] = 0;
                self.scrub_pending_return_delta[ch_idx] = 0.0;
                continue;
            }

            let delta = self.scrub_pending_return_delta[ch_idx];
            self.scrub_pending_return_ms[ch_idx] = 0;
            self.scrub_pending_return_delta[ch_idx] = 0.0;

            if delta.abs() <= f32::EPSILON {
                continue;
            }

            let playing = self
                .mixer
                .get_channel(ch_idx)
                .map(|ch| ch.playing)
                .unwrap_or(false);
            if playing {
                continue;
            }

            if let Some(engine) = self.audio_engine.as_ref() {
                engine.scrub_relative(ch_idx, delta as f64);
            }
            if self.send_route_seek_relative(ch_idx, delta) {
                self.route_seek_last_ms[ch_idx] = now;
                self.route_scrub_lock_until_ms[ch_idx] = now.saturating_add(700);
            }

            if let Some(ch_live) = self.mixer.get_channel_mut(ch_idx) {
                let new_pos = (ch_live.time_pos + delta).max(0.0);
                ch_live.time_pos = if ch_live.duration > 0.0 {
                    new_pos.min(ch_live.duration)
                } else {
                    new_pos
                };
                self.route_seek_target_pos[ch_idx] = ch_live.time_pos;
                self.route_scrub_last_ms[ch_idx] = now;
            }
        }
    }

    /// Tick scrub state for all channels.
    /// Advances accumulated seek amount and sends seek commands to MPV.
    pub fn tick_scrub(&mut self) {
        self.process_scrub_tap_returns();

        let now = self.elapsed_ms;
        let dt = if self.last_scrub_tick_ms == 0 {
            (self.tick_rate.as_secs_f32()).max(0.001)
        } else {
            (now.saturating_sub(self.last_scrub_tick_ms) as f32 / 1000.0).clamp(0.001, 0.1)
        };
        self.last_scrub_tick_ms = now;
        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;
        let deck_c_ch = self.mixer.dj.deck_c_channel;

        for ch_idx in [deck_a_ch, deck_b_ch, deck_c_ch] {
            let (direction, coarse, uses_supercollider) = self
                .mixer
                .get_channel(ch_idx)
                .map(|c| (c.scrub_direction, c.scrub_coarse, c.uses_supercollider))
                .unwrap_or((0.0, false, false));

            let has_capture = self
                .audio_engine
                .as_ref()
                .map(|engine| engine.has_capture(ch_idx))
                .unwrap_or(false);
            let has_decoder = self
                .audio_engine
                .as_ref()
                .map(|engine| engine.has_decoder(ch_idx))
                .unwrap_or(false);

            if uses_supercollider || direction == 0.0 {
                if has_capture {
                    if let Some(engine) = self.audio_engine.as_ref() {
                        engine.set_capture_reverse_scrub(ch_idx, false);
                    }
                } else if has_decoder && let Some(engine) = self.audio_engine.as_ref() {
                    engine.set_decoder_reverse_scrub(ch_idx, false);
                }
                continue;
            }

            let input_fresh = ch_idx < self.scrub_input_last_ms.len()
                && now.saturating_sub(self.scrub_input_last_ms[ch_idx]) <= SCRUB_INPUT_HOLD_MS;
            if !input_fresh {
                if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
                    ch.scrub_direction = 0.0;
                    ch.scrub_speed = 0.0;
                    ch.scrub_accumulator = 0.0;
                }
                if has_capture {
                    if let Some(engine) = self.audio_engine.as_ref() {
                        engine.set_capture_reverse_scrub(ch_idx, false);
                    }
                } else if has_decoder && let Some(engine) = self.audio_engine.as_ref() {
                    engine.set_decoder_reverse_scrub(ch_idx, false);
                }
                if ch_idx < self.scrub_hold_start_ms.len() {
                    self.scrub_hold_start_ms[ch_idx] = 0;
                }
                continue;
            }

            let hold_elapsed = if ch_idx < self.scrub_hold_start_ms.len()
                && self.scrub_hold_start_ms[ch_idx] > 0
            {
                now.saturating_sub(self.scrub_hold_start_ms[ch_idx])
            } else {
                0
            };
            let ramp_t = (hold_elapsed as f32 / SCRUB_ACCEL_RAMP_MS as f32).clamp(0.0, 1.0);
            let step = if coarse {
                SCRUB_COARSE_STEP_MIN_SECS
                    + (SCRUB_COARSE_STEP_MAX_SECS - SCRUB_COARSE_STEP_MIN_SECS) * ramp_t
            } else {
                SCRUB_FINE_STEP_MIN_SECS
                    + (SCRUB_FINE_STEP_MAX_SECS - SCRUB_FINE_STEP_MIN_SECS) * ramp_t
            };
            let dt_scale = (dt / SCRUB_STEP_BASE_DT_SECS).clamp(0.25, 1.5);
            let seek_amount = direction * step * dt_scale;

            // Accumulate in per-channel field
            if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
                ch.scrub_accumulator += seek_amount;

                // Send batched seeks at a bounded cadence to keep scrub audible
                // without hammering MPV with tiny command bursts.
                let send_due = self.route_seek_last_ms[ch_idx] == 0
                    || now.saturating_sub(self.route_seek_last_ms[ch_idx])
                        >= SCRUB_SEEK_SEND_INTERVAL_MS;
                if send_due && ch.scrub_accumulator.abs() > f32::EPSILON {
                    let seek_to = ch.scrub_accumulator;
                    ch.scrub_accumulator = 0.0;
                    if ch_idx < self.route_seek_input_last_ms.len() {
                        self.route_seek_input_last_ms[ch_idx] = self.elapsed_ms;
                        self.perf_trace.route_seek_input_events =
                            self.perf_trace.route_seek_input_events.saturating_add(1);
                    }

                    if let Some(ref engine) = self.audio_engine {
                        if has_capture {
                            engine.set_capture_reverse_scrub(ch_idx, seek_to < 0.0);
                        } else if has_decoder {
                            engine.set_decoder_reverse_scrub(ch_idx, seek_to < 0.0);
                        }
                        engine.scrub_relative(ch_idx, seek_to as f64);
                        if has_capture {
                            if self.send_route_seek_relative(ch_idx, seek_to) {
                                self.route_seek_last_ms[ch_idx] = self.elapsed_ms;
                                self.route_scrub_lock_until_ms[ch_idx] =
                                    self.elapsed_ms.saturating_add(700);
                            }
                            if let Some(ch_live) = self.mixer.get_channel_mut(ch_idx) {
                                let new_pos = (ch_live.time_pos + seek_to).max(0.0);
                                ch_live.time_pos = if ch_live.duration > 0.0 {
                                    new_pos.min(ch_live.duration)
                                } else {
                                    new_pos
                                };
                                self.route_seek_target_pos[ch_idx] = ch_live.time_pos;
                                self.route_scrub_last_ms[ch_idx] = self.elapsed_ms;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Keep scrub state bounded to active key-repeat windows.
    pub fn decay_scrub_speed(&mut self) {
        let now = self.elapsed_ms;
        for (idx, ch) in self.mixer.channels.iter_mut().enumerate() {
            if idx < self.scrub_input_last_ms.len()
                && now.saturating_sub(self.scrub_input_last_ms[idx]) > SCRUB_INPUT_HOLD_MS
            {
                ch.scrub_speed = 0.0;
                ch.scrub_direction = 0.0;
                ch.scrub_accumulator = 0.0;
                let has_capture = self
                    .audio_engine
                    .as_ref()
                    .map(|engine| engine.has_capture(idx))
                    .unwrap_or(false);
                let has_decoder = self
                    .audio_engine
                    .as_ref()
                    .map(|engine| engine.has_decoder(idx))
                    .unwrap_or(false);
                if !has_capture
                    && has_decoder
                    && let Some(engine) = self.audio_engine.as_ref()
                {
                    engine.set_decoder_reverse_scrub(idx, false);
                }
                if idx < self.scrub_hold_start_ms.len() {
                    self.scrub_hold_start_ms[idx] = 0;
                }
            }
        }
    }

    /// Poll time_pos and duration for all deck channels.
    ///
    /// Regular socket decks are refreshed by `poll_mpv_state`.
    /// This method focuses on route-mode capture decks and keeps work
    /// minimal to avoid blocking the UI tick.
    pub fn poll_scrub_positions(&mut self) {
        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;
        let deck_c_ch = self.mixer.dj.deck_c_channel;
        let now = self.elapsed_ms;
        let selected_channel = match self.mixer.focus {
            SelectionFocus::Channel(idx) => Some(idx),
            _ => None,
        };

        for ch_idx in [deck_a_ch, deck_b_ch, deck_c_ch] {
            let has_capture = self
                .audio_engine
                .as_ref()
                .map(|engine| engine.has_capture(ch_idx))
                .unwrap_or(false);

            if has_capture {
                let poll_time_ms = if selected_channel == Some(ch_idx) {
                    10
                } else {
                    20
                };
                let poll_time =
                    now.saturating_sub(self.route_scrub_last_ms[ch_idx]) >= poll_time_ms;
                let poll_duration = {
                    let ch_duration = self
                        .mixer
                        .get_channel(ch_idx)
                        .map(|ch| ch.duration)
                        .unwrap_or(0.0);
                    if ch_duration <= 0.0 {
                        now.saturating_sub(self.route_duration_last_ms[ch_idx]) >= 500
                    } else {
                        now.saturating_sub(self.route_duration_last_ms[ch_idx]) >= 5000
                    }
                };
                if !poll_time && !poll_duration {
                    continue;
                }

                let stale_timeline = self.route_timeline_workers[ch_idx]
                    .as_ref()
                    .map(|w| {
                        w.latest.generated_ms > 0
                            && now.saturating_sub(w.latest.generated_ms) > ROUTE_TIMELINE_STALE_MS
                    })
                    .unwrap_or(false);
                if stale_timeline {
                    self.route_timeline_workers[ch_idx] = None;
                }

                let mut duration: Option<f32> = None;
                let mut timeline_pos: Option<f32> = None;

                if let Some(worker) = self.ensure_route_timeline_worker(ch_idx) {
                    if poll_duration {
                        duration = worker.latest.duration;
                    }
                    if poll_time {
                        timeline_pos = worker.latest.time_pos;
                    }

                    if poll_duration && duration.is_none() {
                        self.route_timeline_workers[ch_idx] = None;
                        if let Some(worker) = self.ensure_route_timeline_worker(ch_idx) {
                            if poll_duration && duration.is_none() {
                                duration = worker.latest.duration;
                            }
                            if poll_time && timeline_pos.is_none() {
                                timeline_pos = worker.latest.time_pos;
                            }
                        }
                    }
                }

                let timeline_pos = timeline_pos.filter(|v| v.is_finite() && *v >= 0.0);

                if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
                    let mut timeline_age = ch.timeline_age_ms;

                    if self.route_seek_pending[ch_idx] {
                        let target = self.route_seek_target_pos[ch_idx];
                        let since = self.route_seek_pending_since_ms[ch_idx];
                        let timed_out =
                            since > 0 && now.saturating_sub(since) > ROUTE_SEEK_PENDING_MAX_MS;
                        let matched = timeline_pos
                            .map(|tp| (tp - target).abs() <= ROUTE_SEEK_TARGET_EPSILON_SECS)
                            .unwrap_or(false);
                        if matched || timed_out {
                            self.route_seek_pending[ch_idx] = false;
                            self.route_seek_pending_since_ms[ch_idx] = 0;
                            if self.route_seek_send_last_ms[ch_idx] > 0 {
                                let lag =
                                    now.saturating_sub(self.route_seek_send_last_ms[ch_idx]) as u32;
                                self.perf_trace.route_seek_send_to_apply_max_ms =
                                    self.perf_trace.route_seek_send_to_apply_max_ms.max(lag);
                                self.route_seek_send_last_ms[ch_idx] = 0;
                            }
                        }
                    }

                    if let Some(tp) = timeline_pos {
                        let sample_age_secs = self.route_timeline_workers[ch_idx]
                            .as_ref()
                            .map(|worker| {
                                now.saturating_sub(worker.latest.generated_ms) as f32 / 1_000.0
                            })
                            .unwrap_or(0.0);
                        let live = if ch.playing && !self.route_seek_pending[ch_idx] {
                            tp + sample_age_secs * self.route_speed_cache[ch_idx].clamp(0.1, 4.0)
                        } else {
                            tp
                        }
                        .max(0.0);
                        ch.time_pos = live;
                        if !self.route_seek_pending[ch_idx] {
                            self.route_seek_target_pos[ch_idx] = live;
                        }
                        self.route_scrub_last_ms[ch_idx] = now;

                        let prev = self.route_last_time_pos[ch_idx];
                        if prev > 0.0 {
                            let delta_ms = ((ch.time_pos - prev).abs() * 1000.0) as u32;
                            self.perf_trace.route_timepos_delta_max_ms =
                                self.perf_trace.route_timepos_delta_max_ms.max(delta_ms);
                        }
                        self.route_last_time_pos[ch_idx] = ch.time_pos;
                    } else if self.route_seek_pending[ch_idx] {
                        ch.time_pos = self.route_seek_target_pos[ch_idx].max(0.0);
                    }

                    if let Some(dur) = duration {
                        ch.duration = dur.max(0.0);
                        self.route_duration_last_ms[ch_idx] = now;
                    }

                    if let Some(worker) = self.route_timeline_workers[ch_idx].as_ref() {
                        timeline_age = if worker.latest.generated_ms > 0 {
                            now.saturating_sub(worker.latest.generated_ms) as u32
                        } else {
                            0
                        };
                    }
                    ch.timeline_age_ms = timeline_age;
                }
            } else if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
                ch.timeline_age_ms = 0;
            }
        }
    }

    /// Apply pending BPM results from background analysis to channel state
    fn apply_pending_bpm(&mut self) {
        let results: Vec<(usize, f32, Option<String>)> = {
            if let Ok(mut queue) = self.pending_bpm.lock() {
                queue.drain(..).collect()
            } else {
                Vec::new()
            }
        };
        for (ch_idx, bpm, key) in results {
            // Sentinel: ch_idx == usize::MAX means analysis error in `key`
            if ch_idx == usize::MAX {
                if let Some(msg) = key {
                    self.log_debug(format!("BPM error: {}", msg));
                }
                continue;
            }
            let mut should_sync_speed = false;
            self.log_debug(format!(
                "BPM applied ch{}: {:.1} key={:?}",
                ch_idx, bpm, key
            ));
            if let Some(ch) = self.mixer.get_channel_mut(ch_idx) {
                ch.bpm = Some(bpm);
                if ch.base_bpm <= 0.0 {
                    ch.base_bpm = bpm;
                    ch.target_bpm = bpm;
                    should_sync_speed = true;
                }
                if key.is_some() {
                    ch.key = key;
                    ch.key_offset = 0;
                    should_sync_speed = true;
                }
                if ch.base_bpm > 0.0 {
                    should_sync_speed = true;
                }
            }
            if should_sync_speed {
                self.sync_bpm_to_mpv(ch_idx);
            }
        }
    }

    fn poll_tidal_bpm(&mut self) {
        let Ok(text) = std::fs::read_to_string(TIDAL_BPM_PATH) else {
            return;
        };
        let Ok(bpm) = text.trim().parse::<f32>() else {
            return;
        };
        if !(10.0..=400.0).contains(&bpm) {
            return;
        }

        for idx in 0..self.mixer.channels.len() {
            if let Some(ch) = self.mixer.get_channel_mut(idx)
                && ch.connected
            {
                ch.bpm = Some(bpm);
                ch.base_bpm = bpm;
                ch.target_bpm = bpm;
            }
        }

        if self.mixer.cue_channel.connected {
            self.mixer.cue_channel.bpm = Some(bpm);
            self.mixer.cue_channel.base_bpm = bpm;
            self.mixer.cue_channel.target_bpm = bpm;
        }
    }

    fn poll_route_bpm_key(&mut self) {
        let selected_channel = match self.mixer.focus {
            SelectionFocus::Channel(idx) => Some(idx),
            _ => None,
        };

        let deck_indices = [
            self.mixer.dj.deck_a_channel,
            self.mixer.dj.deck_b_channel,
            self.mixer.dj.deck_c_channel,
        ];

        for ch_idx in deck_indices {
            self.perf_trace.route_meta_polls = self.perf_trace.route_meta_polls.saturating_add(1);

            let connected = self
                .mixer
                .get_channel(ch_idx)
                .map(|c| c.connected)
                .unwrap_or(false);
            if !connected {
                self.perf_trace.route_meta_failures =
                    self.perf_trace.route_meta_failures.saturating_add(1);
                continue;
            }

            let has_capture = self
                .audio_engine
                .as_ref()
                .map(|e| e.has_capture(ch_idx))
                .unwrap_or(false);
            if !has_capture {
                self.perf_trace.route_meta_failures =
                    self.perf_trace.route_meta_failures.saturating_add(1);
                continue;
            }

            let force_selected = selected_channel == Some(ch_idx);
            if force_selected {
                self.perf_trace.route_meta_selected_polls =
                    self.perf_trace.route_meta_selected_polls.saturating_add(1);
            }
            if !force_selected && self.route_meta_last_ms[ch_idx] != 0 {
                let age = self
                    .elapsed_ms
                    .saturating_sub(self.route_meta_last_ms[ch_idx]);
                if age < 200 {
                    self.perf_trace.route_meta_failures =
                        self.perf_trace.route_meta_failures.saturating_add(1);
                    continue;
                }
            }

            let source_id = self
                .mixer
                .get_channel(ch_idx)
                .and_then(|c| c.source_id.clone());
            let Some(source_id) = source_id else {
                self.perf_trace.route_meta_failures =
                    self.perf_trace.route_meta_failures.saturating_add(1);
                continue;
            };
            let source_path = Some(std::path::Path::new(source_id.as_str()));
            let Some(meta_path) = Self::resolve_route_meta_path(source_path) else {
                self.perf_trace.route_meta_failures =
                    self.perf_trace.route_meta_failures.saturating_add(1);
                continue;
            };

            let Ok(raw) = std::fs::read_to_string(meta_path) else {
                self.perf_trace.route_meta_failures =
                    self.perf_trace.route_meta_failures.saturating_add(1);
                continue;
            };
            let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) else {
                self.perf_trace.route_meta_failures =
                    self.perf_trace.route_meta_failures.saturating_add(1);
                continue;
            };
            let Some(obj) = data.as_object() else {
                self.perf_trace.route_meta_failures =
                    self.perf_trace.route_meta_failures.saturating_add(1);
                continue;
            };

            let pick_num = |keys: &[&str]| -> Option<f32> {
                keys.iter().find_map(|k| {
                    let v = obj.get(*k)?;
                    if let Some(n) = v.as_f64() {
                        Some(n as f32)
                    } else if let Some(s) = v.as_str() {
                        s.trim().parse::<f32>().ok()
                    } else {
                        None
                    }
                })
            };

            let pick_text = |keys: &[&str]| -> Option<String> {
                keys.iter()
                    .find_map(|k| obj.get(*k).and_then(|v| v.as_str()))
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned)
            };

            let mut should_sync_speed = false;
            let mut updated = false;
            if let Some(channel) = self.mixer.get_channel_mut(ch_idx) {
                if let Some(raw_key) = pick_text(&[
                    "key",
                    "initialkey",
                    "initial_key",
                    "camelot",
                    "camelot_key",
                    "tkey",
                    "KEY",
                    "INITIALKEY",
                    "TKEY",
                ]) {
                    let parsed = parse_camelot(&raw_key)
                        .map(|(pc, is_major)| pitch_class_to_camelot(pc, is_major))
                        .or_else(|| parse_key_name(&raw_key));
                    if let Some(camelot) = parsed
                        && channel.key.as_deref() != Some(camelot.as_str())
                    {
                        channel.key = Some(camelot);
                        updated = true;
                    }
                }

                if let Some(mut bpm) =
                    pick_num(&["bpm", "tempo", "initial_bpm", "initial-bpm", "BPM", "TBPM"])
                {
                    while bpm > 400.0 {
                        bpm *= 0.5;
                    }
                    while bpm > 0.0 && bpm < 40.0 {
                        bpm *= 2.0;
                    }
                    if (10.0..=400.0).contains(&bpm) {
                        if channel.bpm != Some(bpm) {
                            updated = true;
                        }
                        channel.bpm = Some(bpm);
                        if channel.base_bpm <= 0.0 {
                            channel.base_bpm = bpm;
                            channel.target_bpm = bpm;
                            updated = true;
                        }
                        should_sync_speed = true;
                    }
                }
            }

            if updated {
                self.perf_trace.route_meta_updates =
                    self.perf_trace.route_meta_updates.saturating_add(1);
            }
            let age = self
                .elapsed_ms
                .saturating_sub(self.route_meta_last_ms[ch_idx]) as u32;
            self.perf_trace.route_meta_age_max_ms = self.perf_trace.route_meta_age_max_ms.max(age);
            self.route_meta_last_ms[ch_idx] = self.elapsed_ms;

            if should_sync_speed {
                self.sync_bpm_to_mpv(ch_idx);
            }
        }
    }

    /// Read real-time onset-detected BPM from each MPV metering thread.
    /// Read real-time onset-detected BPM from each MPV metering thread.
    /// Captures base_bpm on first detection for stable speed factor.
    fn poll_onset_bpm(&mut self) {
        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;
        let deck_c_ch = self.mixer.dj.deck_c_channel;
        let mut initialized_channels = Vec::new();

        // Deck A: prefer MPV client, fall back to engine meter (FIFO capture)
        if let Some(ref client) = self.mpv_deck_a {
            let bpm = client.get_detected_bpm();
            if bpm > 0.0
                && let Some(ch) = self.mixer.get_channel_mut(deck_a_ch)
            {
                if ch.base_bpm == 0.0 {
                    ch.base_bpm = bpm;
                    ch.target_bpm = bpm;
                    initialized_channels.push(deck_a_ch);
                }
                ch.bpm = Some(bpm);
            }
        } else if let Some(ref engine) = self.audio_engine {
            let raw = engine.meters[0]
                .detected_bpm
                .load(std::sync::atomic::Ordering::Relaxed);
            let bpm = raw as f32 / 100.0;
            if bpm > 0.0
                && let Some(ch) = self.mixer.get_channel_mut(deck_a_ch)
            {
                if ch.base_bpm == 0.0 {
                    ch.base_bpm = bpm;
                    ch.target_bpm = bpm;
                    initialized_channels.push(deck_a_ch);
                }
                ch.bpm = Some(bpm);
            }
        }
        // Deck B: prefer MPV client, fall back to engine meter
        if let Some(ref client) = self.mpv_deck_b {
            let bpm = client.get_detected_bpm();
            if bpm > 0.0
                && let Some(ch) = self.mixer.get_channel_mut(deck_b_ch)
            {
                if ch.base_bpm == 0.0 {
                    ch.base_bpm = bpm;
                    ch.target_bpm = bpm;
                    initialized_channels.push(deck_b_ch);
                }
                ch.bpm = Some(bpm);
            }
        } else if let Some(ref engine) = self.audio_engine {
            let raw = engine.meters[1]
                .detected_bpm
                .load(std::sync::atomic::Ordering::Relaxed);
            let bpm = raw as f32 / 100.0;
            if bpm > 0.0
                && let Some(ch) = self.mixer.get_channel_mut(deck_b_ch)
            {
                if ch.base_bpm == 0.0 {
                    ch.base_bpm = bpm;
                    ch.target_bpm = bpm;
                    initialized_channels.push(deck_b_ch);
                }
                ch.bpm = Some(bpm);
            }
        }
        // Deck C: prefer MPV client, fall back to engine meter
        if let Some(ref client) = self.mpv_deck_c {
            let bpm = client.get_detected_bpm();
            if bpm > 0.0
                && let Some(ch) = self.mixer.get_channel_mut(deck_c_ch)
            {
                if ch.base_bpm == 0.0 {
                    ch.base_bpm = bpm;
                    ch.target_bpm = bpm;
                    initialized_channels.push(deck_c_ch);
                }
                ch.bpm = Some(bpm);
            }
        } else if let Some(ref engine) = self.audio_engine {
            let raw = engine.meters[2]
                .detected_bpm
                .load(std::sync::atomic::Ordering::Relaxed);
            let bpm = raw as f32 / 100.0;
            if bpm > 0.0
                && let Some(ch) = self.mixer.get_channel_mut(deck_c_ch)
            {
                if ch.base_bpm == 0.0 {
                    ch.base_bpm = bpm;
                    ch.target_bpm = bpm;
                    initialized_channels.push(deck_c_ch);
                }
                ch.bpm = Some(bpm);
            }
        }

        for ch_idx in initialized_channels {
            self.sync_bpm_to_mpv(ch_idx);
        }

        // Key detection from engine background thread (FIFO capture path)
        // Stability check: only update ch.key if we see the same key twice in a row
        // (or if ch.key is still None — first detection always applies).
        if let Some(ref engine) = self.audio_engine {
            for (deck_idx, ch_idx) in [(0, deck_a_ch), (1, deck_b_ch), (2, deck_c_ch)] {
                if let Ok(mut guard) = engine.detected_keys[deck_idx].lock()
                    && let Some(new_key) = guard.take()
                {
                    let dominated =
                        self.last_detected_keys[deck_idx].as_deref() == Some(new_key.as_str());
                    let first_run = self
                        .mixer
                        .get_channel(ch_idx)
                        .and_then(|c| c.key.as_ref())
                        .is_none();
                    if (dominated || first_run)
                        && let Some(ch) = self.mixer.get_channel_mut(ch_idx)
                    {
                        ch.key = Some(new_key.clone());
                        ch.key_offset = 0;
                    }
                    self.last_detected_keys[deck_idx] = Some(new_key);
                }
            }
        }
    }

    /// Sync mute state to MPV/SC for a channel
    pub fn sync_mute_to_mpv(&mut self, channel_idx: usize) {
        let muted = self
            .mixer
            .get_channel(channel_idx)
            .map(|c| c.muted)
            .unwrap_or(false);
        let fader = self
            .mixer
            .get_channel(channel_idx)
            .map(|c| c.fader)
            .unwrap_or(0.5);
        let (gain_a, gain_b) = self.calculate_crossfader_gains();
        let gain = if channel_idx == self.mixer.dj.deck_b_channel {
            gain_b
        } else if channel_idx == self.mixer.dj.deck_c_channel {
            1.0
        } else {
            gain_a
        };
        let master = self.mixer.master.fader;

        // Keep MPV volume/mute coherent even when engine owns playback.
        let engine_active = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(channel_idx))
            .unwrap_or(false);
        let has_capture = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_capture(channel_idx))
            .unwrap_or(false);
        let decoder_owned = engine_active && !has_capture;

        if let Some(client) = self.mpv_for_channel(channel_idx) {
            let base_vol = (fader * gain * master * 2.0 * 200.0).clamp(0.0, 200.0);
            if decoder_owned {
                let _ = client.set_mute(true);
                let _ = client.set_volume(base_vol);
            } else {
                let _ = client.set_mute(muted);
                let vol = if muted { 0.0 } else { base_vol };
                let _ = client.set_volume(vol);
            }
        }
        if let Some(client) = self.sc_for_channel(channel_idx) {
            let vol = if muted {
                0.0
            } else {
                (fader * gain * master * 2.0 * SC_GAIN_BOOST).clamp(0.0, 8.0)
            };
            let _ = client.set_volume(vol);
        }

        // Always update engine mute state
        if let Some(ref engine) = self.audio_engine {
            engine.state.set_muted(channel_idx, muted);
        }
        self.sync_capture_dsp_params();
    }

    /// Sync play/pause state to MPV/SC for a channel
    pub fn sync_playpause_to_mpv(&mut self, channel_idx: usize) {
        let playing = self
            .mixer
            .get_channel(channel_idx)
            .map(|c| c.playing)
            .unwrap_or(false);

        if let Some(ref engine) = self.audio_engine {
            engine.state.set_playing(channel_idx, playing);
        }

        let paused = !playing;
        let engine_active = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(channel_idx))
            .unwrap_or(false);
        let has_capture = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_capture(channel_idx))
            .unwrap_or(false);
        let decoder_owned = engine_active && !has_capture;

        if has_capture {
            let _ = self.send_route_command_for_channel(
                channel_idx,
                vec![
                    serde_json::json!("set_property"),
                    serde_json::json!("pause"),
                    serde_json::json!(paused),
                ],
            );
        } else if !decoder_owned {
            if let Some(client) = self.mpv_for_channel(channel_idx) {
                let _ = client.set_pause(paused);
            }
            if let Some(client) = self.sc_for_channel(channel_idx) {
                let _ = client.set_pause(paused);
            }
        }

        // If master is paused but a deck is now playing, unpause master
        if !self.mixer.master.playing && playing {
            self.mixer.master.playing = true;
        }
    }

    /// Sync master play/pause to all connected channels
    pub fn sync_all_playpause(&mut self) {
        let playing = self.mixer.master.playing;

        if playing {
            // Resume: only resume channels that were playing before the pause
            let prev = self.mixer.previously_playing.clone();

            for (idx, &was_playing) in prev.iter().enumerate() {
                if let Some(channel) = self.mixer.get_channel_mut(idx) {
                    channel.playing = was_playing;
                }
                let engine_active = self
                    .audio_engine
                    .as_ref()
                    .map(|e| e.has_decoder(idx))
                    .unwrap_or(false);
                let has_capture = self
                    .audio_engine
                    .as_ref()
                    .map(|e| e.has_capture(idx))
                    .unwrap_or(false);
                let decoder_owned = engine_active && !has_capture;
                let paused = !was_playing;

                if has_capture {
                    let _ = self.send_route_command_for_channel(
                        idx,
                        vec![
                            serde_json::json!("set_property"),
                            serde_json::json!("pause"),
                            serde_json::json!(paused),
                        ],
                    );
                } else if !decoder_owned {
                    if let Some(client) = self.mpv_for_channel(idx) {
                        let _ = client.set_pause(paused);
                    }
                    if let Some(client) = self.sc_for_channel(idx) {
                        let _ = client.set_pause(paused);
                    }
                }
                if let Some(ref engine) = self.audio_engine {
                    engine.state.set_playing(idx, was_playing);
                }
            }
            // Restore only the sequences that were playing before the master pause.
            self.sequence_state.global.mute = self.sequence_state.master_previously_global_mute;
            for (i, seq) in self.sequence_state.sequences.iter_mut().enumerate() {
                if i < self.sequence_state.master_previously_playing.len() {
                    seq.playing = self.sequence_state.master_previously_playing[i];
                }
            }
        } else {
            // Pause: save actual MPV playback state (channel.playing may be stale),
            // then pause all.
            self.mixer.previously_playing.clear();
            for idx in 0..self.mixer.channels.len() {
                let engine_active = self
                    .audio_engine
                    .as_ref()
                    .map(|e| e.has_decoder(idx))
                    .unwrap_or(false);
                let has_capture = self
                    .audio_engine
                    .as_ref()
                    .map(|e| e.has_capture(idx))
                    .unwrap_or(false);
                let decoder_owned = engine_active && !has_capture;
                let channel_playing = self
                    .mixer
                    .get_channel(idx)
                    .map(|c| c.playing)
                    .unwrap_or(false);
                let was_playing = if decoder_owned {
                    channel_playing
                } else {
                    self.mpv_for_channel(idx)
                        .and_then(|c| c.get_pause().ok())
                        .map(|paused| !paused)
                        .unwrap_or(channel_playing)
                };
                self.mixer.previously_playing.push(was_playing);
                if let Some(channel) = self.mixer.get_channel_mut(idx) {
                    channel.playing = false;
                }

                if has_capture {
                    let _ = self.send_route_command_for_channel(
                        idx,
                        vec![
                            serde_json::json!("set_property"),
                            serde_json::json!("pause"),
                            serde_json::json!(true),
                        ],
                    );
                } else if !decoder_owned {
                    if let Some(client) = self.mpv_for_channel(idx) {
                        let _ = client.set_pause(true);
                    }
                    if let Some(client) = self.sc_for_channel(idx) {
                        let _ = client.set_pause(true);
                    }
                }
                if let Some(ref engine) = self.audio_engine {
                    engine.state.set_playing(idx, false);
                }
            }
            // Also save and pause CUE channel
            let cue_idx = self.mixer.dj.deck_c_channel;
            let cue_decoder_owned = self
                .audio_engine
                .as_ref()
                .map(|e| e.has_decoder(cue_idx) && !e.has_capture(cue_idx))
                .unwrap_or(false);
            let cue_playing = self.mixer.cue_channel.playing;
            let cue_was_playing = if cue_decoder_owned {
                cue_playing
            } else {
                self.mpv_for_channel(cue_idx)
                    .and_then(|c| c.get_pause().ok())
                    .map(|paused| !paused)
                    .unwrap_or(cue_playing)
            };
            self.mixer.previously_playing.push(cue_was_playing);
            self.mixer.cue_channel.playing = false;
            if let Some(client) = self.mpv_for_channel(cue_idx) {
                let _ = client.set_pause(true);
            }
            if let Some(client) = self.sc_for_channel(cue_idx) {
                let _ = client.set_pause(true);
            }
            if let Some(ref engine) = self.audio_engine {
                engine.state.set_playing(cue_idx, false);
            }
            // Save and pause all sequences
            self.sequence_state.master_previously_global_mute = self.sequence_state.global.mute;
            self.sequence_state.global.mute = true;
            self.sequence_state.master_previously_playing = self
                .sequence_state
                .sequences
                .iter()
                .map(|seq| seq.playing)
                .collect();
            for seq in &mut self.sequence_state.sequences {
                seq.playing = false;
            }
        }
    }

    /// Sync current control to MPV if it's a fader
    fn sync_current_control_to_mpv(&mut self) {
        match self.mixer.focus {
            SelectionFocus::Channel(ch_idx) => {
                match self.mixer.selected_control {
                    ChannelControl::Fader => {
                        self.sync_volume_to_mpv(ch_idx);
                    }
                    ChannelControl::Scrub => {
                        // Scrub is handled by tick_scrub, no sync needed here
                    }
                    ChannelControl::Bpm => {
                        self.sync_bpm_to_mpv(ch_idx);
                    }
                    ChannelControl::Key => {
                        // Key contributes to the same effective speed as BPM.
                        self.sync_bpm_to_mpv(ch_idx);
                    }
                    ChannelControl::Pan => {
                        self.sync_pan_to_mpv(ch_idx);
                    }
                    ChannelControl::EqLow | ChannelControl::EqMid | ChannelControl::EqHigh => {
                        self.sync_eq_to_mpv(ch_idx);
                    }
                    ChannelControl::FilterCutoff
                    | ChannelControl::FilterFreq
                    | ChannelControl::LfoShape
                    | ChannelControl::LfoSpeed => {
                        self.sync_filter_to_mpv(ch_idx);
                    }
                    _ => {}
                }
            }
            SelectionFocus::Global => {
                // Crossfader adjusts both deck volumes
                if self.mixer.selected_global == GlobalControl::Crossfader {
                    self.sync_crossfader_to_mpv();
                }
                // Master fader adjusts both deck volumes
                if self.mixer.selected_global == GlobalControl::MasterFader {
                    self.sync_deck_volume(true);
                    self.sync_deck_volume(false);
                }
                if self.mixer.selected_global == GlobalControl::MicFader {
                    self.sync_mic_to_engine();
                }
                // Master EQ: sync to all decks
                if self.mixer.selected_global.eq_band_index().is_some() {
                    self.sync_master_eq_to_all_decks();
                }
            }
        }
    }

    fn sync_mic_to_engine(&self) {
        if let Some(engine) = &self.audio_engine {
            engine.set_mic_gain(self.mixer.master.mic_fader * 2.0);
            engine.set_mic_muted(self.mixer.master.mic_muted);
        }
    }

    /// Sync EQ to MPV for a channel
    fn sync_eq_to_mpv(&mut self, channel_idx: usize) {
        let (low, mid, high, low_kill, mid_kill, high_kill) = self
            .mixer
            .get_channel(channel_idx)
            .map(|c| {
                (
                    c.eq_low,
                    c.eq_mid,
                    c.eq_high,
                    c.eq_low_kill,
                    c.eq_mid_kill,
                    c.eq_high_kill,
                )
            })
            .unwrap_or((0.0, 0.0, 0.0, false, false, false));

        let effective_low = if low_kill { -24.0 } else { low };
        let effective_mid = if mid_kill { -24.0 } else { mid };
        let effective_high = if high_kill { -24.0 } else { high };

        let engine_active = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(channel_idx))
            .unwrap_or(false);
        if !engine_active && let Some(client) = self.mpv_for_channel(channel_idx) {
            let _ = client.set_eq(effective_low, effective_mid, effective_high);
        }
        if let Some(client) = self.sc_for_channel(channel_idx) {
            let _ = client.set_eq(effective_low, effective_mid, effective_high);
        }
        // Send to Rust audio engine
        if let Some(ref engine) = self.audio_engine {
            engine.state.set_eq(channel_idx, low, mid, high);
            engine
                .state
                .set_eq_kill(channel_idx, low_kill, mid_kill, high_kill);
        }
        self.sync_capture_dsp_params();
    }

    /// Sync unified filter to MPV for a channel
    ///
    /// The filter has two controls:
    /// - `filter_cutoff` (0.0–1.0): intensity of the filter effect
    /// - `filter_freq` (0.0–1.0): filter position
    ///   - 0.0 = 20 Hz (lowpass)
    ///   - 0.5 = 1000 Hz (center / no effect)
    ///   - 1.0 = 20000 Hz (highpass)
    ///
    /// Plus LFO controls that modulate the cutoff intensity:
    /// - `lfo_shape` (0.0 = square, 1.0 = sine): waveform morph
    /// - `lfo_speed` (0.0 = slow ~0.05Hz, 1.0 = fast ~10Hz): modulation rate (cubic curve)
    ///
    /// A crossfade zone (300Hz–3kHz) smoothly transitions between LPF and HPF
    /// so sweeping through center is continuous with no dead zone.
    /// Each filter sweeps from "fully open" to target as cutoff increases,
    /// so at zero cutoff there is zero effect regardless of frequency position.
    fn sync_filter_to_mpv(&mut self, channel_idx: usize) {
        let (cutoff, freq_pos, lfo_shape, lfo_speed) = self
            .mixer
            .get_channel(channel_idx)
            .map(|c| (c.filter_cutoff, c.filter_freq, c.lfo_shape, c.lfo_speed))
            .unwrap_or((0.0, 0.5, 0.0, 0.0));

        // Linear response so intensity change is spread across the full sweep.
        let raw_cutoff = cutoff;

        // When LFO speed is 0, bypass LFO entirely — shape has no effect
        let modulated = if lfo_speed <= 0.001 {
            raw_cutoff
        } else {
            // LFO modulates the user's cutoff intensity
            // Shape: 0.0 → square (toggles between 0 and 1 every half cycle)
            // Shape: 1.0 → sine (smoothly sweeps between 0 and 1)
            let phase = self
                .mixer
                .get_channel(channel_idx)
                .map(|c| c.lfo_phase)
                .unwrap_or(0.0);
            let raw_lfo = (phase * std::f32::consts::TAU).sin();
            let sq = if raw_lfo > 0.0 { 1.0 } else { 0.0 };
            let sine = raw_lfo * 0.5 + 0.5;
            let lfo_out = sq * (1.0 - lfo_shape) + sine * lfo_shape;
            raw_cutoff * lfo_out
        };

        // Ease-out the last 10% for a gentle approach to max
        let intensity = if modulated > 0.9 {
            let t = (modulated - 0.9) / 0.1;
            0.9 + 0.1 * (1.0 - (1.0 - t).powf(3.0))
        } else {
            modulated
        };

        // Map freq_pos (0–1) to actual frequency (20–20000 Hz, log scale)
        let log_min = 20f32.log10();
        let log_max = 20000f32.log10();
        let actual_freq = 10f32.powf(log_min + freq_pos * (log_max - log_min));

        // Crossfade zone: 300Hz–3kHz
        // blend=0 at 300Hz (pure LPF), blend=1 at 3000Hz (pure HPF)
        let blend = if actual_freq <= 300.0 {
            0.0
        } else if actual_freq >= 3000.0 {
            1.0
        } else {
            let t = (actual_freq - 300.0) / (3000.0 - 300.0);
            t * t * (3.0 - 2.0 * t) // smoothstep
        };

        // LPF target: actual_freq at blend=0, 20000 (open) at blend=1
        let lpf_target = actual_freq + (20000.0 - actual_freq) * blend;
        // HPF target: 20 (open) at blend=0, actual_freq at blend=1
        let hpf_target = 20.0 + (actual_freq - 20.0) * blend;

        // Both sweep from open to target as intensity increases
        let effective_lpf = 20000.0 - (20000.0 - lpf_target) * intensity;
        // Soft approach to 200Hz floor: spread 200-1000Hz with sqrt
        // so descent slows as it nears the clamp
        let soft_lpf = if effective_lpf < 1000.0 {
            let norm = ((effective_lpf - 200.0) / 800.0).clamp(0.0, 1.0);
            200.0 + 800.0 * norm.sqrt()
        } else {
            effective_lpf
        };

        let effective_hpf = 20.0 + (hpf_target - 20.0) * intensity;

        // Skip MPV lavfi calls when the engine has a decoder for this channel.
        // SC controls route over OSC and should always receive updates.
        let engine_active = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(channel_idx))
            .unwrap_or(false);
        if !engine_active && let Some(client) = self.mpv_for_channel(channel_idx) {
            let le = client.set_lpf(soft_lpf.clamp(200.0, 20000.0)).err();
            let he = client.set_hpf(effective_hpf.clamp(20.0, 8000.0)).err();
            if let Some(e) = le {
                self.debug_log.push_back(format!("lpf: {}", e));
            }
            if let Some(e) = he {
                self.debug_log.push_back(format!("hpf: {}", e));
            }
        }
        if let Some(client) = self.sc_for_channel(channel_idx) {
            let _ = client.set_lpf(soft_lpf.clamp(200.0, 20000.0));
            let _ = client.set_hpf(effective_hpf.clamp(20.0, 8000.0));
        }

        // Send to Rust audio engine
        if let Some(ref engine) = self.audio_engine {
            engine.state.set_filter_cutoff(channel_idx, cutoff);
            engine.state.set_filter_freq(channel_idx, freq_pos);
            engine.state.set_lfo(channel_idx, lfo_speed, lfo_shape);
        }

        self.sync_capture_dsp_params();
    }

    /// Sync master EQ to all active decks (MPV + SuperCollider)
    fn sync_master_eq_to_all_decks(&mut self) {
        use crate::state::MASTER_EQ_FREQUENCIES;
        let bands = self.mixer.master.master_eq;

        if let Some(ref engine) = self.audio_engine {
            engine.state.set_master_eq(bands);
        }

        // Send to all 3 MPV decks
        for ch_idx in 0..3 {
            if let Some(client) = self.mpv_for_channel(ch_idx) {
                let _ = client.set_master_eq(&bands, &MASTER_EQ_FREQUENCIES);
            }
            if let Some(client) = self.sc_for_channel(ch_idx) {
                let _ = client.set_master_eq(&bands);
            }
        }
        self.sync_capture_dsp_params();
    }

    /// Sync pan to MPV/SC for a channel
    fn sync_pan_to_mpv(&mut self, channel_idx: usize) {
        let pan = self
            .mixer
            .get_channel(channel_idx)
            .map(|c| c.pan)
            .unwrap_or(0.0);

        let engine_active = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_decoder(channel_idx))
            .unwrap_or(false);
        if !engine_active && let Some(client) = self.mpv_for_channel(channel_idx) {
            let _ = client.set_pan(pan);
        }
        if let Some(client) = self.sc_for_channel(channel_idx) {
            let _ = client.set_pan(pan);
        }
        // Send to Rust audio engine
        if let Some(ref engine) = self.audio_engine {
            engine.state.set_pan(channel_idx, pan);
        }
        self.sync_capture_dsp_params();
    }

    /// Sync BPM-based speed to MPV for a channel
    /// Speed = target_bpm / base_bpm (stable reference from first detection)
    fn sync_bpm_to_mpv(&mut self, channel_idx: usize) {
        let (uses_supercollider, target_bpm, base_bpm, key_offset) = self
            .mixer
            .get_channel(channel_idx)
            .map(|c| (c.uses_supercollider, c.target_bpm, c.base_bpm, c.key_offset))
            .unwrap_or((false, 120.0, 120.0, 0));

        let base = if base_bpm > 0.0 { base_bpm } else { 120.0 };
        let bpm_factor = (target_bpm / base).clamp(0.1, 4.0);
        let semitone_factor = 2.0_f32.powf(key_offset as f32 / 12.0);
        let speed = (bpm_factor * semitone_factor).clamp(0.1, 4.0);

        if let Some(channel) = self.mixer.get_channel_mut(channel_idx) {
            channel.playback_speed = speed;
        }

        if channel_idx < self.route_speed_cache.len() {
            self.route_speed_cache[channel_idx] = speed;
        }

        if let Some(ref engine) = self.audio_engine {
            if uses_supercollider {
                engine.state.set_playback_rate(channel_idx, 1.0);
            } else {
                engine.state.set_playback_rate(channel_idx, speed);
            }
        }

        if uses_supercollider {
            return;
        }

        if let Some(client) = self.mpv_for_channel(channel_idx) {
            let _ = client.set_speed(speed);
        } else if self
            .audio_engine
            .as_ref()
            .map(|e| e.has_capture(channel_idx))
            .unwrap_or(false)
        {
            let _ = self.send_route_command_for_channel(
                channel_idx,
                vec![
                    serde_json::json!("set_property"),
                    serde_json::json!("speed"),
                    serde_json::json!(speed),
                ],
            );
        }

        if channel_idx < self.route_scrub_last_ms.len() {
            self.route_scrub_last_ms[channel_idx] = self.elapsed_ms;
        }
    }

    /// Sync crossfader position to both deck volumes
    fn sync_crossfader_to_mpv(&mut self) {
        // Update both decks - they each apply crossfader gain internally
        self.sync_deck_volume(true);
        self.sync_deck_volume(false);
        // Also update audio capture DSP params
        self.sync_capture_dsp_params();
    }

    /// Sync current mixer state to audio capture DSP parameters (no-op without BlackHole)
    fn sync_capture_dsp_params(&mut self) {}

    /// Cleanup resources before exit.
    ///
    /// Deck A/B handoff policy:
    /// - full-left crossfader: keep only A playing
    /// - full-right crossfader: keep only B playing
    /// - center/overlap: keep both playing
    /// - paused decks stay paused
    ///
    /// This lets MPV continue in a sensible state after TUI exits.
    pub fn cleanup(&mut self) {
        self.handoff_mpv_playback_on_exit();
    }

    fn handoff_mpv_playback_on_exit(&mut self) {
        let deck_a_ch = self.mixer.dj.deck_a_channel;
        let deck_b_ch = self.mixer.dj.deck_b_channel;
        let cue_ch = self.mixer.dj.deck_c_channel;
        let (gain_a, gain_b) = self.calculate_crossfader_gains();
        let master = self.mixer.master.fader;
        let solo_active = self.mixer.solo_active;
        let master_muted = self.mixer.master.muted;

        let deck_a = self.mixer.get_channel(deck_a_ch).map(|ch| {
            let effective_muted = master_muted || ch.muted || (solo_active && !ch.solo);
            let keep_by_crossfader = gain_a > 0.001;
            let keep_playing = ch.playing && keep_by_crossfader && !effective_muted;
            let volume = if keep_playing {
                (ch.fader * gain_a * master * 2.0 * 200.0).clamp(0.0, 200.0)
            } else {
                0.0
            };
            (keep_playing, volume)
        });

        let deck_b = self.mixer.get_channel(deck_b_ch).map(|ch| {
            let effective_muted = master_muted || ch.muted || (solo_active && !ch.solo);
            let keep_by_crossfader = gain_b > 0.001;
            let keep_playing = ch.playing && keep_by_crossfader && !effective_muted;
            let volume = if keep_playing {
                (ch.fader * gain_b * master * 2.0 * 200.0).clamp(0.0, 200.0)
            } else {
                0.0
            };
            (keep_playing, volume)
        });

        let cue = {
            let ch = &self.mixer.cue_channel;
            let effective_muted = master_muted || ch.muted || (solo_active && !ch.solo);
            let keep_playing = ch.playing && !effective_muted;
            let volume = if keep_playing {
                (ch.fader * master * 2.0 * 200.0).clamp(0.0, 200.0)
            } else {
                0.0
            };
            (keep_playing, volume)
        };

        if let Some((keep_playing, volume)) = deck_a {
            self.set_exit_state_for_channel(deck_a_ch, keep_playing, volume);
        }
        if let Some((keep_playing, volume)) = deck_b {
            self.set_exit_state_for_channel(deck_b_ch, keep_playing, volume);
        }
        self.set_exit_state_for_channel(cue_ch, cue.0, cue.1);
    }

    fn set_exit_state_for_channel(&mut self, ch_idx: usize, keep_playing: bool, volume: f32) {
        let mut sent_direct = false;
        if let Some(client) = self.mpv_for_channel(ch_idx) {
            if keep_playing {
                let _ = client.set_volume(volume);
            } else {
                let _ = client.send_command(vec![serde_json::json!("stop")]);
            }
            sent_direct = true;
        }

        if sent_direct {
            return;
        }

        let has_capture = self
            .audio_engine
            .as_ref()
            .map(|e| e.has_capture(ch_idx))
            .unwrap_or(false);
        if !has_capture {
            return;
        }

        if keep_playing {
            let _ = self.send_route_command_for_channel(
                ch_idx,
                vec![
                    serde_json::json!("set_property"),
                    serde_json::json!("volume"),
                    serde_json::json!(volume),
                ],
            );
            let _ = self.send_route_command_for_channel(
                ch_idx,
                vec![
                    serde_json::json!("set_property"),
                    serde_json::json!("pause"),
                    serde_json::json!(false),
                ],
            );
        } else {
            let _ = self.send_route_command_for_channel(ch_idx, vec![serde_json::json!("stop")]);
        }
    }

    /// Add a debug log message (keeps last 500 messages)
    /// Only logs when DEBUG env var is set (e.g. DEBUG=1 ./tidal-mixer)
    pub fn log_debug(&mut self, msg: impl Into<String>) {
        if std::env::var("DEBUG").is_err() {
            return;
        }
        self.debug_log.push_back(msg.into());
        if self.debug_log.len() > 500 {
            self.debug_log.pop_front();
        }
        // Only auto-scroll to bottom if already at the bottom.
        // If user has scrolled up to read, leave them there.
        if self.debug_scroll == 0 {
            // Already at bottom — new messages are visible automatically
        }
    }

    /// Check if debug mode is enabled via DEBUG env var
    #[allow(dead_code)]
    pub fn is_debug_enabled() -> bool {
        std::env::var("DEBUG").is_ok()
    }

    /// Copy the visible debug log lines to the system clipboard.
    fn copy_debug_log_to_clipboard(&mut self) {
        let text = self.debug_log.make_contiguous().join("\n");
        if text.is_empty() {
            return;
        }
        let copied = if cfg!(target_os = "macos") {
            std::process::Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(text.as_bytes())?;
                    child.wait()?;
                    Ok(())
                })
                .is_ok()
        } else if cfg!(target_os = "linux") {
            std::process::Command::new("xclip")
                .args(["-selection", "clipboard"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(text.as_bytes())?;
                    child.wait()?;
                    Ok(())
                })
                .is_ok()
        } else {
            false
        };
        if copied {
            self.log_debug("Debug log copied to clipboard".to_string());
        }
    }
}
