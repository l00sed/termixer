use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::audio::backend::{backend_label, default_backend};
use crate::audio::decoder::{AtomicF64, AudioRingBuf, DecoderThread};
use crate::audio::dsp::{AtomicMeter, Biquad, DcBlocker, FilterType, LfoOsc, Svf, pan_gains, soft_limit};
use crate::state::MASTER_EQ_FREQUENCIES;
use crate::audio::pipe_capture::PipeCaptureThread;

/// Shared cache of pre-decoded pad samples: (samples, sample_rate) per pad slot.
type PadSampleCache = Arc<std::sync::RwLock<Vec<Option<Arc<(Vec<f32>, u32)>>>>>;

// macOS CoreAudio FFI for reading system output volume
#[cfg(target_os = "macos")]
mod macos_volume {
    use std::ffi::c_void;

    type OSStatus = i32;
    type AudioObjectID = u32;
    type AudioObjectPropertySelector = u32;
    type AudioObjectPropertyScope = u32;
    type AudioObjectPropertyElement = u32;

    #[repr(C)]
    #[allow(non_snake_case)]
    struct AudioObjectPropertyAddress {
        mSelector: AudioObjectPropertySelector,
        mScope: AudioObjectPropertyScope,
        mElement: AudioObjectPropertyElement,
    }

    const K_AUDIO_OBJECT_SYSTEM_OBJECT: AudioObjectID = 1;
    const K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: AudioObjectPropertyScope = 0x676C6F62;
    const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: AudioObjectPropertyElement = 0;
    const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE: AudioObjectPropertySelector = 0x644F7574;
    #[allow(dead_code)]
    const K_AUDIO_HARDWARE_SERVICE_DEVICE_PROPERTY_VIRTUAL_MAIN_VOLUME: AudioObjectPropertySelector = 0x766D766C; // 'vmvl'
    #[allow(dead_code)]
    const K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT: AudioObjectPropertyScope = 0x6F757470; // 'outp'

    unsafe extern "C" {
        fn AudioObjectGetPropertyData(
            object_id: AudioObjectID,
            address: *const AudioObjectPropertyAddress,
            qualifier_data_size: u32,
            qualifier_data: *const c_void,
            io_data_size: *mut u32,
            io_data: *mut c_void,
        ) -> OSStatus;
        fn AudioObjectSetPropertyData(
            object_id: AudioObjectID,
            address: *const AudioObjectPropertyAddress,
            qualifier_data_size: u32,
            qualifier_data: *const c_void,
            in_data_size: u32,
            in_data: *const c_void,
        ) -> OSStatus;
    }

    pub fn read_system_volume() -> Option<f32> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static FAIL_LOGCTR: AtomicU64 = AtomicU64::new(0);
        unsafe {
            // Step 1: Get default output device
            let mut prop = AudioObjectPropertyAddress {
                mSelector: K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE,
                mScope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
                mElement: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
            };
            let mut device_id: AudioObjectID = 0;
            let mut size = std::mem::size_of::<AudioObjectID>() as u32;
            let status = AudioObjectGetPropertyData(
                K_AUDIO_OBJECT_SYSTEM_OBJECT,
                &prop,
                0,
                std::ptr::null(),
                &mut size,
                &mut device_id as *mut _ as *mut c_void,
            );
            if status != 0 || device_id == 0 {
                let c = FAIL_LOGCTR.fetch_add(1, Ordering::Relaxed);
                if c < 5 {
                    let _ = std::fs::write("/tmp/termixer_hp_debug.log",
                        format!("read_vol FAIL: default_device status={} id={}\n", status, device_id));
                }
                return None;
            }

            // Step 2: Try kAudioDevicePropertyVolumeScalar (0x766F6C6D = 'volm')
            //         with OUTPUT scope and element 1 (main channel)
            const VOL_SCALAR: u32 = 0x766F6C6D;
            const SCOPE_OUT: u32 = 0x6F757470;

            prop.mSelector = VOL_SCALAR;
            prop.mScope = SCOPE_OUT;
            prop.mElement = 1; // kAudioObjectPropertyElementMain on modern macOS
            let mut volume: f32 = 0.0;
            size = std::mem::size_of::<f32>() as u32;
            let status = AudioObjectGetPropertyData(
                device_id,
                &prop,
                0,
                std::ptr::null(),
                &mut size,
                &mut volume as *mut _ as *mut c_void,
            );
            if status == 0 {
                let c = FAIL_LOGCTR.fetch_add(1, Ordering::Relaxed);
                if c < 3 {
                    let _ = std::fs::write("/tmp/termixer_hp_debug.log",
                        format!("read_vol OK: vol_scalar elem1 = {:.3}\n", volume));
                }
                return Some(volume.clamp(0.0, 1.0));
            }

            // Step 3: Try element 0
            prop.mElement = 0;
            let mut volume: f32 = 0.0;
            size = std::mem::size_of::<f32>() as u32;
            let status = AudioObjectGetPropertyData(
                device_id,
                &prop,
                0,
                std::ptr::null(),
                &mut size,
                &mut volume as *mut _ as *mut c_void,
            );
            if status == 0 {
                let c = FAIL_LOGCTR.fetch_add(1, Ordering::Relaxed);
                if c < 3 {
                    let _ = std::fs::write("/tmp/termixer_hp_debug.log",
                        format!("read_vol OK: vol_scalar elem0 = {:.3}\n", volume));
                }
                return Some(volume.clamp(0.0, 1.0));
            }

            // Step 4: Try vmvl with GLOBAL scope
            prop.mSelector = 0x766D766C;
            prop.mScope = K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL;
            prop.mElement = K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN;
            let mut volume: f32 = 0.0;
            size = std::mem::size_of::<f32>() as u32;
            let status = AudioObjectGetPropertyData(
                device_id,
                &prop,
                0,
                std::ptr::null(),
                &mut size,
                &mut volume as *mut _ as *mut c_void,
            );
            if status == 0 {
                let c = FAIL_LOGCTR.fetch_add(1, Ordering::Relaxed);
                if c < 3 {
                    let _ = std::fs::write("/tmp/termixer_hp_debug.log",
                        format!("read_vol OK: vmvl_global = {:.3}\n", volume));
                }
                return Some(volume.clamp(0.0, 1.0));
            }

            let c = FAIL_LOGCTR.fetch_add(1, Ordering::Relaxed);
            if c < 5 {
                let _ = std::fs::write("/tmp/termixer_hp_debug.log",
                    format!("read_vol FAIL: all 3 properties failed on device_id={}\n", device_id));
            }
            None
        }
    }

    /// Set volume on all output devices except the default output device.
    /// This keeps headphone/external device volumes synced with the speaker
    /// volume controlled by the system media keys.
    pub fn set_all_non_default_volumes(vol: f32) {
        const K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: u32 = 0x676C6F62;
        const K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT: u32 = 0x6F757470;
        const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;
        const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE: u32 = 0x644F7574;
        const K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR: u32 = 0x766F6C6D;

        unsafe {
            // Get default output device ID
            let def_prop = AudioObjectPropertyAddress {
                mSelector: K_AUDIO_HARDWARE_PROPERTY_DEFAULT_OUTPUT_DEVICE,
                mScope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
                mElement: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
            };
            let mut default_id: AudioObjectID = 0;
            let mut size = std::mem::size_of::<AudioObjectID>() as u32;
            let status = AudioObjectGetPropertyData(
                K_AUDIO_OBJECT_SYSTEM_OBJECT, &def_prop, 0, std::ptr::null(),
                &mut size, &mut default_id as *mut _ as *mut c_void,
            );
            if status != 0 || default_id == 0 { return; }

            // Get all device IDs
            let dev_prop = AudioObjectPropertyAddress {
                mSelector: 0x64657663, // kAudioHardwarePropertyDevices
                mScope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
                mElement: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
            };
            let mut size: u32 = 0;
            let status = AudioObjectGetPropertyData(
                K_AUDIO_OBJECT_SYSTEM_OBJECT, &dev_prop, 0, std::ptr::null(),
                &mut size, std::ptr::null_mut(),
            );
            if status != 0 || size == 0 { return; }

            let count = size as usize / std::mem::size_of::<AudioObjectID>();
            let mut device_ids: Vec<AudioObjectID> = vec![0; count];
            let status = AudioObjectGetPropertyData(
                K_AUDIO_OBJECT_SYSTEM_OBJECT, &dev_prop, 0, std::ptr::null(),
                &mut size, device_ids.as_mut_ptr() as *mut c_void,
            );
            if status != 0 { return; }

            // Set volume on every output device except the default
            let vol_prop = AudioObjectPropertyAddress {
                mSelector: K_AUDIO_DEVICE_PROPERTY_VOLUME_SCALAR,
                mScope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
                mElement: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
            };
            let clamped = vol.clamp(0.0, 1.0);

            for &did in &device_ids {
                if did == default_id { continue; }
                // Skip devices that have no output streams
                let streams_prop = AudioObjectPropertyAddress {
                    mSelector: 0x7374726D, // kAudioDevicePropertyStreams
                    mScope: K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT,
                    mElement: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
                };
                let mut stream_size: u32 = 0;
                let s = AudioObjectGetPropertyData(
                    did, &streams_prop, 0, std::ptr::null(),
                    &mut stream_size, std::ptr::null_mut(),
                );
                if s != 0 || stream_size == 0 { continue; }

                AudioObjectSetPropertyData(
                    did, &vol_prop, 0, std::ptr::null(),
                    std::mem::size_of::<f32>() as u32,
                    &clamped as *const _ as *const c_void,
                );
            }
        }
    }
}

const SEEK_SENTINEL: f64 = -1.0;

// NOTE: `assert_no_alloc` was tried here as a runtime guard, but its global
// allocator hook trips on legitimate first-callback threadlocal init inside
// arc-swap. The zero-alloc discipline is now enforced structurally instead:
//   - triple_buffer / ArcSwapOption / rtrb are lock-free and alloc-free once
//     initialized.
//   - Deck buffers, conversion buffers, DSP state are pre-allocated at build.
//   - Only the once-per-second debug string in the audio callback allocates,
//     via try_lock (never blocking).
//
// If you want the runtime guard back, gate it behind a cargo feature and
// warm up arc-swap / rtrb inside a permit_alloc block on the first callback.

mod inner;

pub use inner::{AudioCommand, ControlSnapshot, ControlState, DeckState, SequenceSnapshot};

const MAX_DECKS: usize = 3;
const CMD_RING_CAPACITY: usize = 64;
const MAX_PAD_VOICES: usize = 16;
const PAD_RING_SIZE: usize = 48000; // ~1 second at 48kHz mono
const HPF_MAX_FREQ_POS: f32 = 0.75;
const CAPTURE_REVERSE_LOOKBACK_SECS: f32 = 2.0;

/// Per-deck DSP chain: SVF for cutoff, biquads for EQ, LFO, DC blocker.
struct DspFilters {
    /// Two cascaded SVFs for 4th-order (-24 dB/oct) rolloff.
    /// Single 2nd-order (-12 dB/oct) doesn't cut aggressively enough for DJ use.
    svf1: Svf,
    svf2: Svf,
    eq_lo_l: Biquad,
    eq_lo_r: Biquad,
    eq_mi_l: Biquad,
    eq_mi_r: Biquad,
    eq_hi_l: Biquad,
    eq_hi_r: Biquad,
    lfo: LfoOsc,
    lfo_active: bool,
    dc: DcBlocker,
    prev_l: f32,
    prev_r: f32,
    underrun_gain: f32,
    /// LP/HP blend: 0.0 = pure LP, 1.0 = pure HP.
    hp_blend: f32,
    /// Smoothed per-deck makeup gain to avoid pumping/clipping artifacts.
    makeup_gain: f32,
    /// Smoothed dry input energy envelope for loudness matching.
    in_energy_env: f32,
    /// Smoothed filtered output energy envelope for loudness matching.
    out_energy_env: f32,
    /// Last computed filter intensity (0..1), including LFO modulation.
    filter_intensity: f32,
    /// Smoothed post-EQ trim to keep deck peaks out of hard limiting.
    trim_gain: f32,
    master_eq_l: [Biquad; 10],
    master_eq_r: [Biquad; 10],
    master_eq: [f32; 10],
    prev_eq_low: f32,
    prev_eq_mid: f32,
    prev_eq_hi: f32,
    prev_master_eq: [f32; 10],
}

