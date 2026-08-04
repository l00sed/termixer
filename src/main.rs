//! Tidal Mixer - A TUI audio mixer with LPF/HPF controls
//!
//! A professional-looking terminal-based audio mixer for controlling
//! multiple audio sources (like MPV instances) with vim-like navigation.

mod app;
mod audio;
mod config;
mod debug_log;
mod state;
mod ui;

use anyhow::Result;
use app::App;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect, prelude::Backend};
use std::io::stdout;
use std::path::PathBuf;
use std::time::Instant;

use crate::audio::{SourceDiscovery, SourceType};
use ui::MixerView;

fn main() -> Result<()> {
    // Initialize debug logging. When DEBUG=1, tracing output is routed to
    // the in-app debug pane and stderr is redirected to a log file so
    // crash diagnostics are preserved.
    debug_log::init_logging();

    // Parse command line arguments for MPV socket paths
    let args: Vec<String> = std::env::args().collect();
    let cli = parse_args(&args);

    // Create application
    let mut app = App::new(2); // Start with 2 channels

    // Set music directory if provided
    if let Some(dir) = cli.music_dir {
        app.set_music_dir(dir);
    }

    // Set samples directory if provided
    if let Some(dir) = cli.samples_dir {
        app.set_samples_dir(dir);
    }

    // Initialize Rust-native audio engine before configuring sources,
    // so engine.load_file() works during auto-load.
    // This happens BEFORE terminal setup so ALSA/PipeWire errors are
    // visible if the TUI never starts.
    match crate::audio::engine::AudioEngine::new() {
        Ok(engine) => {
            engine.set_mic_gain(app.mixer.master.mic_fader * 2.0);
            engine.set_mic_muted(true);
            app.mic_input_devices = engine.list_mic_inputs().unwrap_or_default();
            app.selected_mic_input_idx = app
                .mic_input_devices
                .iter()
                .position(|device| device.is_default)
                .unwrap_or(0);
            if let Some(device) = app.mic_input_devices.get(app.selected_mic_input_idx)
                && let Err(error) = engine.select_mic_input(device.index)
            {
                eprintln!("Default microphone input unavailable: {error}");
            }
            app.audio_engine = Some(engine);
        }
        Err(e) => {
            eprintln!("Audio engine unavailable (falling back to MPV/SC): {}", e);
        }
    }

    if !cli.sources.is_empty() {
        app.configure_sources(cli.sources);
    } else if cli.auto_discover {
        // Auto-discover audio sources
        let discovered = discover_sources();
        if !discovered.is_empty() {
            app.configure_sources(discovered);
        }
    }

    // Setup terminal AFTER audio init — if audio init panics or fails,
    // the terminal stays in normal mode so error output is visible.
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Check managed config files against installed destinations
    let tm_no_config = std::env::var("TM_NO_CONFIG").unwrap_or_default();
    if tm_no_config != "1" {
        let diffs = config::check_config_files();
        if !diffs.is_empty() {
            app.config_diffs = diffs;
            app.confirm_selected = true; // Y focused by default
            app.mode = app::AppMode::ConfigCheck;
            run_config_dialog(&mut terminal, &mut app)?;
        }
    }

    // Run the application
    let result = run_app(&mut terminal, &mut app);

    // Hand off MPV state before dropping app-owned clients/capture threads.
    app.cleanup();

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    Ok(())
}

