//! Reads raw f32-LE PCM from a FIFO written by MPV (`--ao=pcm`) and
//! pushes samples into an `rtrb::Producer`. The matching `Consumer` lives
//! inside the audio callback, so the entire path is lock-free SPSC.
//!
//! MPV is configured to output at the engine's native sample rate (e.g.
//! 96 kHz) via `--audio-samplerate=96000`. Its internal libswresample
//! handles high-quality upsampling — far superior to the linear
//! interpolation we used previously (which caused aliasing artifacts
//! that sounded like high-pitched bit-crushing during filter sweeps).

use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// A background thread reading interleaved stereo f32le PCM from a FIFO
/// and pushing samples into an rtrb ring buffer for the audio callback.
pub struct PipeCaptureThread {
    #[allow(dead_code)]
    pub path: PathBuf,
    stop: Arc<AtomicBool>,
    producer_rx: Option<Receiver<rtrb::Producer<f32>>>,
    _handle: Option<thread::JoinHandle<()>>,
}

impl PipeCaptureThread {
    /// Open the FIFO and start pumping samples into the rtrb producer.
    /// No resampling — MPV outputs at the correct rate already.
    pub fn open_with_producer(path: &Path, producer: rtrb::Producer<f32>) -> Result<Self, String> {
        if !path.exists() {
            return Err(format!("FIFO not found: {}", path.display()));
        }

        // Open FIFO as read+write to avoid blocking when no writer is attached yet.
        // This lets termixer start before the mpv route producer is running.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("open {}: {}", path.display(), e))?;
        set_nonblocking(&file)?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_inner = Arc::clone(&stop);
        let path_debug = path.to_path_buf();
        let (producer_tx, producer_thread_rx) = mpsc::sync_channel::<rtrb::Producer<f32>>(1);
        let (producer_thread_tx, producer_rx) = mpsc::sync_channel::<rtrb::Producer<f32>>(1);

        let handle = thread::Builder::new()
            .name(format!(
                "pipe-capture:{}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ))
            .spawn(move || {
                let mut file = file;
                let mut producer = match producer_thread_rx.recv() {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let mut byte_buf = [0u8; 4096];
                let mut total_bytes: u64 = 0;
                let mut log_counter: u32 = 0;
                let mut should_exit = false;
                let mut sample_buf: Vec<f32> = Vec::with_capacity(1024);

                while !should_exit {
                    if stop_inner.load(Ordering::Acquire) {
                        break;
                    }
                    match file.read(&mut byte_buf) {
                        Ok(0) => {
                            thread::sleep(std::time::Duration::from_millis(2));
                            continue;
                        }
                        Ok(n) => {
                            total_bytes += n as u64;
                            log_counter += 1;
                            let usable = n & !3;
                            let samples = usable / 4;

                            if log_counter <= 5 || log_counter % 500 == 0 {
                                let peek: Vec<f32> = (0..4.min(samples))
                                    .map(|i| {
                                        let b = &byte_buf[i * 4..i * 4 + 4];
                                        f32::from_le_bytes([b[0], b[1], b[2], b[3]])
                                    })
                                    .collect();
                                eprintln!(
                                    "pipe-capture: read#{} {} bytes, {} total, slots={}, samples={:?}",
                                    log_counter, n, total_bytes, producer.slots(), peek
                                );
                            }

                            // Direct passthrough — no resampling. MPV outputs at
                            // the engine's native rate (e.g. 96 kHz) via
                            // --audio-samplerate=96000.
                            if samples > 0 {
                                sample_buf.clear();
                                sample_buf.resize(samples, 0.0f32);
                                for (i, out) in sample_buf.iter_mut().enumerate() {
                                    let b = &byte_buf[i * 4..i * 4 + 4];
                                    *out = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                                }

                                let mut offset = 0usize;
                                while offset < sample_buf.len() {
                                    if stop_inner.load(Ordering::Acquire) {
                                        should_exit = true;
                                        break;
                                    }
                                    let slots = producer.slots();
                                    if slots == 0 {
                                        thread::sleep(std::time::Duration::from_micros(200));
                                        continue;
                                    }

                                    let to_write = (sample_buf.len() - offset).min(slots);
                                    let mut written = 0usize;
                                    for i in 0..to_write {
                                        let v = sample_buf[offset + i];
                                        if producer.push(v).is_err() {
                                            break;
                                        }
                                        written += 1;
                                    }
                                    offset += written;
                                }
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(2));
                            continue;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            eprintln!("pipe-capture: read error: {}", e);
                            break;
                        }
                    }
                }
                let _ = producer_thread_tx.send(producer);
                let _ = path_debug;
            })
            .map_err(|e| format!("spawn: {}", e))?;

        if let Err(e) = producer_tx.send(producer) {
            return Err(format!("pipe-capture: producer handoff failed: {}", e));
        }

        Ok(Self {
            path: path.to_path_buf(),
            stop,
            producer_rx: Some(producer_rx),
            _handle: Some(handle),
        })
    }

    pub fn shutdown(mut self) -> Option<rtrb::Producer<f32>> {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self._handle.take() {
            let _ = handle.join();
        }
        self.producer_rx
            .take()
            .and_then(|rx| rx.recv_timeout(std::time::Duration::from_millis(20)).ok())
    }
}

impl Drop for PipeCaptureThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self._handle.take() {
            let _ = handle.join();
        }
    }
}

fn set_nonblocking(file: &std::fs::File) -> Result<(), String> {
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid file descriptor owned by `file`; fcntl calls do not
    // outlive `file` and we check return codes for OS errors.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(format!(
                "fcntl(F_GETFL): {}",
                std::io::Error::last_os_error()
            ));
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(format!(
                "fcntl(F_SETFL): {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}