impl DspFilters {
    fn make_master_eq_bank(sr: f32) -> [Biquad; 10] {
        std::array::from_fn(|idx| {
            let mut biq = Biquad::new(sr);
            biq.set_params(FilterType::Peaking, MASTER_EQ_FREQUENCIES[idx], 1.0, 0.0);
            biq
        })
    }

    fn new(sr: f32) -> Self {
        let mut eq_lo_l = Biquad::new(sr);
        let mut eq_lo_r = Biquad::new(sr);
        let mut eq_mi_l = Biquad::new(sr);
        let mut eq_mi_r = Biquad::new(sr);
        let mut eq_hi_l = Biquad::new(sr);
        let mut eq_hi_r = Biquad::new(sr);
        eq_lo_l.set_params(FilterType::Peaking, 80.0, 0.707, 0.0);
        eq_lo_r.set_params(FilterType::Peaking, 80.0, 0.707, 0.0);
        eq_mi_l.set_params(FilterType::Peaking, 1000.0, 0.707, 0.0);
        eq_mi_r.set_params(FilterType::Peaking, 1000.0, 0.707, 0.0);
        eq_hi_l.set_params(FilterType::Peaking, 8000.0, 0.707, 0.0);
        eq_hi_r.set_params(FilterType::Peaking, 8000.0, 0.707, 0.0);
        Self {
            svf1: Svf::new(sr),
            svf2: Svf::new(sr),
            eq_lo_l, eq_lo_r,
            eq_mi_l, eq_mi_r,
            eq_hi_l, eq_hi_r,
            lfo: LfoOsc::new(sr),
            lfo_active: false,
            dc: DcBlocker::new(sr),
            prev_l: 0.0,
            prev_r: 0.0,
            underrun_gain: 1.0,
            hp_blend: 0.0,
            makeup_gain: 1.0,
            in_energy_env: 1e-4,
            out_energy_env: 1e-4,
            filter_intensity: 0.0,
            trim_gain: 1.0,
            master_eq_l: Self::make_master_eq_bank(sr),
            master_eq_r: Self::make_master_eq_bank(sr),
            master_eq: [0.0; 10],
            prev_eq_low: f32::NAN,
            prev_eq_mid: f32::NAN,
            prev_eq_hi: f32::NAN,
            prev_master_eq: [f32::NAN; 10],
        }
    }

    /// Update filter parameters. SVF handles cutoff modulation per-sample via
    /// set_params() (coefficients interpolate automatically in tick()).
    fn update_params(&mut self, ctrl: &DeckState, frames: usize, master_eq: &[f32; 10]) {
        self.lfo.set_speed(ctrl.lfo_speed);
        self.lfo.set_shape(ctrl.lfo_shape);
        self.lfo_active = ctrl.lfo_speed > 0.001;

        // Compute filter frequency from freq_pos (log scale, 20–20000 Hz)
        let log_min = 20.0f32.log10();
        let log_max = 20000.0f32.log10();
        let actual_freq = 10.0f32.powf(log_min + ctrl.filter_freq * (log_max - log_min));
        let hpf_max_freq = 10.0f32.powf(log_min + HPF_MAX_FREQ_POS * (log_max - log_min));

        // Crossfade zone: 300Hz–3kHz between LPF and HPF
        let blend = if actual_freq <= 300.0 {
            0.0
        } else if actual_freq >= 3000.0 {
            1.0
        } else {
            let t = (actual_freq - 300.0) / (3000.0 - 300.0);
            t * t * (3.0 - 2.0 * t)
        };

        let lpf_target = actual_freq + (20000.0 - actual_freq) * blend;
        let hpf_target = 20.0 + (hpf_max_freq - 20.0) * blend;

        // Apply intensity (cutoff) as frequency sweep toward the target.
        // Use a linear knob response so the audible change is distributed
        // more evenly across the full 0.0-1.0 travel.
        // Floor at 80 Hz so max intensity doesn't fully silence.
        // LFO modulates the filter intensity — sweeps between the user's
        // cutoff setting and "wide open" (no filtering). At LFO peak: filter
        // fully engaged. At LFO trough: transparent audio passes through.
        let raw_intensity = ctrl.filter_cutoff;
        let intensity = if self.lfo_active {
            if let Some(lfo_val) = self.lfo.tick_buffer(frames) {
                // lfo_val is 0.0–1.0. Multiply with intensity:
                // 0.0 = no filtering (open), 1.0 = full user cutoff.
                raw_intensity * lfo_val
            } else {
                raw_intensity
            }
        } else {
            raw_intensity
        };
        let lpf_hz = (20000.0 - (20000.0 - lpf_target) * intensity).clamp(300.0, 12000.0);
        let hpf_hz = (20.0 + (hpf_target - 20.0) * intensity).clamp(20.0, hpf_max_freq);
        let hp_blend = blend * intensity;
        let cutoff_hz = lpf_hz * (1.0 - hp_blend) + hpf_hz * hp_blend;
        self.filter_intensity = intensity;

        // SVF: blend between LP and HP using the SVF's built-in outputs.
        // At intensity=0 (filter off), use 20kHz LP (pass everything).
        // At intensity=1, use the computed cutoff frequencies.
        // The SVF internally interpolates — no crossfade needed.
        //
        // For the combined LP+HP DJ filter: we run the SVF in LP mode and
        // mix in the HP output when in the crossfade zone.
        //
        // Primary use: LPF mode (most DJ filters are LPF-based).
        // When hpf_hz > 100Hz, blend in HP output.
        // SVF: set cutoff frequency on both cascaded stages. Q=0.707 per
        // stage — two Butterworth stages cascade to a 4th-order Linkwitz-Riley
        // alignment (-24 dB/oct, flat passband, no resonant peak).
        self.svf1.set_params(cutoff_hz, 0.707);
        self.svf2.set_params(cutoff_hz, 0.707);
        self.hp_blend = hp_blend;

        // EQ: set coefficients directly on per-channel biquads (no crossfade — EQ changes slowly)
        let eq_lo_gain = if ctrl.eq_low_kill { -48.0 } else { ctrl.eq_low };
        let eq_mi_gain = if ctrl.eq_mid_kill { -48.0 } else { ctrl.eq_mid };
        let eq_hi_gain = if ctrl.eq_high_kill { -48.0 } else { ctrl.eq_high };
        // Only recalculate biquad coefficients when gain actually changes
        if eq_lo_gain != self.prev_eq_low {
            self.eq_lo_l.set_params(FilterType::Peaking, 80.0, 0.707, eq_lo_gain);
            self.eq_lo_r.set_params(FilterType::Peaking, 80.0, 0.707, eq_lo_gain);
            self.prev_eq_low = eq_lo_gain;
        }
        if eq_mi_gain != self.prev_eq_mid {
            self.eq_mi_l.set_params(FilterType::Peaking, 1000.0, 0.707, eq_mi_gain);
            self.eq_mi_r.set_params(FilterType::Peaking, 1000.0, 0.707, eq_mi_gain);
            self.prev_eq_mid = eq_mi_gain;
        }
        if eq_hi_gain != self.prev_eq_hi {
            self.eq_hi_l.set_params(FilterType::Peaking, 8000.0, 0.707, eq_hi_gain);
            self.eq_hi_r.set_params(FilterType::Peaking, 8000.0, 0.707, eq_hi_gain);
            self.prev_eq_hi = eq_hi_gain;
        }

        self.master_eq = *master_eq;
        // Only recalculate master EQ biquads when gains change
        for (i, band_db) in self.master_eq.iter().enumerate() {
            if *band_db != self.prev_master_eq[i] {
                self.master_eq_l[i].set_params(FilterType::Peaking, MASTER_EQ_FREQUENCIES[i], 1.0, *band_db);
                self.master_eq_r[i].set_params(FilterType::Peaking, MASTER_EQ_FREQUENCIES[i], 1.0, *band_db);
                self.prev_master_eq[i] = *band_db;
            }
        }
    }

    /// Process stereo sample through the DSP chain.
    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        // Cascaded SVF: blend LP/HP at stage 1, then run stage 2 on the blended
        // signal so both paths share the same phase/order behavior. This avoids
        // the roughness from mixing a 4th-order LP with a 2nd-order HP directly.
        let (lp1_l, lp1_r, hp1_l, hp1_r) = self.svf1.tick(l, r);
        let b = self.hp_blend;
        let a = (1.0 - b).sqrt();
        let c = b.sqrt();
        let s1_l = lp1_l * a + hp1_l * c;
        let s1_r = lp1_r * a + hp1_r * c;
        let (mut l_filt, mut r_filt, _, _) = self.svf2.tick(s1_l, s1_r);

        // Program-dependent loudness compensation: match filtered level toward
        // dry level while restricting boosts in LP-heavy mode to avoid bass fuzz.
        let freq = self.svf1.current_freq();
        let openness = (freq / 12000.0).clamp(0.0, 1.0); // 1.0=open, 0.0=max cut
        let input_energy = ((l * l) + (r * r)) * 0.5 + 1e-9;
        let filtered_energy = ((l_filt * l_filt) + (r_filt * r_filt)) * 0.5 + 1e-9;
        let in_attack = 0.07;
        let in_release = 0.006;
        let out_attack = 0.12;
        let out_release = 0.01;
        let in_coef = if input_energy > self.in_energy_env { in_attack } else { in_release };
        let out_coef = if filtered_energy > self.out_energy_env { out_attack } else { out_release };
        self.in_energy_env += (input_energy - self.in_energy_env) * in_coef;
        self.out_energy_env += (filtered_energy - self.out_energy_env) * out_coef;

        let energy_match = (self.in_energy_env / self.out_energy_env).sqrt().clamp(0.35, 3.0);
        let hp_focus = self.hp_blend;
        let lp_focus = 1.0 - hp_focus;
        let intensity = self.filter_intensity;
        let max_boost = 1.0
            + intensity * (0.08 + hp_focus * (0.75 + 0.35 * (1.0 - openness)));
        let min_gain = (1.0 - intensity * (0.30 + 0.30 * lp_focus)).max(0.35);

        let pre_peak = l_filt.abs().max(r_filt.abs()).max(1e-6);
        let safe_makeup = (0.88 / pre_peak).clamp(0.2, 4.0);
        let target_makeup = energy_match.clamp(min_gain, max_boost).min(safe_makeup);

        let smooth = if target_makeup < self.makeup_gain { 0.24 } else { 0.05 };
        self.makeup_gain += (target_makeup - self.makeup_gain) * smooth;

        l_filt *= self.makeup_gain;
        r_filt *= self.makeup_gain;

        // EQ chain (plain biquads — no crossfade overhead)
        l_filt = self.eq_lo_l.tick(l_filt);
        r_filt = self.eq_lo_r.tick(r_filt);
        l_filt = self.eq_mi_l.tick(l_filt);
        r_filt = self.eq_mi_r.tick(r_filt);
        l_filt = self.eq_hi_l.tick(l_filt);
        r_filt = self.eq_hi_r.tick(r_filt);

        // Post-EQ peak management: prevent deck-level overload from driving
        // the master limiter into audible fuzz when cutoff/EQ are aggressive.
        let post_peak = l_filt.abs().max(r_filt.abs()).max(1e-6);
        let trim_ceiling = 0.82 - 0.16 * self.filter_intensity * (1.0 - self.hp_blend);
        let trim_target = if post_peak > trim_ceiling {
            (trim_ceiling / post_peak).clamp(0.22, 1.0)
        } else {
            1.0
        };
        let trim_smooth = if trim_target < self.trim_gain { 0.42 } else { 0.04 };
        self.trim_gain += (trim_target - self.trim_gain) * trim_smooth;
        l_filt *= self.trim_gain;
        r_filt *= self.trim_gain;

        for i in 0..10 {
            l_filt = self.master_eq_l[i].tick(l_filt);
            r_filt = self.master_eq_r[i].tick(r_filt);
        }

        // DC blocker — removes DC accumulated from filter transients,
        // preventing thump and preserving headroom.
        let (l_dc, r_dc) = self.dc.tick(l_filt, r_filt);
        (l_dc, r_dc)
    }

    fn lfo_debug_line(&self) -> String {
        format!("LFO: act={} ph={:.3} sp={:.3}",
            self.lfo_active, self.lfo.phase, self.lfo.speed)
    }
}