struct CliArgs {
    sources: Vec<(String, String)>,
    auto_discover: bool,
    music_dir: Option<PathBuf>,
    samples_dir: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> CliArgs {
    let mut sources = Vec::new();
    let mut auto_discover = false;
    let mut music_dir = None;
    let mut samples_dir = None;
    let mut i = 1;

    while i < args.len() {
        match args[i].as_str() {
            "--source" | "-s" => {
                if i + 2 < args.len() {
                    let name = args[i + 1].clone();
                    let socket = args[i + 2].clone();
                    sources.push((name, socket));
                    i += 3;
                } else {
                    eprintln!("--source requires NAME and SOCKET_PATH arguments");
                    i += 1;
                }
            }
            "--music-dir" | "-m" => {
                if i + 1 < args.len() {
                    music_dir = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    eprintln!("--music-dir requires a PATH argument");
                    i += 1;
                }
            }
            "--samples-dir" | "-S" => {
                if i + 1 < args.len() {
                    samples_dir = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    eprintln!("--samples-dir requires a PATH argument");
                    i += 1;
                }
            }
            "--discover" | "-d" => {
                auto_discover = true;
                i += 1;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            _ => {
                i += 1;
            }
        }
    }

    // Auto-discover if no sources specified
    if sources.is_empty() {
        auto_discover = true;
    }

    // Default music dir to ~/Music when not provided
    if music_dir.is_none()
        && let Ok(home) = std::env::var("HOME")
    {
        music_dir = Some(PathBuf::from(home).join("Music"));
    }

    CliArgs {
        sources,
        auto_discover,
        music_dir,
        samples_dir,
    }
}

/// Discover audio sources (MPV sockets, PulseAudio, etc.)
fn discover_sources() -> Vec<(String, String)> {
    let mut discovery = SourceDiscovery::new();
    let sources = discovery.discover_all();

    sources
        .iter()
        .filter(|s| s.source_type == SourceType::Mpv) // Start with MPV only
        .map(|s| (s.name.clone(), s.identifier.clone()))
        .collect()
}

fn print_help() {
    println!(
        r#"Termixer - Terminal DJ Mixer

USAGE:
    termixer [OPTIONS]

OPTIONS:
    -s, --source NAME SOCKET    Add an audio source (MPV IPC socket)
    -m, --music-dir PATH        Directory for audio file browser (default: ~/Music)
    -S, --samples-dir PATH      Directory for sample pad files
                                (default: macOS ~/Library/Application Support/SuperCollider/downloaded-quarks/Dirt-Samples,
                                          Linux ~/.local/share/SuperCollider/downloaded-quarks/Dirt-Samples)
    -d, --discover              Auto-discover audio sources (default if no -s)
    -h, --help                  Show this help message

EXAMPLES:
    # Start with two MPV instances
    termixer -s "Music" /tmp/mpv-music.sock -s "Effects" /tmp/mpv-fx.sock

    # Start with music and samples directories
    termixer -m ~/Music -S ~/Samples

    # Route MPV through termixer (stable socket/fifo names):
    TM=1 mpv \
      --input-ipc-server=/tmp/termixer.sock \
      --ao=pcm \
      --ao-pcm-file=/tmp/termixer.pcm \
      --ao-pcm-waveheader=no \
      --audio-format=float \
      --audio-samplerate=48000 \
      --audio-channels=stereo \
      music.mp3

KEYBOARD CONTROLS:
    Tab, h/l     Navigate between panes (Deck A, DJ, Deck B, Master)
    Enter        Enter control select / edit mode
    Esc          Go back one mode level
    A            Open source picker for Deck A
    B            Open source picker for Deck B
    P            Toggle sample pads mode
    ?            Show help
    q            Quit
    J/K          Coarse adjustment
    +/-          Fine adjustment
    SPACE/ENTER  Toggle mute/solo
    0            Reset control to default
    c            Center pan
    m            Toggle mute
    s            Toggle solo
    ?            Show help
    q, ESC       Quit

MOUSE:
    Click        Select control
    Drag         Adjust fader/knob value
    Scroll       Fine adjustment
"#
    );
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()>
where
    <B as Backend>::Error: Send + Sync + 'static,
{
    let mut frame_counter: u8 = 0;
    let mut next_tick = Instant::now() + app.tick_rate;
    let mut play_steps: Vec<usize> = Vec::with_capacity(8);

    loop {
        // Update terminal dimensions for scroll calculations
        let terminal_size = terminal.size()?;
        app.terminal_height = terminal_size.height;
        app.mixer.terminal_height = app.terminal_height;
        app.term_width = terminal_size.width;

        // Draw
        terminal.draw(|frame| {
            let control_select =
                matches!(app.mode, app::AppMode::ControlSelect | app::AppMode::Edit);
            let pad_config = app.mode == app::AppMode::SamplePadConfig;
            let confirm_action = if let app::AppMode::ConfirmAction(a) = app.mode {
                Some(a)
            } else {
                None
            };
            let confirm_selected = app.confirm_selected;

            // Bind device lists to extend lifetime
            let master_devices = app.master_output.devices();
            let cue_devices = app.cue_output.devices();
            let mic_devices: Vec<String> = app
                .mic_input_devices
                .iter()
                .map(|device| device.display_name.clone())
                .collect();
            let selected_mic_device = app.audio_engine.as_ref().and_then(|engine| {
                let selected = engine.selected_mic_input()?;
                app.mic_input_devices
                    .iter()
                    .find(|device| device.index == selected)
                    .map(|device| device.display_name.as_str())
            });

            play_steps.clear();
            play_steps.extend(app.sequence_state.sequences.iter().map(|s| s.current_step));
            let mut view = MixerView::new(&app.mixer, &app.sample_pads)
                .show_help(app.show_help())
                .editing(app.is_editing())
                .control_select(control_select)
                .frame(frame_counter)
                .elapsed_ms(app.elapsed_ms)
                .selected_pane(app.selected_pane)
                .selected_pad_idx(app.selected_pad_idx)
                .pad_config_mode(pad_config)
                .pad_config_editing(app.sample_pads.editing_control)
                .sequences(&app.sequence_state)
                .current_play_steps(&play_steps)
                .layout_start_end(Some((app.mixer_window_start, app.mixer_window_end)))
                .master_output_device(app.master_output.selected_device())
                .cue_output_device(app.cue_output.selected_device())
                .mic_input_device(selected_mic_device)
                .output_picker_active(app.output_picker_active)
                .output_picker_target(app.output_picker_target)
                .master_output_devices(&master_devices)
                .cue_output_devices(&cue_devices)
                .mic_input_devices(&mic_devices)
                .selected_master_output_idx(app.selected_master_output_idx)
                .selected_cue_output_idx(app.selected_cue_output_idx)
                .selected_mic_input_idx(app.selected_mic_input_idx)
                .confirm_action(confirm_action)
                .confirm_selected(confirm_selected)
                .help_scroll(app.help_scroll)
                .debug_log(app.debug_log.make_contiguous())
                .debug_scroll(app.debug_scroll)
                .samples_dir(Some(&app.samples_dir));

            // Add source picker if active
            if let app::AppMode::SourcePicker(deck) = app.mode {
                view = view
                    .source_picker(deck, &app.source_picker)
                    .deck_queue_indices(&app.deck_queue_indices);
            }

            // Add sample picker if active
            if let app::AppMode::SamplePicker(pad_idx) = app.mode {
                view = view.sample_picker(pad_idx, &app.source_picker);
            }

            frame.render_widget(view, frame.area());

            // Keep mixer window in sync with viewport (handles resize)
            app.ensure_mixer_pane_visible();

            // Update areas for mouse hit testing
            let (channel_areas, crossfader_area, master_area, cue_area, loops_area, pad_areas) =
                calculate_all_areas(frame.area(), app.mixer.channels.len(), app.selected_pane);
            app.update_channel_areas(channel_areas);
            app.update_pane_areas(
                crossfader_area,
                master_area,
                cue_area,
                loops_area,
                pad_areas,
            );
        })?;

        // Handle events
        let timeout = next_tick.saturating_duration_since(Instant::now());

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    // Ctrl+c always quits
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return Ok(());
                    }
                    app.handle_key(key);
                }
                Event::Mouse(mouse) => {
                    app.handle_mouse(mouse);
                }
                Event::Resize(_, _) => {
                    // Terminal will redraw automatically
                }
                _ => {}
            }
        }

