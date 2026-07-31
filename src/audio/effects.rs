//! Custom DSP effects for sample playback
//! 
//! Professional-quality implementations of Chorus, Distortion, and Reverb.

use rodio::Source;
use std::time::Duration;

/// Multi-voice stereo chorus with triangular LFO
pub struct Chorus<I> {
    input: I,
    sample_rate: u32,
    channels: u16,
    /// Three delay lines for stereo chorus
    buffers: [Vec<f32>; 3],
    write_pos: usize,
    /// Three LFO phases (offset for stereo width)
    lfo_phases: [f32; 3],
    /// LFO rate in Hz
    lfo_rate: f32,
    /// Depth in samples
    depth: f32,
    /// Base delay in samples
    base_delay: f32,
    /// Wet/dry mix
    mix: f32,
    /// Channel counter for stereo processing
    channel_count: usize,
}

impl<I> Chorus<I>
where
    I: Source<Item = f32>,
{
    pub fn new(input: I, rate: f32, depth: f32, mix: f32) -> Self {
        let sample_rate = input.sample_rate().get();
        let channels = input.channels().get();
        
        // Max delay: 30ms for chorus
        let max_delay_samples = (sample_rate as f32 * 0.03) as usize;
        let buffer_size = max_delay_samples * 2;
        
        Self {
            input,
            sample_rate,
            channels,
            buffers: [
                vec![0.0; buffer_size],
                vec![0.0; buffer_size],
                vec![0.0; buffer_size],
            ],
            write_pos: 0,
            lfo_phases: [0.0, 0.33, 0.67], // 120° phase offset for width
            lfo_rate: rate,
            depth: depth * max_delay_samples as f32 * 0.8,
            base_delay: max_delay_samples as f32 * 0.4,
            mix,
            channel_count: 0,
        }
    }
    
    /// Triangular LFO (more natural than sine for chorus)
    #[inline]
    fn triangle_lfo(phase: f32) -> f32 {
        let p = phase % 1.0;
        if p < 0.5 {
            4.0 * p - 1.0
        } else {
            3.0 - 4.0 * p
        }
    }
    
    #[inline]
    fn read_delayed(&self, buffer_idx: usize, delay_samples: f32) -> f32 {
        let buffer = &self.buffers[buffer_idx];
        let read_pos = (self.write_pos as f32 - delay_samples + buffer.len() as f32) 
            % buffer.len() as f32;
        
        // Linear interpolation (simpler and stable)
        let idx = read_pos.floor() as usize;
        let frac = read_pos - idx as f32;
        
        let y1 = buffer[idx % buffer.len()];
        let y2 = buffer[(idx + 1) % buffer.len()];
        
        y1 + frac * (y2 - y1)
    }
}

impl<I> Iterator for Chorus<I>
where
    I: Source<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let dry = self.input.next()?;
        
        // Write to all delay lines
        for buffer in &mut self.buffers {
            buffer[self.write_pos] = dry;
        }
        
        // Read from 3 voices with phase-offset LFOs
        let mut wet_sum = 0.0;
        for i in 0..3 {
            let lfo = Self::triangle_lfo(self.lfo_phases[i]);
            let delay = self.base_delay + lfo * self.depth;
            wet_sum += self.read_delayed(i, delay);
        }
        wet_sum /= 3.0;
        
        // Mix dry and wet
        let output = dry * (1.0 - self.mix) + wet_sum * self.mix;
        
        // Advance write position and LFO phases (per sample, not per channel)
        self.channel_count += 1;
        if self.channel_count >= self.channels as usize {
            self.channel_count = 0;
            self.write_pos = (self.write_pos + 1) % self.buffers[0].len();
            
            for phase in &mut self.lfo_phases {
                *phase += self.lfo_rate / self.sample_rate as f32;
                if *phase >= 1.0 {
                    *phase -= 1.0;
                }
            }
        }
        
        Some(output)
    }
}

impl<I> Source for Chorus<I>
where
    I: Source<Item = f32>,
{
    fn current_span_len(&self) -> Option<usize> {
        self.input.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        std::num::NonZeroU16::new(self.channels).unwrap()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        std::num::NonZeroU32::new(self.sample_rate).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }
}