/// Thread-safe handle to a ring buffer for a single deck.
/// Uses a plain Mutex<Option<Arc>> for the custom AudioRingBuf path
/// (symphonia decoder). The FIFO capture path bypasses this entirely
/// and uses rtrb directly.
struct SharedBuf(Mutex<Option<Arc<AudioRingBuf>>>);
impl SharedBuf {
    fn new() -> Self { Self(Mutex::new(None)) }
    fn set(&self, buf: Arc<AudioRingBuf>) {
        *self.0.lock().unwrap() = Some(buf);
    }
    fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }
    fn read(&self, out: &mut [f32]) -> usize {
        match self.0.try_lock() {
            Ok(guard) => match guard.as_ref() {
                Some(rb) => rb.read(out),
                None => 0,
            },
            Err(_) => 0,
        }
    }

    fn has_data(&self) -> bool {
        self.0
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|rb| rb.readable() > 0))
            .unwrap_or(false)
    }
}

struct CaptureReverseState {
    active: AtomicBool,
    cursor_frames: Mutex<Option<usize>>,
}

impl CaptureReverseState {
    fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            cursor_frames: Mutex::new(None),
        }
    }

    fn set_active(&self, enabled: bool) {
        self.active.store(enabled, Ordering::Release);
        if !enabled {
            self.reset_cursor();
        }
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn reset_cursor(&self) {
        if let Ok(mut cursor) = self.cursor_frames.lock() {
            *cursor = None;
        }
    }
}

struct CaptureLookback {
    samples: Vec<f32>,
    write_idx: usize,
    len: usize,
    channels: usize,
}

impl CaptureLookback {
    fn new(frames: usize, channels: usize) -> Self {
        let capacity = (frames.max(1) * channels.max(1)).max(2);
        Self {
            samples: vec![0.0; capacity],
            write_idx: 0,
            len: 0,
            channels: channels.max(1),
        }
    }

    fn frame_count(&self) -> usize {
        self.len / self.channels
    }

    fn push_interleaved(&mut self, input: &[f32]) {
        if input.is_empty() {
            return;
        }
        for &s in input {
            self.samples[self.write_idx] = s;
            self.write_idx = (self.write_idx + 1) % self.samples.len();
            if self.len < self.samples.len() {
                self.len += 1;
            }
        }
        let rem = self.len % self.channels;
        if rem != 0 {
            self.len -= rem;
        }
    }

    fn fill_reverse_from_cursor(
        &self,
        out: &mut [f32],
        cursor_frames: &mut Option<usize>,
    ) -> usize {
        let frames_out = out.len() / self.channels;
        if frames_out == 0 {
            return 0;
        }
        let available_frames = self.frame_count();
        if available_frames == 0 {
            return 0;
        }

        let mut cursor = cursor_frames.unwrap_or(available_frames.saturating_sub(1));
        if cursor >= available_frames {
            cursor = available_frames.saturating_sub(1);
        }

        let mut produced_frames = 0usize;
        for frame in 0..frames_out {
            if cursor >= available_frames {
                break;
            }
            let start = self.frame_start_index(cursor, available_frames);
            let dst = frame * self.channels;
            for ch in 0..self.channels {
                out[dst + ch] = self.samples[(start + ch) % self.samples.len()];
            }
            produced_frames += 1;

            if cursor == 0 {
                break;
            }
            cursor -= 1;
        }

        *cursor_frames = Some(cursor);
        produced_frames * self.channels
    }

    fn frame_start_index(&self, frame_idx: usize, available_frames: usize) -> usize {
        let cap = self.samples.len();
        let newest_start = if self.write_idx >= self.channels {
            self.write_idx - self.channels
        } else {
            cap + self.write_idx - self.channels
        };
        let back = (available_frames - 1 - frame_idx) * self.channels;
        (newest_start + cap - (back % cap)) % cap
    }
}

/// The audio engine: opens cpal output and processes audio in a callback.
#[allow(dead_code)]
pub struct AudioEngine {
    pub state: Arc<ControlState>,
    pub meters: [Arc<AtomicMeter>; 3],
    pub master_meter: Arc<AtomicMeter>,
    pub time_pos: [Arc<AtomicF64>; 3],
    pub duration: [Arc<AtomicF64>; 3],
    pub seek_requests: [Arc<AtomicF64>; 3],
    pub lfo_debug: Arc<Mutex<String>>,
    cmd_tx: Mutex<Producer<AudioCommand>>,
    _stream: Option<cpal::Stream>,
    decoders: [Mutex<Option<DecoderThread>>; 3],
    pub captures: [Mutex<Option<PipeCaptureThread>>; 3],
    bufs: [Arc<SharedBuf>; 3],
    /// rtrb producers for the FIFO capture path. Pipe capture thread
    /// pushes f32 samples here; the callback's consumer reads them.
    /// Wrapped in Mutex because `attach_capture` hands the producer
    /// to the pipe thread (which needs ownership).
    capture_producers: [Mutex<Option<rtrb::Producer<f32>>>; 3],
    capture_seek_baseline: [Mutex<f64>; 3],
    capture_reverse: [Arc<CaptureReverseState>; 3],
    /// Device sample rate, stored for pipe capture upsampling.
    device_sr: u32,
    /// Headphone (CUE) output: second cpal stream on a separate device.
    /// Deck C audio is routed here instead of the main mix.
    _headphone_stream: Mutex<Option<cpal::Stream>>,
    headphone_producer: Arc<Mutex<Option<rtrb::Producer<f32>>>>,
    /// Channel for key detection: callback sends accumulated samples,
    /// background thread runs FFT-based key detection.
    key_sample_tx: std::sync::mpsc::Sender<(usize, Vec<f32>, u32)>,
    /// Detected keys from background key analysis thread.
    pub detected_keys: [Arc<Mutex<Option<String>>>; 3],
    /// Per-pad voice ring buffer producers (tick loop writes sample data here).
    pad_voice_producers: Vec<Mutex<Option<rtrb::Producer<f32>>>>,
    /// Direct pad trigger flags: UI sets pad_triggers[pad_idx] = true to request
    /// one-shot playback. The audio callback consumes these and activates a voice.
    pub pad_triggers: Arc<Vec<AtomicBool>>,
    /// Current step per sequence, read by UI for display.
    pub sequence_steps: Arc<Vec<AtomicUsize>>,
    /// Cached sample data per pad, indexed by pad index.
    /// Set from UI thread when a sample is assigned; read by audio callback
    /// when a sequencer step triggers. Tuple: (samples, sample_rate).
    pub pad_sample_cache: PadSampleCache,
}

