//! Inventory and cleanup for files created by the local web UI.
//!
//! The cleanup never walks arbitrary user folders. It only touches the app's
//! staging/cache directories and `meta` directories that are either registered
//! by the web UI or referenced by a surviving `input-folder` batch pointer.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use dj_music_core::config::Paths;
use dj_music_core::cover;
use serde::{Deserialize, Serialize};

pub const META_MARKER: &str = ".dj-music-suite-web-owned";
const REGISTRY_FILE: &str = "web-meta-dirs.json";

#[derive(Clone)]
pub struct Layout {
    pub config_file: PathBuf,
    pub config_root: PathBuf,
    pub staging_root: PathBuf,
    pub cache_root: PathBuf,
    pub registry_file: PathBuf,
}

impl Layout {
    pub fn from_paths(paths: &Paths) -> Self {
        let config_root = paths
            .config_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Layout {
            config_file: paths.config_file.clone(),
            config_root: config_root.clone(),
            staging_root: paths.staging_root.clone(),
            cache_root: cover::cache_dir(),
            registry_file: config_root.join(REGISTRY_FILE),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DirectoryUsage {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub batches: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct MetaUsage {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    /// False means this is a legacy `<source>/meta` found through an old
    /// `input-folder` pointer, not a directory carrying our ownership marker.
    pub managed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    pub config_file: PathBuf,
    pub config_bytes: u64,
    pub staging: DirectoryUsage,
    pub cache: DirectoryUsage,
    pub meta: Vec<MetaUsage>,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ClearOptions {
    #[serde(default = "default_true")]
    pub staging: bool,
    #[serde(default = "default_true")]
    pub cache: bool,
    #[serde(default = "default_true")]
    pub meta: bool,
    #[serde(default)]
    pub config: bool,
}

impl Default for ClearOptions {
    fn default() -> Self {
        ClearOptions {
            staging: true,
            cache: true,
            meta: true,
            config: false,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ClearSummary {
    pub freed_bytes: u64,
    pub staging_bytes: u64,
    pub cache_bytes: u64,
    pub meta_bytes: u64,
    pub meta_dirs: usize,
    pub config_removed: bool,
}

#[derive(Default, Deserialize, Serialize)]
struct Registry {
    #[serde(default)]
    meta_dirs: Vec<String>,
}

/// Mark a `meta` directory as web-owned when the web UI creates it. Existing
/// directories are never claimed, because they may contain user-managed art.
pub fn ensure_meta_dir(layout: &Layout, meta_dir: &Path) -> std::io::Result<()> {
    let existed = meta_dir.exists();
    if !existed {
        fs::create_dir_all(meta_dir)?;
        fs::write(meta_dir.join(META_MARKER), b"dj-music-suite-web\n")?;
    }
    if meta_dir.join(META_MARKER).is_file() && !meta_dir.starts_with(&layout.staging_root) {
        register_meta(layout, meta_dir)?;
    }
    Ok(())
}

fn register_meta(layout: &Layout, meta_dir: &Path) -> std::io::Result<()> {
    let mut registry = read_registry(layout);
    let value = canonical_string(meta_dir);
    if !registry.meta_dirs.contains(&value) {
        registry.meta_dirs.push(value);
        registry.meta_dirs.sort();
        write_registry(layout, &registry)?;
    }
    Ok(())
}

fn read_registry(layout: &Layout) -> Registry {
    fs::read_to_string(&layout.registry_file)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_registry(layout: &Layout, registry: &Registry) -> std::io::Result<()> {
    if let Some(parent) = layout.registry_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_vec_pretty(registry).unwrap_or_else(|_| b"{}".to_vec());
    let tmp = layout.registry_file.with_extension("json.tmp");
    fs::write(&tmp, raw)?;
    fs::rename(tmp, &layout.registry_file)
}

pub fn inventory(layout: &Layout) -> std::io::Result<Inventory> {
    let staging = directory_usage(&layout.staging_root);
    let cache = directory_usage(&layout.cache_root);
    let meta = meta_candidates(layout)
        .into_iter()
        .map(|(path, managed)| {
            let usage = directory_usage(&path);
            MetaUsage {
                path,
                bytes: usage.bytes,
                files: usage.files,
                managed,
            }
        })
        .collect::<Vec<_>>();
    let config_bytes = fs::metadata(&layout.config_file)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .unwrap_or(0);
    let total_bytes =
        staging.bytes + cache.bytes + meta.iter().map(|entry| entry.bytes).sum::<u64>();
    Ok(Inventory {
        config_file: layout.config_file.clone(),
        config_bytes,
        staging: DirectoryUsage {
            batches: count_batches(&layout.staging_root),
            ..staging
        },
        cache,
        meta,
        total_bytes,
    })
}

pub fn clear(layout: &Layout, options: &ClearOptions) -> std::io::Result<ClearSummary> {
    let before = inventory(layout)?;
    let mut summary = ClearSummary::default();

    if options.staging && layout.staging_root.exists() {
        summary.staging_bytes = before.staging.bytes;
        clear_dir_contents(&layout.staging_root)?;
    }
    if options.cache && layout.cache_root.exists() {
        summary.cache_bytes = before.cache.bytes;
        remove_path(&layout.cache_root)?;
    }
    if options.meta {
        for entry in &before.meta {
            if !safe_meta_path(layout, &entry.path) {
                continue;
            }
            if remove_path(&entry.path).is_ok() {
                summary.meta_bytes += entry.bytes;
                summary.meta_dirs += 1;
            }
        }
        let _ = fs::remove_file(&layout.registry_file);
    }
    if options.config && layout.config_file.exists() {
        let bytes = fs::metadata(&layout.config_file)
            .map(|m| m.len())
            .unwrap_or(0);
        if fs::remove_file(&layout.config_file).is_ok() {
            summary.config_removed = true;
            summary.freed_bytes += bytes;
        }
    }

    summary.freed_bytes += summary.staging_bytes + summary.cache_bytes + summary.meta_bytes;
    Ok(summary)
}

fn meta_candidates(layout: &Layout) -> Vec<(PathBuf, bool)> {
    let registry = read_registry(layout);
    let mut candidates: BTreeMap<String, (PathBuf, bool)> = BTreeMap::new();

    for raw in registry.meta_dirs {
        let path = PathBuf::from(raw);
        if !path.is_dir() {
            continue;
        }
        let managed = path.join(META_MARKER).is_file();
        candidates.insert(canonical_string(&path), (path, managed));
    }

    if let Ok(entries) = fs::read_dir(&layout.staging_root) {
        for entry in entries.flatten() {
            let pointer = entry.path().join("input-folder");
            let Ok(source) = fs::read_to_string(pointer) else {
                continue;
            };
            let meta = PathBuf::from(source.trim()).join("meta");
            if !meta.exists() {
                continue;
            }
            let managed = meta.join(META_MARKER).is_file();
            if !managed && !looks_like_cover_cache(&meta) {
                continue;
            }
            candidates
                .entry(canonical_string(&meta))
                .and_modify(|(_, known)| *known |= managed)
                .or_insert((meta, managed));
        }
    }

    candidates.into_values().collect()
}

fn looks_like_cover_cache(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let file = entry.path();
        let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        name.starts_with("track-")
            && file
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "jpg" | "jpeg" | "png" | "webp" | "gif"
                    )
                })
    })
}

fn canonical_string(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn safe_meta_path(layout: &Layout, path: &Path) -> bool {
    if path.file_name().and_then(|name| name.to_str()) != Some("meta") {
        return false;
    }
    if path == layout.config_root
        || layout.config_root.starts_with(path)
        || path.starts_with(&layout.staging_root)
    {
        return false;
    }
    path.components().count() >= 3
}

fn directory_usage(path: &Path) -> DirectoryUsage {
    let mut usage = DirectoryUsage {
        path: path.to_path_buf(),
        bytes: 0,
        files: 0,
        batches: 0,
    };
    walk_usage(path, &mut usage.bytes, &mut usage.files);
    usage
}

fn walk_usage(path: &Path, bytes: &mut u64, files: &mut u64) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_file() {
        *bytes += meta.len();
        *files += 1;
        return;
    }
    if !meta.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        walk_usage(&entry.path(), bytes, files);
    }
}

fn count_batches(path: &Path) -> usize {
    fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .count()
        })
        .unwrap_or(0)
}

fn clear_dir_contents(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        remove_path(&entry?.path())?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || meta.is_file() {
        fs::remove_file(path)
    } else if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_layout(tag: &str) -> Layout {
        let root =
            std::env::temp_dir().join(format!("djms-storage-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Layout {
            config_file: root.join("config.toml"),
            config_root: root.clone(),
            staging_root: root.join("staging"),
            cache_root: root.join("cache"),
            registry_file: root.join(REGISTRY_FILE),
        }
    }

    #[test]
    fn inventory_and_clear_only_remove_web_data() {
        let layout = temp_layout("clear");
        fs::create_dir_all(layout.staging_root.join("batch/uploaded")).unwrap();
        fs::write(layout.staging_root.join("batch/uploaded/a.ncm"), b"1234").unwrap();
        fs::create_dir_all(layout.cache_root.join("artwork")).unwrap();
        fs::write(layout.cache_root.join("artwork/a.img"), b"123456").unwrap();
        fs::write(&layout.config_file, b"library_root = '/tmp/music'\n").unwrap();

        let source = layout.config_root.join("source");
        let meta = source.join("meta");
        ensure_meta_dir(&layout, &meta).unwrap();
        fs::write(meta.join("track-1.jpg"), b"12345678").unwrap();
        let output = layout.config_root.join("converted");
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("keep.mp3"), b"keep").unwrap();

        let before = inventory(&layout).unwrap();
        assert_eq!(before.staging.bytes, 4);
        assert_eq!(before.cache.bytes, 6);
        assert_eq!(before.meta.len(), 1);
        assert!(before.meta[0].managed);

        let summary = clear(&layout, &ClearOptions::default()).unwrap();
        assert_eq!(summary.staging_bytes, 4);
        assert_eq!(summary.cache_bytes, 6);
        assert_eq!(summary.meta_bytes, 27);
        assert!(!meta.exists());
        assert!(output.join("keep.mp3").is_file());
        assert!(layout.config_file.is_file());
        assert!(!layout.staging_root.join("batch").exists());
        assert!(!layout.cache_root.exists());
    }

    #[test]
    fn legacy_meta_is_found_through_staging_pointer() {
        let layout = temp_layout("legacy");
        let source = layout.config_root.join("source");
        let meta = source.join("meta");
        fs::create_dir_all(&meta).unwrap();
        fs::write(meta.join("track-1.jpg"), b"legacy").unwrap();
        let batch = layout.staging_root.join("batch");
        fs::create_dir_all(&batch).unwrap();
        fs::write(
            batch.join("input-folder"),
            source.to_string_lossy().as_bytes(),
        )
        .unwrap();

        let before = inventory(&layout).unwrap();
        assert_eq!(before.meta.len(), 1);
        assert!(!before.meta[0].managed);

        clear(&layout, &ClearOptions::default()).unwrap();
        assert!(!meta.exists());
    }

    #[test]
    fn config_is_preserved_unless_requested() {
        let layout = temp_layout("config");
        fs::write(&layout.config_file, b"library_root = '/tmp/music'\n").unwrap();
        clear(&layout, &ClearOptions::default()).unwrap();
        assert!(layout.config_file.is_file());

        let options = ClearOptions {
            config: true,
            ..ClearOptions::default()
        };
        let summary = clear(&layout, &options).unwrap();
        assert!(summary.config_removed);
        assert!(!layout.config_file.exists());
    }
}
