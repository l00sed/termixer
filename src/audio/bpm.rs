use std::f32::consts::TAU;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use stratum_dsp::{analyze_audio, AnalysisConfig};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

type BpmCallback = Arc<Mutex<dyn Fn(Result<BpmResult, String>) + Send>>;

/// Result of BPM + key detection for a single track
#[derive(Debug, Clone)]
pub struct BpmResult {
    pub bpm: f32,
    #[allow(dead_code)]
    pub confidence: f32,
    /// Detected key in Camelot Wheel notation (e.g., "8A", "12B"), or None
    pub key: Option<String>,
}

/// Camelot Wheel mapping: pitch_class + is_major -> Camelot code
/// Camelot: 1=C, 2=G, 3=D, 4=A, 5=E, 6=B, 7=F#, 8=C#, 9=Ab, 10=Eb, 11=Bb, 12=F
/// A=minor, B=major
pub fn pitch_class_to_camelot(pc: usize, is_major: bool) -> String {
    // Camelot number for each pitch class (1-indexed)
    let camelot_num = match pc {
        0 => 8,  // C  -> 8
        1 => 3,  // C# -> 3 (Db)
        2 => 10, // D  -> 10 (Eb)
        3 => 5,  // D# -> 5
        4 => 12, // E  -> 12 (F)
        5 => 7,  // F  -> 7
        6 => 2,  // F# -> 2 (Gb)
        7 => 9,  // G  -> 9 (Ab)
        8 => 4,  // G# -> 4
        9 => 11, // A  -> 11 (Bb)
        10 => 6, // A# -> 6
        11 => 1, // B  -> 1
        _ => 0,
    };
    let mode = if is_major { "B" } else { "A" };
    format!("{}{}", camelot_num, mode)
}

/// Parse a Camelot Wheel string into (pitch_class, is_major)
pub fn parse_camelot(s: &str) -> Option<(usize, bool)> {
    let s = s.trim();
    if s.len() < 2 {
        return None;
    }
    let num_part = &s[..s.len() - 1];
    let mode_char = s.chars().last()?;
    let is_major = match mode_char {
        'B' => true,
        'A' => false,
        _ => return None,
    };
    let camelot_num: usize = num_part.parse().ok()?;
    if !(1..=12).contains(&camelot_num) {
        return None;
    }
    // Reverse Camelot number to pitch class
    let pc = match camelot_num {
        1 => 11, // B
        2 => 6,  // F#
        3 => 1,  // C#
        4 => 8,  // G#
        5 => 3,  // D#
        6 => 10, // A#
        7 => 5,  // F
        8 => 0,  // C
        9 => 7,  // G
        10 => 2, // D
        11 => 9, // A
        12 => 4, // E
        _ => return None,
    };
    Some((pc, is_major))
}

/// Read key from metadata tags (ID3v2 TKEY, Vorbis INITIALKEY/KEY)
fn read_key_from_metadata(format: &mut Box<dyn symphonia::core::formats::FormatReader>) -> Option<String> {
    let metadata = format.metadata();
    let current = metadata.current()?;
    let tags = current.tags();
    tracing::debug!("Metadata tags found: {}", tags.len());
    for tag in tags {
        tracing::debug!("  tag: key={}, val={:?}", tag.key, tag.value);
        let val = match &tag.value {
            symphonia::core::meta::Value::String(s) => s.as_str(),
            _ => continue,
        };
        let is_key_tag = matches!(tag.key.as_str(), "TKEY" | "INITIALKEY" | "KEY");
        if is_key_tag {
            // Try to parse as Camelot first
            if let Some((pc, is_major)) = parse_camelot(val) {
                return Some(pitch_class_to_camelot(pc, is_major));
            }
            // Try to parse as standard key name (e.g., "C major", "A minor", "Am", "Bb")
            if let Some(camelot) = parse_key_name(val) {
                return Some(camelot);
            }
        }
    }
    None
}

/// Parse a standard key name (e.g., "C major", "A minor", "Am", "Bb") to Camelot
pub fn parse_key_name(s: &str) -> Option<String> {
    let s = s.trim().to_lowercase();
    // Try "X major"/"X minor" or "Xmaj"/"Xmin" formats
    let (note_str, is_major) = if let Some(pos) = s.find(" major") {
        (&s[..pos], true)
    } else if let Some(pos) = s.find(" minor") {
        (&s[..pos], false)
    } else if s.ends_with("maj") {
        (&s[..s.len() - 3], true)
    } else if s.ends_with("min") {
        (&s[..s.len() - 3], false)
    } else if s.ends_with("m") && s.len() > 1 {
        // "Am" = A minor
        (&s[..s.len() - 1], false)
    } else {
        // Assume major if just a note name
        (s.as_str(), true)
    };
    let pc = parse_note_name(note_str)?;
    Some(pitch_class_to_camelot(pc, is_major))
}