/// Rate-limited error callback: logs the first error, then at most once every 5 s.
/// Prevents log/POLLERR spam when a device is misconfigured or unavailable.
fn rate_limited_err(prefix: &'static str) -> impl Fn(cpal::StreamError) + Send + Sync + 'static {
    use std::sync::atomic::{AtomicU64, Ordering};
    let last_secs = Arc::new(AtomicU64::new(0));
    let suppressed = Arc::new(AtomicU64::new(0));
    move |err| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let prev = last_secs.swap(now, Ordering::Relaxed);
        if now.saturating_sub(prev) >= 5 {
            let n = suppressed.swap(0, Ordering::Relaxed);
            let extra = if n > 0 {
                format!(" ({} suppressed)", n)
            } else {
                String::new()
            };
            eprintln!("{}{}{}", prefix, err, extra);
        } else {
            suppressed.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl AudioEngine {
    pub fn new() -> Result<Self, String> {
        let selected_backend = default_backend();
        eprintln!("Audio backend requested: {}", backend_label(selected_backend));

        // Detect audio system and ensure ALSA routes through it.
        // On Steam Deck and PipeWire systems, raw ALSA dmix fails because
        // PipeWire has exclusive device control.
        let has_pipewire = crate::audio::backend::detect_pipewire();
        let has_pulse = crate::audio::backend::detect_pulseaudio();
        if has_pipewire {
            eprintln!("Audio: PipeWire detected — ALSA will route through pipewire-alsa");
        } else if has_pulse {
            eprintln!("Audio: PulseAudio detected — ALSA will route through pulse plugin");
        } else {
            eprintln!("Audio: No sound server detected — using raw ALSA");
        }
        crate::audio::backend::ensure_alsa_sound_server_routing();

        let (cmd_producer, cmd_consumer) = RingBuffer::<AudioCommand>::new(CMD_RING_CAPACITY);
        // ControlState split: UI-side handle + audio-side snapshot reader.
        let (state_inner, ctrl_output) = ControlState::new();
        let state = Arc::new(state_inner);
        // ctrl_output is moved into the callback closure below.
        let mut ctrl_output_slot = Some(ctrl_output);
        let mut cmd_consumer_slot = Some(cmd_consumer);

        let meters = [(); 3].map(|_| Arc::new(AtomicMeter::new()));
        let master_meter = Arc::new(AtomicMeter::new());
        let time_pos = [(); 3].map(|_| Arc::new(AtomicF64::new(0.0)));
        let duration = [(); 3].map(|_| Arc::new(AtomicF64::new(0.0)));
        let seek_requests = [(); 3].map(|_| Arc::new(AtomicF64::new(SEEK_SENTINEL)));

        // These don't depend on the device, define before the loop
        let bufs: [Arc<SharedBuf>; 3] = [(); 3].map(|_| Arc::new(SharedBuf::new()));
        let decoders: [Mutex<Option<DecoderThread>>; 3] = [
            Mutex::new(None), Mutex::new(None), Mutex::new(None),
        ];
        let captures: [Mutex<Option<PipeCaptureThread>>; 3] = [
            Mutex::new(None), Mutex::new(None), Mutex::new(None),
        ];
        // rtrb ring buffers for FIFO capture: consumer goes into callback,
        // producer stored in engine for handoff to pipe thread.
        // 32768 f32 samples = ~340 ms at 48 kHz stereo.
        let (cap_prod_0, cap_cons_0) = RingBuffer::<f32>::new(32768);
        let (cap_prod_1, cap_cons_1) = RingBuffer::<f32>::new(32768);
        let (cap_prod_2, cap_cons_2) = RingBuffer::<f32>::new(32768);
        let capture_producers: [Mutex<Option<rtrb::Producer<f32>>>; 3] = [
            Mutex::new(Some(cap_prod_0)),
            Mutex::new(Some(cap_prod_1)),
            Mutex::new(Some(cap_prod_2)),
        ];
        let capture_seek_baseline: [Mutex<f64>; 3] = [
            Mutex::new(0.0),
            Mutex::new(0.0),
            Mutex::new(0.0),
        ];
        let capture_reverse: [Arc<CaptureReverseState>; 3] = [
            Arc::new(CaptureReverseState::new()),
            Arc::new(CaptureReverseState::new()),
            Arc::new(CaptureReverseState::new()),
        ];
        let mut capture_consumers: [Option<rtrb::Consumer<f32>>; 3] = [
            Some(cap_cons_0), Some(cap_cons_1), Some(cap_cons_2),
        ];
        let lfo_debug = Arc::new(Mutex::new(String::new()));

        // Pad sample cache: holds Arc<Vec<f32>> for each pad's cached audio.
        // Updated from UI thread when samples are assigned; read by audio callback.
        let pad_sample_cache: PadSampleCache =
            Arc::new(std::sync::RwLock::new(vec![None; 16]));

        // Direct pad trigger flags: one AtomicBool per pad slot.
        // UI sets pad_triggers[i] = true; audio callback consumes and clears.
        let pad_triggers: Arc<Vec<AtomicBool>> =
            Arc::new((0..16).map(|_| AtomicBool::new(false)).collect());

        // Key detection: background thread receives accumulated mono samples
        // and runs FFT-based key analysis.
        let (key_tx, key_rx) = std::sync::mpsc::channel::<(usize, Vec<f32>, u32)>();
        let detected_keys: [Arc<Mutex<Option<String>>>; 3] = [
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
        ];
        {
            let dk = detected_keys.clone();
            std::thread::Builder::new()
                .name("key-detect".into())
                .spawn(move || {
                    while let Ok((ch, samples, sr)) = key_rx.recv() {
                        if samples.is_empty() || ch >= 3 { continue; }
                        let result = stratum_dsp::analyze_audio(&samples, sr, stratum_dsp::AnalysisConfig::default());
                        if let Ok(analysis) = result {
                            let key_str = analysis.key.numerical();
                            if let Ok(mut guard) = dk[ch].lock() {
                                *guard = Some(key_str);
                            }
                        }
                    }
                })
                .ok();
        }

        // Headphone (CUE) output ring buffer: main callback writes Deck C
        // samples here; headphone cpal callback reads and outputs them.
        let (hp_prod, hp_cons) = RingBuffer::<f32>::new(32768);
        let hp_producer = Arc::new(Mutex::new(Some(hp_prod)));

        // ── Host selection ──
        //
        // On PipeWire systems (Steam Deck), cpal's default ALSA host opens
        // raw ALSA hardware devices directly, bypassing pipewire-alsa.
        // This causes `alsa::poll() returned POLLERR` because PipeWire
        // has exclusive device control.
        //
        // Fix: use cpal's JACK host when PipeWire/JACK is detected.
        // PipeWire provides native JACK compatibility, so cpal streams
        // through JACK → PipeWire → hardware, avoiding the poll error.
        //
        // The device scoring system below ensures the built-in codec
        // (sof-nau8821-max on Steam Deck) is tried first instead of HDMI
        // (which often has no speakers attached).
        let host = cpal::default_host();
        let all_devices: Vec<cpal::Device> = host.output_devices()
            .map_err(|e| format!("Output devices: {}", e))?
            .collect();

        let host_label = format!("{:?}", host.id());

        eprintln!("Audio: cpal host = {}", host_label);
        eprintln!("Audio: found {} output device(s)", all_devices.len());
        for d in &all_devices {
            if let Ok(n) = d.description() {
                eprintln!("Audio:   - {}", n);
            }
        }

        if all_devices.is_empty() {
            let hint = if has_pipewire {
                " PipeWire is running but cpal cannot see any devices. \
                 Ensure pipewire-alsa is installed (e.g. \
                 `pacman -S pipewire-pulse pipewire-alsa` on Arch/SteamOS)."
            } else if has_pulse {
                " PulseAudio is running but cpal cannot see any devices. \
                 Ensure `libpulse-simple` or `pulseaudio` is installed."
            } else {
                ""
            };
            return Err(format!("No audio output device found.{}", hint));
        }

        // ── PipeWire default device preference ──
        //
        // On PipeWire systems, cpal's ALSA host enumerates raw hardware devices
        // (e.g. `sof-nau8821-max`, `hw:0,0`). Opening these directly bypasses
        // pipewire-alsa and causes `alsa::poll() returned POLLERR` because
        // PipeWire has exclusive device control.
        //
        // The ALSA "default" device routes through pipewire-alsa → PipeWire,
        // which handles all device sharing and mixing correctly.
        // Prefer it when PipeWire is detected and a default device exists.
        let pipewire_default: Option<cpal::Device> = if has_pipewire {
            let dev = host.default_output_device();
            if let Some(ref d) = dev
                && let Ok(n) = d.description() {
                    eprintln!("Audio: PipeWire default output device: {}", n);
                }
            dev
        } else {
            None
        };

        // ── Device classification and scoring ──
        //
        // cpal on Linux enumerates raw ALSA devices. On PipeWire systems, these
        // include both real hardware (sof-nau8821-max, USB DACs) and virtual/null
        // devices (dmix, "Discard all samples"). The default ALSA device may point
        // to HDMI (which often has no speakers) instead of the built-in codec.
        //
        // Score devices so the best candidate is tried first:
        //   100 = built-in codec (sof-*, realtek, nau8821, etc.)
        //    90 = generic analog / hw:*
        //    80 = USB audio
        //    70 = Bluetooth / A2DP
        //    40 = HDMI / DisplayPort (often no speakers attached)
        //    10 = virtual / pulse / pipewire sinks
        //     0 = null / discard (skipped entirely)

        fn score_device(name: &str) -> u8 {
            let lower = name.to_lowercase();

            // Null / dummy — skip entirely
            if lower.contains("discard all samples")
                || lower.contains("generate zero samples")
                || lower == "null"
                || lower.contains("(null)")
            {
                return 0;
            }

            // Built-in audio codec — highest priority
            // macOS: "MacBook Pro Speakers", "Mac mini Speakers", etc.
            // Steam Deck: sof-nau8821-max
            // Generic: realtek, alc, hda-intel analog, etc.
            if lower.contains("macbook")
                || lower.contains("mac mini")
                || lower.contains("imac")
                || lower.starts_with("sof-")
                || lower.starts_with("sof_")
                || lower.contains("realtek")
                || lower.contains("nau8821")
                || lower.contains("alc8")
                || lower.contains("alc2")
                || lower.contains("built-in")
                || (lower.contains("hda-intel")
                    && !lower.contains("hdmi")
                    && !lower.contains("displayport"))
                || (lower.contains("analog")
                    && !lower.contains("hdmi"))
            {
                return 100;
            }

            // USB audio
            if lower.contains("usb")
                || lower.contains("dac")
                || lower.contains("focusrite")
                || lower.contains("scarlett")
                || lower.contains("uca222")
                || lower.contains("audiophile")
            {
                return 80;
            }

            // Thunderbolt audio
            if lower.contains("thunderbolt") || lower.contains("caldigit")
            {
                return 75;
            }

            // Bluetooth / A2DP
            if lower.contains("bluetooth")
                || lower.contains("a2dp")
                || lower.contains("bluez")
            {
                return 70;
            }

            // HDMI / DisplayPort / external monitors — often no speakers
            if lower.contains("hdmi")
                || lower.contains("displayport")
                || lower.contains("dp-")
                || lower.starts_with("phl ")    // Philips monitors
                || lower.starts_with("dell ")   // Dell monitors
                || lower.starts_with("lg ")     // LG monitors
                || lower.starts_with("samsung ") // Samsung monitors
                || lower.starts_with("benq ")   // BenQ monitors
                || lower.starts_with("asus ")   // ASUS monitors
                || lower.starts_with("acer ")   // Acer monitors
                || lower.starts_with("hp ")     // HP monitors
                || lower.starts_with("viewsonic") // ViewSonic monitors
            {
                return 40;
            }

            // Virtual / PulseAudio sinks
            if lower.contains("pulse")
                || lower.contains("pipewire")
                || lower.contains("virtual")
            {
                return 10;
            }

            // Generic device — moderate priority
            60
        }

        // Filter out null devices and score the rest
        let mut scored: Vec<(u8, &cpal::Device)> = all_devices.iter()
            .filter(|d| {
                d.description().ok()
                    .map(|n| score_device(&n.to_string()) > 0)
                    .unwrap_or(true)
            })
            .map(|d| {
                let score = d.description().ok()
                    .map(|n| score_device(&n.to_string()))
                    .unwrap_or(50);
                (score, d)
            })
            .collect();

        // Sort by score descending — best device first
        scored.sort_by(|a, b| b.0.cmp(&a.0));

        if scored.is_empty() && !all_devices.is_empty() {
            eprintln!("Audio: warning — only null/dummy devices found, trying them as last resort");
        }

        eprintln!("Audio: device candidates (by score):");
        for (score, d) in scored.iter().take(8) {
            if let Ok(n) = d.description() {
                eprintln!("Audio:   [score={}] {}", score, n);
            }
        }

        // Build candidate list: ranked real devices, then fall back to unranked.
        // On PipeWire systems, prepend the ALSA "default" device (which routes
        // through pipewire-alsa → PipeWire) before raw hardware devices.
        // This prevents alsa::poll() POLLERR from opening raw ALSA devices
        // while PipeWire has exclusive hardware control.
        let candidates: Vec<&cpal::Device> = if has_pipewire {
            if let Some(ref pw_dev) = pipewire_default {
                let pw_name_str = pw_dev.description().ok()
                    .map(|d| d.to_string()).unwrap_or_default();
                let mut list: Vec<&cpal::Device> = Vec::new();
                list.push(pw_dev);
                for (_, d) in scored.iter() {
                    let d_name_str = d.description().ok()
                        .map(|d| d.to_string()).unwrap_or_default();
                    if !pw_name_str.is_empty() && pw_name_str == d_name_str {
                        continue;
                    }
                    list.push(*d);
                }
                for d in all_devices.iter() {
                    let d_name_str = d.description().ok()
                        .map(|d| d.to_string()).unwrap_or_default();
                    if !pw_name_str.is_empty() && pw_name_str == d_name_str {
                        continue;
                    }
                    if !list.iter().any(|c| {
                        c.description().ok().map(|d| d.to_string()) == Some(d_name_str.clone())
                    }) {
                        list.push(d);
                    }
                }
                eprintln!("Audio: PipeWire default device prioritized in candidate list");
                list
            } else {
                let mut list: Vec<&cpal::Device> = scored.iter().map(|(_, d)| *d).collect();
                if list.is_empty() {
                    list = all_devices.iter().collect();
                }
                list
            }
        } else {
            let mut list: Vec<&cpal::Device> = scored.iter().map(|(_, d)| *d).collect();
            if list.is_empty() {
                list = all_devices.iter().collect();
            }
            list
        };

        let mut last_err = String::new();
        let mut stream: Option<cpal::Stream> = None;
        let mut actual_sr: u32 = 48000;
        let mut main_device_name = String::new();
        let mut pad_voice_producers_vec: Vec<rtrb::Producer<f32>> = Vec::with_capacity(MAX_PAD_VOICES);
        let sequence_steps = Arc::new((0..MAX_PAD_VOICES).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());

        for device in &candidates {
            let name = device.description().ok().map(|d| d.to_string()).unwrap_or_default();
            let configs: Vec<_> = match device.supported_output_configs() {
                Ok(c) => c.collect(),
                Err(_) => continue,
            };
            let picked = configs.iter()
                .find(|c| c.channels() >= 2 && c.sample_format() == cpal::SampleFormat::F32)
                .or_else(|| configs.first());
            let picked = match picked {
                Some(p) => p,
                None => continue,
            };

            let fmt = picked.sample_format();
            // Use 48 kHz when available — matches MPV's reliable output rate.
            // Higher rates cause rate mismatch if MPV doesn't honor
            // --audio-samplerate. Fall back to max if 48k isn't in range.
            let sr: u32 = if picked.min_sample_rate() <= 48000
                && picked.max_sample_rate() >= 48000
            {
                48000
            } else {
                picked.max_sample_rate()
            };
            let cfg = (*picked).with_sample_rate(sr).config();
            let mut cfg = cfg;
            cfg.buffer_size = cpal::BufferSize::Fixed(256);
            let cb_meters_inner = meters.clone();
            let cb_master_meter_inner = Arc::clone(&master_meter);
            let cb_bufs_inner = bufs.clone();
            let cb_lfo_debug_inner = Arc::clone(&lfo_debug);
            let cb_key_sample_tx = key_tx.clone();
            let cb_time_pos = time_pos.clone();
            let cb_duration = duration.clone();
            let cb_seek_requests = seek_requests.clone();
            let cb_capture_reverse = capture_reverse.clone();
            let cb_hp_producer = Arc::clone(&hp_producer);
            let cb_pad_sample_cache = Arc::clone(&pad_sample_cache);
            let cb_sequence_steps = Arc::clone(&sequence_steps);
            let cb_pad_triggers = Arc::clone(&pad_triggers);

            // Audio thread owns these outright — moved into the closure.
            // If a previous device's build_output_stream failed, the state was
            // consumed by its closure. Detect that and skip remaining devices.
            let Some(mut ctrl_output) = ctrl_output_slot.take() else {
                last_err = "internal state consumed by previous stream build failure".to_string();
                continue;
            };
            let Some(mut cmd_consumer) = cmd_consumer_slot.take() else {
                ctrl_output_slot = Some(ctrl_output);
                last_err = "internal state consumed by previous stream build failure".to_string();
                continue;
            };
            // rtrb consumers for FIFO capture — moved into the closure.
            let mut cap_consumers: [Option<rtrb::Consumer<f32>>; 3] = [
                capture_consumers[0].take(),
                capture_consumers[1].take(),
                capture_consumers[2].take(),
            ];
            // Headphone (CUE) ring buffer consumer — Deck C samples go here
            let sr_hz = sr;
            let mut capture_lookback: [CaptureLookback; 3] = [
                CaptureLookback::new((sr_hz as f32 * CAPTURE_REVERSE_LOOKBACK_SECS) as usize, 2),
                CaptureLookback::new((sr_hz as f32 * CAPTURE_REVERSE_LOOKBACK_SECS) as usize, 2),
                CaptureLookback::new((sr_hz as f32 * CAPTURE_REVERSE_LOOKBACK_SECS) as usize, 2),
            ];
            let mut dsp_state: [DspFilters; 3] = [
                DspFilters::new(sr_hz as f32),
                DspFilters::new(sr_hz as f32),
                DspFilters::new(sr_hz as f32),
            ];
            let mut onset_state: [OnsetState; 3] = [OnsetState::new(), OnsetState::new(), OnsetState::new()];
            let mut deck_bufs: [Vec<f32>; 3] = [
                vec![0.0f32; 8192], vec![0.0f32; 8192], vec![0.0f32; 8192],
            ];
            let mut float_buf: Vec<f32> = vec![0.0f32; 8192];
            let channels = cfg.channels as usize;

            // Pad voice ring buffers for this stream attempt
            let mut pad_voice_consumers_vec: Vec<rtrb::Consumer<f32>> = Vec::with_capacity(MAX_PAD_VOICES);
            for _ in 0..MAX_PAD_VOICES {
                let (prod, cons) = RingBuffer::<f32>::new(PAD_RING_SIZE);
                pad_voice_producers_vec.push(prod);
                pad_voice_consumers_vec.push(cons);
            }
            let mut pad_voices = PadVoiceState::new(pad_voice_consumers_vec, sr_hz as f32);

            let result = match fmt {
                cpal::SampleFormat::F32 => {
                    device.build_output_stream(
                        &cfg,
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                             audio_callback(data, &mut ctrl_output, &mut cmd_consumer,
                                            &cb_meters_inner, &cb_master_meter_inner,
                                            &cb_bufs_inner, &mut dsp_state,
                                            &cb_lfo_debug_inner, &mut deck_bufs,
                                             &mut cap_consumers,
                                             &mut capture_lookback,
                                             &cb_capture_reverse,
                                             sr_hz as f32,
                                             &cb_time_pos,
                                             &cb_duration,
                                             &cb_seek_requests,
                                              cb_hp_producer.as_ref(),
                                              &mut pad_voices,
                                              &cb_pad_sample_cache,
                                              &cb_sequence_steps,
                                              &cb_pad_triggers,
                                              &mut onset_state,
                                              &cb_key_sample_tx);
                        },
                        rate_limited_err("Audio: "),
                        None,
                    )
                }
                cpal::SampleFormat::I16 => {
                    device.build_output_stream(
                        &cfg,
                        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                            let frames = data.len() / channels;
                            let need = frames * 2;
                            if float_buf.len() < need { float_buf.resize(need, 0.0); }
                            let fb = &mut float_buf[..need];
                             audio_callback(fb, &mut ctrl_output, &mut cmd_consumer,
                                           &cb_meters_inner, &cb_master_meter_inner,
                                           &cb_bufs_inner, &mut dsp_state,
                                           &cb_lfo_debug_inner, &mut deck_bufs,
                                             &mut cap_consumers,
                                             &mut capture_lookback,
                                             &cb_capture_reverse,
                                             sr_hz as f32,
                                             &cb_time_pos,
                                             &cb_duration,
                                              &cb_seek_requests,
                                               cb_hp_producer.as_ref(),
                                              &mut pad_voices,
                                              &cb_pad_sample_cache,
                                              &cb_sequence_steps,
                                              &cb_pad_triggers,
                                              &mut onset_state,
                                              &cb_key_sample_tx);
                            for f in 0..frames {
                                for ci in 0..channels.min(2) {
                                    let idx = f * channels + ci;
                                    data[idx] = (soft_limit(fb[f * 2 + ci]) * 32767.0) as i16;
                                }
                            }
                        },
                        rate_limited_err("Audio: "),
                        None,
                    )
                }
                cpal::SampleFormat::I32 => {
                    device.build_output_stream(
                        &cfg,
                        move |data: &mut [i32], _: &cpal::OutputCallbackInfo| {
                            let frames = data.len() / channels;
                            let need = frames * 2;
                            if float_buf.len() < need { float_buf.resize(need, 0.0); }
                            let fb = &mut float_buf[..need];
                              audio_callback(fb, &mut ctrl_output, &mut cmd_consumer,
                                            &cb_meters_inner, &cb_master_meter_inner,
                                            &cb_bufs_inner, &mut dsp_state,
                                            &cb_lfo_debug_inner, &mut deck_bufs,
                                              &mut cap_consumers,
                                              &mut capture_lookback,
                                              &cb_capture_reverse,
                                              sr_hz as f32,
                                              &cb_time_pos,
                                              &cb_duration,
                                               &cb_seek_requests,
                                                cb_hp_producer.as_ref(),
                                                &mut pad_voices,
                                                 &cb_pad_sample_cache,
                                                 &cb_sequence_steps,
                                                 &cb_pad_triggers,
                                                 &mut onset_state,
                                                 &cb_key_sample_tx);
                            for f in 0..frames {
                                for ci in 0..channels.min(2) {
                                    let idx = f * channels + ci;
                                    data[idx] = (soft_limit(fb[f * 2 + ci]) * 2147483647.0) as i32;
                                }
                            }
                        },
                        rate_limited_err("Audio: "),
                        None,
                    )
                }
                other => {
                    last_err = format!("Unsupported format {:?} on {}", other, name);
                    // Return the taken slots so the next device attempt can use them.
                    ctrl_output_slot = Some(ctrl_output);
                    cmd_consumer_slot = Some(cmd_consumer);
                    continue;
                }
            };

            match result {
                Ok(s) => {
                    match s.play() {
                        Ok(_) => {
                            stream = Some(s);
                            actual_sr = sr;
                            main_device_name = name.clone();
                            eprintln!("Audio: streaming on '{}' at {}Hz ch={}", name, sr_hz, cfg.channels);
                            break;
                        }
                        Err(e) => {
                            // Stream built but couldn't play — the captured state
                            // (ctrl_output, cmd_consumer) was consumed by the closure,
                            // so the next iteration will detect missing state and skip.
                            let hint = if has_pipewire {
                                " On PipeWire/Steam Deck, try: \
                                 `pw-dump | jq .info.name` to check device names, \
                                 or install `pipewire-alsa` if missing."
                            } else {
                                ""
                            };
                            last_err = format!("Device '{}': play failed: {}{}", name, e, hint);
                            eprintln!("Audio: {}", last_err);
                            continue;
                        }
                    }
                }
                Err(e) => {
                    let hint = if has_pipewire {
                        " On PipeWire/Steam Deck, ensure `pipewire-alsa` is installed \
                         and ALSA is not hardcoded to dmix."
                    } else {
                        ""
                    };
                    last_err = format!("Device '{}': build_output_stream failed: {}{}", name, e, hint);
                    eprintln!("Audio: {}", last_err);
                    continue;
                }
            }
        }

        let stream = stream.ok_or_else(|| {
            let hint = if has_pipewire {
                " PipeWire is running but no audio device could be opened. \
                 Try running `wpctl status` to check PipeWire nodes, \
                 or ensure pipewire-alsa is installed."
            } else {
                ""
            };
            format!("No working audio device found. Last error: {}{}", last_err, hint)
        })?;

        // Try to find a headphone device: prefer a device with a different score
        // (i.e. different type — BT, USB, etc.) from the main device.
        // All cpal Device instances for the same ALSA device share the same
        // description string, so filtering by name comparison fails on systems
        // with multiple sub-devices (e.g. Steam Deck's many `sof-nau8821-max`
        // entries). Instead, walk the ranked `candidates` list (already sorted
        // by score descending) and pick the first entry whose score differs
        // from the main device's score. If nothing differs, leave headphone
        // unassigned — routing to HDMI on a device with no speaker causes
        // POLLERR spam and no audible output.
        let hp_device = {
            let main_score = score_device(&main_device_name);
            candidates.iter()
                .find(|d| {
                    let name = d.description().ok().map(|n| n.to_string()).unwrap_or_default();
                    let s = score_device(&name);
                    s > 0 && s != main_score
                })
                .cloned()
        };

        let mut hp_stream: Option<cpal::Stream> = None;

        if let Some(hp_dev) = hp_device {
            let hp_name = match hp_dev.description() {
                Ok(n) => n.to_string(),
                Err(_) => String::from("unknown"),
            };
            let hp_configs: Vec<_> = match hp_dev.supported_output_configs() {
                Ok(c) => c.collect(),
                Err(_) => vec![],
            };
            let hp_picked = hp_configs.iter()
                .find(|c| c.channels() >= 2 && c.sample_format() == cpal::SampleFormat::F32)
                .or_else(|| hp_configs.first());
            if let Some(hp_cfg) = hp_picked {
                let sr: u32 = if hp_cfg.min_sample_rate() <= 48000
                    && hp_cfg.max_sample_rate() >= 48000
                {
                    48000
                } else {
                    hp_cfg.max_sample_rate()
                };
                let cfg = (*hp_cfg).with_sample_rate(sr).config();
                let mut cfg = cfg;
                cfg.buffer_size = cpal::BufferSize::Fixed(256);
                let channels = cfg.channels as usize;
                // Move the consumer into the headphone callback
                let mut hp_cons_move = hp_cons;
                let hp_result = hp_dev.build_output_stream(
                    &cfg,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        headphone_callback(data, &mut hp_cons_move, channels);
                    },
                    rate_limited_err("Headphone audio: "),
                    None,
                );
                match hp_result {
                    Ok(s) => {
                        hp_stream = Some(s);
                        eprintln!("Audio: headphone stream on '{}'", hp_name);
                    }
                    Err(e) => eprintln!("Audio: headphone stream failed: {}", e),
                }
            }
        }

        // Spawn a background thread that syncs all non-default output device
        // volumes to match the default output (speaker) volume. This way when
        // the user changes the system volume via media keys, both the speakers
        // and any headphone/external device track together.
        #[cfg(target_os = "macos")]
        {
            std::thread::Builder::new()
                .name("hp-volume-sync".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        if let Some(speaker_vol) = macos_volume::read_system_volume() {
                            macos_volume::set_all_non_default_volumes(speaker_vol);
                        }
                    }
                })
                .ok();
        }

        Ok(Self {
            state, meters, master_meter, time_pos, duration, lfo_debug,
            seek_requests,
            cmd_tx: Mutex::new(cmd_producer), _stream: Some(stream),
            decoders, captures, bufs, capture_producers, capture_seek_baseline,
            capture_reverse, device_sr: actual_sr,
            _headphone_stream: Mutex::new(hp_stream),
            headphone_producer: hp_producer,
            key_sample_tx: key_tx,
            detected_keys,
            pad_voice_producers: pad_voice_producers_vec.into_iter()
                .map(|p| Mutex::new(Some(p)))
                .collect(),
            sequence_steps,
            pad_sample_cache,
            pad_triggers,
        })
    }

    /// Load a file for a deck: creates a decoder thread and wires the ring buffer.
    /// I/O happens on the calling thread (UI thread) — do not call from audio callback.
    pub fn load_file(&self, ch: usize, path: String) {
        if ch >= MAX_DECKS { return; }
        match DecoderThread::load(Path::new(&path)) {
            Ok(decoder) => {
                self.time_pos[ch].store(0.0);
                self.duration[ch].store(decoder.duration_secs);
                self.seek_requests[ch].store(SEEK_SENTINEL);
                self.bufs[ch].set(Arc::clone(&decoder.ring));
                decoder.play();
                *self.decoders[ch].lock().unwrap() = Some(decoder);
            }
            Err(e) => eprintln!("Audio: load error: {}", e),
        }
    }

    /// Switch the headphone (CUE) output to a different device.
    /// Drops the old headphone stream, creates a new ring buffer pair,
    /// and builds a new cpal stream on the target device.
    pub fn set_headphone_device(&self, device_name: &str) {
        let host = cpal::default_host();
        let devices: Vec<_> = match host.output_devices() {
            Ok(it) => it.collect(),
            Err(e) => {
                eprintln!("Audio: output_devices: {}", e);
                return;
            }
        };
        let target = devices.iter().find(|d| {
            d.description().ok().map(|n| n.to_string()).as_deref() == Some(device_name)
        });
        let target = match target {
            Some(d) => d,
            None => {
                eprintln!("Audio: headphone device '{}' not found", device_name);
                return;
            }
        };

        let configs: Vec<_> = match target.supported_output_configs() {
            Ok(c) => c.collect(),
            Err(_) => {
                eprintln!("Audio: headphone device '{}' has no configs", device_name);
                return;
            }
        };
        let picked = configs.iter()
            .find(|c| c.channels() >= 2 && c.sample_format() == cpal::SampleFormat::F32)
            .or_else(|| configs.first());
        let picked = match picked {
            Some(p) => p,
            None => {
                eprintln!("Audio: headphone device '{}' has no F32 config", device_name);
                return;
            }
        };
        let sr: u32 = if picked.min_sample_rate() <= 48000
            && picked.max_sample_rate() >= 48000
        {
            48000
        } else {
            picked.max_sample_rate()
        };
        let cfg = (*picked).with_sample_rate(sr).config();
        let mut cfg = cfg;
        cfg.buffer_size = cpal::BufferSize::Fixed(256);
        let channels = cfg.channels as usize;

        // Create new ring buffer pair (old one is dropped with old stream)
        let (hp_prod, hp_cons) = RingBuffer::<f32>::new(32768);
        let mut hp_cons_move = hp_cons;

        let hp_result = target.build_output_stream(
            &cfg,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                headphone_callback(data, &mut hp_cons_move, channels);
            },
            rate_limited_err("Headphone audio: "),
            None,
        );
        match hp_result {
            Ok(s) => {
                // Replace stream and producer (old stream is dropped here,
                // which drops old consumer, and old producer is replaced).
                *self._headphone_stream.lock().unwrap() = Some(s);
                *self.headphone_producer.lock().unwrap() = Some(hp_prod);
                eprintln!("Audio: headphone switched to '{}'", device_name);
            }
            Err(e) => eprintln!("Audio: headphone stream to '{}' failed: {}", device_name, e),
        }
    }

    /// Attach an MPV FIFO capture to a deck. MPV must be writing raw
    /// stereo f32-LE PCM to the given path (see `auto-pcm-pipe.lua`).
    /// The engine's SVF, EQ, LFO chain will apply to the captured audio,
    /// giving smooth cutoff sweeps without ffmpeg biquad transients.
    ///
    /// Audio flows: FIFO → pipe thread → rtrb producer → rtrb consumer
    /// (in callback) → DSP → cpal output. No SharedBuf indirection.
    pub fn attach_capture(&self, ch: usize, path: &Path) -> Result<(), String> {
        if ch >= MAX_DECKS {
            return Err(format!("bad deck index {}", ch));
        }
        if !path.exists() {
            return Err(format!("FIFO not found: {}", path.display()));
        }
        // Detach any existing decoder/capture on this deck first.
        *self.decoders[ch].lock().unwrap() = None;
        self.detach_capture(ch);
        self.bufs[ch].clear();
        self.time_pos[ch].store(0.0);
        self.seek_requests[ch].store(SEEK_SENTINEL);
        self.capture_reverse[ch].set_active(false);
        if let Ok(mut base) = self.capture_seek_baseline[ch].lock() {
            *base = 0.0;
        }

        // Take the rtrb producer for this deck — pipe thread needs ownership.
        let producer = self.capture_producers[ch].lock().unwrap().take()
            .ok_or_else(|| format!("capture producer for ch {} already taken", ch))?;

        match PipeCaptureThread::open_with_producer(path, producer) {
            Ok(cap) => {
                *self.captures[ch].lock().unwrap() = Some(cap);
                eprintln!("Audio: capture attached ch={} path={}", ch, path.display());
                Ok(())
            }
            Err(e) => {
                // open_with_producer consumed the producer on failure — we can't
                // get it back, so create a fresh ring buffer pair for future use.
                let (prod, _cons) = rtrb::RingBuffer::<f32>::new(32768);
                *self.capture_producers[ch].lock().unwrap() = Some(prod);
                // Note: the audio callback's consumer is stale now; it will stop
                // receiving samples. A re-attach is needed to fully recover.
                Err(e)
            }
        }
    }

    pub fn scrub_relative(&self, ch: usize, delta_secs: f64) {
        if ch >= MAX_DECKS {
            return;
        }
        let reverse = delta_secs < 0.0;
        if let Ok(guard) = self.decoders[ch].lock()
            && let Some(decoder) = guard.as_ref() {
                decoder.set_reverse_scrub(reverse);
                let mut target = self.time_pos[ch].load() + delta_secs;
                let dur = self.duration[ch].load();
                if dur > 0.0 {
                    target = target.clamp(0.0, dur);
                } else {
                    target = target.max(0.0);
                }
                decoder.seek_to(target);
                self.time_pos[ch].store(target);
                self.seek_requests[ch].store(target);
                return;
            }

        if self.has_capture(ch) {
            self.capture_reverse[ch].set_active(reverse);
            let anchor = self.time_pos[ch].load().max(0.0);
            let target = (anchor + delta_secs).max(0.0);
            if let Ok(mut base) = self.capture_seek_baseline[ch].lock() {
                *base = target;
            }
            self.time_pos[ch].store(target);
            self.seek_requests[ch].store(target);
        }
    }

    pub fn reset_capture_time_pos(&self, ch: usize) {
        if ch >= MAX_DECKS || !self.has_capture(ch) {
            return;
        }
        self.capture_reverse[ch].set_active(false);
        self.time_pos[ch].store(0.0);
        self.seek_requests[ch].store(SEEK_SENTINEL);
        if let Ok(mut base) = self.capture_seek_baseline[ch].lock() {
            *base = 0.0;
        }
    }

    /// Whether a deck is currently receiving PCM from an MPV FIFO capture.
    pub fn has_capture(&self, ch: usize) -> bool {
        self.captures.get(ch)
            .map(|m| m.lock().unwrap().is_some())
            .unwrap_or(false)
    }

    /// Whether a given deck has a decoder OR capture active (i.e. audio
    /// flowing through engine — MPV af LPF/HPF should be skipped in this case).
    pub fn has_decoder(&self, ch: usize) -> bool {
        let has_dec = self.decoders.get(ch)
            .map(|m| m.lock().unwrap().is_some())
            .unwrap_or(false);
        has_dec || self.has_capture(ch)
    }

    /// Stop decoder for a deck (drops the decoder thread, clears ring buffer).
    pub fn stop_decoder(&self, ch: usize) {
        if let Some(decoder) = self.decoders.get(ch) {
            let mut guard = decoder.lock().unwrap();
            if let Some(dec) = guard.as_ref() {
                dec.set_reverse_scrub(false);
            }
            *guard = None;
        }
        self.detach_capture(ch);
        if ch < MAX_DECKS {
            self.capture_reverse[ch].set_active(false);
            self.bufs[ch].clear();
            self.time_pos[ch].store(0.0);
            self.duration[ch].store(0.0);
            self.seek_requests[ch].store(SEEK_SENTINEL);
            if let Ok(mut base) = self.capture_seek_baseline[ch].lock() {
                *base = 0.0;
            }
        }
    }

    fn detach_capture(&self, ch: usize) {
        if ch >= MAX_DECKS {
            return;
        }

        let cap = self.captures[ch].lock().unwrap().take();
        let Some(cap) = cap else {
            return;
        };

        self.capture_reverse[ch].set_active(false);

        let producer = cap.shutdown();
        if let Some(prod) = producer {
            let mut slot = self.capture_producers[ch].lock().unwrap();
            if slot.is_none() {
                *slot = Some(prod);
            } else {
                eprintln!("Audio: capture producer already present for ch={}, dropping reclaimed producer", ch);
            }
        } else {
            eprintln!("Audio: failed to reclaim capture producer for ch={}", ch);
        }
    }

    pub fn set_decoder_reverse_scrub(&self, ch: usize, enabled: bool) {
        if ch >= MAX_DECKS {
            return;
        }
        if let Some(decoder) = self.decoders[ch].lock().unwrap().as_ref() {
            decoder.set_reverse_scrub(enabled);
        }
    }

    pub fn set_capture_reverse_scrub(&self, ch: usize, enabled: bool) {
        if ch >= MAX_DECKS {
            return;
        }
        self.capture_reverse[ch].set_active(enabled);
    }

    /// Store cached sample data for a pad. Called from UI thread when a sample
    /// is assigned to a pad. The audio callback reads this when a sequencer
    /// step triggers.
    pub fn set_pad_sample(&self, pad_idx: usize, samples: Vec<f32>, sample_rate: u32, channels: u16) {
        if let Ok(mut cache) = self.pad_sample_cache.write()
            && pad_idx < cache.len() {
                // Ensure samples are always stereo interleaved for the voice mixer.
                // Mono files get each sample duplicated to L/R.
                let stereo_samples = if channels == 1 {
                    let mut out = Vec::with_capacity(samples.len() * 2);
                    for &s in &samples {
                        out.push(s);
                        out.push(s);
                    }
                    out
                } else {
                    samples
                };
                cache[pad_idx] = Some(Arc::new((stereo_samples, sample_rate)));
            }
    }

    /// Clear cached sample data for a pad.
    #[allow(dead_code)]
    pub fn clear_pad_sample(&self, pad_idx: usize) {
        if let Ok(mut cache) = self.pad_sample_cache.write()
            && pad_idx < cache.len() {
                cache[pad_idx] = None;
            }
    }

    /// Publish sequence state from UI to audio thread via the control snapshot.
    pub fn sync_sequences(&self, seqs: Vec<SequenceSnapshot>) {
        self.state.set_sequences(seqs);
    }

    /// Read the current step per sequence (set by the audio callback).
    pub fn read_sequence_steps(&self) -> Vec<usize> {
        self.sequence_steps.iter()
            .map(|a| a.load(Ordering::Relaxed))
            .collect()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        // Suppress cpal's "Dropping DeviceSink" warning on exit.
        // The stream is inert at this point (process is terminating).
        if let Some(stream) = self._stream.take() {
            std::mem::forget(stream);
        }
    }
}

