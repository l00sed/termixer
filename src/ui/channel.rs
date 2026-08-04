//! Channel strip component - represents a single mixer channel

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::{Block, Borders, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::state::{ChannelControl, GlobalControl, MixerChannel};
use crate::ui::colors::*;
use crate::ui::widgets::{DeckIndicator, Fader, LevelMeter};

const PLAYLIST_EXEC_FLASH_MS: u64 = 180;

/// A complete channel strip widget - minimalist futuristic design
pub struct ChannelStrip<'a> {
    channel: &'a MixerChannel,
    selected: bool,
    selected_control: Option<ChannelControl>,
    deck_label: Option<&'a str>,
    deck_color: Color,
    editing: bool,
    frame: u8,
    elapsed_ms: u64,
    show_border: bool,
}

impl<'a> ChannelStrip<'a> {
    pub fn new(channel: &'a MixerChannel) -> Self {
        Self {
            channel,
            selected: false,
            selected_control: None,
            deck_label: None,
            deck_color: Color::White,
            editing: false,
            frame: 0,
            elapsed_ms: 0,
            show_border: true,
        }
    }

    pub fn show_border(mut self, show: bool) -> Self {
        self.show_border = show;
        self
    }

    pub fn selected(mut self, selected: bool, control: Option<ChannelControl>) -> Self {
        self.selected = selected;
        if selected {
            self.selected_control = control;
        }
        self
    }

    pub fn deck_label(mut self, label: Option<&'a str>) -> Self {
        self.deck_label = label;
        self
    }

    pub fn deck_color(mut self, color: Color) -> Self {
        self.deck_color = color;
        self
    }

    #[allow(dead_code)]
    pub fn playing(self, _playing: bool) -> Self {
        self // Now using channel.playing directly
    }

    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
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

    fn is_control_selected(&self, control: ChannelControl) -> bool {
        self.selected && self.selected_control == Some(control)
    }

    fn is_control_editing(&self, control: ChannelControl) -> bool {
        self.editing && self.is_control_selected(control)
    }
}

