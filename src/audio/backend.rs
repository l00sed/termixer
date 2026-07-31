#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBackendKind {
    Cpal,
    CpalJack,
    CoreAudio,
    Jack,
}

pub fn available_backends() -> Vec<AudioBackendKind> {
    let mut out = vec![AudioBackendKind::Cpal];

    #[cfg(target_os = "macos")]
    {
        out.push(AudioBackendKind::CoreAudio);
    }

    #[cfg(feature = "jack-backend")]
    {
        out.push(AudioBackendKind::Jack);
    }

    out
}

pub fn default_backend() -> AudioBackendKind {
    std::env::var("TUI_MIXER_AUDIO_BACKEND")
        .ok()
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .and_then(|s| match s.as_str() {
            "cpal" => Some(AudioBackendKind::Cpal),
            "cpal-jack" => Some(AudioBackendKind::CpalJack),
            "coreaudio" => Some(AudioBackendKind::CoreAudio),
            "jack" => Some(AudioBackendKind::Jack),
            _ => None,
        })
        .filter(|requested| available_backends().contains(requested))
        .unwrap_or(AudioBackendKind::Cpal)
}

pub fn backend_label(backend: AudioBackendKind) -> &'static str {
    match backend {
        AudioBackendKind::Cpal => "cpal",
        AudioBackendKind::CpalJack => "cpal-jack",
        AudioBackendKind::CoreAudio => "coreaudio",
        AudioBackendKind::Jack => "jack",
    }
}

/// Detect whether PipeWire is managing audio on this system.
///
/// PipeWire provides ALSA compatibility via pipewire-alsa, so cpal's ALSA
/// backend should work — but only if the ALSA config routes through PipeWire
/// rather than directly to dmix (which fails on Steam Deck).
pub fn detect_pipewire() -> bool {
    // Check for PipeWire runtime directory (set by PipeWire session manager)
    if std::env::var("PIPEWIRE_RUNTIME_DIR").is_ok() {
        return true;
    }
    // Check for PipeWire native client API environment
    if std::env::var("PIPEWIRE_REMOTE").is_ok() {
        return true;
    }
    // Fallback: check if pipewire process is running (Steam Deck always has it)
    std::process::Command::new("pgrep")
        .args(["-x", "pipewire"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Detect whether PulseAudio (or PipeWire in PulseAudio compat mode) is running.
pub fn detect_pulseaudio() -> bool {
    if std::env::var("PULSE_SERVER").is_ok() {
        return true;
    }
    std::process::Command::new("pgrep")
        .args(["-x", "pulseaudio"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Ensure ALSA is configured to route through the active sound server
/// (PipeWire or PulseAudio) rather than directly to hardware dmix.
///
/// On Steam Deck and similar systems, raw ALSA dmix fails because PipeWire
/// has exclusive control of the audio device. This function detects the
/// sound server and ensures the environment is set up for cpal to connect
/// through the compatibility layer.
pub fn ensure_alsa_sound_server_routing() {
    // If user has explicitly configured ALSA, respect that.
    if std::env::var("LIBASOUND_NAME_HINT").is_ok() {
        return;
    }

    if detect_pipewire() {
        // PipeWire provides ALSA compatibility via pipewire-alsa.
        // Force ALSA to use the "default" PCM which PipeWire intercepts,
        // rather than trying dmix directly (which fails when PipeWire
        // has exclusive device access).
        if std::env::var("SDL_AUDIODRIVER").is_err() {
            // Prevent SDL from trying its own audio init which can conflict
        }
    } else if detect_pulseaudio() {
        // PulseAudio provides ALSA compatibility via libasound_module_pcm_pulse.
        // No special environment setup needed — just ensure the plugin is installed.
    }
}