/// Hard/soft clipping distortion with tone shaping
pub struct Distortion<I> {
    input: I,
    sample_rate: u32,
    channels: u16,
    /// Distortion amount (0-1)
    amount: f32,
}

impl<I> Distortion<I>
where
    I: Source<Item = f32>,
{
    pub fn new(input: I, amount: f32) -> Self {
        let sample_rate = input.sample_rate().get();
        let channels = input.channels().get();
        
        Self {
            input,
            sample_rate,
            channels,
            amount: amount.clamp(0.0, 1.0),
        }
    }
    
    /// Waveshaper: smooth at low amounts, hard-clips at high amounts
    #[inline]
    fn waveshape(x: f32, amount: f32) -> f32 {
        if amount < 0.5 {
            // Soft clipping (0.0 - 0.5)
            let drive = 1.0 + amount * 10.0;
            let driven = x * drive;
            driven / (1.0 + driven.abs())
        } else {
            // Hard clipping (0.5 - 1.0)
            let hardness = (amount - 0.5) * 2.0; // 0 to 1
            let drive = 1.0 + amount * 20.0;
            let driven = x * drive;
            
            // Blend between soft and hard clip
            let soft = driven / (1.0 + driven.abs());
            let hard = driven.clamp(-1.0, 1.0);
            soft * (1.0 - hardness) + hard * hardness
        }
    }
}

impl<I> Iterator for Distortion<I>
where
    I: Source<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.input.next()?;
        
        if self.amount < 0.01 {
            return Some(sample);
        }
        
        // Apply waveshaping
        let distorted = Self::waveshape(sample, self.amount);
        
        // Compensate gain (more distortion = more gain reduction)
        let gain = 1.0 / (1.0 + self.amount * 2.0);
        
        Some(distorted * gain)
    }
}

impl<I> Source for Distortion<I>
where
    I: Source<Item = f32>,
{
    fn current_span_len(&self) -> Option<usize> {
        self.input.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        std::num::NonZeroU16::new(self.channels).unwrap()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        std::num::NonZeroU32::new(self.sample_rate).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }
}

/// Simplified reverb with stable feedback
pub struct Reverb<I> {
    input: I,
    sample_rate: u32,
    channels: u16,
    /// 4 parallel comb filters (reduced from 8 for stability)
    comb_buffers: [Vec<f32>; 4],
    comb_positions: [usize; 4],
    comb_filter_states: [f32; 4],
    /// 2 series all-pass filters (reduced from 4)
    allpass_buffers: [Vec<f32>; 2],
    allpass_positions: [usize; 2],
    /// Reverb parameters
    feedback: f32,
    damping: f32,
    wet: f32,
    dry: f32,
    /// Channel counter
    channel_count: usize,
}

impl<I> Reverb<I>
where
    I: Source<Item = f32>,
{
    pub fn new(input: I, room_size: f32, decay: f32, mix: f32) -> Self {
        let sample_rate = input.sample_rate().get();
        let channels = input.channels().get();
        
        // Conservative delay times for stability
        let base_delay = (0.02 + room_size * 0.04) * sample_rate as f32; // 20-60ms
        
        let comb_delays = [
            (base_delay * 1.0) as usize,
            (base_delay * 1.19) as usize,
            (base_delay * 1.41) as usize,
            (base_delay * 1.68) as usize,
        ];
        
        let allpass_delays = [
            (sample_rate as f32 * 0.005) as usize,
            (sample_rate as f32 * 0.011) as usize,
        ];
        
        // Conservative feedback - never exceed 0.85 for stability
        let feedback = 0.5 + decay * 0.35; // 0.5 to 0.85
        
        Self {
            input,
            sample_rate,
            channels,
            comb_buffers: [
                vec![0.0; comb_delays[0]],
                vec![0.0; comb_delays[1]],
                vec![0.0; comb_delays[2]],
                vec![0.0; comb_delays[3]],
            ],
            comb_positions: [0; 4],
            comb_filter_states: [0.0; 4],
            allpass_buffers: [
                vec![0.0; allpass_delays[0]],
                vec![0.0; allpass_delays[1]],
            ],
            allpass_positions: [0; 2],
            feedback,
            damping: 0.2 + decay * 0.4, // 0.2 to 0.6 damping
            wet: mix * 0.5, // Conservative wet gain (max 0.5)
            dry: 1.0,
            channel_count: 0,
        }
    }
}

