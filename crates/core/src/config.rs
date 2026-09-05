use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{usage, Result};

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

pub fn load(config_file: &Path) -> Config {
    fs::read_to_string(config_file)
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save(config_file: &Path, config: &Config) -> std::io::Result<()> {
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        config_file,
        toml::to_string_pretty(config).unwrap_or_default(),
    )
}

/// Resolve the music library root for commands that operate on it
/// (import/reorg/dedup): an explicit `--root` wins, otherwise the configured
/// library root is used.
pub fn resolve_library_root(root: Option<&Path>) -> Result<PathBuf> {
    let raw = match root {
        Some(raw) => raw.to_path_buf(),
        None => {
            let Some(paths) = paths() else {
                return Err(usage("--root is required"));
            };
            match load(&paths.config_file).library_root {
                Some(s) if !s.trim().is_empty() => PathBuf::from(s),
                _ => {
                    return Err(usage(format!(
                        "--root is required (or set the library root once in the web UI; \
                         it would be stored in {})",
                        paths.config_file.display()
                    )))
                }
            }
        }
    };
    match fs::canonicalize(&raw) {
        Ok(p) if p.is_dir() => Ok(p),
        Ok(_) => Err(usage(format!("root is not a directory: {}", raw.display()))),
        Err(e) => Err(usage(format!("cannot resolve root {}: {e}", raw.display()))),
    }
}
