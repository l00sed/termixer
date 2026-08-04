use std::f32::consts::TAU;

/// Biquad filter (Direct Form 1) with RBJ cookbook coefficient calculation.
/// Coefficients are interpolated per-sample to prevent zipper noise when
/// parameters change (e.g. filter cutoff sweep).
#[derive(Clone)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    sample_rate: f32,
}

#[derive(Clone, Copy)]
pub enum FilterType {
    Peaking,
}

impl Biquad {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
            sample_rate,
        }
    }

    /// Set coefficients using RBJ cookbook formulas (pre-warped bilinear transform).
    pub fn set_params(&mut self, filter_type: FilterType, freq: f32, q: f32, gain_db: f32) {
        let fs = self.sample_rate;
        let freq = freq.clamp(1.0, fs * 0.49);
        let w0 = TAU * (freq / fs);
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q.max(0.001));
        let a = 10.0_f32.powf(gain_db.clamp(-48.0, 48.0) / 40.0);

        let (b0, b1, b2, a0, a1, a2) = match filter_type {
            FilterType::Peaking => {
                let b0 = 1.0 + alpha * a;
                let b1 = -2.0 * cos_w0;
                let b2 = 1.0 - alpha * a;
                (b0, b1, b2, 1.0 + alpha / a, -2.0 * cos_w0, 1.0 - alpha / a)
            }
        };

        let inv_a0 = 1.0 / a0;
        self.b0 = b0 * inv_a0;
        self.b1 = b1 * inv_a0;
        self.b2 = b2 * inv_a0;
        self.a1 = a1 * inv_a0;
        self.a2 = a2 * inv_a0;
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;

        // Flush denormals to zero. When filters heavily attenuate, state
        // variables go subnormal — on x86 that's ~100x slower, causing the
        // audio callback to overrun and cpal to drop buffers (pausing).
        const DENORMAL: f32 = 1e-18;
        self.x2 = if self.x1.abs() < DENORMAL {
            0.0
        } else {
            self.x1
        };
        self.x1 = x;
        self.y2 = if self.y1.abs() < DENORMAL {
            0.0
        } else {
            self.y1
        };
        self.y1 = if y.abs() < DENORMAL { 0.0 } else { y };
        self.y1
    }
}

/// Per-sample LFO oscillator with cubic speed curve and shape morphing.
pub struct LfoOsc {
    pub phase: f32,
    sample_rate: f32,
    pub speed: f32,
    pub shape: f32,
    freq_hz: f32,
    prev_speed: f32,
}

impl LfoOsc {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            phase: 0.0,
            sample_rate,
            speed: 0.0,
            shape: 0.0,
            freq_hz: 0.0,
            prev_speed: 0.0,
        }
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
        if speed <= 0.001 {
            self.freq_hz = 0.0;
        } else {
            self.freq_hz = 0.05 + speed.powf(3.0) * 29.95;
            // Start at peak (phase 0.25) when LFO activates from idle
            if self.prev_speed <= 0.001 {
                self.phase = 0.25;
            }
        }
        self.prev_speed = speed;
    }

    pub fn set_shape(&mut self, shape: f32) {
        self.shape = shape.clamp(0.0, 1.0);
    }

    /// Advance phase by a full buffer's worth of samples.
    /// Call once per buffer (in update_params) instead of per-sample.
    #[inline]
    pub fn tick_buffer(&mut self, frames: usize) -> Option<f32> {
        if self.freq_hz <= 0.001 {
            self.phase = 0.0;
            return None;
        }
        self.phase = (self.phase + self.freq_hz * frames as f32 / self.sample_rate) % 1.0;
        let raw = (self.phase * TAU).sin();
        let sq = if raw > 0.0 { 1.0 } else { 0.0 };
        let sine = raw * 0.5 + 0.5;
        let blend = sq * (1.0 - self.shape) + sine * self.shape;
        Some(blend)
    }
}

/// Stereo panning using sine/cosine equal-power law.
/// pan: -1.0 = full left, 0.0 = center (equal), +1.0 = full right.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    // Map pan [-1, +1] to angle [0, π/4..π/2]:
    //   pan=-1 → angle=0    → (1.0, 0.0)  full left
    //   pan= 0 → angle=π/4  → (0.707, 0.707)  center
    //   pan=+1 → angle=π/2  → (0.0, 1.0)  full right
    let angle = ((pan + 1.0) * 0.25 * std::f32::consts::FRAC_PI_2 * 2.0)
        .clamp(0.0, std::f32::consts::FRAC_PI_2);
    (angle.cos(), angle.sin())
}