/// Volume boost applied to Deck C headphone output to compensate for
/// headphone output being quieter than the main speakers. Multiplied
/// on top of the fader, master, and system volume.
const DECK_C_VOLUME_OFFSET: f32 = 6.0;

/// Headphone (CUE) output callback: reads Deck C samples from ring buffer
/// and writes to the headphone cpal stream. Outputs silence when buffer empty.
/// Applies macOS system output volume so Deck C scales with the same slider
/// that controls Decks A/B on the speakers.
fn headphone_callback(
    data: &mut [f32],
    consumer: &mut rtrb::Consumer<f32>,
    channels: usize,
) {
    let sys_vol: f32 = if cfg!(target_os = "macos") {
        #[cfg(target_os = "macos")]
        {
            let v = macos_volume::read_system_volume();
            use std::sync::atomic::AtomicU64;
            static LOG_COUNTER: AtomicU64 = AtomicU64::new(0);
            let c = LOG_COUNTER.fetch_add(1, Ordering::Relaxed);
            if (c < 5 || c % 2000 == 0)
                && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true)
                    .open("/tmp/termixer_hp_debug.log") {
                    use std::io::Write;
                    let _ = writeln!(f, "hp cb#{}: sys_vol={:?}", c, v);
                }
            v.unwrap_or(1.0)
        }
        #[cfg(not(target_os = "macos"))]
        { 1.0 }
    } else {
        1.0
    };

    let frames = data.len() / channels;
    let available = consumer.slots();
    let stereo_samples = available / 2;
    let to_read = frames.min(stereo_samples);
    if to_read > 0
        && let Ok(chunk) = consumer.read_chunk(to_read * 2) {
            let (first, second) = chunk.as_slices();
            let mut written = 0;
            for sample_pair in first.chunks(2) {
                if written >= frames { break; }
                let l = soft_limit(sample_pair.first().copied().unwrap_or(0.0) * sys_vol * DECK_C_VOLUME_OFFSET);
                let r = soft_limit(sample_pair.get(1).copied().unwrap_or(l) * sys_vol * DECK_C_VOLUME_OFFSET);
                for ci in 0..channels.min(2) {
                    data[written * channels + ci] = if ci == 0 { l } else { r };
                }
                for ci in 2..channels {
                    data[written * channels + ci] = 0.0;
                }
                written += 1;
            }
            for sample_pair in second.chunks(2) {
                if written >= frames { break; }
                let l = soft_limit(sample_pair.first().copied().unwrap_or(0.0) * sys_vol * DECK_C_VOLUME_OFFSET);
                let r = soft_limit(sample_pair.get(1).copied().unwrap_or(l) * sys_vol * DECK_C_VOLUME_OFFSET);
                for ci in 0..channels.min(2) {
                    data[written * channels + ci] = if ci == 0 { l } else { r };
                }
                for ci in 2..channels {
                    data[written * channels + ci] = 0.0;
                }
                written += 1;
            }
            chunk.commit_all();
        }
    let written_frames = to_read;
    for f in written_frames..frames {
        for c in 0..channels {
            data[f * channels + c] = 0.0;
        }
    }
}