        // Check if we should quit
        if app.should_quit {
            return Ok(());
        }

        // Drain tracing log queue into debug_log buffer
        debug_log::drain_log_queue(&mut app.debug_log);

        // Tick for animations/meters
        if Instant::now() >= next_tick {
            app.update_sequences();
            app.tick();
            app.last_tick = std::time::Instant::now();
            frame_counter = frame_counter.wrapping_add(1);
            next_tick += app.tick_rate;

            let now = Instant::now();
            if now > next_tick + app.tick_rate {
                next_tick = now + app.tick_rate;
            }
        }
    }
}

/// Mini event loop for the config check dialog. Blocks until the user
/// confirms or cancels, then returns to the caller.
fn run_config_dialog<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()>
where
    <B as Backend>::Error: Send + Sync + 'static,
{
    use ratatui::prelude::Widget;
    use std::time::Duration;

    loop {
        terminal.draw(|frame| {
            let diffs = &app.config_diffs;
            let files = config::managed_files();
            let confirm_selected = app.confirm_selected;
            let msg = app.config_check_msg.as_deref();

            let area = frame.area();
            let popup_width = 52u16.min(area.width.saturating_sub(4));
            let num_files = diffs.len() as u16;
            let popup_height = (10 + num_files).min(area.height.saturating_sub(4));
            let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
            let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
            let popup_area =
                ratatui::layout::Rect::new(popup_x, popup_y, popup_width, popup_height);

            // Clear and draw background
            let buf = frame.buffer_mut();
            ratatui::widgets::Clear.render(popup_area, buf);
            for y in popup_area.y..popup_area.y + popup_area.height {
                for x in popup_area.x..popup_area.x + popup_area.width {
                    buf.set_string(
                        x,
                        y,
                        " ",
                        ratatui::style::Style::default().bg(ratatui::style::Color::Rgb(20, 20, 20)),
                    );
                }
            }

            let block = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(ratatui::style::Color::Yellow))
                .title(ratatui::text::Span::styled(
                    " UPDATE CONFIG FILES ",
                    ratatui::style::Style::default()
                        .fg(ratatui::style::Color::Yellow)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ));

            let inner = block.inner(popup_area);
            block.render(popup_area, buf);

            let mut lines: Vec<ratatui::text::Line> = Vec::new();
            lines.push(ratatui::text::Line::from(""));

            let summary = format!(" {} file(s) differ from installed:", diffs.len());
            lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                summary,
                ratatui::style::Style::default().fg(ratatui::style::Color::White),
            )));
            lines.push(ratatui::text::Line::from(""));

            for diff in diffs {
                let label = files[diff.file_index].label;
                lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                    format!("  \u{2022} {}", label),
                    ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(180, 180, 180)),
                )));
            }

            lines.push(ratatui::text::Line::from(""));

            if let Some(m) = msg {
                lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                    format!("  {}", m),
                    ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(0, 200, 150)),
                )));
                lines.push(ratatui::text::Line::from(""));
            }

            lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                "  TIP: Add `export TM_NO_CONFIG=1` to your shell",
                ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(100, 100, 100)),
            )));
            lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                "  environment to self-manage your MPV config.",
                ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(100, 100, 100)),
            )));
            lines.push(ratatui::text::Line::from(""));

            // Y/n hint
            let y_style = if confirm_selected {
                ratatui::style::Style::default()
                    .fg(ratatui::style::Color::White)
                    .add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(100, 100, 100))
            };
            let n_style = if !confirm_selected {
                ratatui::style::Style::default()
                    .fg(ratatui::style::Color::White)
                    .add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(100, 100, 100))
            };
            let dim =
                ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(100, 100, 100));

            lines.push(ratatui::text::Line::from(vec![
                ratatui::text::Span::raw("    "),
                ratatui::text::Span::styled("[", y_style),
                ratatui::text::Span::styled("Y", y_style),
                ratatui::text::Span::styled("]", y_style),
                ratatui::text::Span::styled("es", y_style),
                ratatui::text::Span::styled("  ", dim),
                ratatui::text::Span::styled("[", n_style),
                ratatui::text::Span::styled("n", n_style),
                ratatui::text::Span::styled("]", n_style),
                ratatui::text::Span::styled("o", n_style),
            ]));

            let paragraph = ratatui::widgets::Paragraph::new(lines);
            paragraph.render(inner, buf);
        })?;

        if crossterm::event::poll(Duration::from_millis(50))?
            && let crossterm::event::Event::Key(key) = crossterm::event::read()?
        {
            app.handle_config_check_key(key);
            if !matches!(app.mode, app::AppMode::ConfigCheck) {
                break;
            }
        }
    }
    Ok(())
}