impl<'a> Widget for ChannelStrip<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 18 {
            let style = Style::default().fg(Color::Rgb(40, 40, 40));
            buf.set_string(area.x + 1, area.y + area.height / 2, "···", style);
            return;
        }

        // Determine inner area and render border if enabled
        let inner = if self.show_border {
            // Minimalist border - thin line style
            let border_color = if self.selected {
                if self.selected_control.is_some() {
                    BORDER_ACTIVE
                } else {
                    BORDER_NAVIGATED
                }
            } else if self.deck_label.is_some() {
                if self.channel.connected {
                    match self.deck_color {
                        Color::Cyan => DECK_A_BRIGHT,
                        Color::Magenta => Color::Rgb(255, 100, 255),
                        _ => self.deck_color,
                    }
                } else {
                    Color::Rgb(60, 60, 60)
                }
            } else if self.channel.connected {
                FADER_FILL
            } else {
                Color::Rgb(40, 40, 40)
            };

            let border_style = Style::default().fg(border_color);

            let title = if let Some(deck) = self.deck_label {
                if self.channel.connected {
                    if deck.contains('A') {
                        " A ● "
                    } else if deck.contains('B') {
                        " B ● "
                    } else {
                        " C ● "
                    }
                } else if deck.contains('A') {
                    " A ○ "
                } else if deck.contains('B') {
                    " B ○ "
                } else {
                    " C ○ "
                }
            } else {
                ""
            };

            let title_style = if self.selected {
                Style::default()
                    .fg(BORDER_ACTIVE)
                    .add_modifier(Modifier::BOLD)
            } else if self.channel.connected {
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(80, 80, 80))
            };

            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(Span::styled(title, title_style));

            let inner_area = block.inner(area);
            block.render(area, buf);
            inner_area
        } else {
            area
        };

        // Minimalist layout
        let has_deck = self.deck_label.is_some();

        let constraints = if has_deck {
            vec![
                Constraint::Length(1), // Scrubber (hidden until track loaded)
                Constraint::Length(7), // Deck indicator (ring + space + marquee)
                Constraint::Length(1), // BPM display (combined BPM+KEY)
                Constraint::Length(1), // Separator below BPM
                Constraint::Length(7), // EQ section (3 bars + separator + 4 filter knobs + gaps)
                Constraint::Length(1), // Separator below EQ
                Constraint::Length(1), // Pan
                Constraint::Length(1), // Separator between pan and fader
                Constraint::Min(5),    // Fader + Meter
                Constraint::Length(1), // Separator before buttons
                Constraint::Length(1), // Buttons row
            ]
        } else {
            vec![
                Constraint::Length(7), // EQ section (3 bars + separator + 4 filter knobs + gaps)
                Constraint::Length(1), // Separator below EQ
                Constraint::Length(1), // Pan
                Constraint::Length(1), // Separator between pan and fader
                Constraint::Min(7),    // Fader + Meter
                Constraint::Length(1), // Separator before buttons
                Constraint::Length(1), // Buttons row
            ]
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);

        let mut idx = 0;

        // Deck indicator (only for deck channels)
        if has_deck {
            // Scrubber (shown for playable non-SC sources)
            if self.channel.scrub_available() {
                self.render_scrub_bar(
                    chunks[idx],
                    buf,
                    self.is_control_selected(ChannelControl::Scrub),
                );
            }
            idx += 1;

            let deck_label = self.deck_label.unwrap_or("");
            let deck_char = if deck_label.contains('A') {
                'A'
            } else if deck_label.contains('B') {
                'B'
            } else {
                'C'
            };
            let source_name = if self.channel.connected {
                Some(self.channel.name.clone())
            } else {
                None
            };
            let now_ms = self.elapsed_ms;
            let prev_executed_recently =
                now_ms.saturating_sub(self.channel.prev_exec_flash_ms) <= PLAYLIST_EXEC_FLASH_MS;
            let next_executed_recently =
                now_ms.saturating_sub(self.channel.next_exec_flash_ms) <= PLAYLIST_EXEC_FLASH_MS;
            DeckIndicator::new(deck_char)
                .playing(self.channel.playing)
                .speed(self.channel.playback_speed)
                .frame(self.frame)
                .color(self.deck_color)
                .source_name(source_name)
                .connected(self.channel.connected)
                .selected(self.is_control_selected(ChannelControl::PlayPause))
                .scrub(self.channel.scrub_direction, self.channel.scrub_speed)
                .playlist_nav(self.channel.has_prev_track, self.channel.has_next_track)
                .playlist_selected(
                    self.is_control_selected(ChannelControl::PrevTrack),
                    self.is_control_selected(ChannelControl::NextTrack),
                )
                .playlist_executed(prev_executed_recently, next_executed_recently)
                .elapsed_ms(self.elapsed_ms.into())
                .render(chunks[idx], buf);
            idx += 1;

            // Combined BPM+KEY display: speed factor BPM │ key
            let bpm_selected = self.is_control_selected(ChannelControl::Bpm);
            let bpm_editing = self.is_control_editing(ChannelControl::Bpm);
            let key_selected = self.is_control_selected(ChannelControl::Key);
            let key_editing = self.is_control_editing(ChannelControl::Key);
            let bpm_value = if let Some(bpm) = self.channel.bpm {
                format!("{:.0}", bpm)
            } else {
                "---".to_string()
            };
            let key_str = if let Some(ref key) = self.channel.key {
                if self.channel.key_offset != 0 {
                    let sign = if self.channel.key_offset > 0 { "+" } else { "" };
                    format!("{}{}{}", key, sign, self.channel.key_offset)
                } else {
                    key.clone()
                }
            } else {
                "---".to_string()
            };
            let base = self.channel.base_bpm;
            let speed = if base > 0.0 {
                self.channel.target_bpm / base
            } else {
                1.0
            };
            let factor_str = format!("x{:.2}", speed);

            let speed_style = if bpm_editing {
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else if bpm_selected {
                Style::default().fg(TEXT_DEFAULT)
            } else {
                Style::default().fg(TEXT_DIM)
            };
            let bpm_style = if bpm_editing {
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else if bpm_selected {
                Style::default().fg(TEXT_BRIGHT)
            } else {
                Style::default().fg(TEXT_DEFAULT)
            };
            let key_style = if key_editing {
                Style::default()
                    .fg(TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else if key_selected {
                Style::default().fg(TEXT_BRIGHT)
            } else {
                Style::default().fg(TEXT_DIM)
            };

            let bpm_area = chunks[idx];
            let val_right = bpm_area.x + bpm_area.width;
            // Separator position: between BPM and key (key gets 5 chars: " 12B ")
            let sep_x = val_right.saturating_sub(6);

            // Separator above BPM row with ┬ junction
            self.draw_separator_with_junction(chunks[idx - 1], buf, sep_x, "┬");

            // Speed factor left-aligned (4 chars: "x1.00")
            buf.set_string(bpm_area.x, bpm_area.y, &factor_str, speed_style);
            // BPM right-aligned before separator (4 chars)
            buf.set_string(
                sep_x.saturating_sub(5),
                bpm_area.y,
                format!("{:>4}", bpm_value),
                bpm_style,
            );
            // Vertical separator
            let sep_style = Style::default().fg(SEPARATOR);
            buf.set_string(sep_x, bpm_area.y, "│", sep_style);
            // Key centered with 1 space on each side (5 chars available)
            let key_padded = format!(" {} ", key_str);
            buf.set_string(sep_x + 1, bpm_area.y, &key_padded, key_style);
            idx += 1;

            // Separator below BPM row with ┴ junction
            self.draw_separator_with_junction(chunks[idx], buf, sep_x, "┴");
            idx += 1;
        }

        // EQ section with vertical bars
        let eq_sep_positions = self.render_eq_section(chunks[idx], buf);
        idx += 1;

        // Separator below EQ
        self.draw_separator(chunks[idx], buf);
        // Bottom junctions (┴) where vertical separators meet the horizontal line
        let sep_style = Style::default().fg(SEPARATOR);
        for sep_x in &eq_sep_positions {
            buf.set_string(*sep_x, chunks[idx].y, "┴", sep_style);
        }
        idx += 1;

        // Pan
        self.render_pan_bar(
            chunks[idx],
            buf,
            self.channel.pan,
            self.is_control_selected(ChannelControl::Pan),
            self.is_control_editing(ChannelControl::Pan),
        );
        idx += 1;

        // Separator between pan and fader
        self.draw_separator(chunks[idx], buf);
        idx += 1;

        // Fader and meter area
        let fader_area = chunks[idx];
        idx += 1;

        if fader_area.width >= 5 {
            let fader_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(3), Constraint::Min(2)])
                .split(fader_area);

            LevelMeter::new(0.0)
                .stereo(self.channel.rms_left, self.channel.rms_right)
                .peaks(self.channel.peak_left, self.channel.peak_right)
                .render(fader_chunks[0], buf);

            let db = self.channel.fader_db();
            let db_label = if self.channel.muted {
                "×".to_string()
            } else if db <= -60.0 {
                "∞".to_string()
            } else {
                format!("{:+.0}", db)
            };
            Fader::new(self.channel.fader)
                .selected(self.is_control_selected(ChannelControl::Fader))
                .editing(self.is_control_editing(ChannelControl::Fader))
                .label(db_label)
                .render(fader_chunks[1], buf);
        }

        // Separator before buttons
        self.draw_separator(chunks[idx], buf);
        idx += 1;

        // Compact button row: M S C
        let btn_area = chunks[idx];
        let is_cue = self.deck_label == Some("C");
        self.render_button_row(btn_area, buf, is_cue);
    }
}