/// State-Variable Filter (Cytomic/Andrew Simper canonical form).
/// Inherently stable under per-sample parameter changes — ideal for
/// real-time cutoff modulation (LFO, manual sweeps) without crossfade overhead.
///
/// Parameter smoothing runs on **log-frequency**, not on `g = tan(πf/fs)` — a
/// log sweep sounds linear to the ear and prevents zipper artifacts at the
/// nonlinear extremes of `tan()`. `k` smooths linearly.
pub struct Svf {
    // Per-channel state
    ic1eq_l: f32,
    ic2eq_l: f32,
    ic1eq_r: f32,
    ic2eq_r: f32,
    // Current smoothed values
    log_freq: f32,
    k: f32,
    // Targets
    target_log_freq: f32,
    target_k: f32,
    // One-pole smoothing coefficient (per sample)
    smooth_coef: f32,
    sample_rate: f32,
}

impl Svf {
    pub fn new(sample_rate: f32) -> Self {
        // Start wide open (20kHz, Q=0.707 Butterworth)
        let log_freq = 20000.0f32.ln();
        let k = 1.0 / 0.707;
        // ~4ms time constant — smooth to the ear, fast enough for musical sweeps.
        // T60-ish per-sample coefficient: exp(-1 / (tau * sr))
        let tau_s = 0.004;
        let smooth_coef = 1.0 - (-1.0 / (tau_s * sample_rate)).exp();
        Self {
            ic1eq_l: 0.0,
            ic2eq_l: 0.0,
            ic1eq_r: 0.0,
            ic2eq_r: 0.0,
            log_freq,
            k,
            target_log_freq: log_freq,
            target_k: k,
            smooth_coef,
            sample_rate,
        }
    }

    /// Set target frequency and Q. Coefficients interpolate per-sample in tick().
    pub fn set_params(&mut self, freq: f32, q: f32) {
        let freq = freq.clamp(20.0, self.sample_rate * 0.49);
        self.target_log_freq = freq.ln();
        self.target_k = 1.0 / q.clamp(0.5, 20.0);
    }

    /// Current smoothed frequency (for gain compensation calculations).
    #[inline]
    pub fn current_freq(&self) -> f32 {
        self.log_freq.exp()
    }

    /// Tick one stereo sample pair. Returns (lowpass_l, lowpass_r, highpass_l, highpass_r).
    ///
    /// Uses the correct Cytomic/Zavalishin TPT (topology-preserving transform)
    /// SVF equations. Reference: Vadim Zavalishin, "The Art of VA Filter Design"
    /// and Andrew Simper's technical notes.
    #[inline]
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32, f32, f32) {
        // Smooth log-frequency + k with one-pole toward target.
        let a = self.smooth_coef;
        self.log_freq += (self.target_log_freq - self.log_freq) * a;
        self.k += (self.target_k - self.k) * a;

        let freq = self.log_freq.exp();
        let g = (std::f32::consts::PI * freq / self.sample_rate).tan();
        let k = self.k;

        // Denominator: 1 + k*g + g²
        let a1 = 1.0 / (1.0 + k * g + g * g);
        let a2 = g * a1;
        let a3 = g * a2;

        // Left channel — correct TPT SVF
        let v3_l = l - self.ic2eq_l;
        let v1_l = a1 * self.ic1eq_l + a2 * v3_l;
        let v2_l = self.ic2eq_l + a2 * self.ic1eq_l + a3 * v3_l;

        let lp_l = v2_l;
        let bp_l = v1_l;
        let hp_l = l - k * bp_l - lp_l;

        // State update: s = 2*output - s (trapezoidal integrator)
        self.ic1eq_l = 2.0 * bp_l - self.ic1eq_l;
        self.ic2eq_l = 2.0 * lp_l - self.ic2eq_l;

        // Right channel — same equations
        let v3_r = r - self.ic2eq_r;
        let v1_r = a1 * self.ic1eq_r + a2 * v3_r;
        let v2_r = self.ic2eq_r + a2 * self.ic1eq_r + a3 * v3_r;

        let lp_r = v2_r;
        let bp_r = v1_r;
        let hp_r = r - k * bp_r - lp_r;

        self.ic1eq_r = 2.0 * bp_r - self.ic1eq_r;
        self.ic2eq_r = 2.0 * lp_r - self.ic2eq_r;

        // Flush denormals and clamp state variables.
        const DENORMAL: f32 = 1e-18;
        const STATE_MAX: f32 = 1e6;
        self.ic1eq_l = Self::sanitize(self.ic1eq_l, DENORMAL, STATE_MAX);
        self.ic2eq_l = Self::sanitize(self.ic2eq_l, DENORMAL, STATE_MAX);
        self.ic1eq_r = Self::sanitize(self.ic1eq_r, DENORMAL, STATE_MAX);
        self.ic2eq_r = Self::sanitize(self.ic2eq_r, DENORMAL, STATE_MAX);

        (lp_l, lp_r, hp_l, hp_r)
    }

    /// Flush denormals to zero and clamp extreme values.
    #[inline]
    fn sanitize(v: f32, denormal: f32, max: f32) -> f32 {
        let v = if v.abs() < denormal { 0.0 } else { v };
        v.clamp(-max, max)
    }
}

