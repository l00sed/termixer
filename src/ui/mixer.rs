//! Main mixer layout that arranges all channel strips

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
        StatefulWidget, Widget, Wrap,
    },
};

use crate::app::{
    AddSourceTab, ConfirmAction, Deck, MixerLayout, OutputPickerTarget, PickerInputMode,
    SelectedPane, SessionPickerState, SourcePickerState, SourcePickerTab,
};
use crate::state::{
    ChannelControl, EditTarget, GlobalControl, GlobalSequenceControl, MixerState, PAD_KEYS,
    SamplePadGrid, SelectionFocus, SequenceState,
};
use crate::ui::channel::{ChannelStrip, MasterStrip};
use crate::ui::colors::*;
use crate::ui::sampler::{PadConfigPane, SequenceRow, SequenceTopBar};
use crate::ui::widgets::Crossfader;

/// The main mixer view
pub struct MixerView<'a> {
    state: &'a MixerState,
    pads: &'a SamplePadGrid,
    show_help: bool,
    editing: bool,
    control_select: bool,
    frame: u8,
    elapsed_ms: u64,
    selected_pane: SelectedPane,
    source_picker: Option<(Deck, &'a SourcePickerState)>,
    sample_picker: Option<(usize, &'a SourcePickerState)>,
    selected_pad_idx: Option<usize>,
    pad_config_mode: bool,
    pad_config_editing: bool,
    sequences: Option<&'a SequenceState>,
    current_play_steps: &'a [usize],
    master_output_device: Option<&'a str>,
    cue_output_device: Option<&'a str>,
    mic_input_device: Option<&'a str>,
    output_picker_active: bool,
    output_picker_target: OutputPickerTarget,
    master_output_devices: &'a [String],
    cue_output_devices: &'a [String],
    mic_input_devices: &'a [String],
    selected_master_output_idx: usize,
    selected_cue_output_idx: usize,
    selected_mic_input_idx: usize,
    debug_log: Option<&'a [String]>,
    debug_scroll: usize,
    samples_dir: Option<&'a std::path::Path>,
    layout_start_end: Option<(usize, usize)>,
    confirm_action: Option<ConfirmAction>,
    confirm_selected: bool,
    help_scroll: usize,
    deck_queue_indices: &'a [usize; 3],
    session_picker: Option<&'a SessionPickerState>,
}

impl<'a> MixerView<'a> {
    pub fn new(state: &'a MixerState, pads: &'a SamplePadGrid) -> Self {
        Self {
            state,
            pads,
            show_help: false,
            editing: false,
            control_select: false,
            frame: 0,
            elapsed_ms: 0,
            selected_pane: SelectedPane::DeckA,
            source_picker: None,
            sample_picker: None,
            selected_pad_idx: None,
            pad_config_mode: false,
            pad_config_editing: false,
            sequences: None,
            current_play_steps: &[],
            master_output_device: None,
            cue_output_device: None,
            mic_input_device: None,
            output_picker_active: false,
            output_picker_target: OutputPickerTarget::Master,
            master_output_devices: &[],
            cue_output_devices: &[],
            mic_input_devices: &[],
            selected_master_output_idx: 0,
            selected_cue_output_idx: 0,
            selected_mic_input_idx: 0,
            debug_log: None,
            debug_scroll: 0,
            samples_dir: None,
            layout_start_end: None,
            confirm_action: None,
            confirm_selected: false,
            help_scroll: 0,
            deck_queue_indices: &[0, 0, 0],
            session_picker: None,
        }
    }

    pub fn show_help(mut self, show: bool) -> Self {
        self.show_help = show;
        self
    }

    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    pub fn control_select(mut self, control_select: bool) -> Self {
        self.control_select = control_select;
        self
    }

    pub fn frame(mut self, frame: u8) -> Self {
        self.frame = frame;
        self
    }

    pub fn elapsed_ms(mut self, elapsed_ms: u64) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }

    pub fn selected_pane(mut self, pane: SelectedPane) -> Self {
        self.selected_pane = pane;
        self
    }

    pub fn source_picker(mut self, deck: Deck, picker: &'a SourcePickerState) -> Self {
        self.source_picker = Some((deck, picker));
        self
    }

    pub fn deck_queue_indices(mut self, indices: &'a [usize; 3]) -> Self {
        self.deck_queue_indices = indices;
        self
    }

    pub fn sample_picker(mut self, pad_idx: usize, picker: &'a SourcePickerState) -> Self {
        self.sample_picker = Some((pad_idx, picker));
        self
    }

    pub fn pad_config_mode(mut self, config_mode: bool) -> Self {
        self.pad_config_mode = config_mode;
        self
    }

    pub fn pad_config_editing(mut self, editing: bool) -> Self {
        self.pad_config_editing = editing;
        self
    }

    pub fn sequences(mut self, sequences: &'a SequenceState) -> Self {
        self.sequences = Some(sequences);
        self
    }

    pub fn current_play_steps(mut self, steps: &'a [usize]) -> Self {
        self.current_play_steps = steps;
        self
    }

    pub fn layout_start_end(mut self, start_end: Option<(usize, usize)>) -> Self {
        self.layout_start_end = start_end;
        self
    }

    pub fn selected_pad_idx(mut self, idx: Option<usize>) -> Self {
        self.selected_pad_idx = idx;
        self
    }

    pub fn master_output_device(mut self, device: Option<&'a str>) -> Self {
        self.master_output_device = device;
        self
    }

    pub fn cue_output_device(mut self, device: Option<&'a str>) -> Self {
        self.cue_output_device = device;
        self
    }

    pub fn mic_input_device(mut self, device: Option<&'a str>) -> Self {
        self.mic_input_device = device;
        self
    }

    pub fn output_picker_active(mut self, active: bool) -> Self {
        self.output_picker_active = active;
        self
    }

    pub fn output_picker_target(mut self, target: OutputPickerTarget) -> Self {
        self.output_picker_target = target;
        self
    }

    pub fn confirm_action(mut self, action: Option<ConfirmAction>) -> Self {
        self.confirm_action = action;
        self
    }

    pub fn confirm_selected(mut self, selected: bool) -> Self {
        self.confirm_selected = selected;
        self
    }

    pub fn session_picker(mut self, picker: &'a SessionPickerState) -> Self {
        self.session_picker = Some(picker);
        self
    }

    pub fn help_scroll(mut self, offset: usize) -> Self {
        self.help_scroll = offset;
        self
    }

    pub fn master_output_devices(mut self, devices: &'a [String]) -> Self {
        self.master_output_devices = devices;
        self
    }

    pub fn cue_output_devices(mut self, devices: &'a [String]) -> Self {
        self.cue_output_devices = devices;
        self
    }

    pub fn mic_input_devices(mut self, devices: &'a [String]) -> Self {
        self.mic_input_devices = devices;
        self
    }

    pub fn selected_master_output_idx(mut self, idx: usize) -> Self {
        self.selected_master_output_idx = idx;
        self
    }

    pub fn selected_cue_output_idx(mut self, idx: usize) -> Self {
        self.selected_cue_output_idx = idx;
        self
    }

    pub fn selected_mic_input_idx(mut self, idx: usize) -> Self {
        self.selected_mic_input_idx = idx;
        self
    }

    pub fn debug_log(mut self, log: &'a [String]) -> Self {
        self.debug_log = Some(log);
        self
    }

    pub fn debug_scroll(mut self, scroll: usize) -> Self {
        self.debug_scroll = scroll;
        self
    }

    pub fn samples_dir(mut self, dir: Option<&'a std::path::Path>) -> Self {
        self.samples_dir = dir;
        self
    }
}

impl<'a> Widget for MixerView<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Split area for debug log at bottom (only when DEBUG env var is set)
        let debug_enabled = std::env::var("DEBUG").is_ok();
        let (main_area, debug_area) =
            if debug_enabled && self.debug_log.is_some() && !self.debug_log.unwrap().is_empty() {
                let chunks = Layout::vertical([
                    Constraint::Min(0),
                    Constraint::Length(10), // 10 lines for debug log
                ])
                .split(area);
                (chunks[0], Some(chunks[1]))
            } else {
                (area, None)
            };

        // Always use full-width layout
        self.render_full_width_layout(main_area, buf);

        // Render help overlay if enabled
        if self.show_help {
            self.render_help_overlay(main_area, buf);
        }

        // Render source picker overlay if active
        if let Some((deck, picker)) = self.source_picker {
            self.render_source_picker(main_area, buf, deck, picker);
        }

        // Sample picker is rendered inline in render_dj_center

        // Render output device picker overlay if active
        if self.output_picker_active {
            self.render_output_picker(main_area, buf);
        }

        // Render confirm dialog overlay if active
        if let Some(action) = self.confirm_action {
            self.render_confirm_dialog(main_area, buf, action);
        }

        // Render session picker dialog overlay if active
        if let Some(picker) = self.session_picker {
            self.render_session_picker(main_area, buf, picker);
        }

        // Render debug log if present
        if let Some(area) = debug_area
            && let Some(log) = self.debug_log
        {
            self.render_debug_log(area, buf, log);
        }
    }
}

