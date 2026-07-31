//! Custom widget implementations for the mixer UI

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use super::colors::*;

/// Vertical fader widget that mimics a physical mixer fader
pub struct Fader {
    /// Current value (0.0 to 1.0)
    value: f32,
    /// Is this fader selected
    selected: bool,
    /// Is this fader being edited
    editing: bool,
    /// Label text
    label: String,
}

impl Fader {
    pub fn new(value: f32) -> Self {
        Self {
            value: value.clamp(0.0, 1.0),
            selected: false,
            editing: false,
            label: String::new(),
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

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

impl Widget for Fader {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 3 || area.height < 5 {
            return;
        }

        let track_x = area.x + area.width / 2;
        let track_top = area.y + 1;
        let track_bottom = area.y + area.height - 2;
        let track_height = track_bottom - track_top;

        // Draw the fader track
        let track_style = Style::default().fg(METER_TRACK);
        for y in track_top..=track_bottom {
            buf.set_string(track_x, y, "│", track_style);
        }

        // Draw top and bottom junction characters
        buf.set_string(track_x, track_top, "┬", track_style);
        buf.set_string(track_x, track_bottom, "┴", track_style);

        // Draw tick marks
        let tick_positions = [0.0, 0.25, 0.5, 0.75, 1.0];
        for &pos in &tick_positions {
            let y = track_bottom - (pos * track_height as f32) as u16;
            if y >= track_top && y <= track_bottom {
                let is_zero_db = (pos - 0.5).abs() < 0.01;  // 0dB at center (0.5)
                let is_top = (pos - 1.0).abs() < 0.01;

                let (ch, style) = if is_zero_db {
                    // 0dB mark: bright white, wider
                    ("─", Style::default().fg(Color::White))
                } else if is_top {
                    ("─", Style::default().fg(Color::DarkGray))
                } else {
                    ("─", track_style)
                };

                // Draw left tick
                if track_x > area.x {
                    buf.set_string(track_x - 1, y, ch, style);
                    // Extra wide for 0dB mark
                    if is_zero_db && track_x > area.x + 1 {
                        buf.set_string(track_x - 2, y, ch, style);
                    }
                }

                // Draw right tick
                if track_x + 1 < area.x + area.width {
                    buf.set_string(track_x + 1, y, ch, style);
                    // Extra wide for 0dB mark
                    if is_zero_db && track_x + 2 < area.x + area.width {
                        buf.set_string(track_x + 2, y, ch, style);
                    }
                }
            }
        }

        // Draw the fader cap (the part you grab)
        let fader_y = track_bottom - (self.value * track_height as f32) as u16;
        let fader_y = fader_y.clamp(track_top, track_bottom);

        let fader_style = if self.editing {
            Style::default()
                .fg(TEXT_EDITING)
                .add_modifier(Modifier::BOLD)
        } else if self.selected {
            Style::default()
                .fg(TEXT_BRIGHT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(100, 100, 100))
        };

        // Draw fader cap as a wider element
        let cap_left = track_x.saturating_sub(1);
        let cap_right = (track_x + 1).min(area.x + area.width - 1);

        buf.set_string(cap_left, fader_y, "▐", fader_style);
        buf.set_string(track_x, fader_y, "█", fader_style);
        buf.set_string(cap_right, fader_y, "▌", fader_style);

        // Draw dB label at bottom
        if !self.label.is_empty() && area.height > 3 {
            let label_x = area.x + (area.width.saturating_sub(self.label.len() as u16)) / 2;
            buf.set_string(
                label_x,
                area.y + area.height - 1,
                &self.label,
                Style::default().fg(TEXT_DEFAULT),
            );
        }
    }
}

/// Minimalist deck playback indicator - clean futuristic design
/// Shows playback state with animated waveform/rotation indicator
pub struct DeckIndicator {
    /// Is the deck playing
    playing: bool,
    /// Playback speed (1.0 = normal)
    speed: f32,
    /// Animation frame (0-15 for smooth rotation)
    frame: u8,
    /// Deck label (A or B)
    label: char,
    /// Deck color
    color: Color,
    /// Source name (displayed when connected)
    source_name: Option<String>,
    /// Whether this control is selected
    selected: bool,
    /// Whether source is connected
    connected: bool,
    /// Scrub direction (-1.0 backward, 0.0 none, 1.0 forward)
    scrub_direction: f32,
    /// Scrub speed (0.0 = not scrubbing)
    scrub_speed: f32,
    has_prev_track: bool,
    has_next_track: bool,
    prev_selected: bool,
    next_selected: bool,
    prev_executed_recently: bool,
    next_executed_recently: bool,
    /// Elapsed milliseconds since app start (for marquee scroll without syscall)
    elapsed_ms: u128,
}

impl DeckIndicator {
    pub fn new(label: char) -> Self {
        Self {
            playing: false,
            speed: 1.0,
            frame: 0,
            label,
            color: Color::White,
            source_name: None,
            selected: false,
            connected: false,
            scrub_direction: 0.0,
            scrub_speed: 0.0,
            has_prev_track: false,
            has_next_track: false,
            prev_selected: false,
            next_selected: false,
            prev_executed_recently: false,
            next_executed_recently: false,
            elapsed_ms: 0,
        }
    }

    pub fn playing(mut self, playing: bool) -> Self {
        self.playing = playing;
        self
    }

    pub fn speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }

    pub fn frame(mut self, frame: u8) -> Self {
        self.frame = frame;
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    pub fn source_name(mut self, name: Option<String>) -> Self {
        self.source_name = name;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn connected(mut self, connected: bool) -> Self {
        self.connected = connected;
        self
    }

    pub fn scrub(mut self, direction: f32, speed: f32) -> Self {
        self.scrub_direction = direction;
        self.scrub_speed = speed;
        self
    }

    pub fn playlist_nav(mut self, has_prev: bool, has_next: bool) -> Self {
        self.has_prev_track = has_prev;
        self.has_next_track = has_next;
        self
    }

    pub fn playlist_selected(mut self, prev_selected: bool, next_selected: bool) -> Self {
        self.prev_selected = prev_selected;
        self.next_selected = next_selected;
        self
    }

    pub fn playlist_executed(mut self, prev_executed_recently: bool, next_executed_recently: bool) -> Self {
        self.prev_executed_recently = prev_executed_recently;
        self.next_executed_recently = next_executed_recently;
        self
    }

    pub fn elapsed_ms(mut self, elapsed_ms: u128) -> Self {
        self.elapsed_ms = elapsed_ms;
        self
    }
}

impl Widget for DeckIndicator {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 5 || area.height < 3 {
            // Ultra-minimal: just show play state
            let symbol = if self.playing { "▶" } else { "󰏤" };
            let style = if self.playing {
                Style::default().fg(self.color)
            } else {
                Style::default().fg(Color::Rgb(40, 40, 40))
            };
            buf.set_string(area.x + area.width / 2, area.y + area.height / 2, symbol, style);
            return;
        }

        let cx = area.x + area.width / 2;
        let cy = area.y + (area.height.saturating_sub(1) / 2).saturating_sub(1);

        // Futuristic ring animation - 8 segments around center
        // When playing, one segment is highlighted and rotates

        // Calculate which segment to highlight based on frame and speed
        let highlight_pos = if self.playing {
            let base = self.frame as f32;
            // Speed up animation when scrubbing
            let speed_mult = 1.0 + self.scrub_speed * 0.5;
            let adjusted = base * speed_mult;
            let pos = (adjusted as usize) % 12;
            // Reverse direction when scrubbing backwards
            if self.scrub_direction < 0.0 {
                11 - pos
            } else {
                pos
            }
        } else {
            12 // No highlight when stopped
        };

        // Dim color for inactive elements
        let dim_style = Style::default().fg(Color::Rgb(35, 35, 35));
        let active_style = Style::default().fg(self.color);
        let glow_style = Style::default().fg(self.color).add_modifier(Modifier::BOLD);
        let selected_style = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);

        // Draw outer ring (positions relative to center) - seamless 5x3
        // Complete ring with all segments connected
        let positions: [(i16, i16, &str); 12] = [
            (-2, -1, "╭"),  // 0: top-left
            (-1, -1, "─"),  // 1: top-left-center
            (0, -1, "─"),   // 2: top-center
            (1, -1, "─"),   // 3: top-right-center
            (2, -1, "╮"),   // 4: top-right
            (2, 0, "│"),    // 5: right
            (2, 1, "╯"),    // 6: bottom-right
            (1, 1, "─"),    // 7: bottom-right-center
            (0, 1, "─"),    // 8: bottom-center
            (-1, 1, "─"),   // 9: bottom-left-center
            (-2, 1, "╰"),   // 10: bottom-left
            (-2, 0, "│"),   // 11: left
        ];

        for (i, (dx, dy, ch)) in positions.iter().enumerate() {
            let x = (cx as i16 + dx) as u16;
            let y = (cy as i16 + dy) as u16;

            let style = if self.selected {
                // When selected, show ring in yellow
                if self.playing && i == highlight_pos {
                    selected_style
                } else {
                    Style::default().fg(Color::Yellow)
                }
            } else if self.playing {
                if i == highlight_pos {
                    glow_style
                } else if i == (highlight_pos + 11) % 12 || i == (highlight_pos + 1) % 12 {
                    // Trail effect
                    active_style
                } else {
                    dim_style
                }
            } else {
                dim_style
            };

            buf.set_string(x, y, ch, style);
        }

        // Center: play/pause icon (toggleable when source connected)
        // When scrubbing, show direction instead of play/pause
        let center_char = if self.connected {
            if self.scrub_direction < 0.0 {
                "󰑟"  // Nerd Font rewind
            } else if self.scrub_direction > 0.0 {
                "󰈑"  // Nerd Font fast-forward
            } else if self.playing {
                "▶"
            } else {
                "󰏤"
            }
        } else {
            // No source - show deck label dimmed
            &self.label.to_string()
        };

        let center_style = if self.selected {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else if self.playing {
            Style::default().fg(self.color).add_modifier(Modifier::BOLD)
        } else if self.connected {
            Style::default().fg(Color::Rgb(100, 100, 100))
        } else {
            Style::default().fg(Color::Rgb(60, 60, 60))
        };
        buf.set_string(cx, cy, center_char, center_style);

        if self.connected {
            let base_icon_style = Style::default().fg(Color::Rgb(120, 120, 120));

            // Keep one blank cell between icon and spinner ring.
            let left_x = cx.saturating_sub(4);
            let right_x = cx.saturating_add(4);

            if left_x >= area.x {
                let style = if self.prev_executed_recently {
                    Style::default().fg(TEXT_EDITING).add_modifier(Modifier::BOLD)
                } else if self.prev_selected {
                    Style::default().fg(TEXT_BRIGHT).add_modifier(Modifier::BOLD)
                } else if !self.has_prev_track {
                    Style::default().fg(Color::Rgb(70, 70, 70))
                } else {
                    base_icon_style
                };
                buf.set_string(left_x, cy, "󰒫", style);
            }
            if right_x < area.x + area.width {
                let style = if self.next_executed_recently {
                    Style::default().fg(TEXT_EDITING).add_modifier(Modifier::BOLD)
                } else if self.next_selected {
                    Style::default().fg(TEXT_BRIGHT).add_modifier(Modifier::BOLD)
                } else if !self.has_next_track {
                    Style::default().fg(Color::Rgb(70, 70, 70))
                } else {
                    base_icon_style
                };
                buf.set_string(right_x, cy, "󰒬", style);
            }
        }

        // Speed indicator below ring padding (only if not 1.0x)
        if area.height >= 5 && (self.speed - 1.0).abs() > 0.01 {
            let speed_str = format!("{:.1}x", self.speed);
            let speed_x = cx.saturating_sub(speed_str.len() as u16 / 2);
            let speed_y = cy + 3; // 1 row below ring + 1 row padding
            let speed_style = Style::default().fg(Color::Rgb(80, 80, 80));
            buf.set_string(speed_x, speed_y, &speed_str, speed_style);
        }

        // Source name below ring (1 row below ring bottom, leaving last row for separator)
        if let Some(ref name) = self.source_name {
            use unicode_width::UnicodeWidthChar;
            use unicode_width::UnicodeWidthStr;

            let max_width = area.width.saturating_sub(2) as usize;
            let name_y = (cy + 3).min(area.y + area.height.saturating_sub(2));
            let name_style = if self.selected {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(self.color)
            };
            let name_width = name.width();

            if name_width <= max_width {
                // Text fits - display normally centered
                let name_x = area.x + (area.width.saturating_sub(name_width as u16)) / 2;
                buf.set_string(name_x, name_y, name, name_style);
            } else {
                // Text too long - smooth marquee scroll
                // Build (char, width) pairs for accurate display-width tracking
                let gap = "   ";
                let scroll_chars: Vec<(char, usize)> = format!("{}{}{}", name, gap, name)
                    .chars()
                    .map(|c| (c, c.width().unwrap_or(1)))
                    .collect();
                let scroll_width: usize = scroll_chars.iter().map(|(_, w)| w).sum();

                let byte_offset = ((self.elapsed_ms / 200) as usize) % scroll_width;

                // Walk the circular buffer accumulating display width until max_width
                let mut display = String::new();
                let mut filled = 0usize;
                let mut i = byte_offset;
                while filled < max_width {
                    let (ch, w) = scroll_chars[i % scroll_chars.len()];
                    if filled + w > max_width {
                        break;
                    }
                    display.push(ch);
                    filled += w;
                    i += 1;
                }

                let name_x = area.x + (area.width.saturating_sub(filled as u16)) / 2;
                buf.set_string(name_x, name_y, &display, name_style);
            }
        }
    }
}

/// Horizontal crossfader widget for DJ mixing
pub struct Crossfader {
    /// Current position (-1.0 = A, 0.0 = center, 1.0 = B)
    position: f32,
    /// Is selected
    selected: bool,
    /// Label for deck A
    label_a: String,
    /// Label for deck B
    label_b: String,
}

impl Crossfader {
    pub fn new(position: f32) -> Self {
        Self {
            position: position.clamp(-1.0, 1.0),
            selected: false,
            label_a: "A".to_string(),
            label_b: "B".to_string(),
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn labels(mut self, a: impl Into<String>, b: impl Into<String>) -> Self {
        self.label_a = a.into();
        self.label_b = b.into();
        self
    }
}

impl Widget for Crossfader {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 10 || area.height < 3 {
            return;
        }

        let track_y = area.y + area.height / 2;
        let track_left = area.x + 3; // Space for "A " + end cap
        let track_right = area.x + area.width - 3;
        let track_width = track_right - track_left;

        // Draw track
        let track_style = Style::default().fg(METER_TRACK);
        buf.set_string(track_left, track_y, "─".repeat(track_width as usize), track_style);

        // Draw end caps (junction characters)
        buf.set_string(track_left - 1, track_y, "├", track_style);
        buf.set_string(track_right, track_y, "┤", track_style);

        // Draw tick marks at positions: -1.0, -0.5, 0.0, 0.5, 1.0
        let tick_positions = [-1.0, -0.5, 0.0, 0.5, 1.0];
        for &pos in &tick_positions {
            let x = track_left + ((pos + 1.0) / 2.0 * track_width as f32) as u16;
            if x >= track_left && x <= track_right {
                let is_center = pos.abs() < 0.01;  // Center position (0.0)
                let is_end = (pos + 1.0).abs() < 0.01 || (pos - 1.0).abs() < 0.01;  // -1.0 or 1.0

                // Skip the left end tick (would be at track_left, overlapping junction)
                // Instead draw it one position to the right
                let adjusted_x = if (pos + 1.0).abs() < 0.01 {
                    x + 1
                } else {
                    x
                };

                if is_center {
                    // Center: 3 characters tall (above, on, below) in bright white
                    let center_style = Style::default().fg(Color::White);
                    if track_y > area.y {
                        buf.set_string(adjusted_x, track_y - 1, "│", center_style);
                    }
                    buf.set_string(adjusted_x, track_y, "┼", center_style);
                    if track_y + 1 < area.y + area.height {
                        buf.set_string(adjusted_x, track_y + 1, "│", center_style);
                    }
                } else if !is_end {
                    // Intermediary marks (-0.5, 0.5): four-way intersection on the track
                    buf.set_string(adjusted_x, track_y, "┼", track_style);
                }
                // End marks: no ticks drawn (junctions handle them)
            }
        }

        // Draw deck labels
        let label_style = Style::default().fg(TEXT_DEFAULT);
        buf.set_string(area.x, track_y, &self.label_a, label_style);
        buf.set_string(area.x + area.width - self.label_b.len() as u16, track_y, &self.label_b, label_style);

        // Draw fader cap
        let cap_pos = ((self.position + 1.0) / 2.0 * track_width as f32) as u16;
        let cap_x = track_left + cap_pos;

        let cap_style = if self.selected {
            Style::default().fg(BORDER_ACTIVE).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(TEXT_BRIGHT)
        };

        buf.set_string(cap_x.saturating_sub(1), track_y - 1, "┌─┐", cap_style);
        buf.set_string(cap_x.saturating_sub(1), track_y, "│█│", cap_style);
        buf.set_string(cap_x.saturating_sub(1), track_y + 1, "└─┘", cap_style);
    }
}


/// Level meter widget (VU meter style)
pub struct LevelMeter {
    /// Current level (0.0 to 1.0)
    level: f32,
    /// Peak hold level
    peak: f32,
    /// Is stereo (draw L/R)
    stereo: bool,
    /// Right channel level (for stereo)
    level_r: f32,
    /// Right peak
    peak_r: f32,
}

impl LevelMeter {
    pub fn new(level: f32) -> Self {
        Self {
            level: level.clamp(0.0, 1.0),
            peak: 0.0,
            stereo: false,
            level_r: 0.0,
            peak_r: 0.0,
        }
    }

    #[allow(dead_code)]
    pub fn peak(mut self, peak: f32) -> Self {
        self.peak = peak.clamp(0.0, 1.0);
        self
    }

    pub fn stereo(mut self, level_l: f32, level_r: f32) -> Self {
        self.stereo = true;
        self.level = level_l.clamp(0.0, 1.0);
        self.level_r = level_r.clamp(0.0, 1.0);
        self
    }

    pub fn peaks(mut self, peak_l: f32, peak_r: f32) -> Self {
        self.peak = peak_l.clamp(0.0, 1.0);
        self.peak_r = peak_r.clamp(0.0, 1.0);
        self
    }
}

impl Widget for LevelMeter {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height < 3 {
            return;
        }

        let has_labels = self.stereo && area.height >= 4;

        let draw_single_meter = |buf: &mut Buffer, x: u16, level: f32, peak: f32| {
            // Layout: [padding] [labels if stereo] [meter bars] [padding]
            let label_offset = if has_labels { 1 } else { 0 };
            let meter_top = area.y + 1 + label_offset; // +1 for top padding
            let meter_bottom = area.y + area.height - 2; // -2 for bottom padding + 0-indexed
            let meter_height = (meter_bottom - meter_top + 1) as f32;

            if meter_height < 1.0 {
                return;
            }

            // Half-block rendering: each character cell = 2 vertical levels.
            // total_steps = meter_height * 2 (upper + lower half per cell).
            let total_steps = (meter_height * 2.0) as u16;
            let level_steps = (level * total_steps as f32) as u16;
                // Scale peak hold slightly so peak marker reaches true top when near 1.0
                let peak_display_scale: f32 = 0.98;
                let peak_step = if peak > 0.0 {
                    let s = (peak * peak_display_scale * total_steps as f32) as u16;
                    Some(s.clamp(0, total_steps - 1))
                } else {
                    None
                };

            for row in 0..(meter_height as u16) {
                let y = meter_bottom - row;
                let lower_step = row * 2;
                let upper_step = row * 2 + 1;

                let lower_filled = level_steps > lower_step;
                let upper_filled = level_steps > upper_step;

                // Determine colors for upper and lower halves
                // Use (step+1)/total_steps so topmost step maps to 100%.
                let color_for_step = |step: u16| -> Color {
                    let pct = (step as f32 + 1.0) / total_steps as f32;
                    if pct > 0.90 {
                        Color::Red
                    } else if pct > 0.75 {
                        Color::Yellow
                    } else {
                        Color::Green
                    }
                };

                let bg_color = Color::Rgb(30, 30, 30);

                if upper_filled && lower_filled {
                    // Both halves filled → full block
                    let c = color_for_step(upper_step);
                    buf.set_string(x, y, "█", Style::default().fg(c).bg(bg_color));
                } else if upper_filled && !lower_filled {
                    // Upper filled, lower empty → upper half block on empty bg
                    let c = color_for_step(upper_step);
                    buf.set_string(x, y, "▀", Style::default().fg(c).bg(bg_color));
                } else if !upper_filled && lower_filled {
                    // Lower filled, upper empty → lower half block on empty bg
                    let c = color_for_step(lower_step);
                    buf.set_string(x, y, "▄", Style::default().fg(c).bg(bg_color));
                } else {
                    // Neither filled → background tick
                    buf.set_string(x, y, " ", Style::default().bg(bg_color));
                }
            }

            // Draw peak hold as a thin line
            if let Some(peak_step) = peak_step {
                let peak_row = peak_step / 2;
                let is_upper = peak_step % 2 == 1;
                let peak_y = meter_bottom - peak_row;
                let peak_color = if peak > 0.9 { Color::Red } else { Color::White };

                if is_upper {
                    buf.set_string(x, peak_y, "▀", Style::default().fg(peak_color));
                } else {
                    buf.set_string(x, peak_y, "▄", Style::default().fg(peak_color));
                }
            }
        };

        if self.stereo && area.width >= 3 {
            let left_x = area.x + area.width / 2;
            let right_x = area.x + area.width / 2 + 2;
            draw_single_meter(buf, left_x, self.level, self.peak);
            draw_single_meter(buf, right_x, self.level_r, self.peak_r);

            // Draw L/R labels below top padding
            if area.height >= 4 {
                buf.set_string(left_x, area.y + 1, "L", Style::default().fg(Color::DarkGray));
                buf.set_string(right_x, area.y + 1, "R", Style::default().fg(Color::DarkGray));
            }
        } else {
            let x = area.x + area.width / 2;
            draw_single_meter(buf, x, self.level, self.peak);
        }
    }
}
