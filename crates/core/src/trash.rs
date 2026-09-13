//! The dedup trash: duplicates moved out of the library instead of deleted.
//!
//! The filesystem is the source of truth for *what* is in the trash; a
//! manifest at `<trash>/manifest.json` carries the one thing the filesystem
//! cannot: where each file came from. Without it a trashed `track01.flac` is
//! just a name, and `track01-1.flac` is a guess, so restoring would be
//! guesswork.
//!
//! Entries with no manifest record — the manifest was deleted or hand-edited,
//! files were dropped into the folder manually, or they were trashed before
//! the manifest existed — are listed as unknown origin: they can be deleted,
//! but not restored.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{usage, Event, Result, Sink};

/// Name of the manifest file inside the trash folder.
pub const MANIFEST: &str = "manifest.json";
/// Trash folder name inside the library root.
pub const DIR_NAME: &str = ".dedup-trash";

pub fn default_dir(root: &Path) -> PathBuf {
    root.join(DIR_NAME)
}

/// One file moved out of the library, as remembered in the manifest.
#[derive(Serialize, Deserialize, Clone)]
pub struct Record {
    /// location inside the trash, `/`-separated: the key for restore/delete
    pub trash: String,
    /// where it came from: relative to the library root when it was inside
    /// it, absolute otherwise
    pub original: String,
    /// the duplicate group kind it lost in
    pub kind: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub moved_at: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    #[serde(default)]
    entries: Vec<Record>,
}

/// A file currently sitting in the trash folder.
#[derive(Debug, Clone)]
pub struct Entry {
    /// absolute path inside the trash folder
    pub path: PathBuf,
    /// key for restore/delete: the path relative to the trash folder
    pub rel: String,
    pub size: u64,
    /// group kind it lost in; for unknown entries, the trash sub-folder
    pub kind: String,
    pub moved_at: Option<u64>,
    pub sha256: Option<String>,
    /// the original location, None when no manifest record matches
    pub original: Option<PathBuf>,
}

/// Outcome of a trash operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    pub restored: usize,
    pub removed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub bytes: u64,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `/`-separated key of `path` relative to `trash` (None when outside it).
pub fn relative(trash: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(trash).ok().map(key)
}

