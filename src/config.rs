//! Config file management — checks embedded config files against
//! installed destinations and prompts the user to update them.

use std::path::{Path, PathBuf};

pub struct ConfigFile {
    pub label: &'static str,
    pub content: &'static [u8],
    pub install_path: PathBuf,
    pub executable: bool,
}

pub struct ConfigDiff {
    pub file_index: usize,
}

pub fn managed_files() -> Vec<ConfigFile> {
    let home = std::env::var("HOME").unwrap_or_default();
    let local_bin = PathBuf::from(&home).join(".local/bin");
    let config_dir = PathBuf::from(&home).join(".config/mpv");
    vec![
        ConfigFile {
            label: "MPV wrapper script",
            content: include_bytes!("../config/mpv"),
            install_path: local_bin.join("mpv"),
            executable: true,
        },
        ConfigFile {
            label: "MPV config",
            content: include_bytes!("../config/mpv.conf"),
            install_path: config_dir.join("mpv.conf"),
            executable: false,
        },
        ConfigFile {
            label: "Auto-socket Lua script",
            content: include_bytes!("../config/auto-socket.lua"),
            install_path: config_dir.join("scripts/auto-socket.lua"),
            executable: false,
        },
    ]
}

/// Compare embedded copies against installed destinations.
/// Returns indices of files that differ or are missing.
pub fn check_config_files() -> Vec<ConfigDiff> {
    let files = managed_files();
    let mut diffs = Vec::new();

    for (i, file) in files.iter().enumerate() {
        match std::fs::read(&file.install_path) {
            Ok(installed_bytes) => {
                if file.content != installed_bytes.as_slice() {
                    diffs.push(ConfigDiff { file_index: i });
                }
            }
            Err(_) => {
                diffs.push(ConfigDiff { file_index: i });
            }
        }
    }

    diffs
}

/// Write embedded config files to install destinations. Returns errors per file.
pub fn apply_config_files(diffs: &[ConfigDiff]) -> Vec<String> {
    let files = managed_files();
    let mut errors = Vec::new();

    for diff in diffs {
        let file = &files[diff.file_index];
        let dst = &file.install_path;

        if let Some(parent) = dst.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            errors.push(format!("{}: mkdir: {}", file.label, e));
            continue;
        }

        if let Err(e) = std::fs::write(dst, file.content) {
            errors.push(format!("{}: write: {}", file.label, e));
            continue;
        }

        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755));
        }
    }

    errors
}

/// Check if `~/.local/bin` is in PATH. If not, offer to add it.
/// Returns any errors from modifying shell config.
pub fn ensure_local_bin_in_path() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let local_bin = format!("{}/.local/bin", home);

    // Check if already in PATH
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if dir == local_bin {
                return None; // already in PATH
            }
        }
    }

    // Try to add to shell rc files
    let rc_files = [".zshrc", ".bashrc", ".profile"];
    let marker = "# termixer: added by config setup";

    for rc_name in &rc_files {
        let rc_path = Path::new(&home).join(rc_name);
        if !rc_path.exists() {
            continue;
        }

        if let Ok(contents) = std::fs::read_to_string(&rc_path) {
            if contents.contains(&local_bin) {
                return None; // already mentioned in this rc file
            }

            let entry = format!("\n{}\nexport PATH=\"{}:$PATH\"\n", marker, local_bin);
            if let Err(e) = std::fs::write(&rc_path, format!("{}\n{}", contents.trim_end(), entry))
            {
                return Some(format!("Failed to update {}: {}", rc_name, e));
            }
            return Some(format!(
                "Added {} to PATH in {}. Restart your shell or run: source ~/{}",
                local_bin, rc_name, rc_name
            ));
        }
    }

    Some(format!(
        "Could not find .zshrc, .bashrc, or .profile. Manually add {} to your PATH.",
        local_bin
    ))
}