impl<'a> ChannelStrip<'a> {
    /// Draw a plain horizontal separator line (no junction characters).
    fn draw_separator(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 1 || area.height < 1 {
            return;
        }
        let style = Style::default().fg(SEPARATOR);
        let y = area.y + area.height.saturating_sub(1);

        // Plain horizontal line - no junction characters
        for x in area.x..area.x + area.width {
            buf.set_string(x, y, "─", style);
        }
    }

    /// Draw a horizontal separator with a junction character at a specific x position.
    fn draw_separator_with_junction(
        &self,
        area: Rect,
        buf: &mut Buffer,
        junction_x: u16,
        junction_char: &str,
    ) {
        if area.width < 1 || area.height < 1 {
            return;
        }
        let style = Style::default().fg(SEPARATOR);
        let y = area.y + area.height.saturating_sub(1);

        for x in area.x..area.x + area.width {
            if x == junction_x {
                buf.set_string(x, y, junction_char, style);
            } else {
                buf.set_string(x, y, "─", style);
            }
        }
    }

    /// Compute the border color for this channel strip
    #[allow(dead_code)]
    fn border_color(&self) -> Color {
        if self.selected {
            if self.selected_control.is_some() {
                Color::Yellow
            } else {
                Color::Rgb(120, 100, 0)
            }
        } else if self.deck_label.is_some() {
            if self.channel.connected {
                match self.deck_color {
                    Color::Cyan => DECK_A_BRIGHT,
                    Color::Magenta => Color::Rgb(255, 100, 255),
                    _ => self.deck_color,
                }
            } else {
                Color::Rgb(60, 60, 60)
            }
        } else if self.channel.connected {
            FADER_FILL
        } else {
            Color::Rgb(40, 40, 40)
        }
    }

    /// Render pan bar: [L][bar][R]
    fn render_pan_bar(
        &self,
        area: Rect,
        buf: &mut Buffer,
        value: f32,
        selected: bool,
        editing: bool,
    ) {
        if area.width < 7 || area.height < 1 {
            return;
        }
        let y = area.y;

        let label_style = if editing {
            Style::default()
                .fg(TEXT_EDITING)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(TEXT_BRIGHT)
        } else {
            Style::default().fg(TEXT_GHOST)
        };

        // Left label
        buf.set_string(area.x, y, "L", label_style);

        // Pan bar (bipolar: -1 to +1)
        let bar_start = area.x + 2;
        let bar_width = area.width.saturating_sub(4) as usize;
        if bar_width >= 3 {
            let center = bar_width / 2;
            let normalized = (value + 1.0) / 2.0; // -1..+1 -> 0..1
            let fill_pos = (normalized * bar_width as f32) as usize;

            for i in 0..bar_width {
                let (ch, is_center) = if i == center {
                    ("│", true)
                } else {
                    ("─", false)
                };

                let color = if editing || selected {
                    if (fill_pos > center && i > center && i <= fill_pos)
                        || (fill_pos < center && i < center && i >= fill_pos)
                    {
                        if editing {
                            TEXT_BRIGHT
                        } else if fill_pos > center {
                            STATUS_PLAYING
                        } else {
                            STATUS_MUTED
                        }
                    } else if editing {
                        TEXT_DIM
                    } else {
                        METER_TRACK
                    }
                } else {
                    METER_TRACK
                };

                let _ = is_center;
                buf.set_string(bar_start + i as u16, y, ch, Style::default().fg(color));
            }
        }

        // Right label
        buf.set_string(area.x + area.width - 1, y, "R", label_style);
    }

