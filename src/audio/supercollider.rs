//! SuperCollider OSC client with SynthDef-based audio routing
//!
//! Instead of controlling SC's global output, we create a SynthDef that wraps
//! the audio chain with per-deck volume, LPF, HPF, EQ, and pan controls.
//! Tidal's synths route through this chain before reaching the speakers.

use std::net::UdpSocket;
use std::time::Duration;

/// Synchronous SuperCollider OSC client with SynthDef support
pub struct SuperColliderClient {
    socket: Option<UdpSocket>,
    addr: String,
    connected: bool,
    base_node_id: i32,
    input_bus: i32,
    synth_def_sent: bool,
    group_id: Option<i32>,
    synth_id: Option<i32>,
}

impl SuperColliderClient {
    pub fn new(addr: impl Into<String>, base_node_id: i32, input_bus: i32) -> Self {
        Self {
            socket: None,
            addr: addr.into(),
            connected: false,
            base_node_id,
            input_bus,
            synth_def_sent: false,
            group_id: None,
            synth_id: None,
        }
    }

    pub fn connect(&mut self) -> Result<(), String> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Failed to bind UDP socket: {}", e))?;
        socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .ok();
        socket
            .set_write_timeout(Some(Duration::from_millis(100)))
            .ok();

        // Ping SC server to verify it's alive before marking connected.
        // Without this, synth/group creation silently fails if SC isn't running.
        let ping_msg = Self::encode_osc_message("/status", &[]);
        let mut alive = false;
        for _ in 0..3 {
            let _ = socket.send_to(&ping_msg, &self.addr);
            let mut buf = [0u8; 256];
            if socket.recv_from(&mut buf).is_ok() {
                alive = true;
                break;
            }
        }
        if !alive {
            return Err("SuperCollider server not responding on ".to_string()
                + &self.addr);
        }

