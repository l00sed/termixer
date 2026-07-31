//! Centralized color definitions for the TUI
//!
//! Uses ANSI named colors wherever possible so the palette adapts to the
//! user's terminal theme. RGB values are only used when no ANSI color is
//! a reasonable semantic match.

use ratatui::style::Color;

// ── Border / chrome ──────────────────────────────────────────────
pub const BORDER_DEFAULT: Color = Color::DarkGray;
pub const BORDER_NAVIGATED: Color = Color::Rgb(120, 100, 0);
pub const BORDER_ACTIVE: Color = Color::Yellow;

// ── Text ─────────────────────────────────────────────────────────
pub const TEXT_DEFAULT: Color = Color::Gray;
pub const TEXT_DIM: Color = Color::DarkGray;
pub const TEXT_BRIGHT: Color = Color::White;
pub const TEXT_GHOST: Color = Color::DarkGray;
pub const TEXT_EDITING: Color = Color::Yellow;

// ── Deck accents ─────────────────────────────────────────────────
pub const DECK_A: Color = Color::Cyan;
pub const DECK_A_BRIGHT: Color = Color::LightCyan;
pub const DECK_B: Color = Color::Blue;
pub const DECK_C: Color = Color::Yellow;

// ── Backgrounds ──────────────────────────────────────────────────
pub const BG_POPUP: Color = Color::Rgb(10, 10, 10);
pub const BG_LIGHT: Color = Color::DarkGray;

// ── Status / feedback ────────────────────────────────────────────
pub const STATUS_PLAYING: Color = Color::Green;
pub const STATUS_MUTED: Color = Color::Red;

// ── Level meter / fader ──────────────────────────────────────────
pub const METER_TRACK: Color = Color::DarkGray;
pub const METER_FILL: Color = Color::Cyan;
pub const FADER_FILL: Color = Color::Green;

// ── Separator ───────────────────────────────────────────────────
pub const SEPARATOR: Color = Color::Rgb(30, 30, 30);

// ── Sampler / pads ───────────────────────────────────────────────
#[allow(dead_code)]
pub const PAD_ACTIVE_LOW: Color = Color::DarkGray;
#[allow(dead_code)]
pub const PAD_ACTIVE_HIGH: Color = Color::DarkGray;

// ── Buttons ──────────────────────────────────────────────────────
pub const BTN_DM_PURPLE: Color = Color::Magenta;

// ── Slider gradient ──────────────────────────────────────────────
pub const SLIDER_MID: Color = Color::Gray;

// ── Mix label / hints ────────────────────────────────────────────
pub const HINT_DEFAULT: Color = Color::DarkGray;