    /// Render the EQ section with vertical bars (H/M/L) and filter/LFO controls
    /// Returns the x-position of the vertical separator for junction drawing
    fn render_eq_section(&self, area: Rect, buf: &mut Buffer) -> Vec<u16> {
        if area.width < 10 || area.height < 4 {
            return vec![];
        }

        // Left-aligned: 3 bars with 1-char gaps between = 5 chars, +1 filter sep
        // Remaining space goes to filter column
        let eq_total = 5u16; // bar gap bar gap bar
        let filter_width = area.width.saturating_sub(eq_total + 1); // +1 for filter separator

        let sections = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(1),            // Low bar
                Constraint::Length(1),            // Separator
                Constraint::Length(1),            // Mid bar
                Constraint::Length(1),            // Separator
                Constraint::Length(1),            // High bar
                Constraint::Length(1),            // Separator before filters
                Constraint::Length(filter_width), // Filters + LFO
            ])
            .split(area);

        // Render three EQ bands with vertical bars (L/M/H from left to right)
        self.render_vertical_eq_bar(
            sections[0],
            buf,
            self.channel.eq_low,
            self.channel.spectrum_peaks[0],
            self.is_control_selected(ChannelControl::EqLow),
        );

        self.render_vertical_eq_bar(
            sections[2],
            buf,
            self.channel.eq_mid,
            self.channel.spectrum_peaks[1],
            self.is_control_selected(ChannelControl::EqMid),
        );

        self.render_vertical_eq_bar(
            sections[4],
            buf,
            self.channel.eq_high,
            self.channel.spectrum_peaks[2],
            self.is_control_selected(ChannelControl::EqHigh),
        );

        // Collect all separator x-positions (between bands + before filters)
        let sep_style = Style::default().fg(SEPARATOR);
        let sep_positions: Vec<u16> = vec![sections[1].x, sections[3].x, sections[5].x];

        for &sep_x in &sep_positions {
            for row in 0..area.height {
                let y = area.y + area.height - 1 - row;
                buf.set_string(sep_x, y, "│", sep_style);
            }
        }

        // Top junctions (┬) connect to separator above (deck channels only)
        if self.deck_label.is_some() && area.y > 0 {
            for &sep_x in &sep_positions {
                buf.set_string(sep_x, area.y - 1, "┬", sep_style);
            }
        }

        // Filter column: 4 knobs top-aligned with 1-row gaps
        let filter_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Filter Cutoff (-/+)
                Constraint::Length(1), // Gap
                Constraint::Length(1), // Filter Freq (H/kH)
                Constraint::Length(1), // Gap
                Constraint::Length(1), // LFO Shape (󱑻/Hz)
                Constraint::Length(1), // Gap
                Constraint::Length(1), // LFO Speed (󰴻/󰤇)
            ])
            .split(sections[6]);

        self.render_filter_bar(
            filter_chunks[0],
            buf,
            self.channel.filter_cutoff,
            Some(("-", "+")),
            self.is_control_selected(ChannelControl::FilterCutoff),
        );

        self.render_filter_bar(
            filter_chunks[2],
            buf,
            self.channel.filter_freq,
            Some(("Hz", "kHz")),
            self.is_control_selected(ChannelControl::FilterFreq),
        );

        self.render_filter_bar(
            filter_chunks[4],
            buf,
            self.channel.lfo_shape,
            Some(("󱑻", "󰥛")),
            self.is_control_selected(ChannelControl::LfoShape),
        );

        self.render_filter_bar(
            filter_chunks[6],
            buf,
            self.channel.lfo_speed,
            Some(("󰴻", "󰤇")),
            self.is_control_selected(ChannelControl::LfoSpeed),
        );

        sep_positions
    }

    /// Render a single vertical EQ bar
    fn render_vertical_eq_bar(
        &self,
        area: Rect,
        buf: &mut Buffer,
        eq_value: f32,
        peak: f32,
        bar_selected: bool,
    ) {
        if area.width < 1 || area.height < 2 {
            return;
        }

        let bar_editing = bar_selected && self.editing;
        let bar_height = area.height;
        let bar_x = area.x + (area.width - 1) / 2;

        // Normalize EQ value: -24dB to +24dB maps to -1.0 to 1.0 (0 = center)
        let normalized = eq_value / 24.0;
        let total_steps = bar_height * 2;
        let gain_normalized = (normalized + 1.0) / 2.0; // 0..1
        let gain_step = (gain_normalized * (total_steps - 1) as f32) as u16;
        // Scale down incoming spectrum peak for display so bars show more movement
        // without changing underlying audio model. Tweak value if too subtle.
        let display_scale: f32 = 0.55;
        let display_peak = (peak * display_scale).clamp(0.0, 1.0);
        let level_steps = (display_peak * total_steps as f32) as u16;

        // Offset step by +1 so topmost step reaches 100% and maps to red.
        let color_for_step = |step: u16| -> Color {
            let pct = (step as f32 + 1.0) / total_steps as f32;
            if pct > 0.90 {
                Color::Red
            } else if pct > 0.80 {
                Color::Rgb(255, 140, 0)
            } else if pct > 0.65 {
                Color::Yellow
            } else {
                Color::Green
            }
        };

        let handle_color = if bar_editing {
            TEXT_EDITING
        } else {
            Color::White
        };

        let handle_char = if bar_selected || bar_editing {
            "◆"
        } else {
            "─"
        };

        for row in 0..bar_height {
            let y = area.y + bar_height - 1 - row;
            let lower_step = row * 2;
            let upper_step = row * 2 + 1;

            let lower_filled = level_steps > lower_step;
            let upper_filled = level_steps > upper_step;

            let gain_in_lower = gain_step == lower_step;
            let gain_in_upper = gain_step == upper_step;

            if gain_in_upper || gain_in_lower {
                let gain_style = Style::default()
                    .fg(handle_color)
                    .add_modifier(Modifier::BOLD);
                buf.set_string(bar_x, y, handle_char, gain_style);
            } else if upper_filled && lower_filled {
                let c = color_for_step(upper_step);
                buf.set_string(bar_x, y, "█", Style::default().fg(c));
            } else if upper_filled && !lower_filled {
                let c = color_for_step(upper_step);
                buf.set_string(bar_x, y, "▀", Style::default().fg(c));
            } else if !upper_filled && lower_filled {
                let c = color_for_step(lower_step);
                buf.set_string(bar_x, y, "▄", Style::default().fg(c));
            }
        }
    }

    /// Render a compact vertical filter/LFO bar with optional label
    fn render_filter_bar(
        &self,
        area: Rect,
        buf: &mut Buffer,
        value: f32,
        side_labels: Option<(&str, &str)>,
        selected: bool,
    ) {
        if area.width < 1 || area.height < 1 {
            return;
        }

        let editing = selected && self.editing;

        // Knob icon progression (0.0 to 1.0)
        let knob_icons = ["󰄰", "󰪞", "󰪟", "󰪠", "󰪡", "󰪢", "󰪣", "󰪤", "󰪥"];
        let icon_index = (value * (knob_icons.len() - 1) as f32).round() as usize;
        let knob_char = knob_icons[icon_index];

        // Knob style
        let knob_style = if editing {
            Style::default()
                .fg(TEXT_EDITING)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(TEXT_BRIGHT)
        } else {
            Style::default().fg(Color::Rgb(100, 100, 100))
        };

        let label_style = Style::default().fg(TEXT_GHOST);
        let knob_y = area.y + area.height / 2;

        if let Some((left_lbl, right_lbl)) = side_labels {
            // Draw knob centered between side labels
            let knob_x = area.x + area.width / 2;
            buf.set_string(knob_x, knob_y, knob_char, knob_style);

            // Left label
            let left_width = left_lbl.chars().count() as u16;
            let left_x = area.x + area.width / 2 - left_width - 1;
            buf.set_string(left_x, knob_y, left_lbl, label_style);

            // Right label
            let right_x = area.x + area.width / 2 + 2;
            buf.set_string(right_x, knob_y, right_lbl, label_style);
        } else {
            // Just center the knob
            let knob_x = area.x + (area.width.saturating_sub(1)) / 2;
            buf.set_string(knob_x, knob_y, knob_char, knob_style);
        }
    }

    /// Render scrub slider showing elapsed/total time with position bar
    fn render_scrub_bar(&self, area: Rect, buf: &mut Buffer, selected: bool) {
        if area.width < 4 || area.height < 1 {
            return;
        }

        let y = area.y;
        let editing = self.is_control_editing(ChannelControl::Scrub);

        let format_time = |secs: f32| -> String {
            let total = secs.max(0.0) as u32;
            let m = total / 60;
            let s = total % 60;
            format!("{}:{:02}", m, s)
        };

        let elapsed = format_time(self.channel.time_pos);
        let total = format_time(self.channel.duration);

        let position = if self.channel.duration > 0.0 {
            (self.channel.time_pos / self.channel.duration).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let time_str = format!("{}/{}", elapsed, total);
        let time_x = area.x + area.width.saturating_sub(time_str.len() as u16);
        let time_style = if editing {
            Style::default()
                .fg(TEXT_EDITING)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default().fg(TEXT_BRIGHT)
        } else {
            Style::default().fg(TEXT_DEFAULT)
        };
        buf.set_string(time_x, y, &time_str, time_style);

        let bar_start = area.x;
        let bar_end = time_x.saturating_sub(1);
        if bar_end > bar_start + 2 {
            let bar_width = (bar_end - bar_start) as usize;
            let filled = ((position * bar_width as f32) as usize).min(bar_width - 1);
            for i in 0..bar_width {
                let ch = if i == filled { "◆" } else { "─" };
                let color = if i < filled {
                    if editing { TEXT_EDITING } else { METER_FILL }
                } else {
                    METER_TRACK
                };
                buf.set_string(bar_start + i as u16, y, ch, Style::default().fg(color));
            }
        }
    }

    fn render_button_row(&self, area: Rect, buf: &mut Buffer, is_cue: bool) {
        if area.width < 3 {
            return;
        }

        // Two equal columns: M │ S (or M │ swap A for CUE)
        let sep_x = area.x + area.width / 2;
        let left_w = sep_x - area.x;
        let right_w = area.x + area.width - sep_x - 1;

        // Center each letter within its half (with 1-cell padding on each side)
        let m_x = area.x + 1 + (left_w - 2) / 2;

        // For CUE deck, right side swaps with A instead of toggling solo
        let (right_label, right_control, right_active) = if is_cue {
            (" A", ChannelControl::CueSendToA, false)
        } else {
            ("S", ChannelControl::Solo, self.channel.solo)
        };

        // Calculate position for right label
        let s_x = if is_cue {
            sep_x + 1 + (right_w.saturating_sub(right_label.width() as u16)) / 2
        } else {
            // "S" is 1 char, centered
            sep_x + 2 + (right_w - 2) / 2
        };

        let sep_style = Style::default().fg(SEPARATOR);
        // ┬ on the separator line above, connecting downward into the M|S split
        // │ on the button row between M and S/swap A
        // Note: bottom junction for CUE is handled by render_cue_pane separator
        if area.y > 0 {
            buf.set_string(sep_x, area.y - 1, "┬", sep_style);
        }
        buf.set_string(sep_x, area.y, "│", sep_style);

        // Highlight background for active toggles (with 1-cell padding on edges)
        let active_m_bg = if self.channel.muted {
            Some(STATUS_MUTED)
        } else {
            None
        };
        let active_s_bg = if right_active {
            Some(BORDER_ACTIVE)
        } else {
            None
        };

        // Fill M column background if active (skip first and last cell)
        if let Some(bg) = active_m_bg {
            for x in area.x + 1..sep_x.saturating_sub(1) {
                buf.set_string(x, area.y, " ", Style::default().bg(bg));
            }
        }
        // Fill S/swap A column background if active (skip first and last cell)
        if let Some(bg) = active_s_bg {
            for x in sep_x + 2..area.x + area.width - 1 {
                buf.set_string(x, area.y, " ", Style::default().bg(bg));
            }
        }

        // M - Mute
        let m_style = if self.channel.muted {
            Style::default().fg(Color::Black).bg(STATUS_MUTED)
        } else if self.is_control_selected(ChannelControl::Mute) {
            Style::default().fg(TEXT_EDITING)
        } else {
            Style::default().fg(TEXT_DIM)
        };
        buf.set_string(m_x, area.y, "M", m_style);

        // S - Solo (or swap A for CUE)
        let s_style = if right_active {
            Style::default().fg(Color::Black).bg(BORDER_ACTIVE)
        } else if self.is_control_selected(right_control) {
            Style::default().fg(TEXT_EDITING)
        } else {
            Style::default().fg(TEXT_DIM)
        };
        buf.set_string(s_x, area.y, right_label, s_style);
    }
}

