//! Synchronous MPV IPC client with background audio metering
//!
//! The main connection handles commands (volume, EQ, filters, etc).
//! A background thread on a separate IPC connection polls `af-metadata`
//! at ~60Hz and stores peak/RMS in atomics — same pattern as the
//! ScreenCaptureKit capture for master meters.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::audio::bpm::pitch_class_to_camelot;

/// Audio device entry from MPV's `audio-device-list` property.
#[derive(Debug, Clone)]
pub struct AudioDeviceEntry {
    /// CoreAudio UID (e.g. "coreaudio/AppleHDAEngineOutput:1.0.0")
    pub name: String,
    /// Human-readable name (e.g. "MacBook Pro Speakers")
    pub description: String,
}

/// Lock-free meter storage — written by the polling thread, read by the UI.
/// Peaks use CAS to keep the highest value between UI ticks (same as master).
/// Also includes an onset-detection ring buffer for real-time BPM.
pub struct AtomicMeter {
    peak_l: AtomicU32,
    peak_r: AtomicU32,
    rms_l: AtomicU32,
    rms_r: AtomicU32,
    /// Detected BPM from onset detection (fixed-point ×100), 0 = not yet detected.
    detected_bpm: AtomicU32,
}

impl AtomicMeter {
    fn new() -> Self {
        Self {
            peak_l: AtomicU32::new(0),
            peak_r: AtomicU32::new(0),
            rms_l: AtomicU32::new(0),
            rms_r: AtomicU32::new(0),
            detected_bpm: AtomicU32::new(0),
        }
    }

