use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_root: Option<String>,
}

pub struct Paths {
    pub config_file: PathBuf,
    pub staging_root: PathBuf,
}

/// App data lives under the platform config dir (~/Library/Application
/// Support/dj-music-suite on macOS, ~/.config/dj-music-suite on Linux).
pub fn paths() -> Option<Paths> {
    let base = dirs::config_dir()?.join("dj-music-suite");
    Some(Paths {
        config_file: base.join("config.toml"),
        staging_root: base.join("staging"),
    })
}

pub fn load(config_file: &PathBuf) -> Config {
    fs::read_to_string(config_file)
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save(config_file: &PathBuf, config: &Config) -> std::io::Result<()> {
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(config_file, toml::to_string_pretty(config).unwrap_or_default())
}
