//! Sample pad grid widget for TUI

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Widget},
};

use crate::state::{
    EditTarget, GlobalSequenceControl, GlobalSequenceControls, PadControl, SEQUENCE_STEPS,
    SamplePad, SamplePadGrid, Sequence,
};
use crate::ui::colors::*;

/// Configuration pane for a single pad — replaces the pad grid when [c] is pressed
pub struct PadConfigPane<'a> {
    grid: &'a SamplePadGrid,
    editing: bool,
    samples_dir: Option<&'a std::path::Path>,
    sequence_tempo: f32,
}

impl<'a> PadConfigPane<'a> {
    pub fn new(grid: &'a SamplePadGrid) -> Self {
        Self {
            grid,
            editing: false,
            samples_dir: None,
            sequence_tempo: 1.0,
        }
    }

    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    pub fn samples_dir(mut self, dir: Option<&'a std::path::Path>) -> Self {
        self.samples_dir = dir;
        self
    }

    pub fn sequence_tempo(mut self, tempo: f32) -> Self {
        self.sequence_tempo = tempo;
        self
    }
}

impl<'a> Widget for PadConfigPane<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let pad_idx = self.grid.selected_pad;
        let pad = &self.grid.pads[pad_idx];
        let key_char = pad.key_char().to_ascii_uppercase();
        let selected_ctrl = self.grid.selected_control;
        let is_editing = self.editing;

        // Title with pad name
        let title = format!(" CONFIG: [{}] {} ", key_char, pad.name);
        let title_style = Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(ratatui::text::Span::styled(title, title_style));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width < 20 || inner.height < 4 {
            buf.set_string(
                inner.x,
                inner.y,
                "Too small",
                Style::default().fg(Color::Red),
            );
            return;
        }

        let controls = PadControl::all();
        let row_height = 1u16;
        let mut y = inner.y;

        for &control in controls {
            // Add separator before Filters header
            if control == PadControl::FiltersHeader {
                let sep_style = Style::default().fg(SEPARATOR);
                for x in inner.x..inner.x + inner.width {
                    buf.set_string(x, y, "─", sep_style);
                }
                y += row_height;
            }

            if y + row_height > inner.y + inner.height {
                break;
            }

            let is_selected = control == selected_ctrl;
            let row_area = Rect::new(inner.x, y, inner.width, row_height);

            render_config_row(
                row_area,
                buf,
                control,
                pad,
                is_selected,
                is_editing,
                self.samples_dir,
                self.sequence_tempo,
            );

            // Add separator after Sample row
            if control == PadControl::Sample {
                y += row_height;
                if y + row_height <= inner.y + inner.height {
                    let sep_style = Style::default().fg(SEPARATOR);
                    for x in inner.x..inner.x + inner.width {
                        buf.set_string(x, y, "─", sep_style);
                    }
                    y += row_height;
                }
            } else {
                y += row_height;
            }
        }

        // Help hint at bottom
        if area.height > inner.height + 2 {
            let hint = if is_editing {
                "h/j/k/l:adjust │ 0:reset │ Esc:back"
            } else {
                "j/k:move │ Enter:edit/open │ SPACE:play │ c:close"
            };
            let hint_y = area.y + area.height - 1;
            buf.set_string(
                area.x + 1,
                hint_y,
                hint,
                Style::default().fg(Color::DarkGray),
            );
        }
    }
}