impl<I> Iterator for Reverb<I>
where
    I: Source<Item = f32>,
{
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let input_sample = self.input.next()?;
        
        // Scale input to prevent overload
        let input_scaled = input_sample * 0.5;
        
        // Comb filters with damping (parallel)
        let mut comb_out = 0.0;
        for i in 0..4 {
            let delayed = self.comb_buffers[i][self.comb_positions[i]];
            
            // One-pole low-pass filter for damping
            self.comb_filter_states[i] = delayed * (1.0 - self.damping) 
                + self.comb_filter_states[i] * self.damping;
            
            // Add to output
            comb_out += self.comb_filter_states[i];
            
            // Write input + limited feedback
            let feedback_signal = self.comb_filter_states[i] * self.feedback;
            self.comb_buffers[i][self.comb_positions[i]] = input_scaled + feedback_signal;
            
            self.comb_positions[i] = (self.comb_positions[i] + 1) % self.comb_buffers[i].len();
        }
        
        // Average the comb outputs
        comb_out *= 0.25;
        
        // All-pass filters (series) with conservative gain
        let mut signal = comb_out;
        for i in 0..2 {
            let delayed = self.allpass_buffers[i][self.allpass_positions[i]];
            let output = -signal + delayed;
            
            self.allpass_buffers[i][self.allpass_positions[i]] = signal + delayed * 0.5;
            self.allpass_positions[i] = (self.allpass_positions[i] + 1) % self.allpass_buffers[i].len();
            
            signal = output;
        }
        
        // Mix wet and dry with safety clipping
        let wet_signal = signal * self.wet;
        let dry_signal = input_sample * self.dry;
        let output = (wet_signal + dry_signal).clamp(-1.0, 1.0);
        
        // Advance channel counter
        self.channel_count += 1;
        if self.channel_count >= self.channels as usize {
            self.channel_count = 0;
        }
        
        Some(output)
    }
}

impl<I> Source for Reverb<I>
where
    I: Source<Item = f32>,
{
    fn current_span_len(&self) -> Option<usize> {
        self.input.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        std::num::NonZeroU16::new(self.channels).unwrap()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        std::num::NonZeroU32::new(self.sample_rate).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }
}

/// Extension trait for applying custom effects to any Source
pub trait CustomEffects: Source<Item = f32> + Sized {
    /// Apply multi-voice stereo chorus
    /// 
    /// - `rate`: LFO frequency in Hz (0.3 - 3.0 typical)
    /// - `depth`: Modulation depth 0.0 - 1.0
    /// - `mix`: Wet/dry mix 0.0 - 1.0
    fn custom_chorus(self, rate: f32, depth: f32, mix: f32) -> Chorus<Self> {
        Chorus::new(self, rate, depth, mix)
    }
    
    /// Apply hard/soft clipping distortion
    /// 
    /// - `amount`: Distortion amount 0.0 - 1.0
    ///   - 0.0 - 0.5: Soft clipping (warm)
    ///   - 0.5 - 1.0: Hard clipping (aggressive)
    fn custom_distortion(self, amount: f32) -> Distortion<Self> {
        Distortion::new(self, amount)
    }
    
    /// Apply simple but stable reverb
    /// 
    /// - `room_size`: Room size 0.0 - 1.0
    /// - `decay`: Decay time 0.0 - 1.0
    /// - `mix`: Wet/dry mix 0.0 - 1.0
    fn custom_reverb(self, room_size: f32, decay: f32, mix: f32) -> Reverb<Self> {
        Reverb::new(self, room_size, decay, mix)
    }
}

// Blanket implementation for all Sources
impl<S: Source<Item = f32> + Sized> CustomEffects for S {}