/// Output limiter: linear passthrough at normal levels, hard clamp at ±1.0.
/// Unlike tanh(), this adds ZERO harmonic distortion below the threshold —
/// critical because nonlinear processing after the SVF filter re-introduces
/// frequencies above the cutoff (the "chirp" artifact).
#[inline]
pub fn soft_limit(x: f32) -> f32 {
    // Soft-knee limiter: gentle compression above 0.8, hard ceiling at 1.0
    const THRESHOLD: f32 = 0.8;
    const RATIO: f32 = 4.0;
    if x.abs() <= THRESHOLD {
        x
    } else {
        let sign = x.signum();
        let abs = x.abs();
        let over = abs - THRESHOLD;
        let compressed = THRESHOLD + over / RATIO;
        sign * compressed.min(1.0)
    }
}

/// One-pole DC blocker (high-pass at ~20 Hz). Removes DC that filter
/// transients / LFO amplitude modulation leave behind, which otherwise
/// eats headroom and causes speaker thump.
#[derive(Clone)]
pub struct DcBlocker {
    x1_l: f32,
    y1_l: f32,
    x1_r: f32,
    y1_r: f32,
    r: f32,
}

impl DcBlocker {
    pub fn new(sample_rate: f32) -> Self {
        // Cutoff ~20 Hz: R = 1 - 2*pi*fc/fs
        let r = 1.0 - (std::f32::consts::TAU * 20.0 / sample_rate);
        Self {
            x1_l: 0.0,
            y1_l: 0.0,
            x1_r: 0.0,
            y1_r: 0.0,
            r,
        }
    }

    #[inline]
    pub fn tick(&mut self, l: f32, r_in: f32) -> (f32, f32) {
        let y_l = l - self.x1_l + self.r * self.y1_l;
        self.x1_l = l;
        self.y1_l = y_l;
        let y_r = r_in - self.x1_r + self.r * self.y1_r;
        self.x1_r = r_in;
        self.y1_r = y_r;
        (y_l, y_r)
    }
}

/// Per-buffer level meter.
pub struct LevelMeter {
    peak_l: f32,
    peak_r: f32,
    rms_ll: f64,
    rms_rr: f64,
    count: usize,
}

impl LevelMeter {
    pub fn new() -> Self {
        Self {
            peak_l: 0.0,
            peak_r: 0.0,
            rms_ll: 0.0,
            rms_rr: 0.0,
            count: 0,
        }
    }

    pub fn push_stereo(&mut self, l: f32, r: f32) {
        self.peak_l = self.peak_l.max(l.abs());
        self.peak_r = self.peak_r.max(r.abs());
        self.rms_ll += (l as f64) * (l as f64);
        self.rms_rr += (r as f64) * (r as f64);
        self.count += 1;
    }

    pub fn read(&mut self) -> (f32, f32, f32, f32) {
        let n = self.count.max(1) as f64;
        let rms_l = (self.rms_ll / n).sqrt() as f32;
        let rms_r = (self.rms_rr / n).sqrt() as f32;
        let peak_l = self.peak_l;
        let peak_r = self.peak_r;
        self.peak_l = 0.0;
        self.peak_r = 0.0;
        self.rms_ll = 0.0;
        self.rms_rr = 0.0;
        self.count = 0;
        (peak_l, peak_r, rms_l, rms_r)
    }
}

/// Lock-free meter values for audio→UI thread handoff.
use std::sync::atomic::{AtomicU32, Ordering};

pub struct AtomicMeter {
    peak_l: AtomicU32,
    peak_r: AtomicU32,
    rms_l: AtomicU32,
    rms_r: AtomicU32,
    /// Detected BPM from onset detection (fixed-point ×100), 0 = not yet detected.
    pub detected_bpm: AtomicU32,
}

impl AtomicMeter {
    pub fn new() -> Self {
        Self {
            peak_l: AtomicU32::new(0),
            peak_r: AtomicU32::new(0),
            rms_l: AtomicU32::new(0),
            rms_r: AtomicU32::new(0),
            detected_bpm: AtomicU32::new(0),
        }
    }

    pub fn store(&self, peak_l: f32, peak_r: f32, rms_l: f32, rms_r: f32) {
        self.peak_l.store(peak_l.to_bits(), Ordering::Release);
        self.peak_r.store(peak_r.to_bits(), Ordering::Release);
        self.rms_l.store(rms_l.to_bits(), Ordering::Release);
        self.rms_r.store(rms_r.to_bits(), Ordering::Release);
    }

    pub fn load(&self) -> (f32, f32, f32, f32) {
        let peak_l = f32::from_bits(self.peak_l.load(Ordering::Acquire));
        let peak_r = f32::from_bits(self.peak_r.load(Ordering::Acquire));
        let rms_l = f32::from_bits(self.rms_l.load(Ordering::Acquire));
        let rms_r = f32::from_bits(self.rms_r.load(Ordering::Acquire));
        (peak_l, peak_r, rms_l, rms_r)
    }
}
