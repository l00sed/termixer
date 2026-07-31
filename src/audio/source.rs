//! Audio source abstraction for MPV IPC control

/// Audio source representing an MPV instance
pub struct AudioSource;

impl AudioSource {
    pub fn new(_name: impl Into<String>, _socket_path: impl Into<String>) -> Self {
        Self
    }
}

/// Manager for multiple audio sources
pub struct AudioSourceManager {
    sources: Vec<AudioSource>,
}

impl AudioSourceManager {
    pub fn new() -> Self {
        Self { sources: Vec::new() }
    }

    pub fn add_source(&mut self, source: AudioSource) {
        self.sources.push(source);
    }
}

impl Default for AudioSourceManager {
    fn default() -> Self {
        Self::new()
    }
}
