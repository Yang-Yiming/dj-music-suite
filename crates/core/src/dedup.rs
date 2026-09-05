use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config;
use crate::quality;
use crate::scan::{file_name, scan_audio};
use crate::tags;
use crate::{usage, Event, Result, Sink};

/// same artist + title with durations within this delta counts as one song
const DURATION_TOLERANCE_SECS: u64 = 2;

pub struct DedupOpts {
    pub root: Option<PathBuf>,
    pub execute: bool,
    pub from: Option<PathBuf>,
    pub keep: KeepMode,
    pub trash: Option<PathBuf>,
    pub delete: bool,
    /// plan json written by the analysis; also seeds the hash/score cache
    /// (None: don't read or write a plan file, e.g. web)
    pub plan: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum KeepMode {
    Best,
    First,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ScoreDetail {
    pub format_rank: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits_per_sample: Option<u32>,
    pub has_cover: bool,
    pub has_lyrics: bool,
    pub tags_complete: bool,
    pub score: i64,
}

impl From<&quality::Quality> for ScoreDetail {
    fn from(q: &quality::Quality) -> Self {
        ScoreDetail {
            format_rank: q.format_rank,
            bitrate: q.bitrate,
            sample_rate: q.sample_rate,
            bits_per_sample: q.bits_per_sample,
            has_cover: q.has_cover,
            has_lyrics: q.has_lyrics,
            tags_complete: q.tags_complete,
            score: q.score,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_secs: u64,
    pub sha256: String,
    pub score: ScoreDetail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<(String, String)>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CachedFile {
    pub mtime_secs: u64,
    pub size: u64,
    pub sha256: String,
    pub score: ScoreDetail,
}

#[derive(Serialize, Deserialize)]
pub struct Group {
    /// "identical" (same content hash) or "same-identity" (same normalized
    /// artist + title within the duration tolerance)
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<(String, String)>,
    pub keep: FileEntry,
    pub losers: Vec<FileEntry>,
}

#[derive(Serialize, Deserialize)]
pub struct Plan {
    pub root: PathBuf,
    pub groups: Vec<Group>,
    /// hash/score cache keyed by path, valid while mtime + size are unchanged
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cache: BTreeMap<String, CachedFile>,
}

/// What the analysis scanned (for the report header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedupStats {
    pub scanned: usize,
    pub unreadable: usize,
}

/// Outcome of applying a dedup plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupSummary {
    pub root: PathBuf,
    pub removed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub freed_bytes: u64,
}

/// Measure every file, group the duplicates and optionally write the plan
/// json (which also seeds the next run's hash/score cache).
pub fn analyze(opts: &DedupOpts, sink: Sink) -> Result<(Plan, DedupStats)> {
    let root = config::resolve_library_root(opts.root.as_deref())?;
    let trash_dir = opts
        .trash
        .clone()
        .unwrap_or_else(|| root.join(".dedup-trash"));
    let mut cache = load_cache(opts.plan.as_deref());

    let files: Vec<PathBuf> = scan_audio(&root)
        .into_iter()
        .filter(|p| !p.starts_with(&trash_dir))
        .collect();
    sink(&Event::Start(files.len() as u64));
    let mut entries = Vec::new();
    let mut unreadable = 0usize;
    for path in &files {
        match measure(path, &mut cache) {
            Some(e) => entries.push(e),
            None => {
                unreadable += 1;
                sink(&Event::Warn(format!(
                    "[warn] cannot read, ignored: {}",
                    path.display()
                )));
            }
        }
        sink(&Event::Step(file_name(path)));
    }

    let groups = build_groups(entries, opts.keep);
    let plan = Plan {
        root,
        groups,
        cache,
    };
    if let Some(plan_path) = &opts.plan {
        write_plan(&plan, plan_path, sink);
    }
    Ok((
        plan,
        DedupStats {
            scanned: files.len(),
            unreadable,
        },
    ))
}

/// Read back a plan json written by [`analyze`].
pub fn load_plan(path: &Path) -> Result<Plan> {
    let raw = fs::read_to_string(path)
        .map_err(|e| usage(format!("cannot read plan {}: {e}", path.display())))?;
    serde_json::from_str(&raw).map_err(|e| usage(format!("bad plan file {}: {e}", path.display())))
}

/// Remove the losers of every group (freshly analyzed or loaded plan). Never
/// touches a group whose keeper is gone. Losers are deleted or moved into the
/// trash folder, depending on `delete`.
pub fn execute(opts: &DedupOpts, sink: Sink) -> Result<DedupSummary> {
    let plan = if let Some(from) = &opts.from {
        load_plan(from)?
    } else {
        let root = config::resolve_library_root(opts.root.as_deref())?;
        let trash_dir = opts.trash.clone().unwrap_or_else(|| root.join(".dedup-trash"));
        let mut cache = load_cache(opts.plan.as_deref());
        let files: Vec<PathBuf> = scan_audio(&root)
            .into_iter()
            .filter(|p| !p.starts_with(&trash_dir))
            .collect();
        sink(&Event::Start(files.len() as u64));
        let entries: Vec<FileEntry> = files
            .iter()
            .filter_map(|p| {
                let e = measure(p, &mut cache);
                sink(&Event::Step(file_name(p)));
                e
            })
            .collect();
        let groups = build_groups(entries, opts.keep);
        Plan {
            root,
            groups,
            cache,
        }
    };

    let trash = opts
        .trash
        .clone()
        .unwrap_or_else(|| plan.root.join(".dedup-trash"));
    sink(&Event::Start(plan.groups.len() as u64));
    let mut removed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut freed = 0u64;
    for group in &plan.groups {
        // never touch the losers when the keeper is missing: the group might
        // be the only surviving copy of the song
        if !group.keep.path.is_file() {
            sink(&Event::Line(format!(
                "[skip] keeper is gone, group left untouched: {}",
                group.keep.path.display()
            )));
            skipped += group.losers.len();
            sink(&Event::Step(String::new()));
            continue;
        }
        for loser in &group.losers {
            if !loser.path.is_file() {
                sink(&Event::Line(format!("[skip] source is gone: {}", loser.path.display())));
                skipped += 1;
                continue;
            }
            let res = if opts.delete {
                fs::remove_file(&loser.path).map(|_| "deleted".to_string())
            } else {
                move_to_trash(&loser.path, &trash, &group.kind)
                    .map(|p| format!("-> {}", p.display()))
            };
            match res {
                Ok(msg) => {
                    removed += 1;
                    freed += loser.size;
                    sink(&Event::Line(format!(
                        "[ok] {} {}: {}",
                        msg, group.kind, loser.path.display()
                    )));
                }
                Err(e) => {
                    sink(&Event::Line(format!("[fail] {}: {e}", loser.path.display())));
                    failed += 1;
                }
            }
        }
        sink(&Event::Step(file_name(&group.keep.path)));
    }
    Ok(DedupSummary {
        root: plan.root,
        removed,
        skipped,
        failed,
        freed_bytes: freed,
    })
}

fn write_plan(plan: &Plan, plan_path: &Path, sink: Sink) {
    match serde_json::to_string_pretty(plan) {
        Ok(json) => {
            if let Err(e) = fs::write(plan_path, json + "\n") {
                sink(&Event::Warn(format!(
                    "[warn] cannot write plan {}: {e}",
                    plan_path.display()
                )));
            } else {
                sink(&Event::Line(format!("plan written to {}", plan_path.display())));
            }
        }
        Err(e) => sink(&Event::Warn(format!("[warn] cannot serialize plan: {e}"))),
    }
}

/// Move `src` into `<trash>/<kind>/`, keeping the original file name and
/// uniquifying on collision. Falls back to copy+remove when src and trash
/// live on different filesystems.
fn move_to_trash(src: &Path, trash: &Path, kind: &str) -> std::io::Result<PathBuf> {
    let dir = trash.join(kind);
    fs::create_dir_all(&dir)?;
    let name = file_name(src);
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let stem = name.strip_suffix(&ext).unwrap_or(&name);
    let mut dst = dir.join(&name);
    let mut n = 1u32;
    while dst.exists() {
        dst = dir.join(format!("{stem}-{n}{ext}"));
        n += 1;
    }
    match fs::rename(src, &dst) {
        Ok(()) => Ok(dst),
        Err(_) => {
            fs::copy(src, &dst)?;
            fs::remove_file(src)?;
            Ok(dst)
        }
    }
}

/// Probe + hash a file, consulting/updating the mtime+size keyed cache.
fn measure(path: &Path, cache: &mut BTreeMap<String, CachedFile>) -> Option<FileEntry> {
    let md = fs::metadata(path).ok()?;
    let size = md.len();
    let mtime_secs = md
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    let key = path.to_string_lossy().into_owned();
    if let Some(c) = cache.get(&key) {
        if c.mtime_secs == mtime_secs && c.size == size {
            return Some(entry_from_cache(path, c));
        }
    }
    let q = quality::measure(path)?;
    let sha256 = sha256_file(path).ok()?;
    let duration_secs = tags::read_meta(path)
        .and_then(|m| m.duration)
        .map(|d| d.as_secs());
    let identity = tags::read_meta(path).and_then(|m| tags::identity(&m));
    let entry = FileEntry {
        path: path.to_path_buf(),
        size,
        mtime_secs,
        score: ScoreDetail::from(&q),
        sha256,
        duration_secs,
        identity,
    };
    cache.insert(
        key,
        CachedFile {
            mtime_secs,
            size,
            sha256: entry.sha256.clone(),
            score: entry.score.clone(),
        },
    );
    Some(entry)
}

fn entry_from_cache(path: &Path, c: &CachedFile) -> FileEntry {
    // duration and identity are cheap to re-read and keep the plan complete
    let meta = tags::read_meta(path);
    FileEntry {
        path: path.to_path_buf(),
        size: c.size,
        mtime_secs: c.mtime_secs,
        sha256: c.sha256.clone(),
        score: c.score.clone(),
        duration_secs: meta.as_ref().and_then(|m| m.duration).map(|d| d.as_secs()),
        identity: meta.as_ref().and_then(|m| tags::identity(m)),
    }
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn load_cache(plan_path: Option<&Path>) -> BTreeMap<String, CachedFile> {
    plan_path
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|raw| serde_json::from_str::<Plan>(&raw).ok())
        .map(|p| p.cache)
        .unwrap_or_default()
}

fn build_groups(entries: Vec<FileEntry>, keep: KeepMode) -> Vec<Group> {
    let mut groups = Vec::new();

    // 1. identical content hashes win: those files are fully consumed
    let mut by_hash: BTreeMap<String, Vec<FileEntry>> = BTreeMap::new();
    for e in entries {
        by_hash.entry(e.sha256.clone()).or_default().push(e);
    }
    let mut rest = Vec::new();
    for (_, bucket) in by_hash {
        if bucket.len() > 1 {
            groups.push(make_group("identical", None, bucket, keep));
        } else {
            rest.extend(bucket);
        }
    }

    // 2. same normalized identity, then cluster by duration so different
    //    mixes/versions (outside the tolerance) are never merged
    let mut by_identity: BTreeMap<(String, String), Vec<FileEntry>> = BTreeMap::new();
    for e in rest {
        if let Some(identity) = e.identity.clone() {
            by_identity.entry(identity).or_default().push(e);
        }
    }
    for (identity, bucket) in by_identity {
        let mut bucket = bucket;
        bucket.sort_by(|a, b| a.duration_secs.cmp(&b.duration_secs));
        let mut cluster: Vec<FileEntry> = Vec::new();
        let mut flush = |cluster: &mut Vec<FileEntry>| {
            if cluster.len() > 1 {
                groups.push(make_group(
                    "same-identity",
                    Some(identity.clone()),
                    std::mem::take(cluster),
                    keep,
                ));
            } else if !cluster.is_empty() {
                let _leftovers = std::mem::take(cluster);
            }
        };
        for e in bucket {
            let contiguous = match (cluster.last().and_then(|l| l.duration_secs), e.duration_secs) {
                (Some(a), Some(b)) => b.saturating_sub(a) <= DURATION_TOLERANCE_SECS,
                // missing duration on either side: identity match is strong
                // enough, keep them together
                _ => true,
            };
            if !cluster.is_empty() && !contiguous {
                flush(&mut cluster);
            }
            cluster.push(e);
        }
        flush(&mut cluster);
    }
    groups
}

/// Order within a group: score desc, then mtime asc (oldest wins), then
/// shorter path, then lexicographic - all deterministic.
fn rank(e: &FileEntry) -> (i64, std::cmp::Reverse<u64>, std::cmp::Reverse<usize>, String) {
    let path = e.path.to_string_lossy().into_owned();
    (
        e.score.score,
        std::cmp::Reverse(e.mtime_secs),
        std::cmp::Reverse(path.len()),
        path,
    )
}

fn make_group(kind: &str, identity: Option<(String, String)>, bucket: Vec<FileEntry>, keep: KeepMode) -> Group {
    let mut bucket = bucket;
    match keep {
        KeepMode::Best => bucket.sort_by(|a, b| rank(b).cmp(&rank(a))),
        KeepMode::First => bucket.sort_by(|a, b| a.path.cmp(&b.path)),
    }
    let mut it = bucket.into_iter();
    let keep_entry = it.next().expect("group has at least two files");
    let mut losers: Vec<FileEntry> = it.collect();
    if keep == KeepMode::Best {
        // rank() is already the display order: losers sorted best-first
    } else {
        losers.sort_by(|a, b| rank(b).cmp(&rank(a)));
    }
    Group {
        kind: kind.to_string(),
        identity,
        keep: keep_entry,
        losers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use id3::TagLike;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "djms-dedup-test-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // analyze() canonicalizes the root; match that so path comparisons
        // in the tests line up (/var -> /private/var on macOS)
        fs::canonicalize(&dir).unwrap_or(dir)
    }

    /// `frames` MPEG frames -> distinct, stable durations (100 frames ~ 2s,
    /// 300 frames ~ 8s)
    fn write_tagged(path: &Path, artist: &str, title: &str, frames: usize) {
        let frame = [0xFFu8, 0xFB, 0x90, 0x00]
            .into_iter()
            .chain(std::iter::repeat(0u8).take(413))
            .collect::<Vec<u8>>();
        fs::write(path, frame.repeat(frames)).unwrap();
        let mut tag = id3::Tag::new();
        tag.set_artist(artist);
        tag.set_title(title);
        tag.write_to_path(path, id3::Version::Id3v24).unwrap();
    }

    fn add_cover(path: &Path) {
        let mut tag = id3::Tag::read_from_path(path).unwrap();
        tag.add_frame(id3::frame::Picture {
            mime_type: "image/jpeg".into(),
            picture_type: id3::frame::PictureType::CoverFront,
            description: String::new(),
            data: vec![0xFF, 0xD8, 0xFF, 1, 2, 3],
        });
        tag.write_to_path(path, id3::Version::Id3v24).unwrap();
    }

    fn no_sink(_: &Event) {}

    fn collect(root: &Path, keep: KeepMode) -> Vec<Group> {
        let opts = DedupOpts {
            root: Some(root.to_path_buf()),
            execute: false,
            from: None,
            keep,
            trash: None,
            delete: false,
            plan: None,
        };
        let (plan, _stats) = analyze(&opts, &no_sink).unwrap();
        plan.groups
    }

    fn find<'a>(groups: &'a [Group], path: &Path) -> &'a Group {
        groups
            .iter()
            .find(|g| {
                g.keep.path == path || g.losers.iter().any(|l| l.path == path)
            })
            .unwrap()
    }

    fn exec_opts(root: &Path, delete: bool) -> DedupOpts {
        DedupOpts {
            root: Some(root.to_path_buf()),
            execute: true,
            from: None,
            keep: KeepMode::Best,
            trash: None,
            delete,
            plan: None,
        }
    }

    #[test]
    fn identical_content_groups_even_with_different_tags() {
        let root = tmpdir("ident");
        write_tagged(&root.join("a.mp3"), "A", "T", 100);
        write_tagged(&root.join("b.mp3"), "A", "T", 100);
        // make the bytes identical by stripping b's tag again: rewrite a's
        // bytes over b's
        let bytes = fs::read(&root.join("a.mp3")).unwrap();
        fs::write(&root.join("b.mp3"), &bytes).unwrap();

        let groups = collect(&root, KeepMode::Best);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, "identical");
        assert_eq!(groups[0].losers.len(), 1);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn same_identity_similar_duration_groups_but_not_different_duration() {
        let root = tmpdir("identity");
        write_tagged(&root.join("a.mp3"), "A", "T", 100);
        write_tagged(&root.join("b.mp3"), "A", "T", 101);
        write_tagged(&root.join("long.mp3"), "A", "T", 300);

        let groups = collect(&root, KeepMode::Best);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, "same-identity");
        assert_eq!(
            groups[0].identity,
            Some(("a".to_string(), "t".to_string()))
        );
        // the 300-frame version (~8s) stays out of the group
        let paths: Vec<&PathBuf> = groups[0]
            .losers
            .iter()
            .map(|l| &l.path)
            .chain(std::iter::once(&groups[0].keep.path))
            .collect();
        assert!(!paths.iter().any(|p| p.ends_with("long.mp3")));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn keep_best_prefers_cover() {
        let root = tmpdir("cover");
        write_tagged(&root.join("plain.mp3"), "A", "T", 100);
        write_tagged(&root.join("cover.mp3"), "A", "T", 100);
        add_cover(&root.join("cover.mp3"));

        let groups = collect(&root, KeepMode::Best);
        let g = find(&groups, &root.join("cover.mp3"));
        assert_eq!(g.keep.path, root.join("cover.mp3"));
        assert!(g.keep.score.has_cover);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn keep_first_ignores_scores() {
        let root = tmpdir("first");
        write_tagged(&root.join("a-plain.mp3"), "A", "T", 100);
        write_tagged(&root.join("b-cover.mp3"), "A", "T", 100);
        add_cover(&root.join("b-cover.mp3"));

        let groups = collect(&root, KeepMode::First);
        let g = find(&groups, &root.join("a-plain.mp3"));
        assert_eq!(g.keep.path, root.join("a-plain.mp3"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn execute_moves_losers_to_trash_and_delete_removes() {
        let root = tmpdir("exec");
        write_tagged(&root.join("a.mp3"), "A", "T", 100);
        write_tagged(&root.join("b.mp3"), "A", "T", 101);

        let summary = execute(&exec_opts(&root, false), &no_sink).unwrap();
        assert_eq!(summary.failed, 0);
        assert!(root.join("a.mp3").is_file() || root.join("b.mp3").is_file());
        let kept = if root.join("a.mp3").is_file() {
            root.join("a.mp3")
        } else {
            root.join("b.mp3")
        };
        let lost = if kept == root.join("a.mp3") {
            root.join("b.mp3")
        } else {
            root.join("a.mp3")
        };
        assert!(!lost.exists());
        assert!(root
            .join(".dedup-trash")
            .join("same-identity")
            .join(lost.file_name().unwrap())
            .is_file());
        assert!(kept.is_file());
        assert_eq!(summary.removed, 1);
        assert!(summary.freed_bytes > 0);

        // delete mode on a fresh library
        let root2 = tmpdir("exec-del");
        write_tagged(&root2.join("a.mp3"), "A", "T", 100);
        write_tagged(&root2.join("b.mp3"), "A", "T", 101);
        let summary = execute(&exec_opts(&root2, true), &no_sink).unwrap();
        assert_eq!(summary.failed, 0);
        let remaining = scan_audio(&root2);
        assert_eq!(remaining.len(), 1);
        assert!(!root2.join(".dedup-trash").exists());
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&root2).unwrap();
    }

    #[test]
    fn execute_skips_group_when_keeper_is_gone() {
        let root = tmpdir("gone");
        write_tagged(&root.join("a.mp3"), "A", "T", 100);
        write_tagged(&root.join("b.mp3"), "A", "T", 101);
        // analyze + write the plan, then simulate the keeper disappearing:
        // deleting the losers of the analyzed group leaves only the keeper
        let plan_path = root.join(".plan.json");
        let mut opts = exec_opts(&root, true);
        opts.execute = false;
        opts.plan = Some(plan_path.clone());
        let (plan, _) = analyze(&opts, &no_sink).unwrap();
        for loser in &plan.groups[0].losers {
            let _ = fs::remove_file(&loser.path);
        }
        let before: Vec<PathBuf> = scan_audio(&root);
        let mut exec = exec_opts(&root, true);
        exec.from = Some(plan_path);
        let summary = execute(&exec, &no_sink).unwrap();
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped, 1);
        let after: Vec<PathBuf> = scan_audio(&root);
        assert_eq!(before, after, "nothing must be deleted without the keeper");
        fs::remove_dir_all(&root).unwrap();
    }
}