/// Where a file outside the trash came from: relative to the library root
/// when it is inside it, absolute otherwise.
pub fn original_key(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => key(rel),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

fn key(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// A request/manifest entry name is only usable when it names a path *inside*
/// the trash folder: no absolute paths, no `..`, no `.`.
pub fn is_safe_rel(rel: &str) -> bool {
    safe_rel(rel).is_some()
}

fn safe_rel(rel: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn resolve_original(root: &Path, stored: &str) -> PathBuf {
    let path = Path::new(stored);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Move a file, falling back to copy+remove when the two locations live on
/// different filesystems.
pub fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(src, dst)?;
            fs::remove_file(src)
        }
    }
}

fn manifest_path(trash: &Path) -> PathBuf {
    trash.join(MANIFEST)
}

fn load(trash: &Path) -> Manifest {
    fs::read_to_string(manifest_path(trash))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save(trash: &Path, manifest: &Manifest, sink: Sink) {
    match serde_json::to_string_pretty(manifest) {
        Ok(json) => {
            if let Err(e) = fs::write(manifest_path(trash), json + "\n") {
                sink(&Event::Warn(format!(
                    "[warn] cannot write trash manifest: {e}"
                )));
            }
        }
        Err(e) => sink(&Event::Warn(format!("[warn] cannot serialize manifest: {e}"))),
    }
}

/// Remember files that were just moved into the trash, keeping the records
/// already in the manifest.
pub fn append(trash: &Path, records: &[Record], sink: Sink) {
    if records.is_empty() {
        return;
    }
    let mut manifest = load(trash);
    manifest.entries.extend(records.iter().cloned());
    save(trash, &manifest, sink);
}

fn kind_from_rel(rel: &str) -> String {
    rel.split_once('/').map(|(kind, _)| kind.to_string()).unwrap_or_default()
}

/// Every file physically present in the trash folder.
fn scan(trash: &Path) -> Vec<(String, PathBuf, u64)> {
    let mut found = Vec::new();
    for entry in WalkDir::new(trash).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.parent() == Some(trash) && path.file_name().and_then(|n| n.to_str()) == Some(MANIFEST)
        {
            continue;
        }
        let Some(rel) = relative(trash, path) else {
            continue;
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        found.push((rel, path.to_path_buf(), size));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// Everything currently in the trash, joined with the manifest. Files without
/// a record are still listed (unknown origin).
pub fn list(root: &Path, trash: &Path) -> Vec<Entry> {
    let by_rel: BTreeMap<String, Record> = load(trash)
        .entries
        .into_iter()
        .map(|r| (r.trash.clone(), r))
        .collect();

    scan(trash)
        .into_iter()
        .map(|(rel, path, size)| match by_rel.get(&rel) {
            Some(record) => Entry {
                path,
                kind: record.kind.clone(),
                moved_at: Some(record.moved_at),
                sha256: record.sha256.clone(),
                original: Some(resolve_original(root, &record.original)),
                rel,
                size,
            },
            None => Entry {
                path,
                kind: kind_from_rel(&rel),
                moved_at: None,
                sha256: None,
                original: None,
                rel,
                size,
            },
        })
        .collect()
}

/// Entries that can be put back: the ones with a remembered origin.
pub fn restorable(root: &Path, trash: &Path) -> Vec<String> {
    list(root, trash)
        .into_iter()
        .filter(|e| e.original.is_some())
        .map(|e| e.rel)
        .collect()
}

/// Put entries back where they came from. A target that is occupied again is
/// skipped, never overwritten: rekordbox relinks by path, so silently
/// choosing a different name is worse than leaving the file in the trash.
pub fn restore(root: &Path, trash: &Path, rels: &[String], sink: Sink) -> Result<Summary> {
    let plan = plan_entries(trash, rels)?;
    let by_rel: BTreeMap<String, Record> = load(trash)
        .entries
        .into_iter()
        .map(|r| (r.trash.clone(), r))
        .collect();

    sink(&Event::Start(plan.len() as u64));
    let mut summary = Summary::default();
    let mut done: Vec<String> = Vec::new();
    for (rel, path) in &plan {
        if !path.is_file() {
            sink(&Event::Line(format!("[skip] gone: {}", path.display())));
            summary.skipped += 1;
            sink(&Event::Step(rel.clone()));
            continue;
        }
        let Some(record) = by_rel.get(rel) else {
            sink(&Event::Line(format!(
                "[skip] unknown origin, cannot restore: {rel}"
            )));
            summary.skipped += 1;
            sink(&Event::Step(rel.clone()));
            continue;
        };
        let target = resolve_original(root, &record.original);
        // the manifest is ours, but a hand-edited one must not be able to
        // write anywhere outside the library
        if !target.starts_with(root) {
            sink(&Event::Line(format!(
                "[fail] origin outside the library, refused: {}",
                record.original
            )));
            summary.failed += 1;
            sink(&Event::Step(rel.clone()));
            continue;
        }
        if target.exists() {
            sink(&Event::Line(format!(
                "[skip] target exists, kept in the trash: {}",
                target.display()
            )));
            summary.skipped += 1;
            sink(&Event::Step(rel.clone()));
            continue;
        }
        let placed = target
            .parent()
            .map(fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| move_file(path, &target));
        match placed {
            Ok(()) => {
                summary.restored += 1;
                summary.bytes += record.size;
                done.push(rel.clone());
                sink(&Event::Line(format!(
                    "[ok] {} -> {}",
                    rel,
                    target.display()
                )));
            }
            Err(e) => {
                sink(&Event::Line(format!("[fail] {rel}: {e}")));
                summary.failed += 1;
            }
        }
        sink(&Event::Step(rel.clone()));
    }
    prune(trash, &done, sink);
    Ok(summary)
}

/// Permanently delete entries (irreversible).
pub fn remove(trash: &Path, rels: &[String], sink: Sink) -> Result<Summary> {
    let plan = plan_entries(trash, rels)?;
    sink(&Event::Start(plan.len() as u64));
    let mut summary = Summary::default();
    let mut done: Vec<String> = Vec::new();
    for (rel, path) in &plan {
        if !path.is_file() {
            sink(&Event::Line(format!("[skip] gone: {}", path.display())));
            summary.skipped += 1;
            sink(&Event::Step(rel.clone()));
            continue;
        }
        let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        match fs::remove_file(path) {
            Ok(()) => {
                summary.removed += 1;
                summary.bytes += size;
                done.push(rel.clone());
                sink(&Event::Line(format!("[ok] deleted {rel}")));
            }
            Err(e) => {
                sink(&Event::Line(format!("[fail] {rel}: {e}")));
                summary.failed += 1;
            }
        }
        sink(&Event::Step(rel.clone()));
    }
    prune(trash, &done, sink);
    Ok(summary)
}

/// Permanently delete everything in the trash folder (irreversible).
pub fn clear(trash: &Path, sink: Sink) -> Summary {
    let entries = scan(trash);
    sink(&Event::Start(entries.len() as u64));
    let mut summary = Summary::default();
    for (rel, path, size) in &entries {
        match fs::remove_file(path) {
            Ok(()) => {
                summary.removed += 1;
                summary.bytes += size;
            }
            Err(e) => {
                sink(&Event::Line(format!("[fail] {rel}: {e}")));
                summary.failed += 1;
            }
        }
        sink(&Event::Step(rel.clone()));
    }
    match fs::remove_dir_all(trash) {
        Ok(()) => sink(&Event::Line(format!(
            "[ok] removed {}",
            trash.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            sink(&Event::Line(format!("[fail] {}: {e}", trash.display())));
            summary.failed += 1;
        }
    }
    summary
}

/// Resolve the requested entry names, rejecting malformed ones before any
/// file is touched.
fn plan_entries(trash: &Path, rels: &[String]) -> Result<Vec<(String, PathBuf)>> {
    let mut plan = Vec::with_capacity(rels.len());
    for rel in rels {
        let Some(safe) = safe_rel(rel) else {
            return Err(usage(format!("bad trash entry: {rel}")));
        };
        plan.push((rel.clone(), trash.join(safe)));
    }
    Ok(plan)
}

/// Drop the manifest records of entries that were restored or deleted, along
/// with records whose file is gone.
fn prune(trash: &Path, done: &[String], sink: Sink) {
    let mut manifest = load(trash);
    if manifest.entries.is_empty() {
        return;
    }
    manifest.entries.retain(|r| {
        !done.contains(&r.trash) && trash.join(&r.trash).is_file()
    });
    save(trash, &manifest, sink);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("djms-trash-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(&dir).unwrap_or(dir)
    }

    fn no_sink(_: &Event) {}

    /// Build a trash holding `library/song.mp3`, recorded as trashed.
    fn trashed(root: &Path, rel: &str, original: &str) -> PathBuf {
        let trash = default_dir(root);
        let path = trash.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"audio").unwrap();
        append(
            &trash,
            &[Record {
                trash: rel.to_string(),
                original: original.to_string(),
                kind: "identical".to_string(),
                size: 5,
                sha256: Some("deadbeef".to_string()),
                moved_at: 1_700_000_000,
            }],
            &no_sink,
        );
        path
    }

    #[test]
    fn list_joins_the_manifest_and_flags_unknown_entries() {
        let root = tmpdir("list");
        fs::create_dir_all(root.join("ARTMS")).unwrap();
        trashed(&root, "identical/song.mp3", "ARTMS/song.mp3");
        // a file dropped in by hand: no record, so no origin
        let manual = default_dir(&root).join("same-identity/manual.mp3");
        fs::create_dir_all(manual.parent().unwrap()).unwrap();
        fs::write(&manual, b"junk").unwrap();

        let entries = list(&root, &default_dir(&root));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].rel, "identical/song.mp3");
        assert_eq!(entries[0].original, Some(root.join("ARTMS/song.mp3")));
        assert_eq!(entries[0].kind, "identical");
        assert_eq!(entries[0].moved_at, Some(1_700_000_000));
        assert_eq!(entries[1].rel, "same-identity/manual.mp3");
        assert_eq!(entries[1].original, None);
        assert_eq!(entries[1].kind, "same-identity");
        // the manifest itself is never listed as an entry
        assert!(fs::metadata(default_dir(&root).join(MANIFEST)).is_ok());
        assert!(entries.iter().all(|e| !e.rel.ends_with(MANIFEST)));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn restore_puts_the_file_back_and_keeps_the_manifest_clean() {
        let root = tmpdir("restore");
        fs::create_dir_all(root.join("ARTMS")).unwrap();
        trashed(&root, "identical/song.mp3", "ARTMS/song.mp3");

        let summary = restore(
            &root,
            &default_dir(&root),
            &["identical/song.mp3".to_string()],
            &no_sink,
        )
        .unwrap();
        assert_eq!(summary.restored, 1);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped, 0);
        assert_eq!(fs::read(root.join("ARTMS/song.mp3")).unwrap(), b"audio");
        assert!(list(&root, &default_dir(&root)).is_empty());

        // an empty manifest is left behind, not stale records
        let manifest: Manifest = load(&default_dir(&root));
        assert!(manifest.entries.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn restore_skips_occupied_targets_and_unknown_origins() {
        let root = tmpdir("conflict");
        fs::create_dir_all(root.join("ARTMS")).unwrap();
        trashed(&root, "identical/song.mp3", "ARTMS/song.mp3");
        trashed(&root, "identical/other.mp3", "ARTMS/other.mp3");
        // the library got the file back in the meantime
        fs::write(root.join("ARTMS/song.mp3"), b"newer").unwrap();

        let trash = default_dir(&root);
        let summary = restore(
            &root,
            &trash,
            &[
                "identical/song.mp3".to_string(),
                "identical/other.mp3".to_string(),
                "identical/missing.mp3".to_string(),
            ],
            &no_sink,
        )
        .unwrap();
        assert_eq!(summary.restored, 1);
        assert_eq!(summary.skipped, 2);
        assert_eq!(summary.failed, 0);
        // the occupied target keeps its content, the trashed copy stays put
        assert_eq!(fs::read(root.join("ARTMS/song.mp3")).unwrap(), b"newer");
        assert!(trash.join("identical/song.mp3").is_file());
        assert!(root.join("ARTMS/other.mp3").is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn restore_refuses_origins_outside_the_library() {
        let root = tmpdir("escape");
        let outside = root.parent().unwrap().join("djms-escape-target.mp3");
        let _ = fs::remove_file(&outside);
        // a hand-edited manifest must not be able to write out of the library
        trashed(&root, "identical/evil.mp3", outside.to_str().unwrap());

        let summary = restore(
            &root,
            &default_dir(&root),
            &["identical/evil.mp3".to_string()],
            &no_sink,
        )
        .unwrap();
        assert_eq!(summary.restored, 0);
        assert_eq!(summary.failed, 1);
        assert!(!outside.exists());
        assert!(default_dir(&root).join("identical/evil.mp3").is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn traversal_entry_names_are_rejected_before_anything_moves() {
        assert!(is_safe_rel("identical/song.mp3"));
        assert!(!is_safe_rel("../outside.mp3"));
        assert!(!is_safe_rel("/etc/passwd"));
        assert!(!is_safe_rel(""));
        assert!(!is_safe_rel("./song.mp3"));

        let root = tmpdir("traversal");
        trashed(&root, "identical/song.mp3", "ARTMS/song.mp3");
        let err = restore(
            &root,
            &default_dir(&root),
            &["../song.mp3".to_string()],
            &no_sink,
        )
        .unwrap_err();
        assert!(matches!(err, crate::Error::Usage(_)));
        assert!(default_dir(&root).join("identical/song.mp3").is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn remove_and_clear_delete_only_what_was_asked_for() {
        let root = tmpdir("delete");
        fs::create_dir_all(root.join("ARTMS")).unwrap();
        trashed(&root, "identical/a.mp3", "ARTMS/a.mp3");
        trashed(&root, "identical/b.mp3", "ARTMS/b.mp3");

        let trash = default_dir(&root);
        let summary = remove(&trash, &["identical/a.mp3".to_string()], &no_sink).unwrap();
        assert_eq!(summary.removed, 1);
        assert!(!trash.join("identical/a.mp3").exists());
        assert!(trash.join("identical/b.mp3").is_file());
        assert_eq!(list(&root, &trash).len(), 1);

        let summary = clear(&trash, &no_sink);
        assert_eq!(summary.removed, 1);
        assert_eq!(summary.failed, 0);
        assert!(!trash.exists());
        assert!(list(&root, &trash).is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn append_keeps_earlier_records_and_replaces_nothing() {
        let root = tmpdir("append");
        let trash = default_dir(&root);
        fs::create_dir_all(&trash).unwrap();
        append(
            &trash,
            &[Record {
                trash: "identical/a.mp3".to_string(),
                original: "a.mp3".to_string(),
                kind: "identical".to_string(),
                size: 1,
                sha256: None,
                moved_at: 1,
            }],
            &no_sink,
        );
        append(
            &trash,
            &[Record {
                trash: "identical/b.mp3".to_string(),
                original: "b.mp3".to_string(),
                kind: "identical".to_string(),
                size: 2,
                sha256: None,
                moved_at: 2,
            }],
            &no_sink,
        );
        let manifest: Manifest = load(&trash);
        assert_eq!(manifest.entries.len(), 2);
        assert_eq!(manifest.entries[0].trash, "identical/a.mp3");
        assert_eq!(manifest.entries[1].trash, "identical/b.mp3");
        fs::remove_dir_all(&root).unwrap();
    }
}