/// Master channel strip widget - minimalist design
pub struct MasterStrip<'a> {
    master: &'a crate::state::MasterChannel,
    pane_selected: bool,
    selected: bool,
    selected_control: Option<GlobalControl>,
    editing: bool,
    frame: u8,
    /// True if any deck or CUE channel has playing == true
    any_channel_playing: bool,
    mic_input_device: Option<&'a str>,
}

impl<'a> MasterStrip<'a> {
    pub fn new(master: &'a crate::state::MasterChannel) -> Self {
        Self {
            master,
            pane_selected: false,
            selected: false,
            selected_control: None,
            editing: false,
            frame: 0,
            any_channel_playing: false,
            mic_input_device: None,
        }
    }

    pub fn pane_selected(mut self, selected: bool) -> Self {
        self.pane_selected = selected;
        self
    }

    pub fn selected(mut self, selected: bool, control: Option<GlobalControl>) -> Self {
        self.selected = selected;
        if selected {
            self.selected_control = control;
        }
        self
    }

    pub fn editing(mut self, editing: bool) -> Self {
        self.editing = editing;
        self
    }

    pub fn frame(mut self, frame: u8) -> Self {
        self.frame = frame;
        self
    }

    pub fn any_channel_playing(mut self, v: bool) -> Self {
        self.any_channel_playing = v;
        self
    }

