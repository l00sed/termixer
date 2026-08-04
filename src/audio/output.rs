//! Audio output device enumeration and selection via cpal
//!
//! Tracks available output devices and the user's selection.
//! MPV decks are routed via the `audio-device` IPC property.

use cpal::traits::{DeviceTrait, HostTrait};

/// Device type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DeviceType {
    Speakers,
    Headphones,
    Bluetooth,
    Usb,
    Hdmi,
    Unknown,
}

/// Audio output device — may come from cpal or MPV.
///
/// When sourced from MPV, `mpv_name` holds the CoreAudio UID that MPV
/// expects (e.g. `"coreaudio/AppleHDAEngineOutput:1.0.0"`), while
/// `display_name` is the human-readable description.
pub struct AudioDevice {
    pub display_name: String,
    pub device_type: DeviceType,
    /// CoreAudio UID used by MPV's `audio-device` property.
    /// `None` when sourced from cpal (no MPV routing available).
    pub mpv_name: Option<String>,
}

/// Audio output device manager (device list + selection state)
pub struct AudioOutput {
    devices: Vec<AudioDevice>,
    selected_device: Option<String>,
    /// CoreAudio UID of the selected device (for MPV routing).
    selected_mpv_name: Option<String>,
}

impl AudioOutput {
    /// Create a new audio output manager (cpal enumeration only)
    pub fn new() -> Self {
        let devices = Self::enumerate_devices();
        Self {
            devices,
            selected_device: None,
            selected_mpv_name: None,
        }
    }

    /// Enumerate all output devices via cpal
    fn enumerate_devices() -> Vec<AudioDevice> {
        let host = cpal::default_host();
        let mut devices = Vec::new();

        if let Ok(output_devices) = host.output_devices() {
            for device in output_devices {
                let desc = match device.description() {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let name = desc.name().to_string();
                let device_type = Self::map_device_type(desc.device_type(), desc.interface_type());
                devices.push(AudioDevice {
                    display_name: name,
                    device_type,
                    mpv_name: None,
                });
            }
        }

        devices
    }

    /// Map cpal's DeviceType + InterfaceType to our DeviceType
    fn map_device_type(cpal_type: cpal::DeviceType, interface: cpal::InterfaceType) -> DeviceType {
        match cpal_type {
            cpal::DeviceType::Headphones | cpal::DeviceType::Headset => DeviceType::Headphones,
            cpal::DeviceType::Speaker | cpal::DeviceType::Earpiece => match interface {
                cpal::InterfaceType::Bluetooth => DeviceType::Bluetooth,
                cpal::InterfaceType::Usb => DeviceType::Usb,
                cpal::InterfaceType::Hdmi => DeviceType::Hdmi,
                _ => DeviceType::Speakers,
            },
            _ => match interface {
                cpal::InterfaceType::Bluetooth => DeviceType::Bluetooth,
                cpal::InterfaceType::Usb => DeviceType::Usb,
                cpal::InterfaceType::Hdmi => DeviceType::Hdmi,
                _ => DeviceType::Speakers,
            },
        }
    }

    /// Replace the device list with entries from MPV's `audio-device-list`.
    /// Deduplicates by description (MPV may list multiple UIDs per physical device).
    pub fn set_devices_from_mpv(&mut self, mpv_devices: &[(String, String)]) {
        let mut seen = std::collections::HashSet::new();
        self.devices = mpv_devices
            .iter()
            .filter(|(_, desc)| seen.insert(desc.clone()))
            .map(|(name, desc)| AudioDevice {
                display_name: desc.clone(),
                device_type: DeviceType::Unknown,
                mpv_name: Some(name.clone()),
            })
            .collect();
    }

    /// Get the list of display names for the picker UI.
    pub fn devices(&self) -> Vec<String> {
        self.devices
            .iter()
            .map(|d| d.display_name.clone())
            .collect()
    }

    /// Get devices filtered by type
    #[allow(dead_code)]
    pub fn devices_by_type(&self, device_type: DeviceType) -> Vec<&AudioDevice> {
        self.devices
            .iter()
            .filter(|d| d.device_type == device_type)
            .collect()
    }

    /// Get the currently selected device display name
    pub fn selected_device(&self) -> Option<&str> {
        self.selected_device.as_deref()
    }

    /// Get the CoreAudio UID for the selected device (for MPV routing).
    #[allow(dead_code)]
    pub fn selected_mpv_name(&self) -> Option<&str> {
        self.selected_mpv_name.as_deref()
    }

    /// Select a device by display name. Returns the MPV device name if available.
    pub fn select_device(&mut self, display_name: &str) -> Result<Option<String>, String> {
        let device = self
            .devices
            .iter()
            .find(|d| d.display_name == display_name)
            .ok_or_else(|| format!("Device '{}' not found", display_name))?;
        self.selected_device = Some(display_name.to_string());
        self.selected_mpv_name = device.mpv_name.clone();
        Ok(device.mpv_name.clone())
    }

    /// Refresh the device list from cpal
    pub fn refresh_devices(&mut self) {
        self.devices = Self::enumerate_devices();
    }
}

impl Default for AudioOutput {
    fn default() -> Self {
        Self::new()
    }
}