/// Returns (channel_areas, crossfader_area, master_area, cue_area, loops_area, pad_areas).
#[allow(clippy::type_complexity)]
fn calculate_all_areas(
    area: Rect,
    num_channels: usize,
    selected_pane: app::SelectedPane,
) -> (
    Vec<app::ChannelArea>,
    Option<app::PaneArea>,
    Option<app::PaneArea>,
    Option<app::PaneArea>,
    Option<app::PaneArea>,
    Vec<(usize, u16, u16, u16, u16)>,
) {
    use crate::state::ChannelControl;

    if num_channels == 0 {
        return (Vec::new(), None, None, None, None, Vec::new());
    }

    // Mirror the layout from MixerView::render_mixer_full_width
    let header_height = 3u16;
    let footer_height = 3u16;
    let mixer_area = Rect::new(
        area.x,
        area.y + header_height,
        area.width,
        area.height.saturating_sub(header_height + footer_height),
    );

    // Mirror the renderer's layout so hit-testing matches what's drawn.
    let layout = app::MixerLayout::compute(mixer_area.width, selected_pane, None, None);

    // Build chunk x-positions for visible columns only (start..=end).
    // This matches the renderer which builds Layout constraints the same way.
    let start = layout.start as usize;
    let end = layout.end as usize;
    let mut chunk_x = [0u16; 5];
    let mut x_cursor = mixer_area.x;
    for (i, slot) in chunk_x.iter_mut().enumerate().take(end + 1).skip(start) {
        *slot = x_cursor;
        let w = match i {
            0 => layout.deck_a,
            1 => layout.dj,
            2 => layout.deck_b,
            3 => layout.deck_c,
            4 => layout.master,
            _ => unreachable!(),
        };
        x_cursor += w;
    }

    // DJ center vertical: [Pads] [Sequences] [Crossfader]
    // PADS gets ~2/3, SEQUENCES ~1/3, crossfader gets 5 rows
    let crossfader_height = 5u16;
    let shared_height = mixer_area.height.saturating_sub(crossfader_height);
    let pads_h = (shared_height * 2) / 3;
    let loops_height = shared_height.saturating_sub(pads_h);

    let pads_y = mixer_area.y;
    let loops_y = pads_y + pads_h;
    let crossfader_y = loops_y + loops_height;

    // Master column vertical: [CUE] [Master]
    let cue_h = (mixer_area.height as f32 * 0.66) as u16;
    let master_h = mixer_area.height.saturating_sub(cue_h);
    let cue_y = mixer_area.y;
    let master_y = cue_y + cue_h;

    // ── Channel strip areas (Deck A, column 0) ──
    let dj_center_width = layout.dj;
    let channel_width = (dj_center_width / num_channels as u16).max(10);
    let mut channel_areas = Vec::with_capacity(num_channels);
    if start == 0 {
        for i in 0..num_channels {
            let x = chunk_x[0] + i as u16 * channel_width;
            let w = channel_width.min(mixer_area.width.saturating_sub(x - mixer_area.x));
            let inner_y = mixer_area.y + 1;
            let inner_h = mixer_area.height.saturating_sub(2);
            let row_heights = [4, 4, 4, 3, 3, 3, 3, 4, 8, 3, 3, 3];
            let total: u16 = row_heights.iter().sum();
            let scale = inner_h as f32 / total as f32;
            let controls = [
                ChannelControl::EqHigh,
                ChannelControl::EqMid,
                ChannelControl::EqLow,
                ChannelControl::FilterCutoff,
                ChannelControl::FilterFreq,
                ChannelControl::LfoShape,
                ChannelControl::LfoSpeed,
                ChannelControl::Pan,
                ChannelControl::Fader,
                ChannelControl::Mute,
                ChannelControl::Solo,
            ];
            let mut control_rows = Vec::new();
            let mut current_y = inner_y;
            for (j, &control) in controls.iter().enumerate() {
                let row_h = (row_heights[j] as f32 * scale) as u16;
                let y_end = current_y + row_h;
                control_rows.push((control, current_y, y_end));
                current_y = y_end;
            }
            channel_areas.push(app::ChannelArea {
                bounds: (x, mixer_area.y, w, mixer_area.height),
                control_rows,
            });
        }
    }

    // ── Crossfader area (DJ center, column 1) ──
    let crossfader_area = if start <= 1 && 1 <= end {
        Some(app::PaneArea {
            x: chunk_x[1],
            y: crossfader_y,
            w: dj_center_width,
            h: crossfader_height,
        })
    } else {
        None
    };

    // ── Master area (column 4) ──
    let master_width = layout.master;
    let master_area = if start <= 4 && 4 <= end {
        Some(app::PaneArea {
            x: chunk_x[4],
            y: master_y,
            w: master_width.min(mixer_area.width.saturating_sub(chunk_x[4] - mixer_area.x)),
            h: master_h,
        })
    } else {
        None
    };

    // ── CUE area (column 4) ──
    let cue_area = if start <= 4 && 4 <= end {
        Some(app::PaneArea {
            x: chunk_x[4],
            y: cue_y,
            w: master_width.min(mixer_area.width.saturating_sub(chunk_x[4] - mixer_area.x)),
            h: cue_h,
        })
    } else {
        None
    };

    // ── Loops area (DJ center, column 1) ──
    let loops_area = if start <= 1 && 1 <= end {
        Some(app::PaneArea {
            x: chunk_x[1],
            y: loops_y,
            w: dj_center_width,
            h: loops_height,
        })
    } else {
        None
    };

    // ── Pad areas (4x4 grid centered in pads area, DJ center column 1) ──
    let mut pad_areas = Vec::new();
    if start <= 1 && 1 <= end && pads_h >= 6 && dj_center_width >= 12 {
        let cell_w = 5u16;
        let cell_h = 3u16;
        let gap_x = 1u16;
        let grid_w = cell_w * 4 + gap_x * 3;
        let grid_h = cell_h * 4;
        let offset_x = (dj_center_width.saturating_sub(grid_w)) / 2;
        let offset_y = (pads_h.saturating_sub(grid_h)) / 2;
        for row in 0..4u16 {
            for col in 0..4u16 {
                let pad_idx = (row * 4 + col) as usize;
                let x = chunk_x[1] + offset_x + col * (cell_w + gap_x);
                let y = pads_y + offset_y + row * cell_h;
                if x + cell_w <= chunk_x[1] + dj_center_width && y + cell_h <= pads_y + pads_h {
                    pad_areas.push((pad_idx, x, y, cell_w, cell_h));
                }
            }
        }
    }

    (
        channel_areas,
        crossfader_area,
        master_area,
        cue_area,
        loops_area,
        pad_areas,
    )
}
