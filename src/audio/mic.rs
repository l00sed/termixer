#![allow(dead_code)]

use cpal::traits::{DeviceTrait, HostTrait};

/// Input device metadata suitable for presenting in an application picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicInputDevice {
    /// Index accepted by `AudioEngine::select_mic_input`.
    pub index: usize,
    pub display_name: String,
    pub is_default: bool,
    pub max_channels: u16,
}

pub(crate) fn input_devices() -> Result<Vec<cpal::Device>, String> {
    cpal::default_host()
        .input_devices()
        .map(|devices| devices.collect())
        .map_err(|e| format!("Input devices: {e}"))
}

pub fn enumerate_input_devices() -> Result<Vec<MicInputDevice>, String> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.description().ok())
        .map(|description| description.to_string());

    Ok(input_devices()?
        .into_iter()
        .enumerate()
        .filter_map(|(index, device)| {
            let display_name = device
                .description()
                .map(|description| description.to_string())
                .unwrap_or_else(|_| format!("Input {index}"));
            let max_channels = device
                .supported_input_configs()
                .map(|configs| configs.map(|config| config.channels()).max().unwrap_or(0))
                .unwrap_or(0);
            (max_channels > 0).then_some(MicInputDevice {
                index,
                is_default: default_name.as_deref() == Some(display_name.as_str()),
                display_name,
                max_channels,
            })
        })
        .collect())
}