    /// CAS loop: only update if new value is higher (peak hold).
    fn cas_peak(atom: &AtomicU32, new_val: f32) {
        let new_bits = new_val.to_bits();
        let mut current = atom.load(Ordering::Relaxed);
        loop {
            if new_bits <= current {
                return;
            }
            match atom.compare_exchange_weak(
                current,
                new_bits,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    fn decay_peaks(&self, factor: f32) {
        let decay = |atom: &AtomicU32| {
            let current = f32::from_bits(atom.load(Ordering::Relaxed));
            atom.store((current * factor).to_bits(), Ordering::Relaxed);
        };
        decay(&self.peak_l);
        decay(&self.peak_r);
    }

    fn store(&self, peak_l: f32, peak_r: f32, rms_l: f32, rms_r: f32) {
        Self::cas_peak(&self.peak_l, peak_l);
        Self::cas_peak(&self.peak_r, peak_r);
        self.rms_l.store(rms_l.to_bits(), Ordering::Relaxed);
        self.rms_r.store(rms_r.to_bits(), Ordering::Relaxed);
    }

    fn load(&self) -> (f32, f32, f32, f32) {
        (
            f32::from_bits(self.peak_l.load(Ordering::Relaxed)),
            f32::from_bits(self.peak_r.load(Ordering::Relaxed)),
            f32::from_bits(self.rms_l.load(Ordering::Relaxed)),
            f32::from_bits(self.rms_r.load(Ordering::Relaxed)),
        )
    }
}

/// Synchronous MPV IPC client
pub struct MpvClient {
    reader: Option<BufReader<UnixStream>>,
    socket_path: String,
    request_id: u64,
    meters: Arc<AtomicMeter>,
    stop_flag: Arc<AtomicBool>,
    /// UI-side filter smoothing state. `set_lpf`/`set_hpf` update the
    /// *target*; `tick_smooth_filters` glides `current` toward `target`
    /// and sends af-command only when the delta exceeds a threshold.
    /// This turns a burst of coefficient jumps (crackle) into a small
    /// number of small, evenly-spaced updates ffmpeg can absorb.
    target_lpf: Option<f32>,
    target_hpf: Option<f32>,
    current_lpf: f32,
    current_hpf: f32,
    last_sent_lpf: f32,
    last_sent_hpf: f32,
    /// Tick counter used to gate af-command sends to ~10 Hz.
    send_tick: u32,
}

impl MpvClient {
    fn value_to_f32(val: &serde_json::Value) -> Option<f32> {
        val.as_f64()
            .map(|v| v as f32)
            .or_else(|| val.as_i64().map(|v| v as f32))
            .or_else(|| val.as_u64().map(|v| v as f32))
            .or_else(|| val.as_str().and_then(|s| s.trim().parse::<f32>().ok()))
    }

    pub fn new(socket_path: impl Into<String>) -> Self {
        Self {
            reader: None,
            socket_path: socket_path.into(),
            request_id: 0,
            meters: Arc::new(AtomicMeter::new()),
            stop_flag: Arc::new(AtomicBool::new(false)),
            target_lpf: None,
            target_hpf: None,
            current_lpf: 20000.0,
            current_hpf: 20.0,
            last_sent_lpf: 20000.0,
            last_sent_hpf: 20.0,
            send_tick: 0,
        }
    }

    /// Connect to MPV socket
    pub fn connect(&mut self) -> Result<(), String> {
        let path = Path::new(&self.socket_path);
        if !path.exists() {
            return Err(format!("Socket not found: {}", self.socket_path));
        }

        match UnixStream::connect(path) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .ok();
                stream
                    .set_write_timeout(Some(Duration::from_millis(100)))
                    .ok();
                self.reader = Some(BufReader::new(stream));
                Ok(())
            }
            Err(e) => Err(format!("Failed to connect: {}", e)),
        }
    }

    pub fn set_timeouts(&mut self, read_ms: u64, write_ms: u64) {
        if let Some(reader) = self.reader.as_mut() {
            let _ = reader
                .get_mut()
                .set_read_timeout(Some(Duration::from_millis(read_ms)));
            let _ = reader
                .get_mut()
                .set_write_timeout(Some(Duration::from_millis(write_ms)));
        }
    }

    /// Start the background metering thread (call after `ensure_astats`).
    pub fn start_metering(&self) {
        let socket_path = self.socket_path.clone();
        let meters = self.meters.clone();
        let stop_flag = self.stop_flag.clone();

        thread::Builder::new()
            .name("mpv-meter".into())
            .spawn(move || {
                Self::metering_loop(&socket_path, meters, stop_flag);
            })
            .ok();
    }

    fn metering_loop(socket_path: &str, meters: Arc<AtomicMeter>, stop_flag: Arc<AtomicBool>) {
        let stream = match UnixStream::connect(Path::new(socket_path)) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Metering thread: failed to connect to {}: {}",
                    socket_path,
                    e
                );
                return;
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_millis(50)))
            .ok();
        let mut reader = BufReader::new(stream);
        let mut req_id: u64 = 0;
        let mut line = String::new();

        // Onset detection state
        let mut energy_ring: Vec<f32> = Vec::with_capacity(100);
        let mut onset_times: Vec<Instant> = Vec::with_capacity(32);
        let mut last_onset = Instant::now() - Duration::from_secs(5);

        while !stop_flag.load(Ordering::Relaxed) {
            req_id += 1;
            let cmd = serde_json::json!({
                "command": ["get_property", "af-metadata/astats"],
                "request_id": req_id
            });
            let mut cmd_str = cmd.to_string();
            cmd_str.push('\n');

            if reader.get_mut().write_all(cmd_str.as_bytes()).is_err() {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            let _ = reader.get_mut().flush();

            let mut got_response = false;
            for _ in 0..10 {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&line)
                            && resp.get("request_id").and_then(|v| v.as_u64()) == Some(req_id)
                        {
                            if let Some(data) = resp.get("data")
                                && let Some(obj) = data.as_object()
                            {
                                let get_dbfs = |key: &str| -> f32 {
                                    obj.get(key)
                                        .and_then(|v| v.as_str())
                                        .and_then(|s| s.parse::<f32>().ok())
                                        .unwrap_or(f32::NEG_INFINITY)
                                };

                                let peak_l_db = get_dbfs("lavfi.astats.1.Peak_level");
                                let peak_r_db = get_dbfs("lavfi.astats.2.Peak_level");
                                let rms_l_db = get_dbfs("lavfi.astats.1.RMS_level");
                                let rms_r_db = get_dbfs("lavfi.astats.2.RMS_level");

                                let dbfs_to_linear = |db: f32| -> f32 {
                                    if db <= f32::NEG_INFINITY {
                                        0.0
                                    } else {
                                        (10.0_f32).powf(db / 20.0).clamp(0.0, 1.0)
                                    }
                                };

                                let r_l = dbfs_to_linear(rms_l_db);
                                let r_r = dbfs_to_linear(rms_r_db);
                                meters.store(
                                    dbfs_to_linear(peak_l_db),
                                    dbfs_to_linear(peak_r_db),
                                    r_l,
                                    r_r,
                                );

                                // Onset detection: energy from RMS
                                let energy = (r_l + r_r) * 0.5;
                                energy_ring.push(energy);
                                if energy_ring.len() > 100 {
                                    energy_ring.remove(0);
                                }

                                if energy_ring.len() >= 20 {
                                    let mean: f32 =
                                        energy_ring.iter().sum::<f32>() / energy_ring.len() as f32;
                                    let var: f32 =
                                        energy_ring.iter().map(|e| (e - mean).powi(2)).sum::<f32>()
                                            / energy_ring.len() as f32;
                                    let threshold = mean + 1.2 * var.sqrt();

                                    let now = Instant::now();
                                    let ms_since =
                                        now.duration_since(last_onset).as_millis() as f32;

                                    if energy > threshold
                                        && energy > mean * 1.05
                                        && ms_since >= 250.0
                                    {
                                        onset_times.push(now);
                                        last_onset = now;
                                        if onset_times.len() > 32 {
                                            onset_times.remove(0);
                                        }

                                        // BPM from median IOI (≥3 onsets)
                                        if onset_times.len() >= 3 {
                                            let mut iois: Vec<f32> = onset_times
                                                .windows(2)
                                                .map(|w| {
                                                    w[1].duration_since(w[0]).as_millis() as f32
                                                })
                                                .filter(|ioi| *ioi >= 250.0 && *ioi <= 1000.0)
                                                .collect();
                                            if iois.len() >= 2 {
                                                iois.sort_by(|a, b| a.partial_cmp(b).unwrap());
                                                let median = iois[iois.len() / 2];
                                                let bpm = (60000.0 / median * 100.0) as u32;
                                                if (6000..=20000).contains(&bpm) {
                                                    // 60.00–200.00 BPM
                                                    meters
                                                        .detected_bpm
                                                        .store(bpm, Ordering::Relaxed);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            got_response = true;
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }

            if !got_response {
                let (pl, pr, rl, rr) = meters.load();
                meters.store(pl, pr, rl * 0.85, rr * 0.85);
            }

            thread::sleep(Duration::from_millis(8));
        }
    }

    pub fn send_command(
        &mut self,
        command: Vec<serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, String> {
        let reader = self.reader.as_mut().ok_or("Not connected")?;

        self.request_id += 1;
        let cmd = serde_json::json!({
            "command": command,
            "request_id": self.request_id
        });

        let mut cmd_str = cmd.to_string();
        cmd_str.push('\n');

        reader
            .get_mut()
            .write_all(cmd_str.as_bytes())
            .map_err(|e| format!("Write failed: {}", e))?;
        reader
            .get_mut()
            .flush()
            .map_err(|e| format!("Flush failed: {}", e))?;

        let mut line = String::new();

        for _ in 0..10 {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return Err("Connection closed".to_string()),
                Ok(_) => {
                    if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&line)
                        && resp.get("request_id").and_then(|v| v.as_u64()) == Some(self.request_id)
                    {
                        let error = resp
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("success");
                        if error != "success" {
                            return Err(format!("MPV error: {}", error));
                        }
                        return Ok(resp.get("data").cloned());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    break;
                }
                Err(e) => return Err(format!("Read failed: {}", e)),
            }
        }

        Ok(None)
    }

    pub fn set_property(&mut self, property: &str, value: serde_json::Value) -> Result<(), String> {
        self.send_command(vec!["set_property".into(), property.into(), value])?;
        Ok(())
    }

    pub fn get_property(&mut self, property: &str) -> Result<serde_json::Value, String> {
        let result = self.send_command(vec!["get_property".into(), property.into()])?;
        result.ok_or_else(|| "No data returned".to_string())
    }

    pub fn set_volume(&mut self, volume: f32) -> Result<(), String> {
        self.set_property("volume", serde_json::json!(volume.clamp(0.0, 200.0)))
    }

    pub fn set_mute(&mut self, muted: bool) -> Result<(), String> {
        self.set_property("mute", serde_json::json!(muted))
    }

    pub fn get_volume(&mut self) -> Result<f32, String> {
        let val = self.get_property("volume")?;
        val.as_f64()
            .map(|v| v as f32)
            .ok_or("Invalid volume value".to_string())
    }

    pub fn get_pause(&mut self) -> Result<bool, String> {
        let val = self.get_property("pause")?;
        val.as_bool().ok_or("Invalid pause value".to_string())
    }

    pub fn set_pause(&mut self, paused: bool) -> Result<(), String> {
        self.set_property("pause", serde_json::json!(paused))
    }

    pub fn set_speed(&mut self, speed: f32) -> Result<(), String> {
        self.set_property("speed", serde_json::json!(speed))
    }

    /// Get MPV's displayed position in the current file.
    pub fn get_time_pos(&mut self) -> Result<f32, String> {
        self.get_property("time-pos")
            .ok()
            .and_then(|v| Self::value_to_f32(&v))
            .or_else(|| {
                self.get_property("playback-time")
                    .ok()
                    .and_then(|v| Self::value_to_f32(&v))
            })
            .ok_or("Invalid time-pos".to_string())
    }

    /// Get total duration of current track in seconds.
    pub fn get_duration(&mut self) -> Result<f32, String> {
        if let Some(duration) = self
            .get_property("duration")
            .ok()
            .and_then(|v| Self::value_to_f32(&v))
            .or_else(|| {
                self.get_property("duration/full")
                    .ok()
                    .and_then(|v| Self::value_to_f32(&v))
            })
        {
            return Ok(duration.max(0.0));
        }

        if let (Some(time_pos), Some(remaining)) = (
            self.get_time_pos().ok(),
            self.get_property("playtime-remaining")
                .ok()
                .and_then(|v| Self::value_to_f32(&v)),
        ) {
            return Ok((time_pos + remaining).max(0.0));
        }

        Err("Invalid duration".to_string())
    }

    pub fn get_playlist_nav_available(&mut self) -> Result<(bool, bool), String> {
        let to_i64 = |v: &serde_json::Value| -> Option<i64> {
            v.as_i64()
                .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
                .or_else(|| v.as_f64().map(|n| n as i64))
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        };

        let playlist = self
            .get_property("playlist")
            .ok()
            .and_then(|v| v.as_array().cloned());

        let pos_from_playlist = playlist.as_ref().and_then(|entries| {
            entries
                .iter()
                .position(|item| {
                    item.get("current")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                        || item
                            .get("playing")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                })
                .map(|idx| idx as i64)
        });

        let pos = pos_from_playlist
            .or_else(|| {
                self.get_property("playlist-pos")
                    .ok()
                    .and_then(|v| to_i64(&v))
            })
            .or_else(|| {
                self.get_property("playlist-pos-1")
                    .ok()
                    .and_then(|v| to_i64(&v))
                    .map(|v| v - 1)
            });

        let count = playlist
            .as_ref()
            .map(|a| a.len() as i64)
            .or_else(|| {
                self.get_property("playlist-count")
                    .ok()
                    .and_then(|v| to_i64(&v))
            })
            .unwrap_or(0);

        if count <= 0 {
            return Ok((false, false));
        }

        let has_prev = pos.map(|p| p > 0).unwrap_or(false);
        let has_next = pos.map(|p| p >= 0 && p < count - 1).unwrap_or(count > 1);
        Ok((has_prev, has_next))
    }

    pub fn playlist_next(&mut self) -> Result<(), String> {
        self.send_command(vec![
            serde_json::Value::String("playlist-next".to_string()),
            serde_json::Value::String("weak".to_string()),
        ])?;
        Ok(())
    }

    pub fn playlist_prev(&mut self) -> Result<(), String> {
        self.send_command(vec![
            serde_json::Value::String("playlist-prev".to_string()),
            serde_json::Value::String("weak".to_string()),
        ])?;
        Ok(())
    }

    fn has_filter(&mut self, label: &str) -> bool {
        if let Ok(af) = self.get_property("af")
            && let Some(arr) = af.as_array()
        {
            return arr
                .iter()
                .any(|f| f.get("label").and_then(|v| v.as_str()) == Some(label));
        }
        false
    }

    /// Send a command to an existing audio filter without removing/re-adding it.
    /// lavfi filters (lowpass, highpass, etc.) support parameter changes via commands,
    /// avoiding the crackling that comes from rebuilding the filter graph.
    pub fn af_command(&mut self, label: &str, cmd: &str, arg: &str) -> Result<(), String> {
        // NOTE: No @ prefix — MPV stores labels without it internally.
        // af add @lpf:... creates label "lpf", so af-command uses "lpf".
        self.send_command(vec![
            "af-command".into(),
            label.into(),
            cmd.into(),
            arg.into(),
        ])?;
        Ok(())
    }

    /// Store target LPF frequency. Actual af-command is sent later by
    /// `tick_smooth_filters` at ~20 Hz with a UI-side smoother — this
    /// eliminates crackle from ffmpeg biquad coefficient jumps that
    /// happen on every raw af-command.
    pub fn set_lpf(&mut self, freq: f32) -> Result<(), String> {
        self.target_lpf = Some(freq.clamp(20.0, 20000.0));
        Ok(())
    }

    /// Store target HPF frequency. See `set_lpf` for rationale.
    pub fn set_hpf(&mut self, freq: f32) -> Result<(), String> {
        self.target_hpf = Some(freq.clamp(20.0, 20000.0));
        Ok(())
    }

    /// Immediately send a raw LPF af-command (bypasses smoother). Used at
    /// startup and when the filter needs to snap (e.g. reset).
    ///
    /// The filter is added with `width_type=q:width=0.5` — a lower-than-
    /// Butterworth Q. This softens the resonant peak that would otherwise
    /// amplify the coefficient-jump transient whenever `af-command frequency`
    /// updates the biquad. Trade-off: slightly less "punch" at the knee, in
    /// exchange for audibly cleaner sweeps.
    fn send_lpf_raw(&mut self, freq: f32) -> Result<(), String> {
        if freq >= 19000.0 {
            if self.has_filter("lpf") {
                self.af_command("lpf", "frequency", "20000").ok();
            }
        } else if self.has_filter("lpf") {
            if self
                .af_command("lpf", "frequency", &format!("{:.0}", freq))
                .is_err()
            {
                self.send_command(vec!["af".into(), "remove".into(), "@lpf".into()])
                    .ok();
                let filter = format!("@lpf:lavfi=[lowpass=f={:.0}:width_type=q:width=0.5]", freq);
                self.send_command(vec!["af".into(), "add".into(), filter.into()])?;
            }
        } else {
            let filter = format!("@lpf:lavfi=[lowpass=f={:.0}:width_type=q:width=0.5]", freq);
            self.send_command(vec!["af".into(), "add".into(), filter.into()])?;
        }
        Ok(())
    }

    fn send_hpf_raw(&mut self, freq: f32) -> Result<(), String> {
        if freq <= 25.0 {
            if self.has_filter("hpf") {
                self.af_command("hpf", "frequency", "20").ok();
            }
        } else if self.has_filter("hpf") {
            if self
                .af_command("hpf", "frequency", &format!("{:.0}", freq))
                .is_err()
            {
                self.send_command(vec!["af".into(), "remove".into(), "@hpf".into()])
                    .ok();
                let filter = format!("@hpf:lavfi=[highpass=f={:.0}:width_type=q:width=0.5]", freq);
                self.send_command(vec!["af".into(), "add".into(), filter.into()])?;
            }
        } else {
            let filter = format!("@hpf:lavfi=[highpass=f={:.0}:width_type=q:width=0.5]", freq);
            self.send_command(vec!["af".into(), "add".into(), filter.into()])?;
        }
        Ok(())
    }

    /// Glide `current_{lpf,hpf}` toward `target_{lpf,hpf}` and send a
    /// stepped af-command. Call once per UI tick (~20 Hz).
    ///
    /// Design notes:
    /// - Sends are gated to ~10 Hz (every other tick) — halves ffmpeg
    ///   biquad coefficient-jump transients per second, and reduces MPV
    ///   IPC pressure that can stall the MPV audio thread.
    /// - Smoothing is on **log-frequency** so a 20 Hz → 20 kHz sweep is
    ///   evenly paced to the ear.
    /// - `MIN_DELTA_HZ = 5` skips sub-audible micro-updates.
    /// - When the smoothed value is within a musical threshold of target,
    ///   we snap and clear the target — no more sends until user moves knob.
    pub fn tick_smooth_filters(&mut self) {
        const ALPHA: f32 = 0.30;
        const MIN_DELTA_HZ: f32 = 5.0;
        // Musical settle: 1 semitone ≈ ratio 1.06 — below this the sweep
        // is done to any listener. Snap and stop sending.
        const SETTLE_RATIO: f32 = 1.02;

        // Rate-limit to every other tick (~10 Hz at 20 fps UI).
        let should_send = {
            let n = self.send_tick.wrapping_add(1);
            self.send_tick = n;
            n & 1 == 0
        };

        if let Some(target) = self.target_lpf {
            let log_cur = self.current_lpf.max(1.0).ln();
            let log_tar = target.max(1.0).ln();
            let log_new = log_cur + (log_tar - log_cur) * ALPHA;
            let new = log_new.exp().clamp(20.0, 20000.0);
            self.current_lpf = new;
            let ratio = (new.max(target) / new.min(target)).max(1.0);
            if ratio < SETTLE_RATIO {
                // Snap: send the exact target once, then clear.
                if (target - self.last_sent_lpf).abs() >= MIN_DELTA_HZ
                    && self.send_lpf_raw(target).is_ok()
                {
                    self.last_sent_lpf = target;
                }
                self.current_lpf = target;
                self.target_lpf = None;
            } else if should_send
                && (new - self.last_sent_lpf).abs() >= MIN_DELTA_HZ
                && self.send_lpf_raw(new).is_ok()
            {
                self.last_sent_lpf = new;
            }
        }

        if let Some(target) = self.target_hpf {
            let log_cur = self.current_hpf.max(1.0).ln();
            let log_tar = target.max(1.0).ln();
            let log_new = log_cur + (log_tar - log_cur) * ALPHA;
            let new = log_new.exp().clamp(20.0, 20000.0);
            self.current_hpf = new;
            let ratio = (new.max(target) / new.min(target)).max(1.0);
            if ratio < SETTLE_RATIO {
                if (target - self.last_sent_hpf).abs() >= MIN_DELTA_HZ
                    && self.send_hpf_raw(target).is_ok()
                {
                    self.last_sent_hpf = target;
                }
                self.current_hpf = target;
                self.target_hpf = None;
            } else if should_send
                && (new - self.last_sent_hpf).abs() >= MIN_DELTA_HZ
                && self.send_hpf_raw(new).is_ok()
            {
                self.last_sent_hpf = new;
            }
        }
    }

    pub fn set_eq(&mut self, low: f32, mid: f32, high: f32) -> Result<(), String> {
        if self.has_filter("eq") {
            self.send_command(vec!["af".into(), "remove".into(), "@eq".into()])
                .ok();
        }

        let mut filters = Vec::new();

        if low < 0.0 {
            let normalized = (-low / 24.0).clamp(0.0, 1.0);
            let low_freq: f32 = 20000.0 * (500.0f32 / 20000.0).powf(normalized);
            if low_freq < 20000.0 {
                filters.push(format!("lowpass=f={:.0}", low_freq));
            }
        } else if low > 0.1 {
            let gain = (low / 2.0).clamp(0.0, 12.0);
            filters.push(format!("equalizer=f=120:t=h:w=220:g={:.1}", gain));
        }

        // Mid: equalizer with sweepable frequency and gain
        if mid.abs() > 0.1 {
            let normalized = mid.abs() / 24.0;
            let mid_freq: f32 = 200.0 * (8000.0f32 / 200.0).powf(normalized);
            let mid_gain: f32 = (mid / 2.0).clamp(-12.0, 12.0);
            filters.push(format!(
                "equalizer=f={:.0}:t=h:w=500:g={:.1}",
                mid_freq, mid_gain
            ));
        }

        if high < 0.0 {
            let normalized = (-high / 24.0).clamp(0.0, 1.0);
            let high_freq: f32 = 20.0 * (5000.0f32 / 20.0).powf(normalized);
            if high_freq > 20.0 {
                filters.push(format!("highpass=f={:.0}", high_freq));
            }
        } else if high > 0.1 {
            let gain = (high / 2.0).clamp(0.0, 12.0);
            filters.push(format!("equalizer=f=8000:t=h:w=6000:g={:.1}", gain));
        }

        if !filters.is_empty() {
            let filter = format!("@eq:lavfi=[{}]", filters.join(","));
            self.send_command(vec!["af".into(), "add".into(), filter.into()])?;
        }

        Ok(())
    }

    /// Set 10-band master EQ (frequencies: 32, 64, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz)
    pub fn set_master_eq(&mut self, bands: &[f32; 10], freqs: &[f32; 10]) -> Result<(), String> {
        if self.has_filter("meq") {
            self.send_command(vec!["af".into(), "remove".into(), "@meq".into()])
                .ok();
        }

        let filters: Vec<String> = bands
            .iter()
            .zip(freqs.iter())
            .filter(|(g, _)| g.abs() > 0.1)
            .map(|(g, f)| {
                // Width = half the center frequency for musical Q
                let w = f * 0.5;
                format!("equalizer=f={:.0}:t=h:w={:.0}:g={:.1}", f, w, g)
            })
            .collect();

        if !filters.is_empty() {
            let filter = format!("@meq:lavfi=[{}]", filters.join(","));
            self.send_command(vec!["af".into(), "add".into(), filter.into()])?;
        }

        Ok(())
    }

    pub fn set_pan(&mut self, pan: f32) -> Result<(), String> {
        let pan_clamped = pan.clamp(-1.0, 1.0);

        if pan_clamped.abs() < 0.01 {
            if self.has_filter("pan") {
                self.send_command(vec!["af".into(), "remove".into(), "@pan".into()])
                    .ok();
            }
        } else {
            let filter = format!("@pan:lavfi=[stereotools=balance_out={:.2}]", pan_clamped);
            if self.has_filter("pan") {
                self.send_command(vec!["af".into(), "remove".into(), "@pan".into()])
                    .ok();
            }
            self.send_command(vec!["af".into(), "add".into(), filter.into()])?;
        }
        Ok(())
    }

    pub fn get_path(&mut self) -> Result<String, String> {
        let val = self.get_property("path")?;
        val.as_str()
            .map(|s| s.to_string())
            .ok_or("Invalid path value".to_string())
    }

    /// Best-effort display title for the currently loaded media.
    /// Prefers `media-title`, then metadata TITLE fields.
    pub fn get_media_title(&mut self) -> Option<String> {
        if let Ok(val) = self.get_property("media-title")
            && let Some(s) = val.as_str()
        {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }

        let metadata = self.get_property("metadata").ok()?;
        let obj = metadata.as_object()?;
        for key in ["title", "TITLE", "Title"] {
            if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
        None
    }

    /// Get musical key from MPV metadata (TKEY, INITIALKEY, or KEY tags).
    /// Returns Camelot notation (e.g., "8A", "12B") if found and parseable.
    pub fn get_key_from_metadata(&mut self) -> Option<String> {
        let metadata = self.get_property("metadata").ok()?;
        let obj = metadata.as_object()?;

        tracing::debug!("MPV metadata keys: {:?}", obj.keys().collect::<Vec<_>>());

        // Try standard key tag names (case-insensitive lookup)
        let key_tags = ["KEY", "INITIALKEY", "TKEY", "key", "initialkey", "tkey"];
        for tag in &key_tags {
            if let Some(val) = obj.get(*tag).and_then(|v| v.as_str()) {
                tracing::debug!("Found key tag '{}': {}", tag, val);
                // Try Camelot first
                if let Some((pc, is_major)) = crate::audio::bpm::parse_camelot(val) {
                    let result = pitch_class_to_camelot(pc, is_major);
                    tracing::debug!("Parsed as Camelot: {}", result);
                    return Some(result);
                }
                // Try standard key names (e.g., "C major", "Am", "Bb")
                if let Some(camelot) = crate::audio::bpm::parse_key_name(val) {
                    tracing::debug!("Parsed as key name: {}", camelot);
                    return Some(camelot);
                }
                tracing::warn!("Could not parse key tag '{}' value '{}'", tag, val);
            }
        }
        None
    }

    pub fn get_bpm_from_metadata(&mut self) -> Option<f32> {
        let metadata = self.get_property("metadata").ok()?;
        let obj = metadata.as_object()?;

        let bpm_tags = ["TBPM", "bpm", "BPM", "tempo", "TEMPO", "Tempo"];
        for tag in &bpm_tags {
            if let Some(val) = obj.get(*tag) {
                let num = if let Some(n) = val.as_f64() {
                    Some(n as f32)
                } else if let Some(s) = val.as_str() {
                    s.trim().parse::<f32>().ok()
                } else {
                    None
                };
                if let Some(mut bpm) = num {
                    while bpm > 400.0 {
                        bpm *= 0.5;
                    }
                    while bpm > 0.0 && bpm < 40.0 {
                        bpm *= 2.0;
                    }
                    if (10.0..=400.0).contains(&bpm) {
                        tracing::debug!("Found BPM from metadata tag '{}': {}", tag, bpm);
                        return Some(bpm);
                    }
                }
            }
        }
        None
    }

    /// An audio device known to MPV (CoreAudio UID + human description).
    pub fn get_audio_device_list(&mut self) -> Result<Vec<AudioDeviceEntry>, String> {
        let data = self.get_property("audio-device-list")?;
        let arr = data.as_array().ok_or("audio-device-list is not an array")?;
        let mut devices = Vec::new();
        for item in arr {
            if let (Some(name), Some(desc)) = (
                item.get("name").and_then(|v| v.as_str()),
                item.get("description").and_then(|v| v.as_str()),
            ) {
                devices.push(AudioDeviceEntry {
                    name: name.to_string(),
                    description: desc.to_string(),
                });
            }
        }
        Ok(devices)
    }

    /// Switch MPV's audio output to the given device (CoreAudio UID string).
    pub fn set_audio_device(&mut self, device_name: &str) -> Result<(), String> {
        self.set_property("audio-device", serde_json::json!(device_name))
    }

    /// Get MPV's current audio output device (CoreAudio UID string).
    pub fn get_audio_device(&mut self) -> Result<String, String> {
        let val = self.get_property("audio-device")?;
        val.as_str()
            .map(|s| s.to_string())
            .ok_or("Invalid audio-device value".to_string())
    }

    pub fn ensure_astats(&mut self) -> Result<(), String> {
        if let Ok(af) = self.get_property("af")
            && let Some(arr) = af.as_array()
        {
            for filter in arr {
                if filter.get("label").and_then(|v| v.as_str()) == Some("astats") {
                    return Ok(());
                }
            }
        }

        self.send_command(vec![
            "af".into(),
            "add".into(),
            "@astats:astats=metadata=1:reset=1:measure_overall=Peak_level+RMS_level:measure_perchannel=Peak_level+RMS_level".into(),
        ])?;
        Ok(())
    }

    /// Read audio levels from the background metering thread atomics.
    /// Returns `(peak_l, peak_r, rms_l, rms_r)` in linear 0.0–1.0 scale.
    pub fn get_audio_levels(&self) -> (f32, f32, f32, f32) {
        self.meters.decay_peaks(0.92);
        self.meters.load()
    }

    /// Read detected BPM from the onset detector (0.0 if not yet detected).
    pub fn get_detected_bpm(&self) -> f32 {
        let raw = self.meters.detected_bpm.load(Ordering::Relaxed);
        raw as f32 / 100.0
    }

    /// Reset all MPV properties to defaults: volume, mute, pause, speed, filters.
    pub fn reset_all(&mut self) {
        let _ = self.set_volume(100.0);
        let _ = self.set_mute(false);
        let _ = self.set_pause(false);
        let _ = self.set_speed(1.0);
        let _ = self.set_lpf(20000.0);
        let _ = self.set_hpf(20.0);
        let _ = self.set_pan(0.0);
        // Remove all audio filters (eq, astats, pan, lpf, hpf)
        if let Ok(af) = self.get_property("af")
            && let Some(arr) = af.as_array()
        {
            for filter in arr {
                if let Some(label) = filter.get("label").and_then(|v| v.as_str()) {
                    let _ = self.send_command(vec![
                        "af".into(),
                        "remove".into(),
                        format!("@{}", label).into(),
                    ]);
                }
            }
        }
    }
}

impl Drop for MpvClient {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }
}