/// Per-pad voice state for sequencer playback in the audio callback.
pub struct PadVoiceState {
    /// Sample rate for the voices.
    pub sample_rate: f32,
    /// Active voice playback: which voice is playing, current position,
    /// and index into pad_sample_cache.
    pub voice_active: Vec<bool>,
    pub voice_position: Vec<usize>,
    pub voice_pad_idx: Vec<usize>,
    /// Per-voice envelope gain (0.0–1.0). Ramps up on attack, stays at 1.0.
    pub voice_gain: Vec<f32>,
    /// Per-voice sequence index (to look up volume/mute).
    pub voice_seq_idx: Vec<usize>,
}

impl PadVoiceState {
    pub fn new(_consumers: Vec<rtrb::Consumer<f32>>, sample_rate: f32) -> Self {
        let n = _consumers.len();
        Self {
            sample_rate,
            voice_active: vec![false; n],
            voice_position: vec![0; n],
            voice_pad_idx: vec![0; n],
            voice_gain: vec![0.0; n],
            voice_seq_idx: vec![0; n],
        }
    }
}

/// Fixed-size ring buffer for RT-safe use in the audio callback.
struct RingBuf<T, const N: usize> {
    buf: [MaybeUninit<T>; N],
    head: usize,
    len: usize,
}