/// Render a single config control row
#[allow(clippy::too_many_arguments)]
fn render_config_row(
    area: Rect,
    buf: &mut Buffer,
    control: PadControl,
    pad: &SamplePad,
    selected: bool,
    editing: bool,
    samples_dir: Option<&std::path::Path>,
    sequence_tempo: f32,
) {
    if area.width < 10 {
        return;
    }

    let label_width = 10usize.min(area.width as usize / 3);
    let value_area_width = area.width as usize - label_width - 1;
    let bar_width = (value_area_width as f32 * 0.6) as usize;
    let value_width = value_area_width - bar_width - 1;

    // Label
    let label_style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if control == PadControl::FiltersHeader {
        Style::default().fg(TEXT_GHOST)
    } else {
        Style::default().fg(TEXT_DEFAULT)
    };
    let label = control.label();
    buf.set_string(area.x, area.y, label, label_style);

    let control_x = area.x + label_width as u16;

    match control {
        PadControl::Sample => {
            let has_sample = pad.sample_path.is_some();
            let sample_name = pad.sample_path.as_ref().map(|p| {
                if let Some(dir) = samples_dir {
                    p.strip_prefix(dir).unwrap_or(p).display().to_string()
                } else {
                    p.display().to_string()
                }
            });
            let name = if let Some(name) = sample_name {
                if name.len() > bar_width + value_width {
                    format!("{}…", &name[..bar_width + value_width - 1])
                } else {
                    name.to_string()
                }
            } else {
                "(click to set)".to_string()
            };
            let style = if selected && has_sample {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else if selected {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else if has_sample {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            buf.set_string(control_x, area.y, &name, style);
        }
        PadControl::Mute => {
            let (label, color) = if pad.config.mute {
                ("ON", Color::Red)
            } else {
                ("OFF", Color::DarkGray)
            };
            let style = if selected {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            };
            buf.set_string(control_x, area.y, label, style);
        }

        _ => {
            // Continuous controls: draw a bar + value
            let (value, display) = match control {
                PadControl::Volume => {
                    (pad.config.volume / 2.0, format!("{:.2}", pad.config.volume))
                }
                PadControl::BpmMultiplier => {
                    let norm = (sequence_tempo - 0.25) / 3.75; // 0.25..4.0 → 0..1
                    (norm, format!("{:.2}x", sequence_tempo))
                }
                PadControl::HighPass => {
                    let norm = (pad.config.high_pass - 20.0) / 19980.0;
                    (norm, format!("{} Hz", format_hz(pad.config.high_pass)))
                }
                PadControl::LowPass => {
                    let norm = (pad.config.low_pass - 20.0) / 19980.0;
                    (norm, format!("{} Hz", format_hz(pad.config.low_pass)))
                }
                PadControl::EqLow => (pad.config.eq_low / 2.0, format!("{:.1}", pad.config.eq_low)),
                PadControl::EqMid => (pad.config.eq_mid / 2.0, format!("{:.1}", pad.config.eq_mid)),
                PadControl::EqHigh => (
                    pad.config.eq_high / 2.0,
                    format!("{:.1}", pad.config.eq_high),
                ),
                PadControl::Reverb => (pad.config.reverb, format!("{:.2}", pad.config.reverb)),
                PadControl::Chorus => (pad.config.chorus, format!("{:.2}", pad.config.chorus)),
                PadControl::Distortion => (
                    pad.config.distortion,
                    format!("{:.2}", pad.config.distortion),
                ),
                _ => return,
            };

            // Bar
            let bar_x = control_x;
            let bar_chars = bar_width.max(2);
            let fill = (value * (bar_chars - 2) as f32) as usize;

            let bar_style = if selected && editing {
                Style::default().fg(Color::Yellow)
            } else if selected {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            buf.set_string(bar_x, area.y, "├", bar_style);
            for i in 0..bar_chars - 2 {
                let ch = if i < fill { "━" } else { "─" };
                buf.set_string(bar_x + 1 + i as u16, area.y, ch, bar_style);
            }
            buf.set_string(bar_x + bar_chars as u16 - 1, area.y, "┤", bar_style);

            // Value text
            let val_x = bar_x + bar_chars as u16 + 1;
            let val_style = if selected && editing {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if selected {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            if val_x + display.len() as u16 <= area.x + area.width {
                buf.set_string(val_x, area.y, &display, val_style);
            }
        }
    }
}

/// Top bar showing global sequence controls: volume, BPM, mute
pub struct SequenceTopBar<'a> {
    global: &'a GlobalSequenceControls,
    selected: bool,
    selected_control: GlobalSequenceControl,
    editing: bool,
    border_color: Color,
}

impl<'a> SequenceTopBar<'a> {
    pub fn new(global: &'a GlobalSequenceControls) -> Self {
        Self {
            global,
            selected: false,
            selected_control: GlobalSequenceControl::Volume,
            editing: false,
            border_color: Color::DarkGray,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn selected_control(mut self, control: GlobalSequenceControl) -> Self {
        self.selected_control = control;
        self
    }

    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    pub fn border_color(mut self, color: Color) -> Self {
        self.border_color = color;
        self
    }
}

impl<'a> Widget for SequenceTopBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height < 1 || area.width < 30 {
            return;
        }

        let y = area.y;
        let border_style = Style::default().fg(self.border_color);

        // Layout (right to left):  Play │ Load │ Save │ BPM │ Vol
        // Play: 3 cells " ▶ ", Load: 8 cells " 📂 LOAD ", Save: 8 cells " 💾 SAVE "
        // Separators are 1 cell:  "│"

        // --- Play/pause (rightmost, 3 cells) ---
        let pp_x = area.x + area.width - 3;
        let pp_is_target = self.selected && self.selected_control == GlobalSequenceControl::Mute;
        let pp_active = pp_is_target && self.editing;
        let (mute_icon, mute_fg) = if self.global.mute {
            (
                "\u{F03E4}",
                if pp_active || pp_is_target {
                    Color::Red
                } else {
                    Color::DarkGray
                },
            )
        } else {
            (
                "\u{25B6}",
                if pp_active {
                    TEXT_EDITING
                } else if pp_is_target {
                    TEXT_BRIGHT
                } else {
                    TEXT_DIM
                },
            )
        };
        let mute_style = if pp_is_target {
            Style::default().fg(mute_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(mute_fg)
        };
        buf.set_string(pp_x, y, " ", mute_style);
        buf.set_string(pp_x + 1, y, mute_icon, mute_style);
        buf.set_string(pp_x + 2, y, " ", mute_style);

        // --- Separator: │ Play ---
        let sep_pp = pp_x - 1;
        buf.set_string(sep_pp, y, "\u{2502}", border_style);

        // --- Load button (8 cells: " 📂 LOAD ") ---
        let load_x = sep_pp - 8;
        let load_is_target = self.selected && self.selected_control == GlobalSequenceControl::Load;
        let load_fg = if load_is_target {
            TEXT_BRIGHT
        } else {
            TEXT_DIM
        };
        let load_style = if load_is_target {
            Style::default().fg(load_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(load_fg)
        };
        buf.set_string(load_x, y, " ", load_style);
        buf.set_string(load_x + 1, y, "\u{EAF7}", load_style); // nf-cod-folder-opened
        buf.set_string(load_x + 2, y, " LOAD ", load_style);

        // --- Separator: Load │ Save ---
        let sep_ls = load_x - 1;
        buf.set_string(sep_ls, y, "\u{2502}", border_style);

        // --- Save button (8 cells: " 💾 SAVE ") ---
        let save_x = sep_ls - 8;
        let save_is_target = self.selected && self.selected_control == GlobalSequenceControl::Save;
        let save_fg = if save_is_target {
            TEXT_BRIGHT
        } else {
            TEXT_DIM
        };
        let save_style = if save_is_target {
            Style::default().fg(save_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(save_fg)
        };
        buf.set_string(save_x, y, " ", save_style);
        buf.set_string(save_x + 1, y, "\u{EB4B}", save_style); // nf-cod-save
        buf.set_string(save_x + 2, y, " SAVE ", save_style);

        // --- Separator: BPM │ Save ---
        let sep_save = save_x - 1;
        buf.set_string(sep_save, y, "\u{2502}", border_style);

        // --- BPM (padded cell) ---
        let bpm_str = format!("{:.0}", self.global.bpm);
        let bpm_is_target = self.selected && self.selected_control == GlobalSequenceControl::Bpm;
        let bpm_active = bpm_is_target && self.editing;
        let bpm_fg = if bpm_active {
            TEXT_EDITING
        } else if bpm_is_target {
            TEXT_BRIGHT
        } else {
            TEXT_DIM
        };
        let bpm_style = if bpm_is_target {
            Style::default().fg(bpm_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(bpm_fg)
        };
        let bpm_text_x = sep_save - bpm_str.len() as u16 - 1;
        buf.set_string(bpm_text_x - 1, y, " ", bpm_style);
        buf.set_string(bpm_text_x, y, &bpm_str, bpm_style);
        buf.set_string(bpm_text_x + bpm_str.len() as u16, y, " ", bpm_style);

        // --- Separator: Vol │ BPM ---
        let sep1_x = bpm_text_x - 2;
        buf.set_string(sep1_x, y, "\u{2502}", border_style);

        // --- Volume slider (fills remaining space) ---
        let slider_x = area.x + 1;
        let slider_end = sep1_x - 1;
        let slider_w = if slider_end > slider_x {
            slider_end - slider_x
        } else {
            5
        };
        let vol_is_target = self.selected && self.selected_control == GlobalSequenceControl::Volume;
        let vol_active = vol_is_target && self.editing;
        let filled = (self.global.volume * slider_w as f32) as usize;
        let vol_fg = if vol_active {
            TEXT_EDITING
        } else if vol_is_target {
            TEXT_BRIGHT
        } else {
            TEXT_DIM
        };
        let bar_style = if vol_is_target {
            Style::default().fg(vol_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(vol_fg)
        };
        for i in 0..slider_w as usize {
            let ch = if i < filled { "\u{2501}" } else { "\u{2500}" };
            buf.set_string(slider_x + i as u16, y, ch, bar_style);
        }

        // Connect all separators to the top border with ┬ (T-junction)
        // Note: this is done in the mixer render code instead, since the
        // top border is outside this widget's 1-row area.
    }
}

/// Format frequency in Hz to a human-readable string
fn format_hz(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{:.0}", hz)
    }
}

/// A single sequence row: name, 16-step grid, mute, gear
pub struct SequenceRow<'a> {
    sequence: &'a Sequence,
    selected: bool,
    editing: bool,
    cursor: EditTarget,
    current_play_step: usize,
    frame: u8,
    border_color: Color,
}

impl<'a> SequenceRow<'a> {
    pub fn new(sequence: &'a Sequence) -> Self {
        Self {
            sequence,
            selected: false,
            editing: false,
            cursor: EditTarget::Step(0),
            current_play_step: 0,
            frame: 0,
            border_color: Color::DarkGray,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    pub fn cursor(mut self, cursor: EditTarget) -> Self {
        self.cursor = cursor;
        self
    }

    pub fn current_play_step(mut self, step: usize) -> Self {
        self.current_play_step = step;
        self
    }

    pub fn frame(mut self, frame: u8) -> Self {
        self.frame = frame;
        self
    }

    pub fn border_color(mut self, color: Color) -> Self {
        self.border_color = color;
        self
    }
}

impl<'a> Widget for SequenceRow<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height < 1 || area.width < 20 {
            return;
        }

        let y = area.y;

        // Name (left-aligned, padded: " X ")
        let name = format!(" {} ", self.sequence.name);
        let name_style = if self.selected {
            Style::default()
                .fg(TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_DIM)
        };
        buf.set_string(area.x, y, &name, name_style);

        let sep_x = area.x + name.len() as u16;

        // Vertical separator after name — only highlight when this row is selected
        let sep_color = if self.selected {
            self.border_color
        } else {
            Color::DarkGray
        };
        buf.set_string(sep_x, y, "│", Style::default().fg(sep_color));

        // --- Right-aligned controls: mute + gear ---
        let gear_is_target = self.selected && self.cursor == EditTarget::Gear;
        let gear_style = if gear_is_target {
            Style::default()
                .fg(TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_DIM)
        };

        let mute_is_target = self.selected && self.cursor == EditTarget::Mute;
        let (mute_label, mute_color) = if self.sequence.mute {
            ("\u{F0581}", Color::Red)
        } else {
            (
                "\u{F057E}",
                if mute_is_target {
                    TEXT_BRIGHT
                } else {
                    TEXT_DIM
                },
            )
        };
        let mute_style = if mute_is_target {
            Style::default().fg(mute_color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(mute_color)
        };

        // Gear icon (rightmost), mute (left of gear)
        let right_edge = area.x + area.width - 1;
        let gear_x = right_edge.saturating_sub(1);
        let mute_x = gear_x.saturating_sub(2);

        // Grid fills the space between name separator and mute icon
        let grid_start = sep_x + 2;
        let grid_end_limit = mute_x.saturating_sub(1); // 1 space before mute

        // Render 16-step grid (no scrolling — always show all that fit)
        const STEP_W: usize = 2;
        let mut step_x = grid_start;
        for step in 0..SEQUENCE_STEPS {
            if step_x + STEP_W as u16 > grid_end_limit {
                break;
            }
            self.render_step(step, step_x, y, buf);
            step_x += STEP_W as u16;
        }

        // --- Draw right-aligned controls ---
        buf.set_string(mute_x, y, mute_label, mute_style);
        buf.set_string(gear_x, y, "\u{F013}", gear_style);
    }
}

impl<'a> SequenceRow<'a> {
    fn render_step(&self, step: usize, x: u16, y: u16, buf: &mut Buffer) {
        let is_marked = self.sequence.pattern[step];
        let is_playing = self.sequence.playing && step == self.current_play_step;
        let is_target = self.selected && self.cursor == EditTarget::Step(step);
        let step_active = is_target && self.editing;

        let (ch, style) = if is_playing && is_marked {
            (
                "󱔀",
                Style::default()
                    .fg(STATUS_PLAYING)
                    .add_modifier(Modifier::BOLD),
            )
        } else if is_playing && !is_marked {
            ("󰝣", Style::default().fg(TEXT_DIM))
        } else if step_active {
            (
                if is_marked { "󱔀" } else { "󰝣" },
                Style::default()
                    .fg(TEXT_EDITING)
                    .add_modifier(Modifier::BOLD),
            )
        } else if is_target {
            (
                if is_marked { "󱔀" } else { "󰝣" },
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD),
            )
        } else if is_marked {
            ("󱔀", Style::default().fg(TEXT_DEFAULT))
        } else {
            ("󰝣", Style::default().fg(TEXT_DIM))
        };

        buf.set_string(x, y, ch, style);
    }
}