    pub fn mic_input_device(mut self, device: Option<&'a str>) -> Self {
        self.mic_input_device = device;
        self
    }

    fn is_control_editing(&self, control: GlobalControl) -> bool {
        self.editing && self.selected && self.selected_control == Some(control)
    }

    /// Render compact master EQ bars (10-band, no gaps) inside MasterStrip
    fn render_master_eq_bars(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 2 {
            return;
        }

        let bar_area_height = area.height;
        let num_bands = 10u16;

        // Bars with 1-char gap between columns
        let bar_step = 2u16;
        let total_width = num_bands * bar_step;
        let start_x = if area.width > total_width {
            (area.width - total_width).div_ceil(2)
        } else {
            0
        };

        let eq_selected = self.selected;
        let eq_editing = self.editing;

        for i in 0..10 {
            let x_offset = start_x + (i as u16) * bar_step;
            if x_offset >= area.width {
                break;
            }

            let band_selected =
                eq_selected && self.selected_control == Some(GlobalControl::all_eq_variants()[i]);
            let band_editing = band_selected && eq_editing;

            let peak = self.master.spectrum_peaks[i];
            let db = self.master.master_eq[i];
            let gain_normalized = (db + 12.0) / 24.0;

            let total_steps = bar_area_height * 2;
            // Reduce displayed peak so EQ bars less likely stay at full height.
            // 0.55 chosen after manual testing as reasonable default; adjust if needed.
            let display_scale: f32 = 0.55;
            let display_peak = (peak * display_scale).clamp(0.0, 1.0);
            let level_steps = (display_peak * total_steps as f32) as u16;
            let gain_step = (gain_normalized * (total_steps - 1) as f32) as u16;

            let bar_x = area.x + x_offset;

            for row in 0..bar_area_height {
                let y = area.y + bar_area_height - 1 - row;
                let lower_step = row * 2;
                let upper_step = row * 2 + 1;

                let lower_filled = level_steps > lower_step;
                let upper_filled = level_steps > upper_step;

                let gain_in_lower = gain_step == lower_step;
                let gain_in_upper = gain_step == upper_step;

                let color_for_step = |step: u16| -> Color {
                    let pct = step as f32 / total_steps as f32;
                    if pct > 0.90 {
                        Color::Red
                    } else if pct > 0.80 {
                        Color::Rgb(255, 140, 0)
                    } else if pct > 0.65 {
                        Color::Yellow
                    } else {
                        Color::Green
                    }
                };

                if gain_in_upper || gain_in_lower {
                    let gain_color = if band_editing {
                        TEXT_EDITING
                    } else {
                        Color::White
                    };
                    let gain_style = Style::default().fg(gain_color).add_modifier(Modifier::BOLD);
                    let handle_char = if band_selected { "◆" } else { "─" };
                    buf.set_string(bar_x, y, handle_char, gain_style);
                } else if upper_filled && lower_filled {
                    let c = color_for_step(upper_step);
                    buf.set_string(bar_x, y, "█", Style::default().fg(c));
                } else if upper_filled && !lower_filled {
                    let c = color_for_step(upper_step);
                    buf.set_string(bar_x, y, "▀", Style::default().fg(c));
                } else if !upper_filled && lower_filled {
                    let c = color_for_step(lower_step);
                    buf.set_string(bar_x, y, "▄", Style::default().fg(c));
                }
                // else: transparent (no background)
            }

            // Vertical separator in gap between bands (not after last)
            if i < 9 {
                let sep_x = bar_x + 1;
                for row in 0..bar_area_height {
                    let y = area.y + bar_area_height - 1 - row;
                    buf.set_string(sep_x, y, "│", Style::default().fg(SEPARATOR));
                }
            }
        }
    }
}