        self.socket = Some(socket);
        self.connected = true;
        Ok(())
    }

    fn send_raw(&self, msg: &[u8]) -> Result<(), String> {
        let socket = self.socket.as_ref().ok_or("Not connected")?;
        socket
            .send_to(msg, &self.addr)
            .map_err(|e| format!("Send failed: {}", e))?;
        Ok(())
    }

    /// Send the mixerChannel SynthDef to SC
    ///
    /// The SynthDef reads stereo audio from a bus, applies EQ, LPF, HPF,
    /// volume, and pan, then outputs to the hardware.
    ///
    /// After sending, call `create_group()` and `create_synth()` to activate.
    pub fn send_synth_def(&mut self) -> Result<(), String> {
        let synthdef_files = ["mixerChannel.scsyndef"];
        
        for filename in &synthdef_files {
            let paths = [
                format!("synthdefs/{}", filename),
                format!("../synthdefs/{}", filename),
                format!("../../synthdefs/{}", filename),
            ];
            
            let def_bytes = paths.iter()
                .find_map(|p| std::fs::read(p).ok())
                .ok_or_else(|| format!("{}.scsyndef not found. Run: sclang synthdefs/mixerChannel.scd", filename))?;
            
            let mut msg = Vec::new();
            Self::write_osc_string(&mut msg, "/d_recv");
            Self::write_osc_string(&mut msg, ",b");
            let len = def_bytes.len() as u32;
            msg.extend_from_slice(&len.to_be_bytes());
            msg.extend_from_slice(&def_bytes);
            while msg.len() % 4 != 0 {
                msg.push(0);
            }
            self.send_raw(&msg)?;
        }
        
        self.synth_def_sent = true;
        Ok(())
    }

    /// Create a group for this deck's effects chain
    /// The group is placed at the tail of the default group (node 0)
    pub fn create_group(&mut self) -> Result<(), String> {
        let group_id = self.base_node_id + 100;
        self.group_id = Some(group_id);

        let msg = Self::encode_osc_message(
            "/g_new",
            &[
                OscType::Int(group_id),
                OscType::Int(1), // addToTail
                OscType::Int(0), // target: default group
            ],
        );
        self.send_raw(&msg)
    }

    /// Create the mixer synth in this deck's group
    /// Reads from the deck's SuperDirt orbit bus, processes, outputs to hardware out.
    pub fn create_synth(&mut self) -> Result<(), String> {
        let group_id = self.group_id.unwrap_or(0);
        let synth_id = self.base_node_id + 200;
        self.synth_id = Some(synth_id);

        let msg = Self::encode_osc_message(
            "/s_new",
            &[
                OscType::Str("mixerChannel".to_string()),
                OscType::Int(synth_id),
                OscType::Int(1),  // addToTail
                OscType::Int(group_id), // target: our group
                OscType::Str("in".to_string()),
                OscType::Int(self.input_bus),
                OscType::Str("out".to_string()),
                OscType::Int(0),  // write to bus 0
                OscType::Str("vol".to_string()),
                OscType::Float(4.0),
                OscType::Str("lpf".to_string()),
                OscType::Float(20000.0),
                OscType::Str("hpf".to_string()),
                OscType::Float(20.0),
                OscType::Str("eqLowFreq".to_string()),
                OscType::Float(20000.0),
                OscType::Str("eqLowGain".to_string()),
                OscType::Float(0.0),
                OscType::Str("eqMidFreq".to_string()),
                OscType::Float(1000.0),
                OscType::Str("eqMidGain".to_string()),
                OscType::Float(0.0),
                OscType::Str("eqHighFreq".to_string()),
                OscType::Float(20.0),
                OscType::Str("eqHighGain".to_string()),
                OscType::Float(0.0),
                OscType::Str("pan".to_string()),
                OscType::Float(0.0),
            ],
        );
        self.send_raw(&msg)
    }

    /// Free the mixer synth (stops effect processing)
    pub fn free_synth(&self) -> Result<(), String> {
        if let Some(synth_id) = self.synth_id {
            let msg = Self::encode_osc_message("/n_free", &[OscType::Int(synth_id)]);
            self.send_raw(&msg)
        } else {
            Ok(())
        }
    }

    /// Free the group and all contained synths
    pub fn free_group(&self) -> Result<(), String> {
        if let Some(group_id) = self.group_id {
            let msg = Self::encode_osc_message("/n_free", &[OscType::Int(group_id)]);
            self.send_raw(&msg)
        } else {
            Ok(())
        }
    }

    /// Set a parameter on the mixer synth
    fn set_synth_param(&self, name: &str, value: f32) -> Result<(), String> {
        if let Some(synth_id) = self.synth_id {
            let msg = Self::encode_osc_message(
                "/n_set",
                &[
                    OscType::Int(synth_id),
                    OscType::Str(name.to_string()),
                    OscType::Float(value),
                ],
            );
            self.send_raw(&msg)
        } else {
            Ok(())
        }
    }

    /// Set volume (0.0 - 8.0)
    pub fn set_volume(&self, volume: f32) -> Result<(), String> {
        self.set_synth_param("vol", volume.clamp(0.0, 8.0))
    }

    /// Set low-pass filter frequency (20 - 20000 Hz)
    pub fn set_lpf(&self, freq: f32) -> Result<(), String> {
        self.set_synth_param("lpf", freq.clamp(20.0, 20000.0))
    }

    /// Set high-pass filter frequency (20 - 20000 Hz)
    pub fn set_hpf(&self, freq: f32) -> Result<(), String> {
        self.set_synth_param("hpf", freq.clamp(20.0, 20000.0))
    }


    /// Set EQ (all three bands)
    pub fn set_eq(&self, low: f32, mid: f32, high: f32) -> Result<(), String> {
        // Low: cut with lowpass, boost with low shelf
        if low < 0.0 {
            let normalized = (-low / 24.0).clamp(0.0, 1.0);
            let low_freq: f32 = 20000.0 * (500.0f32 / 20000.0).powf(normalized);
            self.set_synth_param("eqLowFreq", low_freq.clamp(20.0, 20000.0))?;
            self.set_synth_param("eqLowGain", 0.0)?;
        } else {
            self.set_synth_param("eqLowFreq", 20000.0)?;
            self.set_synth_param("eqLowGain", (low / 2.0).clamp(0.0, 12.0))?;
        }

        // Mid: sweepable peak EQ frequency and gain
        if mid.abs() > 0.1 {
            let normalized = mid.abs() / 24.0;
            let mid_freq: f32 = 200.0 * (8000.0f32 / 200.0).powf(normalized);
            let mid_gain: f32 = (mid / 2.0).clamp(-12.0, 12.0);
            self.set_synth_param("eqMidFreq", mid_freq.clamp(20.0, 20000.0))?;
            self.set_synth_param("eqMidGain", mid_gain)?;
        } else {
            self.set_synth_param("eqMidFreq", 1000.0)?;
            self.set_synth_param("eqMidGain", 0.0)?;
        }

        // High: cut with highpass, boost with high shelf
        if high < 0.0 {
            let normalized = (-high / 24.0).clamp(0.0, 1.0);
            let high_freq: f32 = 20.0 * (5000.0f32 / 20.0).powf(normalized);
            self.set_synth_param("eqHighFreq", high_freq.clamp(20.0, 20000.0))?;
            self.set_synth_param("eqHighGain", 0.0)
        } else {
            self.set_synth_param("eqHighFreq", 20.0)?;
            self.set_synth_param("eqHighGain", (high / 2.0).clamp(0.0, 12.0))
        }
    }

    /// Set 10-band master EQ gains (dB)
    /// Bands: 32Hz, 64Hz, 125Hz, 250Hz, 500Hz, 1kHz, 2kHz, 4kHz, 8kHz, 16kHz
    pub fn set_master_eq(&self, bands: &[f32; 10]) -> Result<(), String> {
        let param_names = [
            "mEq32", "mEq64", "mEq125", "mEq250", "mEq500",
            "mEq1k", "mEq2k", "mEq4k", "mEq8k", "mEq16k",
        ];
        for (name, &gain) in param_names.iter().zip(bands.iter()) {
            self.set_synth_param(name, gain.clamp(-12.0, 12.0))?;
        }
        Ok(())
    }

    /// Set pan (-1.0 = left, 0.0 = center, 1.0 = right)
    pub fn set_pan(&self, pan: f32) -> Result<(), String> {
        self.set_synth_param("pan", pan.clamp(-1.0, 1.0))
    }

    /// Pause (free synth = muted, create new = unmuted)
    pub fn set_pause(&mut self, paused: bool) -> Result<(), String> {
        if paused {
            self.free_synth()
        } else {
            self.create_synth()
        }
    }

    /// Free all nodes (cleanup)
    pub fn free_all(&self) -> Result<(), String> {
        self.free_synth()?;
        self.free_group()
    }

    // ---- OSC encoding helpers ----

    fn encode_osc_message(address: &str, args: &[OscType]) -> Vec<u8> {
        let mut buf = Vec::new();
        Self::write_osc_string(&mut buf, address);
        let mut type_tags = String::from(",");
        for arg in args {
            match arg {
                OscType::Int(_) => type_tags.push('i'),
                OscType::Float(_) => type_tags.push('f'),
                OscType::Str(_) => type_tags.push('s'),
            }
        }
        Self::write_osc_string(&mut buf, &type_tags);
        for arg in args {
            match arg {
                OscType::Int(v) => buf.extend_from_slice(&v.to_be_bytes()),
                OscType::Float(v) => buf.extend_from_slice(&v.to_be_bytes()),
                OscType::Str(v) => Self::write_osc_string(&mut buf, v),
            }
        }
        buf
    }

    fn write_osc_string(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
        while buf.len() % 4 != 0 {
            buf.push(0);
        }
    }

}

enum OscType {
    Int(i32),
    Float(f32),
    Str(String),
}
