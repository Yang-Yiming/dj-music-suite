use std::path::{Path, PathBuf};

use walkdir::{DirEntry, WalkDir};

const AUDIO_EXTS: &[&str] = &["mp3", "flac", "m4a", "aac", "wav", "aiff", "aif"];

/// Recursively collect audio files under `root` (sorted, hidden entries and
/// dot-dirs skipped).
pub fn scan_audio(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_hidden(e))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(DirEntry::into_path)
        .filter(|p| is_audio(p))
        .collect();
    files.sort();
    files
}

fn is_audio(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|n| n.starts_with('.'))
        .unwrap_or(false)
}

pub fn file_name(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}