impl<T: Copy, const N: usize> RingBuf<T, N> {
    fn new(init: T) -> Self {
        // SAFETY: T is Copy, so uninitialized -> initialized via copy is fine.
        // We write all N slots during construction.
        let mut buf: [MaybeUninit<T>; N] = unsafe { MaybeUninit::uninit().assume_init() };
        for slot in buf.iter_mut() {
            *slot = MaybeUninit::new(init);
        }
        Self { buf, head: 0, len: 0 }
    }

    fn push(&mut self, val: T) {
        let tail = (self.head + self.len) % N;
        self.buf[tail] = MaybeUninit::new(val);
        if self.len < N {
            self.len += 1;
        } else {
            self.head = (self.head + 1) % N;
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn iter(&self) -> impl Iterator<Item = &T> {
        (0..self.len).map(move |i| {
            let idx = (self.head + i) % N;
            // SAFETY: all slots [0..len] are initialized
            unsafe { self.buf[idx].assume_init_ref() }
        })
    }
}

use std::mem::MaybeUninit;

/// Persistent onset detection state for a single deck.
pub struct OnsetState {
    /// Fixed-size ring buffer for energy history (max 100 samples).
    energy_ring: RingBuf<f32, 100>,
    /// Fixed-size ring buffer for onset timestamps (max 32 entries).
    onset_times: RingBuf<Instant, 32>,
    pub last_onset: Instant,
    pub frame_counter: u64,
    /// Pre-allocated buffer for key detection samples (~5s at 48kHz).
    key_sample_buffer: Vec<f32>,
}

impl OnsetState {
    pub fn new() -> Self {
        Self {
            energy_ring: RingBuf::new(0.0),
            onset_times: RingBuf::new(Instant::now()),
            last_onset: Instant::now() - std::time::Duration::from_secs(5),
            frame_counter: 0,
            key_sample_buffer: Vec::with_capacity(48000 * 5),
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn audio_callback(
    data: &mut [f32],
    ctrl_output: &mut triple_buffer::Output<ControlSnapshot>,
    cmd_consumer: &mut Consumer<AudioCommand>,
    meters: &[Arc<AtomicMeter>],
    master_meter: &AtomicMeter,
    bufs: &[Arc<SharedBuf>; 3],
    dsp_state: &mut [DspFilters; 3],
    lfo_debug: &Mutex<String>,
    deck_bufs: &mut [Vec<f32>; 3],
    cap_consumers: &mut [Option<rtrb::Consumer<f32>>; 3],
    capture_lookback: &mut [CaptureLookback; 3],
    capture_reverse: &[Arc<CaptureReverseState>; 3],
    sample_rate: f32,
    time_pos: &[Arc<AtomicF64>; 3],
    duration: &[Arc<AtomicF64>; 3],
    seek_requests: &[Arc<AtomicF64>; 3],
    hp_producer: &Mutex<Option<Producer<f32>>>,
    pad_voices: &mut PadVoiceState,
    pad_sample_cache: &std::sync::RwLock<Vec<Option<Arc<(Vec<f32>, u32)>>>>,
    sequence_steps: &[AtomicUsize],
    pad_triggers: &[AtomicBool],
    onset_state: &mut [OnsetState; 3],
    key_sample_tx: &std::sync::mpsc::Sender<(usize, Vec<f32>, u32)>,
) {
    // Drain pending commands (lockfree SPSC pop).
    while let Ok(cmd) = cmd_consumer.pop() {
        match cmd {
            AudioCommand::Stop(ch) => {
                if ch < MAX_DECKS { bufs[ch].clear(); }
            }
            AudioCommand::Quit => return,
        }
    }

    // Lockfree snapshot read — triple-buffered, always latest.
    let ctrl: &ControlSnapshot = ctrl_output.read();

    let frames = data.len() / 2;

    let cf = ctrl.master.crossfader.clamp(0.0, 1.0);
    let gain_a = (1.0 - cf).sqrt();
    let gain_b = cf.sqrt();

    let mut dm: [crate::audio::dsp::LevelMeter; 3] =
        [(); 3].map(|_| crate::audio::dsp::LevelMeter::new());
    let mut ml = crate::audio::dsp::LevelMeter::new();

    // Update DSP parameters (owned locally — no lock).
    for (d, deck_ctrl) in dsp_state.iter_mut().zip(ctrl.decks.iter()) {
        d.update_params(deck_ctrl, frames, &ctrl.master.master_eq);
    }

    let solo_active = ctrl.master.solo_active;

    for d in 0..3 {
        let seek_to = seek_requests[d].load();
        if seek_to >= 0.0 {
            time_pos[d].store(seek_to);
            capture_reverse[d].reset_cursor();
            if let Some(ref mut consumer) = cap_consumers[d] {
                let queued = consumer.slots();
                if queued > 0
                    && let Ok(chunk) = consumer.read_chunk(queued) {
                        chunk.commit_all();
                    }
                dsp_state[d].prev_l = 0.0;
                dsp_state[d].prev_r = 0.0;
                dsp_state[d].underrun_gain = 0.0;
            }
            seek_requests[d].store(SEEK_SENTINEL);
        }
    }

    // Deck buffers pre-allocated to 8192 samples at build. resize is a no-op
    // in the steady state; only grows if cpal requests a bigger block.
    for buf in deck_bufs.iter_mut() {
        if buf.len() < frames * 2 {
            buf.resize(frames * 2, 0.0f32);
        }
    }
    // Read samples for each deck. Prefer rtrb consumer (FIFO capture
    // path) over SharedBuf (symphonia decoder path).
    let need = frames * 2;
    let mut deck_reads: [usize; 3] = [0; 3];
    let mut deck_has_any_data: [bool; 3] = [false; 3];
    for d in 0..3 {
        if capture_reverse[d].is_active() {
            let out = &mut deck_bufs[d][..need];
            out.fill(0.0);
            let produced = if let Ok(mut cursor) = capture_reverse[d].cursor_frames.try_lock() {
                capture_lookback[d].fill_reverse_from_cursor(out, &mut cursor)
            } else {
                0
            };
            if produced > 0 {
                deck_reads[d] = produced;
                deck_has_any_data[d] = true;
            }
            continue;
        }

        // Try rtrb capture consumer first.
        if let Some(ref mut consumer) = cap_consumers[d] {
            let avail = consumer.slots();
            if avail > 0 {
                deck_has_any_data[d] = true;
            }
            if avail > 0 {
                let to_read = need.min(avail);
                if let Ok(chunk) = consumer.read_chunk(to_read) {
                    let (first, second) = chunk.as_slices();
                    deck_bufs[d][..first.len()].copy_from_slice(first);
                    if !second.is_empty() {
                        deck_bufs[d][first.len()..first.len() + second.len()]
                            .copy_from_slice(second);
                    }
                    let total = first.len() + second.len();
                    chunk.commit_all();
                    capture_lookback[d].push_interleaved(&deck_bufs[d][..total]);
                    deck_reads[d] = total;
                    continue;
                }
            }
        }
        // Fallback: SharedBuf (symphonia decoder)
        if bufs[d].has_data() {
            deck_has_any_data[d] = true;
        }
        deck_reads[d] = bufs[d].read(&mut deck_bufs[d][..need]);
    }

    // Underrun fade: multiplicative decay per sample. ~5ms to -60dB at 48k.
    const UNDERRUN_DECAY: f32 = 0.9986;

    // --- Direct pad triggers: consume one-shot flags from UI thread ---
    // The audio callback owns pad_triggers; UI sets pad_triggers[i] = true.
    // Always activate a voice on trigger — the voice mixing section handles
    // cache lookup and won't produce audio if sample data isn't loaded.
    for (i, trigger) in pad_triggers.iter().enumerate().take(pad_voices.voice_active.len()) {
        if trigger.swap(false, Ordering::Relaxed) {
            // Find a free voice slot
            for v in 0..pad_voices.voice_active.len() {
                if !pad_voices.voice_active[v] {
                    pad_voices.voice_active[v] = true;
                    pad_voices.voice_position[v] = 0;
                    pad_voices.voice_pad_idx[v] = i;
                    pad_voices.voice_gain[v] = 0.0;
                    // usize::MAX signals a direct trigger — the mixing
                    // section skips the mute check and uses full volume.
                    pad_voices.voice_seq_idx[v] = usize::MAX;
                    break;
                }
            }
        }
    }

    // --- Sequencer timer: advance steps and trigger pad voices ---
    // Single global step counter. Each sequence derives its current step
    // from this counter, ensuring all sequences are perfectly aligned.
    static GLOBAL_SAMPLE_POS: AtomicU64 = AtomicU64::new(0);
    let global_pos = GLOBAL_SAMPLE_POS.load(Ordering::Relaxed);

    // Master clock: global_bpm from the first sequence
    let global_bpm = ctrl.sequences.first().map(|s| s.global_bpm).unwrap_or(120.0);
    let global_step_interval = (60.0 / global_bpm.clamp(20.0, 400.0) / 4.0) * sample_rate;

    // Current global step position (which 16th note are we on?)
    let global_step = if global_step_interval > 0.0 {
        (global_pos as f64 / global_step_interval as f64) as usize % crate::state::SEQUENCE_STEPS
    } else {
        0
    };

    // Detect when we cross a step boundary (for triggering)
    static LAST_GLOBAL_STEP: AtomicU64 = AtomicU64::new(0);
    let prev_step = LAST_GLOBAL_STEP.load(Ordering::Relaxed) as usize;
    let step_crossed = global_step != prev_step;
    if step_crossed {
        LAST_GLOBAL_STEP.store(global_step as u64, Ordering::Relaxed);
    }

    // Per-sequence previous step tracking (for per-sequence tempo)
    static LAST_SEQ_STEPS: [AtomicU64; 32] = [
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
        AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0),
    ];

    // Try to lock pad sample cache (non-blocking — skip if contended)
    let cache_guard = pad_sample_cache.try_read().ok();
    let cache_ref = cache_guard.as_deref();

    for (seq_idx, seq) in ctrl.sequences.iter().enumerate() {
        if !seq.playing || seq_idx >= pad_voices.voice_active.len() {
            continue;
        }

        // If muted, skip
        if seq.mute {
            continue;
        }

        // Per-sequence step: apply tempo_multiplier to the global clock
        let seq_bpm = (seq.global_bpm * seq.tempo_multiplier).clamp(20.0, 400.0);
        let seq_step_interval = (60.0 / seq_bpm / 4.0) * sample_rate;
        let step = if seq_step_interval > 0.0 {
            (global_pos as f64 / seq_step_interval as f64) as usize % crate::state::SEQUENCE_STEPS
        } else {
            0
        };

        // Detect per-sequence step boundary crossing
        let seq_step_crossed = if seq_idx < LAST_SEQ_STEPS.len() {
            let prev = LAST_SEQ_STEPS[seq_idx].load(Ordering::Relaxed) as usize;
            let crossed = step != prev;
            if crossed {
                LAST_SEQ_STEPS[seq_idx].store(step as u64, Ordering::Relaxed);
            }
            crossed
        } else {
            step_crossed // fallback to global for sequences beyond array
        };

        // Only trigger once per step boundary crossing
        if !seq_step_crossed {
            continue;
        }

        // If the triggered step is active in the pattern, start playback
        if seq.pattern[step]
            && let Some(cache) = cache_ref {
                let pad_idx = seq.pad_idx;
                if pad_idx < cache.len() && cache[pad_idx].is_some() {
                    // Find a free voice slot
                    for v in 0..pad_voices.voice_active.len() {
                        if !pad_voices.voice_active[v] {
                            pad_voices.voice_active[v] = true;
                            pad_voices.voice_position[v] = 0;
                            pad_voices.voice_pad_idx[v] = pad_idx;
                            pad_voices.voice_gain[v] = 0.0;
                            pad_voices.voice_seq_idx[v] = seq_idx;
                            break;
                        }
                    }
                }
            }

        // Publish step to UI via atomic
        if let Some(atomic) = sequence_steps.get(seq_idx) {
            atomic.store(step, Ordering::Relaxed);
        }
    }

    let global_pos_end = global_pos + frames as u64;
    GLOBAL_SAMPLE_POS.store(global_pos_end, Ordering::Relaxed);

    // Pad voice mixing is done per-frame inside the mixing loop

    // --- Per-frame mixing loop ---
    for f in 0..frames {
        let mut mix_l = 0.0;
        let mut mix_r = 0.0;

    #[allow(clippy::needless_range_loop)]
    for d in 0..3 {
            let cd = &ctrl.decks[d];
            let n = deck_reads[d];

            let (mut l, mut r) = if f * 2 + 1 < n {
                let l = deck_bufs[d][f * 2];
                let r = deck_bufs[d][f * 2 + 1];
                dsp_state[d].prev_l = l;
                dsp_state[d].prev_r = r;
                dsp_state[d].underrun_gain = 1.0;
                (l, r)
            } else {
                dsp_state[d].underrun_gain *= UNDERRUN_DECAY;
                let g = dsp_state[d].underrun_gain;
                (dsp_state[d].prev_l * g, dsp_state[d].prev_r * g)
            };

            let rate = cd.playback_rate.clamp(0.1, 4.0);
            l *= rate;
            r *= rate;

            // DSP: filters, EQ, LFO, DC blocker
            let processed = dsp_state[d].process(l, r);
            l = processed.0;
            r = processed.1;

            if !l.is_finite() || !r.is_finite() {
                l = 0.0;
                r = 0.0;
                dsp_state[d].svf1 = Svf::new(sample_rate);
                dsp_state[d].svf2 = Svf::new(sample_rate);
            }

            let (lg, rg) = pan_gains(cd.pan);

            let ch_active = if solo_active { cd.solo } else { !cd.muted };
            let cf_gain = if d == 1 { gain_b } else if d == 2 { 1.0 } else { gain_a };
            let vol = if ch_active && cd.playing && !ctrl.master.muted {
                // 4× makeup gain compensates for the multiplicative chain:
                // fader(0.5) × crossfader(0.707) × master(0.5) × pan(0.707) = 0.125
                // With 4×: effective = 0.5 at center settings — reasonable listening level.
                cd.volume * cf_gain * ctrl.master.fader * 4.0
            } else {
                0.0
            };

            // Apply volume and pan. No per-deck soft_limit — the double
            // tanh (per-deck + master) was creating harmonic distortion
            // that sounded like high-pitched bit-crushing during filter sweeps.
            // Single master limiter at the sum is sufficient.
            l = l * vol * lg;
            r = r * vol * rg;

            dm[d].push_stereo(l, r);

            // Deck C (d==2) routes to headphone output instead of main mix
            if d == 2 {
                if let Ok(mut guard) = hp_producer.try_lock()
                    && let Some(ref mut prod) = *guard {
                        let _ = prod.push(l);
                        let _ = prod.push(r);
                    }
            } else {
                mix_l += l;
                mix_r += r;
            }
        }

        // Pad voice mixing: read one sample from each active voice
        let mut pad_mix_l = 0.0f32;
        let mut pad_mix_r = 0.0f32;
        const ATTACK_SAMPLES: f32 = 128.0; // ~2.7ms at 48kHz

        if let Some(cache) = cache_ref {
            for v in 0..pad_voices.voice_active.len() {
                if !pad_voices.voice_active[v] { continue; }
                let pos = pad_voices.voice_position[v];
                let pad_idx = pad_voices.voice_pad_idx[v];
                if pad_idx < cache.len() {
                    if let Some(cached) = cache[pad_idx].as_ref() {
                        let samples = &cached.0;
                        let cached_sr = cached.1;
                        let sample_rate_ratio = cached_sr as f32 / pad_voices.sample_rate;
                        let adjusted_pos = (pos as f32 * sample_rate_ratio) as usize;
                        let stereo_pos = adjusted_pos * 2;
                        if stereo_pos + 1 < samples.len() {
                            let (seq_vol, pad_vol) =
                                if pad_voices.voice_seq_idx[v] == usize::MAX {
                                    // Direct trigger: skip mute check, use full volume
                                    (1.0, 1.0)
                                } else {
                                    let seq = ctrl.sequences.get(pad_voices.voice_seq_idx[v]);
                                    // Check if sequence is muted — kill voice if so
                                    if seq.map(|s| s.mute).unwrap_or(false) {
                                        pad_voices.voice_active[v] = false;
                                        continue;
                                    }
                                    (seq.map(|s| s.volume).unwrap_or(1.0),
                                     seq.map(|s| s.pad_volume).unwrap_or(1.0))
                                };
                            // Envelope: linear attack over ATTACK_SAMPLES
                            let gain = if pad_voices.voice_gain[v] < 1.0 {
                                pad_voices.voice_gain[v] = (pad_voices.voice_gain[v] + 1.0 / ATTACK_SAMPLES).min(1.0);
                                pad_voices.voice_gain[v]
                            } else {
                                1.0
                            };
                            // Apply sequence volume and pad config volume
                            let vol = gain * seq_vol * pad_vol;
                            let l = samples[stereo_pos] * vol;
                            let r = samples[stereo_pos + 1] * vol;
                            pad_mix_l += l;
                            pad_mix_r += r;
                        } else {
                            pad_voices.voice_active[v] = false;
                        }
                    } else {
                        pad_voices.voice_active[v] = false;
                    }
                } else {
                    pad_voices.voice_active[v] = false;
                }
                pad_voices.voice_position[v] += 1;
            }
        }

        // Soft-limit pad submix before adding to main mix
        let pad_mix_l = soft_limit(pad_mix_l);
        let pad_mix_r = soft_limit(pad_mix_r);
        mix_l += pad_mix_l;
        mix_r += pad_mix_r;

        ml.push_stereo(mix_l, mix_r);
        data[f * 2] = soft_limit(mix_l);
        data[f * 2 + 1] = soft_limit(mix_r);
    }

    for d in 0..3 {
        let (pl, pr, rl, rr) = dm[d].read();
        meters[d].store(pl, pr, rl, rr);
        if deck_has_any_data[d] && ctrl.decks[d].playing {
            let delta = (frames as f64) / (sample_rate as f64);
            let rate = ctrl.decks[d].playback_rate.clamp(0.1, 4.0) as f64;
            let dur = duration[d].load();
            let stepped = time_pos[d].load() + delta * rate;
            let bounded = if dur > 0.0 { stepped.min(dur) } else { stepped.max(0.0) };
            time_pos[d].store(bounded);

            // Onset detection for capture channels (~10Hz, every ~10 buffers at 48kHz/256)
            onset_state[d].frame_counter += 1;
            if onset_state[d].frame_counter % 10 == 0 && deck_reads[d] > 0 {
                // Compute RMS from the raw deck buffer
                let n = deck_reads[d].min(frames * 2);
                let mut sum_sq = 0.0f32;
                let mut count = 0u32;
                for s in 0..n {
                    let v = deck_bufs[d][s];
                    sum_sq += v * v;
                    count += 1;
                }
                let rms = if count > 0 { (sum_sq / count as f32).sqrt() } else { 0.0 };

                let energy = rms;
                onset_state[d].energy_ring.push(energy);

                if onset_state[d].energy_ring.len() >= 20 {
                    let ring = &onset_state[d].energy_ring;
                    let mean: f32 = ring.iter().sum::<f32>() / ring.len() as f32;
                    let var: f32 = ring.iter().map(|e| (e - mean).powi(2)).sum::<f32>() / ring.len() as f32;
                    let threshold = mean + 1.2 * var.sqrt();

                    let now = Instant::now();
                    let ms_since = now.duration_since(onset_state[d].last_onset).as_millis() as f32;

                    if energy > threshold && energy > mean * 1.05 && ms_since >= 250.0 {
                        onset_state[d].onset_times.push(now);
                        onset_state[d].last_onset = now;

                        if onset_state[d].onset_times.len() >= 3 {
                            // Stack-allocated IOI buffer (max 31 intervals from 32 onsets)
                            let mut iois = [0.0f32; 31];
                            let mut ioi_count = 0;
                            let times: Vec<Instant> = onset_state[d].onset_times.iter().copied().collect();
                            for pair in times.windows(2) {
                                let ioi = pair[1].duration_since(pair[0]).as_millis() as f32;
                                if (250.0..=1000.0).contains(&ioi) && ioi_count < iois.len() {
                                    iois[ioi_count] = ioi;
                                    ioi_count += 1;
                                }
                            }
                            if ioi_count >= 2 {
                                iois[..ioi_count].sort_by(|a, b| a.partial_cmp(b).unwrap());
                                let median = iois[ioi_count / 2];
                                let bpm = (60000.0 / median * 100.0) as u32;
                                if (6000..=20000).contains(&bpm) {
                                    meters[d].detected_bpm.store(bpm, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                }

                // Key detection: accumulate downmixed mono samples (~5s worth)
                if deck_reads[d] > 0 {
                    let n = deck_reads[d].min(frames * 2);
                    let mut i = 0;
                    while i + 1 < n {
                        // Downmix stereo to mono
                        let mono = (deck_bufs[d][i] + deck_bufs[d][i + 1]) * 0.5;
                        onset_state[d].key_sample_buffer.push(mono);
                        i += 2;
                    }
                    // When we have ~5 seconds, send to key detection thread
                    let sr = sample_rate as usize;
                    if onset_state[d].key_sample_buffer.len() >= sr * 5 {
                        let buf = std::mem::take(&mut onset_state[d].key_sample_buffer);
                        // Re-claim capacity for next cycle
                        onset_state[d].key_sample_buffer = Vec::with_capacity(sr * 5);
                        let _ = key_sample_tx.send((d, buf, sr as u32));
                    }
                }
            }
        }
    }
    let (mpl, mpr, mrl, mrr) = ml.read();
    master_meter.store(mpl, mpr, mrl, mrr);

    // Once per ~1s, push LFO debug string. String ops allocate but happen
    // ~once/sec; try_lock never blocks the audio thread.
    static DBG_FRAME: AtomicU64 = AtomicU64::new(0);
    let frame_count = DBG_FRAME.fetch_add(1, Ordering::Relaxed);
    if frame_count % 50 == 0
        && let Ok(mut s) = lfo_debug.try_lock() {
            s.clear();
            for (di, deck_dsp) in dsp_state.iter().enumerate() {
                if di > 0 { s.push(' '); }
                let dl = deck_dsp.lfo_debug_line();
                let ss = ctrl.decks[di].lfo_speed;
                s.push_str(&format!("[{}]{} ss={:.3}", di, dl, ss));
            }
        }
}