impl<'a> MixerView<'a> {
    /// Full-width layout that scales to terminal size
    fn render_full_width_layout(&self, area: Rect, buf: &mut Buffer) {
        // Main layout: header, mixer area, footer
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // Header (compact)
                Constraint::Min(16),   // Mixer area
                Constraint::Length(1), // Footer (single line)
            ])
            .split(area);

        self.render_header(main_chunks[0], buf);
        self.render_mixer_full_width(main_chunks[1], buf);
        self.render_footer(main_chunks[2], buf);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        // Minimalist header - just mode indicator and status
        let mode_str = if self.editing { "[EDIT]" } else { "" };
        let title = format!("T E R M I X E R {}", mode_str);

        let title_style = Style::default()
            .fg(TEXT_BRIGHT)
            .add_modifier(Modifier::BOLD);
        buf.set_string(area.x + 1, area.y, &title, title_style);

        // Status on the right: pane name, optionally with control name
        let pane_label = match self.selected_pane {
            SelectedPane::DeckA => "DECK A",
            SelectedPane::DeckB => "DECK B",
            SelectedPane::DeckC => "DECK C",
            SelectedPane::Master => "MASTER",
            SelectedPane::DjCenter => "PADS",
            SelectedPane::Loops => "SEQUENCES",
            SelectedPane::Crossfader => "CROSSFADE",
        };

        let status = if self.control_select || self.editing {
            // ControlSelect or Edit mode: show pane name | control name
            // Skip the control name for Crossfader pane (it's redundant)
            if matches!(self.selected_pane, SelectedPane::Crossfader) {
                pane_label.to_string()
            } else if matches!(self.selected_pane, SelectedPane::DjCenter) {
                // PADS pane: show pad key or CONFIG
                if self.pad_config_mode {
                    if let Some(pad_idx) = self.selected_pad_idx {
                        let key = self.pads.pads[pad_idx].key_char().to_ascii_uppercase();
                        format!("{} | CONFIG {}", pane_label, key)
                    } else {
                        format!("{} | CONFIG", pane_label)
                    }
                } else if let Some(pad_idx) = self.selected_pad_idx {
                    let key = self.pads.pads[pad_idx].key_char().to_ascii_uppercase();
                    format!("{} | {}", pane_label, key)
                } else {
                    pane_label.to_string()
                }
            } else if matches!(self.selected_pane, SelectedPane::Loops) {
                // SEQUENCES pane: derive label from sequence state
                if let Some(seq_state) = self.sequences {
                    if seq_state.global_focused {
                        // Global bar focused: show Volume/Bpm/Play/Pause
                        let ctrl_label = match seq_state.global_control {
                            GlobalSequenceControl::Volume => "VOLUME",
                            GlobalSequenceControl::Bpm => "BPM",
                            GlobalSequenceControl::Mute => {
                                if seq_state.global.mute {
                                    "PAUSE"
                                } else {
                                    "PLAY"
                                }
                            }
                            GlobalSequenceControl::Save => "SAVE",
                            GlobalSequenceControl::Load => "LOAD",
                        };
                        format!("{} | {}", pane_label, ctrl_label)
                    } else if let Some(seq_idx) = seq_state.selected {
                        // Sequence row focused: show pad and cursor target
                        if let Some(seq) = seq_state.sequences.get(seq_idx) {
                            let ctrl_label = match seq_state.cursor {
                                EditTarget::Step(step) => {
                                    format!("TRIGGER {}", step)
                                }
                                EditTarget::Mute => "MUTE".to_string(),
                                EditTarget::Gear => "CONFIG".to_string(),
                            };
                            format!("{} | {} | {}", pane_label, seq.name, ctrl_label)
                        } else {
                            pane_label.to_string()
                        }
                    } else {
                        pane_label.to_string()
                    }
                } else {
                    pane_label.to_string()
                }
            } else {
                match self.state.focus {
                    SelectionFocus::Channel(_) => {
                        format!("{} | {}", pane_label, self.state.selected_control.label())
                    }
                    SelectionFocus::Global => {
                        format!("{} | {}", pane_label, self.state.selected_global.label())
                    }
                }
            }
        } else {
            // PaneSelect mode: just show the pane name
            pane_label.to_string()
        };

        let status_x = area.x + area.width.saturating_sub(status.len() as u16 + 1);
        buf.set_string(status_x, area.y, &status, Style::default().fg(METER_TRACK));

        // Thin separator line
        let sep = "─".repeat(area.width as usize);
        buf.set_string(area.x, area.y + 1, &sep, Style::default().fg(SEPARATOR));
    }

    /// Full-width mixer layout - DJ section centered, decks on sides
    fn render_mixer_full_width(&self, area: Rect, buf: &mut Buffer) {
        // Layout:
        // [Deck A] [DJ Center (pads)] [Deck B] [Deck C] [Master]
        //                   [Loops]
        //                   [Crossfader]
        //
        // DJ Center must stay ≥20 for the 4×4 pad grid. If there isn't room
        // for Master at minimum widths, Master is pushed to overflow (right
        // of viewport, reachable via horizontal scrolling).
        // Compute the horizontal layout (columns + scroll) from the viewport
        // width and which pane is selected. Centralised so the renderer,
        // scroll logic, and mouse hit-testing all agree.
        let (cur_start, cur_end) = self
            .layout_start_end
            .map(|(s, e)| (Some(s), Some(e)))
            .unwrap_or((None, None));
        let layout = MixerLayout::compute(area.width, self.selected_pane, cur_start, cur_end);

        // Build constraints ONLY for visible columns (start..=end).
        // This prevents ratatui from squeezing when off-screen columns
        // inflate the constraint sum beyond the viewport width.
        let mut constraints = vec![];
        for i in layout.start..=layout.end {
            let w = match i {
                0 => layout.deck_a,
                1 => layout.dj,
                2 => layout.deck_b,
                3 => layout.deck_c,
                4 => layout.master,
                _ => unreachable!(),
            };
            constraints.push(Constraint::Length(w));
        }
        let horizontal_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);

        // Map logical column index to chunk index: chunk_idx = col_idx - start
        let start = layout.start as usize;
        let chunk = |col: usize| -> Rect { horizontal_chunks[col - start] };

        // Only show selected control when in control select mode (or editing)
        let show_control = self.control_select || self.editing;

        // ── Deck A (column 0) ──
        if layout.start == 0
            && let Some(channel) = self.state.channels.get(self.state.dj.deck_a_channel)
        {
            let pane_selected = self.selected_pane == SelectedPane::DeckA;
            let control = if show_control && pane_selected {
                Some(self.state.selected_control)
            } else {
                None
            };
            ChannelStrip::new(channel)
                .selected(pane_selected, control)
                .deck_label(Some("A"))
                .deck_color(DECK_A)
                .editing(self.editing && pane_selected)
                .frame(self.frame)
                .elapsed_ms(self.elapsed_ms)
                .render(chunk(0), buf);
        }

        // ── DJ Center (column 1) — split vertically into Pads, Loops, Crossfader ──
        if layout.start <= 1 && 1 <= layout.end {
            let crossfader_height = 5u16;
            let available = chunk(1).height.saturating_sub(crossfader_height);
            let pads_height = (available * 2) / 3;
            let loops_height_actual = available.saturating_sub(pads_height);
            let dj_vertical = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(pads_height),         // Pads area
                    Constraint::Length(loops_height_actual), // Sequences pane
                    Constraint::Length(crossfader_height),   // Crossfader pane
                ])
                .split(chunk(1));

            let dj_center_area = dj_vertical[0];
            let loops_area = dj_vertical[1];
            let crossfader_area = dj_vertical[2];

            self.render_dj_center(dj_center_area, buf);
            self.render_sequences(loops_area, buf);
            self.render_crossfader(crossfader_area, buf);
        }

        // ── Deck B (column 2) ──
        if layout.start <= 2
            && 2 <= layout.end
            && let Some(channel) = self.state.channels.get(self.state.dj.deck_b_channel)
        {
            let pane_selected = self.selected_pane == SelectedPane::DeckB;
            let control = if show_control && pane_selected {
                Some(self.state.selected_control)
            } else {
                None
            };
            ChannelStrip::new(channel)
                .selected(pane_selected, control)
                .deck_label(Some("B"))
                .deck_color(DECK_B)
                .editing(self.editing && pane_selected)
                .frame(self.frame)
                .elapsed_ms(self.elapsed_ms)
                .render(chunk(2), buf);
        }

        // ── Deck C (column 3) ──
        if layout.start <= 3 && 3 <= layout.end {
            self.render_cue_pane(chunk(3), buf);
        }

        // ── Master (column 4) ──
        if layout.start <= 4 && 4 <= layout.end {
            let master_pane_selected = self.selected_pane == SelectedPane::Master;
            let master_control_selected = master_pane_selected && show_control;
            let master_control = if master_control_selected {
                Some(self.state.selected_global)
            } else {
                None
            };
            let any_playing =
                self.state.channels.iter().any(|c| c.playing) || self.state.cue_channel.playing;
            MasterStrip::new(&self.state.master)
                .mic_input_device(self.mic_input_device)
                .pane_selected(master_pane_selected)
                .selected(master_control_selected, master_control)
                .editing(self.editing && master_pane_selected)
                .frame(self.frame)
                .any_channel_playing(any_playing)
                .render(chunk(4), buf);
        }
    }

    /// Centered DJ section with pads
    fn render_dj_center(&self, area: Rect, buf: &mut Buffer) {
        let is_active = self.control_select || self.editing;
        let border_color = if self.selected_pane == SelectedPane::DjCenter {
            if is_active {
                BORDER_ACTIVE
            } else {
                BORDER_NAVIGATED
            }
        } else {
            BG_LIGHT
        };

        let pane_selected = self.selected_pane == SelectedPane::DjCenter;
        let title_style = if pane_selected {
            Style::default()
                .fg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(border_color)
        };

        let block = Block::default()
            .title(Span::styled(" PADS ", title_style))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        block.render(area, buf);

        if let Some((pad_idx, picker)) = self.sample_picker {
            // Sample picker replaces config pane
            self.render_sample_picker_inline(inner, buf, pad_idx, picker);
        } else if self.pad_config_mode {
            // Config pane replaces pad grid
            // Look up the tempo for this pad's sequence
            let seq_tempo = self
                .sequences
                .and_then(|ss| {
                    ss.sequences
                        .iter()
                        .find(|s| s.pad_idx == self.pads.selected_pad)
                })
                .map(|s| s.tempo)
                .unwrap_or(1.0);
            PadConfigPane::new(self.pads)
                .editing(self.pad_config_editing)
                .samples_dir(self.samples_dir)
                .sequence_tempo(seq_tempo)
                .render(inner, buf);
        } else {
            // Pads (centered in the section)
            self.render_pad_grid_centered(inner, buf);
        }
    }

    /// Render pads centered horizontally in the available area
    fn render_pad_grid_centered(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 12 || area.height < 6 {
            return;
        }

        // Pads: 5 wide x 3 tall with 1 char gap between columns
        // This matches the visual gap from double horizontal borders between rows
        let cell_w = 5u16;
        let cell_h = 3u16;
        let gap_x = 1u16; // Gap between columns

        let grid_w = cell_w * 4 + gap_x * 3; // 4 cells + 3 gaps
        let grid_h = cell_h * 4;

        // Center the grid in available space
        let offset_x = (area.width.saturating_sub(grid_w)) / 2;
        let offset_y = (area.height.saturating_sub(grid_h)) / 2;

        for row in 0..4 {
            for col in 0..4 {
                let pad_idx = row * 4 + col;
                let x = area.x + offset_x + col as u16 * (cell_w + gap_x);
                let y = area.y + offset_y + row as u16 * cell_h;

                if x + cell_w <= area.x + area.width && y + cell_h <= area.y + area.height {
                    let cell_area = Rect::new(x, y, cell_w, cell_h);
                    self.render_pad_cell(cell_area, buf, pad_idx);
                }
            }
        }
    }

    /// Render a single pad cell with border
    fn render_pad_cell(&self, area: Rect, buf: &mut Buffer, pad_idx: usize) {
        let pad = &self.pads.pads[pad_idx];
        let is_triggered = pad.triggered;
        let is_selected = self.selected_pad_idx == Some(pad_idx);

        let border_color = if is_selected || is_triggered {
            BORDER_ACTIVE
        } else if pad.has_sample() {
            // Brighter color for pads with samples
            let (r, g, b) = pad.color;
            Color::Rgb(
                r.saturating_add(30),
                g.saturating_add(30),
                b.saturating_add(30),
            )
        } else {
            BORDER_DEFAULT
        };
        let style = Style::default().fg(border_color);

        // Draw box border
        let horiz = "─".repeat((area.width - 2) as usize);
        buf.set_string(area.x, area.y, "┌", style);
        buf.set_string(area.x + 1, area.y, &horiz, style);
        buf.set_string(area.x + area.width - 1, area.y, "┐", style);

        buf.set_string(area.x, area.y + area.height - 1, "└", style);
        buf.set_string(area.x + 1, area.y + area.height - 1, &horiz, style);
        buf.set_string(
            area.x + area.width - 1,
            area.y + area.height - 1,
            "┘",
            style,
        );

        // Side borders for middle rows
        for row in 1..(area.height - 1) {
            buf.set_string(area.x, area.y + row, "│", style);
            buf.set_string(area.x + area.width - 1, area.y + row, "│", style);
        }

        // Key label centered in inner area
        let key = pad.key_char().to_ascii_uppercase();
        let inner_w = area.width.saturating_sub(2);
        let cx = area.x + 1 + inner_w / 2;
        let cy = area.y + area.height / 2;

        let key_style = if is_selected || is_triggered {
            Style::default()
                .fg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else if pad.has_sample() {
            Style::default().fg(SLIDER_MID)
        } else {
            Style::default().fg(HINT_DEFAULT)
        };
        // Single-char key without String allocation
        let mut key_buf = [0u8; 4];
        let key_str = key.encode_utf8(&mut key_buf);
        buf.set_string(cx, cy, key_str, key_style);
    }

    /// Render the sequences pane (below DJ center, between Deck A and Deck B)
    fn render_sequences(&self, area: Rect, buf: &mut Buffer) {
        let is_active = self.control_select || self.editing;
        let border_color = if self.selected_pane == SelectedPane::Loops {
            if is_active {
                BORDER_ACTIVE
            } else {
                BORDER_NAVIGATED
            }
        } else {
            BG_LIGHT
        };

        let pane_selected = self.selected_pane == SelectedPane::Loops;
        let title_style = if pane_selected {
            Style::default()
                .fg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(border_color)
        };

        let block = Block::default()
            .title(Span::styled(" SEQUENCES ", title_style))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 1 || inner.width < 5 {
            return;
        }

        let seq_state = match self.sequences {
            Some(s) => s,
            None => return,
        };

        // Top bar (row 0)
        let global_bar_area = Rect::new(inner.x, inner.y, inner.width, 1);
        let global_focused = pane_selected && seq_state.global_focused;
        let global_ctrl = seq_state.global_control;
        let global_editing = self.editing && global_focused;
        SequenceTopBar::new(&seq_state.global)
            .selected(global_focused)
            .selected_control(global_ctrl)
            .editing(global_editing)
            .border_color(border_color)
            .render(global_bar_area, buf);

        // Separator line (row 1)
        let sep_y = inner.y + 1;
        if sep_y < inner.y + inner.height {
            let sep_line = "─".repeat(inner.width as usize);
            buf.set_string(inner.x, sep_y, &sep_line, Style::default().fg(border_color));

            // Draw ┴ intersections connecting top bar │ to separator line
            // Positions match SequenceTopBar render formula (from right edge)
            // Buttons: Play(3) │ Load(8) │ Save(8) │ BPM │ Vol
            let bpm_str_len = format!("{:.0}", seq_state.global.bpm).len() as u16;
            let sep1_x = inner.x + inner.width - 25 - bpm_str_len; // Vol│BPM
            let sep_save_x = inner.x + inner.width - 22; // BPM│Save
            let sep_ls_x = inner.x + inner.width - 13; // Save│Load
            let sep_pp_x = inner.x + inner.width - 4; // Load│Play
            for sx in [sep1_x, sep_save_x, sep_ls_x, sep_pp_x] {
                if sx >= inner.x && sx < inner.x + inner.width {
                    buf.set_string(sx, sep_y, "\u{2534}", Style::default().fg(border_color));
                }
            }

            // Connect separator line to pane borders: ├ on left, ┤ on right
            let left_border = inner.x.saturating_sub(1);
            let right_border = inner.x + inner.width;
            buf.set_string(left_border, sep_y, "├", Style::default().fg(border_color));
            buf.set_string(right_border, sep_y, "┤", Style::default().fg(border_color));
        }

        // Draw ┬ intersections on the top border connecting to top bar │
        {
            let top_border_y = inner.y.saturating_sub(1);
            let bpm_str_len = format!("{:.0}", seq_state.global.bpm).len() as u16;
            let sep1_x = inner.x + inner.width - 25 - bpm_str_len;
            let sep_save_x = inner.x + inner.width - 22;
            let sep_ls_x = inner.x + inner.width - 13;
            let sep_pp_x = inner.x + inner.width - 4;
            for sx in [sep1_x, sep_save_x, sep_ls_x, sep_pp_x] {
                if sx >= inner.x && sx < inner.x + inner.width {
                    buf.set_string(
                        sx,
                        top_border_y,
                        "\u{252C}",
                        Style::default().fg(border_color),
                    );
                }
            }
        }

        // Sequence rows (scrollable, starting from row 2)
        let rows_area = Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(2),
        );
        let max_visible = rows_area.height as usize;

        // Ensure selected row is visible (scroll to keep it in view)
        // We need mutable access to scroll_offset, but seq_state is borrowed immutably.
        // Instead, compute the effective scroll offset for rendering.
        let effective_scroll = if let Some(sel) = seq_state.selected {
            let sel_display = sel.saturating_sub(seq_state.scroll_offset);
            if sel_display >= max_visible && max_visible > 0 {
                sel.saturating_sub(max_visible - 1)
            } else if sel < seq_state.scroll_offset {
                sel
            } else {
                seq_state.scroll_offset
            }
        } else {
            seq_state.scroll_offset
        };

        for (display_idx, global_idx) in (effective_scroll..seq_state.sequences.len()).enumerate() {
            if display_idx >= max_visible {
                break;
            }
            let seq = &seq_state.sequences[global_idx];
            let row_y = rows_area.y + display_idx as u16;
            if row_y < rows_area.y + rows_area.height {
                let row_area = Rect::new(rows_area.x, row_y, rows_area.width, 1);
                let is_selected = pane_selected
                    && !seq_state.global_focused
                    && seq_state.selected == Some(global_idx);
                let current_step = self
                    .current_play_steps
                    .get(global_idx)
                    .copied()
                    .unwrap_or(0);
                SequenceRow::new(seq)
                    .selected(is_selected)
                    .editing(self.editing && is_selected)
                    .cursor(seq_state.cursor)
                    .current_play_step(current_step)
                    .frame(self.frame)
                    .border_color(border_color)
                    .render(row_area, buf);
            }
        }
    }

    /// Render the crossfader pane (below loops)
    fn render_crossfader(&self, area: Rect, buf: &mut Buffer) {
        let is_active = self.control_select || self.editing;
        let border_color = if self.selected_pane == SelectedPane::Crossfader {
            if is_active {
                BORDER_ACTIVE
            } else {
                BORDER_NAVIGATED
            }
        } else {
            BG_LIGHT
        };

        let pane_selected = self.selected_pane == SelectedPane::Crossfader;
        let title_style = if pane_selected {
            Style::default()
                .fg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(border_color)
        };

        let block = Block::default()
            .title(Span::styled(" CROSSFADE ", title_style))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 1 || inner.width < 10 {
            return;
        }

        // Crossfader with horizontal padding
        let padded = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(10),
                Constraint::Length(2),
            ])
            .split(inner);

        let show_control = self.control_select || self.editing;
        let crossfader_selected = show_control && self.selected_pane == SelectedPane::Crossfader;
        Crossfader::new(self.state.dj.crossfader)
            .selected(crossfader_selected)
            .labels("A", "B")
            .render(padded[1], buf);
    }

    /// Render the CUE deck (Deck C) with channel strip and controls
    fn render_cue_pane(&self, area: Rect, buf: &mut Buffer) {
        let is_active = self.control_select || self.editing;
        let pane_selected = self.selected_pane == SelectedPane::DeckC;

        // Render CUE pane border
        let border_color = if pane_selected {
            if is_active {
                BORDER_ACTIVE
            } else {
                BORDER_NAVIGATED
            }
        } else {
            BG_LIGHT
        };

        let border_title_style = if pane_selected {
            Style::default()
                .fg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else if self.state.cue_channel.connected {
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_GHOST)
        };

        let border_title = if self.state.cue_channel.connected {
            " C ● "
        } else {
            " C ○ "
        };

        let block = Block::default()
            .title(Span::styled(border_title, border_title_style))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 3 || inner.width < 6 {
            return;
        }

        // Split inner area: top part for channel strip, separator, bottom for controls
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(8),    // Channel strip
                Constraint::Length(1), // Separator line
                Constraint::Length(3), // Controls (swap A | swap B, sep, OUTPUT)
            ])
            .split(inner);

        // Render the CUE channel strip (no border - it's nested inside the CUE pane)
        let show_control = self.control_select || self.editing;
        let control = if show_control && pane_selected {
            Some(self.state.selected_control)
        } else {
            None
        };
        ChannelStrip::new(&self.state.cue_channel)
            .selected(pane_selected, control)
            .deck_label(Some("C"))
            .deck_color(DECK_C)
            .editing(self.editing && pane_selected)
            .frame(self.frame)
            .elapsed_ms(self.elapsed_ms)
            .show_border(false)
            .render(chunks[0], buf);

        // Render separator between channel strip and controls
        // Use ┼ at center to connect the two-column control rows
        let sep_area = chunks[1];
        let sep_style = Style::default().fg(SEPARATOR);
        let sep_center = sep_area.x + sep_area.width / 2;
        for x in sep_area.x..sep_area.x + sep_area.width {
            if x != sep_center {
                buf.set_string(x, sep_area.y, "─", sep_style);
            }
        }
        buf.set_string(sep_center, sep_area.y, "┼", sep_style);

        // Render CUE controls (S | swap B, sep, OUTPUT) - two-column layout like M/S
        let controls_area = chunks[2];

        // Two equal columns: S │ swap B
        let sep_x = controls_area.x + controls_area.width / 2;
        let left_w = sep_x - controls_area.x;
        let right_w = controls_area.x + controls_area.width - sep_x - 1;

        // Center each label within its half
        let solo_label = "S";
        let send_b_label = " B";
        let solo_x = controls_area.x + 1 + (left_w.saturating_sub(solo_label.len() as u16 + 2)) / 2;
        let send_b_x = sep_x
            + 1
            + (right_w.saturating_sub(unicode_width::UnicodeWidthStr::width(send_b_label) as u16))
                / 2;

        // │ separator between S and swap B on the button row
        buf.set_string(sep_x, controls_area.y, "│", sep_style);

        // Separator line below with ┴ junction where │ meets it
        if controls_area.height > 1 {
            let sep_y = controls_area.y + 1;
            for x in controls_area.x..controls_area.x + controls_area.width {
                buf.set_string(x, sep_y, "─", sep_style);
            }
            buf.set_string(sep_x, sep_y, "┴", sep_style);
        }

        // S (Solo) button - with background highlight when active
        let solo_selected =
            pane_selected && show_control && self.state.selected_control == ChannelControl::Solo;
        let solo_active = self.state.cue_channel.solo;

        // Fill left column background if solo is active
        if solo_active {
            for x in controls_area.x + 1..sep_x.saturating_sub(1) {
                buf.set_string(x, controls_area.y, " ", Style::default().bg(BORDER_ACTIVE));
            }
        }

        let solo_style = if solo_active {
            Style::default().fg(Color::Black).bg(BORDER_ACTIVE)
        } else if solo_selected {
            Style::default().fg(BORDER_ACTIVE)
        } else {
            Style::default().fg(METER_TRACK)
        };
        buf.set_string(solo_x, controls_area.y, solo_label, solo_style);

        // Swap B button
        let send_b_selected = pane_selected
            && show_control
            && self.state.selected_control == ChannelControl::CueSendToB;
        let send_b_style = if send_b_selected {
            Style::default().fg(BORDER_ACTIVE)
        } else {
            Style::default().fg(METER_TRACK)
        };
        buf.set_string(send_b_x, controls_area.y, send_b_label, send_b_style);

        // OUTPUT button (centered)
        let output_selected = pane_selected
            && show_control
            && self.state.selected_control == ChannelControl::CueOutputSelect;
        let output_style = if output_selected {
            Style::default().fg(BORDER_ACTIVE)
        } else {
            Style::default().fg(METER_TRACK)
        };
        if controls_area.height > 2 {
            let output_label = "OUT";
            let output_x = controls_area.x
                + (controls_area
                    .width
                    .saturating_sub(output_label.len() as u16))
                    / 2;
            buf.set_string(output_x, controls_area.y + 2, output_label, output_style);
        }
    }

    /// Render a vertical slider with label at bottom
    fn render_footer(&self, area: Rect, buf: &mut Buffer) {
        // Single line footer with context hints
        let hint = if self.editing {
            if self.state.selected_global == GlobalControl::Crossfader {
                "h/j/k/l:adjust  a:A  b:B  0/c:center  Enter/Esc:done"
            } else {
                // Check if a filter, pan, or BPM is selected
                match self.state.focus {
                    SelectionFocus::Channel(_) => match self.state.selected_control {
                        ChannelControl::FilterCutoff | ChannelControl::FilterFreq => {
                            "h/j/k/l:adjust  H:min  L:max  0:reset  Enter/Esc:done"
                        }
                        ChannelControl::Pan => {
                            "h/j/k/l:adjust  H:left  L:right  0/c:center  Enter/Esc:done"
                        }
                        ChannelControl::Scrub => "h/j/k/l:scrub  H/J/K/L:coarse  Enter/Esc:done",
                        ChannelControl::PrevTrack | ChannelControl::NextTrack => {
                            "h/l:select icon  Enter:skip track  Esc:back"
                        }
                        ChannelControl::Bpm => "h/j/k/l:adjust BPM  0/c:x1.00  Enter/Esc:done",
                        _ => "h/j/k/l:adjust  0:reset  c:center  Enter/Esc:done",
                    },
                    SelectionFocus::Global => "h/j/k/l:adjust  0:reset  c:center  Enter/Esc:done",
                }
            }
        } else if self.pads.active {
            "4567/RTYU/FGHJ/VBNM:trigger  Space:stop  P/Esc:exit"
        } else {
            match self.state.focus {
                SelectionFocus::Channel(_) => {
                    if self.state.selected_control.is_continuous() {
                        "h/j/k/l:nav  Enter:edit  m:mute  s:solo  Tab:pane  ?:help"
                    } else {
                        "h/j/k/l:nav  Enter:toggle  Tab:pane  ?:help"
                    }
                }
                SelectionFocus::Global => {
                    // Show crossfader slam shortcuts when Crossfader pane is selected
                    if self.selected_pane == SelectedPane::Crossfader {
                        "a:A  b:B  Enter:edit  Tab:deck  ?:help"
                    } else {
                        "h/j/k/l:nav  Enter:edit  Tab:deck  ?:help"
                    }
                }
            }
        };

        buf.set_string(area.x + 1, area.y, hint, Style::default().fg(TEXT_DIM));

        // Mode indicators on right side
        // Half-block cap characters extend the label's bg by half a cell on
        // each side so the highlighted block visually continues the MASTER
        // pane's right border above into the footer.
        // Cap fg matches the label's bg; no bg set → cell stays transparent.
        let mut right_x = area.x + area.width;

        if !self.state.master.playing {
            let label = " PAUSED ";
            let style = Style::default()
                .fg(Color::Black)
                .bg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD);
            let cap = Style::default().fg(BORDER_ACTIVE);
            right_x = right_x.saturating_sub(label.len() as u16 + 1);
            buf.set_string(right_x, area.y, label, style);
            buf.set_string(right_x.saturating_sub(1), area.y, "▐", cap);
            buf.set_string(right_x + label.len() as u16, area.y, "▌", cap);
        }

        if self.pads.active {
            let label = " PADS ";
            let style = Style::default()
                .fg(Color::Black)
                .bg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD);
            let cap = Style::default().fg(BORDER_ACTIVE);
            right_x = right_x.saturating_sub(label.len() as u16 + 1);
            buf.set_string(right_x, area.y, label, style);
            buf.set_string(right_x.saturating_sub(1), area.y, "▐", cap);
            buf.set_string(right_x + label.len() as u16, area.y, "▌", cap);
        }

        if self.state.solo_active {
            let label = " SOLO ";
            let style = Style::default()
                .fg(Color::Black)
                .bg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD);
            let cap = Style::default().fg(BORDER_ACTIVE);
            right_x = right_x.saturating_sub(label.len() as u16 + 1);
            buf.set_string(right_x, area.y, label, style);
            buf.set_string(right_x.saturating_sub(1), area.y, "▐", cap);
            buf.set_string(right_x + label.len() as u16, area.y, "▌", cap);
        }

        if self.state.mute_active() {
            let label = " MUTE ";
            let style = Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD);
            let cap = Style::default().fg(Color::Red);
            right_x = right_x.saturating_sub(label.len() as u16 + 1);
            buf.set_string(right_x, area.y, label, style);
            buf.set_string(right_x.saturating_sub(1), area.y, "▐", cap);
            buf.set_string(right_x + label.len() as u16, area.y, "▌", cap);
        }
    }

    fn render_help_overlay(&self, area: Rect, buf: &mut Buffer) {
        let help_width = 52u16.min(area.width.saturating_sub(4));
        let help_height = 31u16.min(area.height.saturating_sub(4));
        let help_x = area.x + (area.width.saturating_sub(help_width)) / 2;
        let help_y = area.y + (area.height.saturating_sub(help_height)) / 2;
        let help_area = Rect::new(help_x, help_y, help_width, help_height);

        // Clear background
        Clear.render(help_area, buf);
        for y in help_area.y..help_area.y + help_area.height {
            for x in help_area.x..help_area.x + help_area.width {
                buf.set_string(x, y, " ", Style::default().bg(BG_POPUP));
            }
        }

        let help_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_ACTIVE))
            .title(Span::styled(
                " HELP ",
                Style::default()
                    .fg(BORDER_ACTIVE)
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = help_block.inner(help_area);
        help_block.render(help_area, buf);

        let dim = Style::default().fg(TEXT_DIM);
        let key_style = Style::default().fg(TEXT_BRIGHT);
        let sep_style = Style::default().fg(SEPARATOR);
        let sep = Line::from(Span::styled(
            "──────────────────────────────────────────────────",
            sep_style,
        ));
        let help_text = vec![
            Line::from(Span::styled(
                "NAVIGATION",
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            )),
            sep.clone(),
            Line::from(vec![
                Span::styled("   Tab", key_style),
                Span::styled("      Switch panes (Deck A/Pads/Deck B/Master)", dim),
            ]),
            Line::from(vec![
                Span::styled("   Enter", key_style),
                Span::styled("    Select pane / Enter control mode", dim),
            ]),
            Line::from(vec![
                Span::styled("   Esc", key_style),
                Span::styled("      Go back one level", dim),
            ]),
            Line::from(vec![
                Span::styled("   h/j/k/l", key_style),
                Span::styled("  Navigate controls / adjust values", dim),
            ]),
            sep.clone(),
            Line::from(Span::styled(
                "EDITING",
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            )),
            sep.clone(),
            Line::from(vec![
                Span::styled("   Enter", key_style),
                Span::styled("    Edit control / Toggle button", dim),
            ]),
            Line::from(vec![
                Span::styled("   0", key_style),
                Span::styled("        Reset to default", dim),
            ]),
            Line::from(vec![
                Span::styled("   Master", key_style),
                Span::styled("   MIC gain/mute/input and output controls", dim),
            ]),
            Line::from(vec![
                Span::styled("   c", key_style),
                Span::styled("        Center (pan/crossfader)", dim),
            ]),
            sep.clone(),
            Line::from(Span::styled(
                "SEQUENCES",
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            )),
            sep.clone(),
            Line::from(vec![
                Span::styled("   Enter", key_style),
                Span::styled("  Enter edit mode / Toggle step", dim),
            ]),
            Line::from(vec![
                Span::styled("   h/l", key_style),
                Span::styled("    Move left/right in edit mode", dim),
            ]),
            Line::from(vec![
                Span::styled("   j/k", key_style),
                Span::styled("    Move between sequences", dim),
            ]),
            Line::from(vec![
                Span::styled("   Esc", key_style),
                Span::styled("     Exit edit mode / pane", dim),
            ]),
            Line::from(vec![
                Span::styled("   x", key_style),
                Span::styled("        Delete selected loop", dim),
            ]),
            sep.clone(),
            Line::from(Span::styled(
                "QUICK KEYS",
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            )),
            sep.clone(),
            Line::from(vec![
                Span::styled("   A/B/C", key_style),
                Span::styled("    Open source picker for deck A/B/C", dim),
            ]),
            Line::from(vec![
                Span::styled("   P", key_style),
                Span::styled("        Toggle pad trigger mode", dim),
            ]),
            Line::from(vec![
                Span::styled("   m/s", key_style),
                Span::styled("      Mute / Solo", dim),
            ]),
            Line::from(vec![
                Span::styled("   X", key_style),
                Span::styled("        Clear focused deck (confirms)", dim),
            ]),
            Line::from(vec![
                Span::styled("   R", key_style),
                Span::styled("        Reset deck / all controls (confirms)", dim),
            ]),
            Line::from(vec![
                Span::styled("   q/?", key_style),
                Span::styled("      Quit / Toggle help", dim),
            ]),
            sep.clone(),
        ];

        let paragraph = Paragraph::new(help_text)
            .wrap(Wrap { trim: true })
            .scroll((0, self.help_scroll as u16));
        paragraph.render(inner, buf);
    }

    fn render_source_picker(
        &self,
        area: Rect,
        buf: &mut Buffer,
        deck: Deck,
        picker: &SourcePickerState,
    ) {
        // Centered popup
        let popup_width = 60u16.min(area.width.saturating_sub(4));
        let popup_height = 20u16.min(area.height.saturating_sub(4));
        let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear background
        Clear.render(popup_area, buf);
        for y in popup_area.y..popup_area.y + popup_area.height {
            for x in popup_area.x..popup_area.x + popup_area.width {
                buf.set_string(x, y, " ", Style::default().bg(BG_POPUP));
            }
        }

        let title = match deck {
            Deck::A => " SOURCE [Deck A] ",
            Deck::B => " SOURCE [Deck B] ",
            Deck::C => " SOURCE [Deck C] ",
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_ACTIVE))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(BORDER_ACTIVE)
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        // If in add-to-queue sub-view, render that instead
        if picker.adding_to_queue {
            self.render_add_source_view(inner, buf, picker);
            return;
        }

        // Layout: tabs, content area, hint
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Tabs
                Constraint::Min(5),    // Content area
                Constraint::Length(1), // Hint
            ])
            .split(inner);

        // Tabs — Queue, SuperCollider, DeckActions
        let viewport_width = chunks[0].width as usize;
        let tab_widths: Vec<(SourcePickerTab, usize)> = vec![
            (SourcePickerTab::Queue, 10),         // " Queue "
            (SourcePickerTab::SuperCollider, 16), // " SuperCollider "
            (SourcePickerTab::DeckActions, 15),   // " Deck Actions "
        ];
        let mut tab_x = 0;
        let mut active_x = 0;
        let mut active_width = 10;
        for (tab, width) in &tab_widths {
            if *tab == picker.tab {
                active_x = tab_x;
                active_width = *width;
            }
            tab_x += width;
        }
        let mut tab_scroll_offset = picker.tab_scroll_offset;
        if active_x < tab_scroll_offset {
            tab_scroll_offset = active_x;
        } else if active_x + active_width > tab_scroll_offset + viewport_width {
            tab_scroll_offset = active_x + active_width - viewport_width;
        }

        let all_tabs: Vec<(SourcePickerTab, &str, &str)> = vec![
            (SourcePickerTab::Queue, " Queue ", " Queue "),
            (
                SourcePickerTab::SuperCollider,
                " SuperCollider ",
                " SuperCollider ",
            ),
            (
                SourcePickerTab::DeckActions,
                " Deck Actions ",
                " Deck Actions ",
            ),
        ];

        let mut tab_line = Line::default();
        let mut char_offset = 0;
        for (tab, active_label, inactive_label) in &all_tabs {
            let label = if picker.tab == *tab {
                *active_label
            } else {
                *inactive_label
            };
            let is_active = picker.tab == *tab;
            let style = if is_active {
                let background = if picker.tab_focused {
                    BORDER_ACTIVE
                } else {
                    BG_TAB_UNFOCUSED
                };
                Style::default()
                    .fg(Color::Black)
                    .bg(background)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT_DEFAULT)
            };
            for ch in label.chars() {
                if char_offset >= tab_scroll_offset
                    && char_offset < tab_scroll_offset + viewport_width
                {
                    tab_line.spans.push(Span::styled(ch.to_string(), style));
                }
                char_offset += 1;
            }
        }
        buf.set_line(chunks[0].x, chunks[0].y, &tab_line, chunks[0].width);

        // Content area
        let content_area = chunks[1];

        match picker.tab {
            SourcePickerTab::Queue => {
                let deck_idx = deck.index();
                let current_queue_idx = self.deck_queue_indices[deck_idx];
                self.render_queue_list(content_area, buf, picker, current_queue_idx);
            }
            SourcePickerTab::SuperCollider | SourcePickerTab::DeckActions => {
                self.render_sc_or_actions_list(content_area, buf, picker);
            }
            SourcePickerTab::AudioFiles => {
                // Should not happen in source picker mode
            }
        }

        // Hint line
        let hint = match picker.tab {
            SourcePickerTab::Queue => {
                if picker.queue_selected.is_some() {
                    "j/k:navigate  J/K or Shift+Up/Down:move  x/d:delete  Esc:quit"
                } else {
                    "j/k:navigate  a:add  Enter:add  Esc:quit"
                }
            }
            _ => "j/k:navigate  h/l:tabs  Enter:select  Esc:quit",
        };
        buf.set_string(
            chunks[2].x,
            chunks[2].y,
            hint,
            Style::default().fg(TEXT_GHOST),
        );
    }

    /// Render the queue list with items and "+" at the bottom
    fn render_queue_list(
        &self,
        area: Rect,
        buf: &mut Buffer,
        picker: &SourcePickerState,
        current_queue_idx: usize,
    ) {
        let visible_items = area.height as usize;
        let scroll = picker.queue_scroll_offset;

        // Queue items
        for (i, item) in picker
            .queue
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible_items)
        {
            let y = area.y + i as u16 - scroll as u16;
            if y >= area.y + area.height {
                break;
            }

            let is_selected = picker.queue_selected == Some(i);
            let is_playing = i == current_queue_idx;
            let style = if is_selected {
                Style::default().fg(Color::Black).bg(BORDER_ACTIVE)
            } else {
                Style::default().fg(TEXT_BRIGHT)
            };
            if is_selected {
                buf.set_style(Rect::new(area.x, y, area.width, 1), style);
            }

            let icon = if is_playing {
                " ▶"
            } else if item.is_socket || item.is_pcm_fifo {
                " "
            } else if item.is_udp {
                "◉ "
            } else {
                "\u{f001} "
            };
            let line = format!("{} {}", icon, item.name);

            let usable_width = area.width as usize;
            let truncated_name = if line.len() > usable_width {
                let truncated: String = line.chars().take(usable_width.saturating_sub(1)).collect();
                format!("{}…", truncated)
            } else {
                line
            };

            buf.set_string(area.x, y, &truncated_name, style);
            if is_playing {
                let icon_style = if is_selected {
                    Style::default().fg(ratatui::style::Color::Black).bg(BORDER_ACTIVE)
                } else {
                    Style::default().fg(STATUS_PLAYING)
                };
                buf.set_string(area.x, y, " ▶", icon_style);
            }
        }

        // "+" item at the bottom (if it's within the visible range)
        let plus_idx = picker.queue.len();
        let plus_screen = plus_idx.saturating_sub(scroll);
        if plus_screen < visible_items {
            let y = area.y + plus_screen as u16;
            if y < area.y + area.height {
                let is_selected = picker.queue_selected.is_none() && !picker.tab_focused;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(BORDER_ACTIVE)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(TEXT_DIM)
                };
                let width = area.width as usize;
                let pad = width.saturating_sub(1) / 2;
                let plus_text = format!("{}+{}", " ".repeat(pad), " ".repeat(width - pad - 1));
                buf.set_string(area.x, y, &plus_text, style);
            }
        }
    }

    /// Render SuperCollider or DeckActions list (same as before)
    fn render_sc_or_actions_list(&self, area: Rect, buf: &mut Buffer, picker: &SourcePickerState) {
        let visible_items = area.height as usize;

        for (i, &item_idx) in picker
            .filtered
            .iter()
            .enumerate()
            .skip(picker.scroll_offset)
            .take(visible_items)
        {
            if let Some(item) = picker.items.get(item_idx) {
                let y = area.y + i as u16 - picker.scroll_offset as u16;
                if y >= area.y + area.height {
                    break;
                }

                let is_selected = i == picker.selected && picker.selected < picker.filtered.len();
                let style = if is_selected {
                    Style::default().fg(Color::Black).bg(BORDER_ACTIVE)
                } else if item.is_dir {
                    Style::default().fg(DECK_A)
                } else {
                    Style::default().fg(TEXT_BRIGHT)
                };
                if is_selected {
                    buf.set_style(Rect::new(area.x, y, area.width, 1), style);
                }

                let icon = if item.is_dir {
                    "\u{f07b}"
                } else if item.is_socket || item.is_pcm_fifo {
                    ""
                } else if item.is_udp {
                    "◉ "
                } else if picker.tab == SourcePickerTab::DeckActions {
                    ""
                } else {
                    "\u{f001} "
                };
                let line = format!("{} {}", icon, item.name);

                let usable_width = area.width as usize;
                let truncated_name = if line.len() > usable_width {
                    let truncated: String =
                        line.chars().take(usable_width.saturating_sub(1)).collect();
                    format!("{}…", truncated)
                } else {
                    line
                };

                buf.set_string(area.x, y, &truncated_name, style);
            }
        }
    }

    /// Render the add-to-queue sub-view (MPV Sockets and Audio Files tabs)
    fn render_add_source_view(&self, area: Rect, buf: &mut Buffer, picker: &SourcePickerState) {
        // Layout: tabs, search, list, hint
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Tabs
                Constraint::Length(1), // Search input
                Constraint::Min(5),    // File list
                Constraint::Length(1), // Hint
            ])
            .split(area);

        // Tabs — MPV Sockets, Audio Files
        let viewport_width = chunks[0].width as usize;
        let tab_widths: Vec<(AddSourceTab, usize)> = vec![
            (AddSourceTab::MpvSockets, 14), // " MPV Sockets "
            (AddSourceTab::AudioFiles, 14), // " Audio Files "
        ];
        let mut tab_x = 0;
        let mut active_x = 0;
        for (tab, width) in &tab_widths {
            if *tab == picker.add_tab {
                active_x = tab_x;
            }
            tab_x += width;
        }

        let all_tabs: Vec<(AddSourceTab, &str, &str)> = vec![
            (AddSourceTab::MpvSockets, " MPV Sockets ", " MPV Sockets "),
            (AddSourceTab::AudioFiles, " Audio Files ", " Audio Files "),
        ];

        let mut tab_line = Line::default();
        let mut char_offset = 0;
        for (tab, active_label, inactive_label) in &all_tabs {
            let label = if picker.add_tab == *tab {
                *active_label
            } else {
                *inactive_label
            };
            let is_active = picker.add_tab == *tab;
            let style = if is_active {
                let background = if picker.tab_focused {
                    BORDER_ACTIVE
                } else {
                    BG_TAB_UNFOCUSED
                };
                Style::default()
                    .fg(Color::Black)
                    .bg(background)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT_DEFAULT)
            };
            for ch in label.chars() {
                if char_offset >= active_x.saturating_sub(active_x) && char_offset < viewport_width
                {
                    tab_line.spans.push(Span::styled(ch.to_string(), style));
                }
                char_offset += 1;
            }
        }
        buf.set_line(chunks[0].x, chunks[0].y, &tab_line, chunks[0].width);

        // Search input with mode indicator
        let mode_label = match picker.add_input_mode {
            PickerInputMode::Normal => {
                Span::styled(" NOR ", Style::default().fg(Color::Black).bg(TEXT_DEFAULT))
            }
            PickerInputMode::Insert => Span::styled(
                " INS ",
                Style::default().fg(Color::Black).bg(STATUS_PLAYING),
            ),
        };
        let search_style = Style::default().fg(TEXT_BRIGHT);
        let mut search_spans = vec![mode_label];
        match picker.add_input_mode {
            PickerInputMode::Normal if !picker.add_query.is_empty() => {
                search_spans.push(Span::styled(format!("  {}", picker.add_query), search_style));
            }
            PickerInputMode::Insert => {
                search_spans.push(Span::styled(format!("  > {}", picker.add_query), search_style));
                search_spans.push(Span::styled(
                    if (self.elapsed_ms / 500).is_multiple_of(2) { "_" } else { " " },
                    search_style,
                ));
            }
            _ => {}
        }
        let search_line = Line::from(search_spans);
        buf.set_line(chunks[1].x, chunks[1].y, &search_line, chunks[1].width);

        // File list
        let list_area = chunks[2];
        let visible_items = list_area.height as usize;

        for (i, &item_idx) in picker
            .add_filtered
            .iter()
            .enumerate()
            .skip(picker.add_scroll_offset)
            .take(visible_items)
        {
            if let Some(item) = picker.add_items.get(item_idx) {
                let y = list_area.y + i as u16 - picker.add_scroll_offset as u16;
                if y >= list_area.y + list_area.height {
                    break;
                }

                let is_cursor = i == picker.add_selected
                    && picker.add_selected < picker.add_filtered.len()
                    && !picker.tab_focused;
                let is_marked = picker.add_marked.contains(&item.path);
                let style = if is_cursor || is_marked {
                    Style::default().fg(Color::Black).bg(BORDER_ACTIVE)
                } else if item.is_dir {
                    Style::default().fg(DECK_A)
                } else {
                    Style::default().fg(TEXT_BRIGHT)
                };

                let icon = if item.is_dir {
                    "\u{f07b}"
                } else if item.is_socket || item.is_pcm_fifo {
                    ""
                } else if item.is_udp {
                    "◉ "
                } else {
                    "\u{f001} "
                };
                let line = format!("{} {}", icon, item.name);

                let usable_width = list_area.width as usize;
                let truncated_name = if line.len() > usable_width {
                    let truncated: String =
                        line.chars().take(usable_width.saturating_sub(1)).collect();
                    format!("{}…", truncated)
                } else {
                    format!("{:width$}", line, width = usable_width)
                };

                buf.set_stringn(
                    list_area.x,
                    y,
                    &truncated_name,
                    list_area.width as usize,
                    style,
                );
            }
        }

        // Hint line
        let hint = match picker.add_input_mode {
            PickerInputMode::Normal => {
                "i:insert  j/k:nav  Shift+j/k:select  Enter:add  Esc:back"
            }
            PickerInputMode::Insert => "Esc:normal  Tab:switch  Enter:add  Type to filter",
        };
        buf.set_string(
            chunks[3].x,
            chunks[3].y,
            hint,
            Style::default().fg(TEXT_GHOST),
        );
    }

    fn render_sample_picker_inline(
        &self,
        area: Rect,
        buf: &mut Buffer,
        pad_idx: usize,
        picker: &SourcePickerState,
    ) {
        let key = PAD_KEYS[pad_idx / 4][pad_idx % 4].to_ascii_uppercase();
        let title = format!(" SAMPLE [{}] ", key);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_ACTIVE))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(BORDER_ACTIVE)
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(area);
        block.render(area, buf);

        self.render_sample_picker_content(inner, buf, pad_idx, picker);
    }

    fn render_sample_picker_content(
        &self,
        area: Rect,
        buf: &mut Buffer,
        _pad_idx: usize,
        picker: &SourcePickerState,
    ) {
        // Layout: path, search, list, hint
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Current path
                Constraint::Length(1), // Search input
                Constraint::Min(5),    // File list
                Constraint::Length(1), // Hint
            ])
            .split(area);

        // Current path
        let rel_path = picker.relative_path();
        let path_display = if rel_path.is_empty() {
            "/".to_string()
        } else {
            format!("/{}/", rel_path)
        };
        let path_truncated = if path_display.len() > chunks[0].width as usize {
            format!(
                "…{}",
                &path_display[path_display.len() - chunks[0].width as usize + 1..]
            )
        } else {
            path_display
        };
        buf.set_string(
            chunks[0].x,
            chunks[0].y,
            &path_truncated,
            Style::default().fg(TEXT_DEFAULT),
        );

        // Search input with mode indicator
        let mode_label = match picker.input_mode {
            PickerInputMode::Normal => {
                Span::styled(" NOR ", Style::default().fg(Color::Black).bg(TEXT_DEFAULT))
            }
            PickerInputMode::Insert => Span::styled(
                " INS ",
                Style::default().fg(Color::Black).bg(STATUS_PLAYING),
            ),
        };
        let search_style = Style::default().fg(TEXT_BRIGHT);
        let mut search_spans = vec![mode_label];
        match picker.input_mode {
            PickerInputMode::Normal if !picker.query.is_empty() => {
                search_spans.push(Span::styled(format!("  {}", picker.query), search_style));
            }
            PickerInputMode::Insert => {
                search_spans.push(Span::styled(format!("  > {}", picker.query), search_style));
                search_spans.push(Span::styled(
                    if (self.elapsed_ms / 500).is_multiple_of(2) { "_" } else { " " },
                    search_style,
                ));
            }
            _ => {}
        }
        let search_line = Line::from(search_spans);
        buf.set_line(chunks[1].x, chunks[1].y, &search_line, chunks[1].width);

        // File list
        let list_area = chunks[2];
        let visible_items = list_area.height as usize;

        for (i, &item_idx) in picker
            .filtered
            .iter()
            .enumerate()
            .skip(picker.scroll_offset)
            .take(visible_items)
        {
            if let Some(item) = picker.items.get(item_idx) {
                let y = list_area.y + i as u16 - picker.scroll_offset as u16;
                if y >= list_area.y + list_area.height {
                    break;
                }

                let is_selected = i == picker.selected;
                let style = if is_selected {
                    Style::default().fg(Color::Black).bg(BORDER_ACTIVE)
                } else if item.is_dir {
                    Style::default().fg(DECK_A)
                } else {
                    Style::default().fg(TEXT_BRIGHT)
                };

                // Nerd Font icons: folder or audio file
                let icon = if item.is_dir { "\u{f07b}" } else { "\u{f001}" };
                let line = format!("{} {}", icon, item.name);
                let display = if line.len() > list_area.width as usize {
                    let truncated: String = line
                        .chars()
                        .take(list_area.width.saturating_sub(1) as usize)
                        .collect();
                    format!("{}…", truncated)
                } else {
                    format!("{:width$}", line, width = list_area.width as usize)
                };

                buf.set_stringn(list_area.x, y, &display, list_area.width as usize, style);
            }
        }

        // Hint line
        let hint = match picker.input_mode {
            PickerInputMode::Normal => "i:insert  j/k:nav  h/l:dirs  Space:preview  Enter:select",
            PickerInputMode::Insert => "Esc:normal  Enter:select  Backspace:up dir  Type to filter",
        };
        buf.set_string(
            chunks[3].x,
            chunks[3].y,
            hint,
            Style::default().fg(TEXT_GHOST),
        );
    }

    /// Render output device picker overlay
    fn render_output_picker(&self, area: Rect, buf: &mut Buffer) {
        let (title, devices, selected_idx) = match self.output_picker_target {
            OutputPickerTarget::Master => (
                " OUTPUT [Master] ",
                self.master_output_devices,
                self.selected_master_output_idx,
            ),
            OutputPickerTarget::Cue => (
                " OUTPUT [CUE] ",
                self.cue_output_devices,
                self.selected_cue_output_idx,
            ),
            OutputPickerTarget::Mic => (
                " INPUT [MIC] ",
                self.mic_input_devices,
                self.selected_mic_input_idx,
            ),
        };

        // Centered popup
        let popup_width = 50u16.min(area.width.saturating_sub(4));
        let popup_height = (devices.len() as u16 + 6).min(area.height.saturating_sub(4));
        let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear background
        Clear.render(popup_area, buf);
        for y in popup_area.y..popup_area.y + popup_area.height {
            for x in popup_area.x..popup_area.x + popup_area.width {
                buf.set_string(x, y, " ", Style::default().bg(BG_POPUP));
            }
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_ACTIVE))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(BORDER_ACTIVE)
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        // Layout: device list, hint
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),    // Device list
                Constraint::Length(1), // Hint
            ])
            .split(inner);

        // Device list
        let list_area = chunks[0];
        for (i, device) in devices.iter().enumerate() {
            if i as u16 >= list_area.height {
                break;
            }

            let is_selected = i == selected_idx;
            let style = if is_selected {
                Style::default().fg(Color::Black).bg(BORDER_ACTIVE)
            } else {
                Style::default().fg(TEXT_BRIGHT)
            };

            let display = if device.len() > list_area.width as usize {
                let truncated: String = device
                    .chars()
                    .take(list_area.width.saturating_sub(1) as usize)
                    .collect();
                format!("{}…", truncated)
            } else {
                format!("{:width$}", device, width = list_area.width as usize)
            };

            buf.set_string(list_area.x, list_area.y + i as u16, &display, style);
        }

        // Hint line
        let hint = "j/k:nav  Enter:select  Esc:cancel";
        buf.set_string(
            chunks[1].x,
            chunks[1].y,
            hint,
            Style::default().fg(TEXT_GHOST),
        );
    }

    /// Render debug log at the bottom of the screen
    fn render_debug_log(&self, area: Rect, buf: &mut Buffer, log: &[String]) {
        let total_lines = log.len();
        let inner_height = area.height.saturating_sub(2) as usize; // subtract borders

        let title = if self.debug_scroll > 0 {
            format!(" DEBUG [scrolled {}] ", self.debug_scroll)
        } else {
            " DEBUG ".to_string()
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(BORDER_DEFAULT));

        let inner = block.inner(area);
        block.render(area, buf);

        // Reserve rightmost column for scrollbar when content overflows
        let has_scrollbar = total_lines > inner_height;
        let text_area = if has_scrollbar && inner.width > 2 {
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width.saturating_sub(1),
                height: inner.height,
            }
        } else {
            inner
        };

        let lines: Vec<Line> = log
            .iter()
            .map(|msg| {
                Line::from(Span::styled(
                    msg.as_str(),
                    Style::default().fg(TEXT_DEFAULT),
                ))
            })
            .collect();

        let scroll_offset = if self.debug_scroll > 0 {
            // Absolute position: debug_scroll IS the top-line index (frozen)
            self.debug_scroll.min(total_lines.saturating_sub(1))
        } else {
            total_lines.saturating_sub(inner_height)
        };

        let paragraph = Paragraph::new(lines).scroll((scroll_offset as u16, 0));
        paragraph.render(text_area, buf);

        // Render scrollbar on the right edge
        if has_scrollbar {
            let scrollbar_area = Rect {
                x: inner.x + inner.width.saturating_sub(1),
                y: inner.y,
                width: 1,
                height: inner.height,
            };
            let mut state = ScrollbarState::new(total_lines).position(scroll_offset);
            Scrollbar::new(ScrollbarOrientation::VerticalRight).render(
                scrollbar_area,
                buf,
                &mut state,
            );
        }
    }

    fn render_confirm_dialog(&self, area: Rect, buf: &mut Buffer, action: ConfirmAction) {
        let (title, message) = match action {
            ConfirmAction::Quit => (" QUIT ".into(), "Quit termixer?".into()),
            ConfirmAction::ClearDeck(deck) => {
                let label = match deck {
                    Deck::A => "Deck A",
                    Deck::B => "Deck B",
                    Deck::C => "Deck C",
                };
                (
                    format!(" CLEAR PLAYLIST [{}] ", label),
                    format!("Clear {} playlist?", label),
                )
            }
            ConfirmAction::ResetDeck(deck) => {
                let label = match deck {
                    Deck::A => "Deck A",
                    Deck::B => "Deck B",
                    Deck::C => "Deck C",
                };
                (
                    format!(" RESET [{}] ", label),
                    format!("Reset {} controls?", label),
                )
            }
            ConfirmAction::ResetAll => (" RESET [GLOBAL] ".into(), "Reset all controls?".into()),
        };

        let popup_width = 46u16.min(area.width.saturating_sub(4));
        let popup_height = 5u16.min(area.height.saturating_sub(4));
        let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        Clear.render(popup_area, buf);
        for y in popup_area.y..popup_area.y + popup_area.height {
            for x in popup_area.x..popup_area.x + popup_area.width {
                buf.set_string(x, y, " ", Style::default().bg(BG_POPUP));
            }
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_ACTIVE))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(BORDER_ACTIVE)
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // spacer
                Constraint::Length(1), // message
                Constraint::Length(1), // spacer
                Constraint::Length(1), // hint
            ])
            .split(inner);

        let msg = Paragraph::new(
            Line::from(Span::styled(message, Style::default().fg(TEXT_DEFAULT))).centered(),
        );
        msg.render(chunks[1], buf);

        let y_style = if !self.confirm_selected {
            Style::default()
                .fg(TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_DIM)
        };
        let n_style = if self.confirm_selected {
            Style::default()
                .fg(TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_DIM)
        };

        let hint = Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled("Y", y_style),
            Span::styled("/", Style::default().fg(TEXT_DIM)),
            Span::styled("n", n_style),
        ]));
        hint.render(chunks[3], buf);
    }

    fn render_session_picker(&self, area: Rect, buf: &mut Buffer, picker: &SessionPickerState) {
        // Centered popup
        let popup_width = 60u16.min(area.width.saturating_sub(4));
        let popup_height = 22u16.min(area.height.saturating_sub(4));
        let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

        // Clear background
        Clear.render(popup_area, buf);
        for y in popup_area.y..popup_area.y + popup_area.height {
            for x in popup_area.x..popup_area.x + popup_area.width {
                buf.set_string(x, y, " ", Style::default().bg(BG_POPUP));
            }
        }

        let title = if picker.is_save {
            " SAVE "
        } else {
            " LOAD "
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_ACTIVE))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(BORDER_ACTIVE)
                    .add_modifier(Modifier::BOLD),
            ));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        // Overwrite confirmation overlay
        if let Some(ref path) = picker.overwrite_confirm {
            let filename = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string());
            let msg = format!("Overwrite {}?", filename);

            let confirm_width = (msg.len() as u16 + 8).min(popup_width.saturating_sub(4));
            let confirm_height = 5u16;
            let confirm_x = inner.x + (inner.width.saturating_sub(confirm_width)) / 2;
            let confirm_y = inner.y + (inner.height.saturating_sub(confirm_height)) / 2;
            let confirm_area = Rect::new(confirm_x, confirm_y, confirm_width, confirm_height);

            Clear.render(confirm_area, buf);
            for y in confirm_area.y..confirm_area.y + confirm_area.height {
                for x in confirm_area.x..confirm_area.x + confirm_area.width {
                    buf.set_string(x, y, " ", Style::default().bg(BG_POPUP));
                }
            }

            let confirm_block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(TEXT_EDITING))
                .title(Span::styled(
                    " OVERWRITE ",
                    Style::default()
                        .fg(TEXT_EDITING)
                        .add_modifier(Modifier::BOLD),
                ));

            let confirm_inner = confirm_block.inner(confirm_area);
            confirm_block.render(confirm_area, buf);

            let confirm_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(confirm_inner);

            buf.set_string(
                confirm_chunks[0].x,
                confirm_chunks[0].y,
                &msg,
                Style::default().fg(TEXT_BRIGHT),
            );

            let hint = Line::from(vec![
                Span::styled("Y", Style::default().fg(TEXT_BRIGHT).add_modifier(Modifier::BOLD)),
                Span::styled(":yes  ", Style::default().fg(TEXT_DIM)),
                Span::styled("n", Style::default().fg(TEXT_BRIGHT).add_modifier(Modifier::BOLD)),
                Span::styled(":no", Style::default().fg(TEXT_DIM)),
            ]);
            let hint_para = Paragraph::new(hint);
            hint_para.render(confirm_chunks[2], buf);
            return;
        }

        // Layout: save name (save mode) / search, file list, hint
        let constraints = if picker.is_save {
            vec![
                Constraint::Length(4), // Input field + helper row
                Constraint::Length(1), // Search input
                Constraint::Min(5),    // File list
                Constraint::Length(1), // Hint
            ]
        } else {
            vec![
                Constraint::Length(1), // Search input
                Constraint::Min(5),    // File list
                Constraint::Length(1), // Hint
            ]
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);

        // --- Save name row (sticky at top, save mode only) ---
        if picker.is_save {
            let save_area = chunks[0];
            let is_editing = picker.selected == 0 && picker.editing_save_name;
            let is_focused = picker.selected == 0;

            // Input field with border, 1 cell padding on each side
            let field_x = save_area.x + 1;
            let field_y = save_area.y;
            let field_w = save_area.width.saturating_sub(2);
            let border_color = if is_focused { BORDER_ACTIVE } else { TEXT_DIM };

            // Top border: ┌─...─┐
            let mut top = String::from("┌");
            top.push_str(&"─".repeat((field_w as usize).saturating_sub(2)));
            top.push('┐');
            buf.set_string(
                field_x,
                field_y,
                &top,
                Style::default().fg(border_color),
            );

            // Middle: │ <value> │
            let inner_w = (field_w as usize).saturating_sub(2);
            let value_w = inner_w.saturating_sub(2);
            let truncated: String = picker.save_name.chars().take(value_w).collect();
            let pad_style = if is_focused {
                Style::default().fg(TEXT_EDITING).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT_DEFAULT)
            };
            let value_style = if is_editing {
                Style::default()
                    .fg(Color::Black)
                    .bg(TEXT_EDITING)
                    .add_modifier(Modifier::BOLD)
            } else if is_focused {
                Style::default()
                    .fg(TEXT_EDITING)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(TEXT_DEFAULT)
            };
            let base_x = field_x + 1;
            buf.set_string(field_x, field_y + 1, "│", Style::default().fg(border_color));
            buf.set_string(base_x, field_y + 1, " ", pad_style);
            buf.set_string(base_x + 1, field_y + 1, &truncated, value_style);
            let pad_r_x =
                base_x + 1 + unicode_width::UnicodeWidthStr::width(truncated.as_str()) as u16;
            if pad_r_x < field_x + field_w - 1 {
                buf.set_string(pad_r_x, field_y + 1, " ", pad_style);
            }
            if is_editing && (self.elapsed_ms / 500).is_multiple_of(2) {
                let cursor = picker.save_name_cursor.min(picker.save_name.len());
                if let Some(prefix) = picker.save_name.get(..cursor) {
                    let cursor_offset = unicode_width::UnicodeWidthStr::width(prefix);
                    if cursor_offset < value_w {
                        buf.set_style(
                            Rect::new(base_x + 1 + cursor_offset as u16, field_y + 1, 1, 1),
                            value_style.add_modifier(Modifier::UNDERLINED),
                        );
                    }
                }
            }
            buf.set_string(
                field_x + field_w - 1,
                field_y + 1,
                "│",
                Style::default().fg(border_color),
            );

            // Bottom border: └─...─┘
            let mut bot = String::from("└");
            bot.push_str(&"─".repeat((field_w as usize).saturating_sub(2)));
            bot.push('┘');
            buf.set_string(
                field_x,
                field_y + 2,
                &bot,
                Style::default().fg(border_color),
            );

            if let Some(ref msg) = picker.status_message {
                let message = msg.replace(".json", "\".json\"");
                let message_width = unicode_width::UnicodeWidthStr::width(message.as_str()) as u16;
                let message_x = (field_x + field_w).saturating_sub(message_width);
                buf.set_stringn(
                    message_x,
                    field_y + 3,
                    message,
                    message_width as usize,
                    Style::default().fg(STATUS_MUTED),
                );
            }
        }

        // --- Search input row ---
        let search_idx = if picker.is_save { 1 } else { 0 };
        let search_area = chunks[search_idx];
        let save_name_has_focus = picker.is_save && picker.selected == 0;

        let mode_label = match picker.input_mode {
            PickerInputMode::Normal => {
                Span::styled(" NOR ", Style::default().fg(Color::Black).bg(TEXT_DEFAULT))
            }
            PickerInputMode::Insert => Span::styled(
                " INS ",
                Style::default().fg(Color::Black).bg(STATUS_PLAYING),
            ),
        };

        let search_style = if save_name_has_focus
            && picker.input_mode == PickerInputMode::Normal
        {
            Style::default().fg(TEXT_DIM)
        } else {
            Style::default().fg(TEXT_BRIGHT)
        };

        let mut search_spans = vec![mode_label];
        match picker.input_mode {
            PickerInputMode::Normal if !picker.query.is_empty() => {
                search_spans.push(Span::styled(format!("  {}", picker.query), search_style));
            }
            PickerInputMode::Insert => {
                search_spans.push(Span::styled(format!("  > {}", picker.query), search_style));
                search_spans.push(Span::styled(
                    if (self.elapsed_ms / 500).is_multiple_of(2) { "_" } else { " " },
                    search_style,
                ));
            }
            _ => {}
        }
        let search_line = Line::from(search_spans);
        buf.set_line(search_area.x, search_area.y, &search_line, search_area.width);

        // --- File list ---
        let list_idx = if picker.is_save { 2 } else { 1 };
        let list_area = chunks[list_idx];
        let visible_items = list_area.height as usize;

        for (i, &item_idx) in picker
            .filtered
            .iter()
            .enumerate()
            .skip(picker.scroll_offset)
            .take(visible_items)
        {
            if let Some(item) = picker.items.get(item_idx) {
                let y = list_area.y + i as u16 - picker.scroll_offset as u16;
                if y >= list_area.y + list_area.height {
                    break;
                }

                // In save mode, selected=0 is save name, selected=1.. maps to filtered indices
                let is_selected = if picker.is_save {
                    picker.selected > 0 && i + 1 == picker.selected
                } else {
                    i == picker.selected && picker.selected < picker.filtered.len()
                };
                let style = if is_selected {
                    Style::default().fg(Color::Black).bg(BORDER_ACTIVE)
                } else if item.is_dir {
                    Style::default().fg(DECK_A)
                } else {
                    Style::default().fg(TEXT_BRIGHT)
                };

                let icon = if item.is_dir { "\u{f07b} " } else { "\u{e60b} " };
                let line = format!("{} {}", icon, item.name);

                let usable_width = list_area.width as usize;
                let truncated_name = if line.len() > usable_width {
                    let truncated: String =
                        line.chars().take(usable_width.saturating_sub(1)).collect();
                    format!("{}…", truncated)
                } else {
                    format!("{:width$}", line, width = usable_width)
                };

                buf.set_stringn(
                    list_area.x,
                    y,
                    &truncated_name,
                    list_area.width as usize,
                    style,
                );
            }
        }

        // --- Hint line ---
        let hint_idx = if picker.is_save { 3 } else { 2 };
        let hint = if picker.is_save {
            if picker.selected == 0 {
                if picker.editing_save_name {
                    "Enter:save  Esc:back to nav  Backspace:delete"
                } else {
                    "j/k:nav  Enter:edit/save  Esc:cancel"
                }
            } else {
                match picker.input_mode {
                    PickerInputMode::Normal => {
                        "j/k:nav  h/l:dirs  i:filter  Enter:open  Esc:cancel"
                    }
                    PickerInputMode::Insert => "Esc:normal  j/k:nav  Enter:open  Type to filter",
                }
            }
        } else {
            match picker.input_mode {
                PickerInputMode::Normal => {
                    "j/k:nav  h/l:dirs  i:filter  Enter:load  Esc:cancel"
                }
                PickerInputMode::Insert => "Esc:normal  j/k:nav  Enter:load  Type to filter",
            }
        };
        buf.set_string(
            chunks[hint_idx].x,
            chunks[hint_idx].y,
            hint,
            Style::default().fg(TEXT_GHOST),
        );
    }
}