/// Parse a note name to pitch class (0-11)
fn parse_note_name(s: &str) -> Option<usize> {
    let s = s.trim().to_lowercase();
    let pc = if s.starts_with("c#") || s.starts_with("db") {
        1
    } else if s.starts_with("d#") || s.starts_with("eb") {
        3
    } else if s.starts_with("f#") || s.starts_with("gb") {
        6
    } else if s.starts_with("g#") || s.starts_with("ab") {
        8
    } else if s.starts_with("a#") || s.starts_with("bb") {
        10
    } else if s.starts_with('c') {
        0
    } else if s.starts_with('d') {
        2
    } else if s.starts_with('e') {
        4
    } else if s.starts_with('f') {
        5
    } else if s.starts_with('g') {
        7
    } else if s.starts_with('a') {
        9
    } else if s.starts_with('b') {
        11
    } else {
        return None;
    };
    Some(pc)
}

/// Detect key from audio samples using chromagram + Krumhansl-Schmuckler algorithm
pub fn detect_key_from_audio(samples: &[f32], sample_rate: u32) -> Option<String> {
    if samples.len() < sample_rate as usize {
        return None; // Need at least 1 second
    }

    // Use a portion of the audio (up to 30 seconds from the middle)
    let len = samples.len().min(sample_rate as usize * 30);
    let start = (samples.len() - len) / 2;
    let segment = &samples[start..start + len];

    // Compute chroma features via FFT
    let fft_size = 8192;
    let hop_size = fft_size / 2;
    let mut chroma = [0.0f32; 12];

    // Krumhansl-Schmuckler key profiles
    // Major profile: [0, 0, 1, 1, 1, 0, 1, 0, 1, 1, 1, 0]
    let major_profile = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
    // Minor profile: [0, 0, 1, 1, 1, 0, 1, 0, 1, 1, 1, 0]
    let minor_profile = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

    let mut window = vec![0.0f32; fft_size];
    let mut num_frames = 0u32;

    // Hann window
    for (i, w) in window.iter_mut().enumerate().take(fft_size) {
        *w = 0.5 * (1.0 - (TAU * i as f32 / fft_size as f32).cos());
    }

    // Process frames
    let mut offset = 0;
    while offset + fft_size <= segment.len() {
        // Use Goertzel algorithm for the 12 pitch class centers

        for (pc, chroma_val) in chroma.iter_mut().enumerate() {
            // Map pitch class to frequency (A4 = 440Hz, A = pitch class 9)
            // We accumulate energy across octaves 2-7 (roughly 80Hz - 8000Hz)
            let mut energy = 0.0f32;

            for octave in 2..=7 {
                let freq = 440.0 * 2.0f32.powf((pc as f32 - 9.0 + (octave as f32 - 4.0) * 12.0) / 12.0);
                if freq >= sample_rate as f32 * 0.5 {
                    break;
                }
                // Goertzel algorithm for this frequency
                let k = (0.5 + fft_size as f32 * freq / sample_rate as f32) as usize;
                let omega = TAU * k as f32 / fft_size as f32;
                let coeff = 2.0 * omega.cos();
                let mut s1 = 0.0f32;
                let mut s2 = 0.0f32;

                for i in 0..fft_size {
                    let sample = segment[offset + i] * window[i];
                    let s0 = sample + coeff * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
                energy += power;
            }
            *chroma_val += energy;
        }

        num_frames += 1;
        offset += hop_size;
    }

    if num_frames == 0 {
        return None;
    }

    // Normalize chroma
    let max_chroma = chroma.iter().cloned().fold(0.0f32, f32::max);
    if max_chroma > 0.0 {
        for c in &mut chroma {
            *c /= max_chroma;
        }
    }

    // Find best matching key using correlation
    let mut best_score = f32::NEG_INFINITY;
    let mut best_pc = 0;
    let mut best_major = true;

    for pc in 0..12 {
        // Rotate chroma to test this key as tonic
        let mut rotated = [0.0f32; 12];
        for i in 0..12 {
            rotated[i] = chroma[(i + pc) % 12];
        }

        // Correlate with major profile
        let major_corr = correlation(&rotated, &major_profile);
        if major_corr > best_score {
            best_score = major_corr;
            best_pc = pc;
            best_major = true;
        }

        // Correlate with minor profile
        let minor_corr = correlation(&rotated, &minor_profile);
        if minor_corr > best_score {
            best_score = minor_corr;
            best_pc = pc;
            best_major = false;
        }
    }

    let detected = Some(pitch_class_to_camelot(best_pc, best_major));
    tracing::debug!("Audio key detection: pc={}, major={}, score={:.3}, result={:?}", best_pc, best_major, best_score, detected);
    detected
}

/// Pearson correlation coefficient
fn correlation(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let n = 12.0;
    let sum_a: f32 = a.iter().sum();
    let sum_b: f32 = b.iter().sum();
    let sum_ab: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let sum_a2: f32 = a.iter().map(|x| x * x).sum();
    let sum_b2: f32 = b.iter().map(|x| x * x).sum();

    let denom = ((n * sum_a2 - sum_a * sum_a) * (n * sum_b2 - sum_b * sum_b)).sqrt();
    if denom < 1e-10 {
        return 0.0;
    }
    (n * sum_ab - sum_a * sum_b) / denom
}

/// Background BPM analyzer that decodes audio files and detects tempo
pub struct BpmAnalyzer;

impl BpmAnalyzer {
    /// Analyze an audio file in a background thread. Calls `on_result` when done.
    pub fn analyze_file(
        path: &Path,
        on_result: BpmCallback,
    ) {
        let path = path.to_path_buf();
        thread::spawn(move || {
            let result = Self::decode_and_analyze(&path);
            if let Ok(cb) = on_result.lock() {
                cb(result.map_err(|e| format!("{}: {}", path.display(), e)));
            }
        });
    }

    fn decode_and_analyze(path: &Path) -> Result<BpmResult, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mss = symphonia::core::io::MediaSourceStream::new(Box::new(file), Default::default());

        let hint = Hint::new();
        let format_opts = FormatOptions::default();
        let meta_opts = MetadataOptions::default();
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &meta_opts)
            .map_err(|e| e.to_string())?;

        let mut format = probed.format;

        // Try to read key from metadata first
        let meta_key = read_key_from_metadata(&mut format);

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.sample_rate.is_some())
            .ok_or("No audio track found")?;

        let track_id = track.id;
        let sample_rate = track.codec_params.sample_rate.unwrap_or(44100) as u32;

        let codec_opts = DecoderOptions::default();
        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &codec_opts)
            .map_err(|e| e.to_string())?;

        let mut samples: Vec<f32> = Vec::new();
        let max_samples = sample_rate as usize * 60; // 60 seconds max

        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break
                }
                Err(_) => break,
            };

            if packet.track_id() != track_id {
                continue;
            }

            match decoder.decode(&packet) {
                Ok(audio_buf) => {
                    let spec = *audio_buf.spec();
                    let num_channels = spec.channels.count() as u32;
                    let mut sample_buf = SampleBuffer::<f32>::new(audio_buf.capacity() as u64, spec);
                    sample_buf.copy_interleaved_ref(audio_buf);

                    // Mix down to mono
                    let interleaved = sample_buf.samples();
                    for frame in interleaved.chunks(num_channels as usize) {
                        let mono_sample: f32 = frame.iter().sum::<f32>() / num_channels as f32;
                        samples.push(mono_sample);
                        if samples.len() >= max_samples {
                            break;
                        }
                    }
                }
                Err(_) => continue,
            }

            if samples.len() >= max_samples {
                break;
            }
        }

        if samples.is_empty() {
            return Err("No samples decoded".to_string());
        }

        let result =
            analyze_audio(&samples, sample_rate, AnalysisConfig::default()).map_err(|e| e.to_string())?;

        // Use metadata key if available, otherwise detect from audio
        let key = if meta_key.is_some() {
            tracing::debug!("Using metadata key for {}: {:?}", path.display(), meta_key);
            meta_key
        } else {
            tracing::debug!("No metadata key for {}, attempting audio analysis ({} samples, {}Hz)", path.display(), samples.len(), sample_rate);
            let detected = detect_key_from_audio(&samples, sample_rate);
            tracing::debug!("Audio key detection result for {}: {:?}", path.display(), detected);
            detected
        };

        Ok(BpmResult {
            bpm: result.bpm as f32,
            confidence: result.bpm_confidence,
            key,
        })
    }
}