impl<'a> Widget for MasterStrip<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 8 || area.height < 18 {
            return;
        }

        // Border: yellow if pane selected, dim otherwise
        let border_style = if self.pane_selected {
            if self.selected {
                Style::default().fg(BORDER_ACTIVE)
            } else {
                Style::default().fg(BORDER_NAVIGATED)
            }
        } else {
            Style::default().fg(BTN_DM_PURPLE)
        };

        let title_style = if self.pane_selected {
            Style::default()
                .fg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else {
            border_style
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(" M ", title_style));

        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Play/Pause spinner (3×3 ring)
                Constraint::Length(1), // Separator
                Constraint::Length(5), // EQ bars
                Constraint::Length(1), // Separator
                Constraint::Length(1), // Mic volume slider with icon
                Constraint::Length(1), // Separator
                Constraint::Length(1), // INPUT button (mic input selector)
                Constraint::Length(1), // Separator
                Constraint::Min(5),    // Meters and fader (reduced from 6 to 5)
                Constraint::Length(1), // Separator
                Constraint::Length(1), // M | OUT buttons (single row)
            ])
            .split(inner);

        // ── Play/Pause spinner (full-width ring) ──
        let pp_area = chunks[0];
        let pp_selected = self.selected_control == Some(GlobalControl::MasterPlayPause);
        let playing = self.master.playing && self.any_channel_playing;

        // Background highlight when paused
        if !playing {
            for y in pp_area.y..pp_area.y + pp_area.height {
                for x in pp_area.x..pp_area.x + pp_area.width {
                    buf.set_string(x, y, " ", Style::default().bg(Color::Rgb(20, 20, 20)));
                }
            }
        }

        let cx = pp_area.x + pp_area.width / 2;
        let cy = pp_area.y + pp_area.height / 2;
        let hw = pp_area.width as i16 / 2 - 1; // 1 col padding each side

        // Build full-width ring segments clockwise: top → right → bottom → left
        // Total segments: 2*(2*hw+1) + 2 = 4*hw + 4
        let num_segments = (4 * hw + 4) as usize;
        let highlight_pos = if playing {
            (self.frame as usize) % num_segments
        } else {
            num_segments
        };

        let dim = Style::default().fg(Color::Rgb(35, 35, 35));
        let glow = Style::default()
            .fg(STATUS_PLAYING)
            .add_modifier(Modifier::BOLD);
        let trail = Style::default().fg(STATUS_PLAYING);
        let sel_glow = Style::default()
            .fg(BORDER_ACTIVE)
            .add_modifier(Modifier::BOLD);
        let sel_ring = Style::default().fg(BORDER_ACTIVE);

        // Generate ring positions: top-left corner, top edge, top-right corner,
        // right edge, bottom-right corner, bottom edge, bottom-left corner, left edge
        let mut ring: Vec<(i16, i16, &str)> = Vec::with_capacity(num_segments);
        // Top-left corner
        ring.push((-hw, -1, "╭"));
        // Top horizontal (left to right)
        for x in (-hw + 1)..hw {
            ring.push((x, -1, "─"));
        }
        // Top-right corner
        ring.push((hw, -1, "╮"));
        // Right edge
        ring.push((hw, 0, "│"));
        // Bottom-right corner
        ring.push((hw, 1, "╯"));
        // Bottom horizontal (right to left)
        for x in (1 - hw..hw).rev() {
            ring.push((x, 1, "─"));
        }
        // Bottom-left corner
        ring.push((-hw, 1, "╰"));
        // Left edge
        ring.push((-hw, 0, "│"));

        for (i, (dx, dy, ch)) in ring.iter().enumerate() {
            let x = (cx as i16 + dx) as u16;
            let y = (cy as i16 + dy) as u16;
            let prev = (i + num_segments - 1) % num_segments;
            let next = (i + 1) % num_segments;
            let style = if pp_selected {
                if playing && i == highlight_pos {
                    sel_glow
                } else {
                    sel_ring
                }
            } else if playing {
                if i == highlight_pos {
                    glow
                } else if i == prev || i == next {
                    trail
                } else {
                    dim
                }
            } else {
                dim
            };
            buf.set_string(x, y, ch, style);
        }

        // Center: play / pause icon
        let center_char = if playing { "▶" } else { "󰏤" };
        let center_style = if pp_selected {
            Style::default()
                .fg(BORDER_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else if playing {
            Style::default()
                .fg(STATUS_PLAYING)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_DIM)
        };
        buf.set_string(cx, cy, center_char, center_style);

        // ── Separator below play/pause ────────────────────────
        let sep1_y = chunks[1].y;
        for x in chunks[1].x..chunks[1].x + chunks[1].width {
            buf.set_string(x, sep1_y, "─", Style::default().fg(SEPARATOR));
        }
        // Junctions: override separator at EQ gap columns
        let eq_area = chunks[2];
        let num_bands = 10u16;
        let bar_step = 2u16;
        let total_width = num_bands * bar_step;
        let start_x = if eq_area.width > total_width {
            (eq_area.width - total_width).div_ceil(2)
        } else {
            0
        };
        for i in 0..9 {
            let sep_x = eq_area.x + start_x + (i as u16) * bar_step + 1;
            if sep_x < eq_area.x + eq_area.width {
                buf.set_string(sep_x, sep1_y, "┬", Style::default().fg(SEPARATOR));
            }
        }

        // ── Master EQ bars (compact, no gaps) ──────────────────
        self.render_master_eq_bars(chunks[2], buf);

        // ── Separator below EQ ────────────────────────────────
        let sep_eq_y = chunks[3].y;
        for x in chunks[3].x..chunks[3].x + chunks[3].width {
            buf.set_string(x, sep_eq_y, "─", Style::default().fg(SEPARATOR));
        }
        // Junctions: override separator at EQ gap columns
        for i in 0..9 {
            let sep_x = eq_area.x + start_x + (i as u16) * bar_step + 1;
            if sep_x < eq_area.x + eq_area.width {
                buf.set_string(sep_x, sep_eq_y, "┴", Style::default().fg(SEPARATOR));
            }
        }

        // ── MIC volume slider with mute icon ───────────────────
        let mic_slider_area = chunks[4];
        let mic_mute_selected = self.selected_control == Some(GlobalControl::MicMute);
        let mic_fader_selected = self.selected_control == Some(GlobalControl::MicFader);
        let mic_fader_editing = self.is_control_editing(GlobalControl::MicFader);
        
        // Space + Mute icon + space (total: 3 chars)
        let mic_icon = if self.master.mic_muted { "󰍭" } else { "󰍬" };
        let mic_icon_style = if mic_mute_selected {
            Style::default().fg(TEXT_EDITING).add_modifier(Modifier::BOLD)
        } else if self.master.mic_muted {
            Style::default().fg(STATUS_MUTED)
        } else {
            Style::default().fg(TEXT_DIM)
        };
        buf.set_string(mic_slider_area.x, mic_slider_area.y, " ", mic_icon_style);
        buf.set_string(mic_slider_area.x + 1, mic_slider_area.y, mic_icon, mic_icon_style);
        buf.set_string(mic_slider_area.x + 2, mic_slider_area.y, " ", Style::default().fg(TEXT_DIM));
        
        // Slider fills the rest of the width (with leading space + icon + gap + trailing space)
        let slider_x = mic_slider_area.x + 3;
        let slider_width = mic_slider_area.width.saturating_sub(4); // -4 for space + icon + gap + trailing space
        let filled = (self.master.mic_fader * slider_width as f32) as usize;
        
        let slider_fg = if mic_fader_editing {
            TEXT_EDITING
        } else if mic_fader_selected {
            TEXT_BRIGHT
        } else {
            TEXT_DIM
        };
        let slider_style = if mic_fader_selected {
            Style::default().fg(slider_fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(slider_fg)
        };
        
        for i in 0..slider_width as usize {
            let ch = if i < filled { "\u{2501}" } else { "\u{2500}" };
            buf.set_string(slider_x + i as u16, mic_slider_area.y, ch, slider_style);
        }

        // ── Separator below mic slider ─────────────────────────
        let sep_mic_y = chunks[5].y;
        for x in chunks[5].x..chunks[5].x + chunks[5].width {
            buf.set_string(x, sep_mic_y, "─", Style::default().fg(SEPARATOR));
        }

        // ── INPUT button (mic input selector) ──────────────────
        let input_area = chunks[6];
        let input_selected = self.selected_control == Some(GlobalControl::MicInputSelect);
        let input_style = if input_selected {
            Style::default().fg(TEXT_EDITING)
        } else {
            Style::default().fg(TEXT_DIM)
        };
        let input_label = "INPUT";
        let input_x = input_area.x + (input_area.width.saturating_sub(input_label.len() as u16)) / 2;
        buf.set_string(input_x, input_area.y, input_label, input_style);

        // ── Separator below INPUT ──────────────────────────────
        let sep_input_y = chunks[7].y;
        for x in input_area.x..input_area.x + input_area.width {
            buf.set_string(x, sep_input_y, "─", Style::default().fg(SEPARATOR));
        }

        // ── MASTER gain (meter + fader) ────────────────────────
        let master_gain_area = chunks[8];
        let master_gain = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(3), Constraint::Min(3)])
            .split(master_gain_area);
        
        LevelMeter::new(0.0)
            .stereo(self.master.rms_left, self.master.rms_right)
            .peaks(self.master.peak_left, self.master.peak_right)
            .render(master_gain[0], buf);

        let db = self.master.fader_db();
        let db_label = if db <= -60.0 {
            "∞".to_string()
        } else {
            format!("{:+.0}", db)
        };
        let fader_selected = self.selected_control == Some(GlobalControl::MasterFader);
        Fader::new(self.master.fader)
            .selected(fader_selected)
            .editing(self.is_control_editing(GlobalControl::MasterFader))
            .label(db_label)
            .render(master_gain[1], buf);

        // ── Separator before buttons ──────────────────────────
        let sep_y = chunks[9].y;
        for x in chunks[8].x..chunks[8].x + chunks[8].width {
            buf.set_string(x, sep_y, "─", Style::default().fg(SEPARATOR));
        }

        // ── M | OUT button row ─────────────────────────────────
        let btn_area = chunks[10];
        if btn_area.width >= 5 && btn_area.height >= 1 {
            // Two equal sections: M | OUT
            let section_width = btn_area.width / 2;
            let sep_x = btn_area.x + section_width;
            
            let sep_style = Style::default().fg(SEPARATOR);
            
            // Draw separator
            if btn_area.y > 0 {
                buf.set_string(sep_x, btn_area.y - 1, "┬", sep_style);
            }
            buf.set_string(sep_x, btn_area.y, "│", sep_style);
            
            // M (Master Mute) - first section (centered)
            let m_selected = self.selected_control == Some(GlobalControl::MasterMute);
            let m_x = btn_area.x + (section_width.saturating_sub(1)) / 2;
            
            // Fill M column background if muted (skip first and last cell for padding)
            if self.master.muted {
                for x in btn_area.x + 1..sep_x.saturating_sub(1) {
                    buf.set_string(x, btn_area.y, " ", Style::default().bg(STATUS_MUTED));
                }
            }
            
            let m_style = if self.master.muted {
                Style::default().fg(Color::Black).bg(STATUS_MUTED)
            } else if m_selected {
                Style::default().fg(TEXT_EDITING)
            } else {
                Style::default().fg(TEXT_DIM)
            };
            buf.set_string(m_x, btn_area.y, "M", m_style);
            
            // OUT (Master Output) - second section (centered)
            // The OUT section spans from sep_x+1 to the end of btn_area
            let out_selected = self.selected_control == Some(GlobalControl::MasterOutputSelect);
            let out_label = "OUT";
            let out_section_width = btn_area.width - section_width - 1; // remaining width after M section and separator
            let out_x = sep_x + 1 + (out_section_width.saturating_sub(out_label.len() as u16)) / 2;
            
            let out_style = if out_selected {
                Style::default().fg(TEXT_EDITING)
            } else {
                Style::default().fg(TEXT_DIM)
            };
            buf.set_string(out_x, btn_area.y, out_label, out_style);
        }
    }
}
