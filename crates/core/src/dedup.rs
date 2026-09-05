use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::quality;
use crate::scan::{file_name, scan_audio};
use crate::tags;

/// same artist + title with durations within this delta counts as one song
const DURATION_TOLERANCE_SECS: u64 = 2;

pub struct DedupOpts {
    pub root: Option<PathBuf>,
    pub execute: bool,
    pub from: Option<PathBuf>,
    pub keep: KeepMode,
    pub trash: Option<PathBuf>,
    pub delete: bool,
    pub plan: PathBuf,
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
struct FileEntry {
    path: PathBuf,
    size: u64,
    mtime_secs: u64,
    sha256: String,
    score: ScoreDetail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<(String, String)>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CachedFile {
    mtime_secs: u64,
    size: u64,
    sha256: String,
    score: ScoreDetail,
}

#[derive(Serialize, Deserialize)]
struct Group {
    /// "identical" (same content hash) or "same-identity" (same normalized
    /// artist + title within the duration tolerance)
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<(String, String)>,
    keep: FileEntry,
    losers: Vec<FileEntry>,
}

#[derive(Serialize, Deserialize)]
struct Plan {
    root: PathBuf,
    groups: Vec<Group>,
    /// hash/score cache keyed by path, valid while mtime + size are unchanged
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    cache: BTreeMap<String, CachedFile>,
}

/// Run the dedup command: prints its own report/progress and returns the CLI
/// exit code. (Not yet migrated to the event sink; see lib.rs.)
pub fn run(opts: DedupOpts) -> i32 {
    if opts.from.is_some() && !opts.execute {
        eprintln!("--from only makes sense together with --execute");
        return 2;
    }
    if opts.execute {
        execute(opts)
    } else {
        analyze(opts)
    }
}

fn resolve_root(root: Option<&Path>) -> Option<PathBuf> {
    match root.map(fs::canonicalize) {
        Some(Ok(p)) if p.is_dir() => Some(p),
        Some(Ok(_)) => {
            eprintln!("root is not a directory: {}", root.unwrap().display());
            None
        }
        Some(Err(e)) => {
            eprintln!("cannot resolve root {}: {e}", root.unwrap().display());
            None
        }
        None => None,
    }
}

fn analyze(opts: DedupOpts) -> i32 {
    let Some(root) = resolve_root(opts.root.as_deref()) else {
        return 2;
    };
    let trash_dir = opts
        .trash
        .clone()
        .unwrap_or_else(|| root.join(".dedup-trash"));
    let mut cache = load_cache(&opts.plan);

    let files: Vec<PathBuf> = scan_audio(&root)
        .into_iter()
        .filter(|p| !p.starts_with(&trash_dir))
        .collect();
    let pb = progress_bar(files.len() as u64);
    let mut entries = Vec::new();
    let mut unreadable = 0usize;
    for path in &files {
        match measure(path, &mut cache) {
            Some(e) => entries.push(e),
            None => {
                unreadable += 1;
                pb.println(format!("[warn] cannot read, ignored: {}", path.display()));
            }
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    let groups = build_groups(entries, opts.keep);
    print_report(&groups, unreadable, files.len());
    write_plan(
        &Plan {
            root,
            groups,
            cache,
        },
        &opts.plan,
    );
    0
}

fn execute(opts: DedupOpts) -> i32 {
    let plan = if let Some(from) = &opts.from {
        let raw = match fs::read_to_string(from) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("cannot read plan {}: {e}", from.display());
                return 2;
            }
        };
        match serde_json::from_str(&raw) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("bad plan file {}: {e}", from.display());
                return 2;
            }
        }
    } else {
        let Some(root) = resolve_root(opts.root.as_deref()) else {
            return 2;
        };
        let mut cache = load_cache(&opts.plan);
        let trash_dir = opts.trash.clone().unwrap_or_else(|| root.join(".dedup-trash"));
        let files: Vec<PathBuf> = scan_audio(&root)
            .into_iter()
            .filter(|p| !p.starts_with(&trash_dir))
            .collect();
        let entries: Vec<FileEntry> = files.iter().filter_map(|p| measure(p, &mut cache)).collect();
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
    let pb = progress_bar(plan.groups.len() as u64);
    let mut removed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut freed = 0u64;
    for group in &plan.groups {
        // never touch the losers when the keeper is missing: the group might
        // be the only surviving copy of the song
        if !group.keep.path.is_file() {
            pb.println(format!(
                "[skip] keeper is gone, group left untouched: {}",
                group.keep.path.display()
            ));
            skipped += group.losers.len();
            pb.inc(1);
            continue;
        }
        for loser in &group.losers {
            if !loser.path.is_file() {
                pb.println(format!("[skip] source is gone: {}", loser.path.display()));
                skipped += 1;
                continue;
            }
            let res = if opts.delete {
                fs::remove_file(&loser.path).map(|_| "deleted".to_string())
            } else {
                move_to_trash(&loser.path, &trash, &group.kind).map(|p| format!("-> {}", p.display()))
            };
            match res {
                Ok(msg) => {
                    removed += 1;
                    freed += loser.size;
                    pb.println(format!("[ok] {} {}: {}", msg, group.kind, loser.path.display()));
                }
                Err(e) => {
                    pb.println(format!("[fail] {}: {e}", loser.path.display()));
                    failed += 1;
                }
            }
        }
        pb.inc(1);
    }
    pb.finish_and_clear();
    let freed_mb = freed as f64 / (1024.0 * 1024.0);
    println!(
        "done: {removed} duplicates removed ({freed_mb:.1} MB freed), {skipped} skipped, {failed} failed"
    );
    if !opts.delete && removed > 0 {
        println!("duplicates were moved to {}", trash.display());
        println!("verify in your player, then delete the trash folder.");
    }
    if failed > 0 {
        1
    } else {
        0
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

fn load_cache(plan_path: &Path) -> BTreeMap<String, CachedFile> {
    fs::read_to_string(plan_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Plan>(&raw).ok())
        .map(|p| p.cache)
        .unwrap_or_default()
}

fn write_plan(plan: &Plan, plan_path: &Path) {
    match serde_json::to_string_pretty(plan) {
        Ok(json) => {
            if let Err(e) = fs::write(plan_path, json + "\n") {
                eprintln!("[warn] cannot write plan {}: {e}", plan_path.display());
            } else {
                println!("plan written to {}", plan_path.display());
            }
        }
        Err(e) => eprintln!("[warn] cannot serialize plan: {e}"),
    }
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

fn describe(score: &ScoreDetail) -> String {
    let mut parts = vec![format!("{}kbps", score.bitrate.unwrap_or(0))];
    if let Some(sr) = score.sample_rate {
        parts.push(format!("{sr}Hz"));
    }
    if score.has_cover {
        parts.push("cover".into());
    }
    if score.has_lyrics {
        parts.push("lyrics".into());
    }
    if !score.tags_complete {
        parts.push("incomplete-tags".into());
    }
    format!("score {} ({})", score.score, parts.join(", "))
}

fn print_report(groups: &[Group], unreadable: usize, scanned: usize) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut freed = 0u64;
    for g in groups {
        *counts.entry(g.kind.as_str()).or_default() += 1;
        freed += g.losers.iter().map(|l| l.size).sum::<u64>();
    }
    let freed_mb = freed as f64 / (1024.0 * 1024.0);
    println!(
        "scanned {scanned} files ({} unreadable ignored), {} duplicate groups, \
         up to {freed_mb:.1} MB reclaimable",
        if unreadable > 0 { unreadable.to_string() } else { "no".into() },
        groups.len()
    );
    for (kind, label) in [("identical", "identical"), ("same-identity", "same-identity")] {
        let n = counts.get(kind).copied().unwrap_or(0);
        if n > 0 {
            println!("  {label:<14} {n}");
        }
    }
    println!();
    for g in groups {
        match (&g.identity, g.kind.as_str()) {
            (Some((artist, title)), _) => println!("[{}] {} / {}", g.kind, artist, title),
            (None, _) => println!("[{}]", g.kind),
        }
        println!("  keep {}  {}", g.keep.path.display(), describe(&g.keep.score));
        for l in &g.losers {
            println!("  lose {}  {}", l.path.display(), describe(&l.score));
        }
    }
    if !groups.is_empty() {
        println!();
        println!("run with --execute to move the losers into .dedup-trash/ (or --delete to remove them).");
    }
}

fn progress_bar(len: u64) -> indicatif::ProgressBar {
    let pb = indicatif::ProgressBar::new(len);
    if let Ok(style) = indicatif::ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{bar:32.cyan/blue}] {pos}/{len} {msg}",
    ) {
        pb.set_style(style.progress_chars("\u{2588}\u{2592}\u{2591}"));
    }
    pb
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
        dir
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

    fn collect(root: &Path, keep: KeepMode) -> Vec<Group> {
        let mut cache = BTreeMap::new();
        let entries: Vec<FileEntry> = scan_audio(root)
            .iter()
            .filter_map(|p| measure(p, &mut cache))
            .collect();
        build_groups(entries, keep)
    }

    fn find<'a>(groups: &'a [Group], path: &Path) -> &'a Group {
        groups
            .iter()
            .find(|g| {
                g.keep.path == path || g.losers.iter().any(|l| l.path == path)
            })
            .unwrap()
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

        let groups = collect(&root, KeepMode::Best);
        let plan = Plan {
            root: root.clone(),
            groups,
            cache: BTreeMap::new(),
        };
        // trash mode
        let code = run_execute(&plan, None, false);
        assert_eq!(code, 0);
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

        // delete mode on a fresh library
        let root2 = tmpdir("exec-del");
        write_tagged(&root2.join("a.mp3"), "A", "T", 100);
        write_tagged(&root2.join("b.mp3"), "A", "T", 101);
        let groups = collect(&root2, KeepMode::Best);
        let plan2 = Plan {
            root: root2.clone(),
            groups,
            cache: BTreeMap::new(),
        };
        let code = run_execute(&plan2, None, true);
        assert_eq!(code, 0);
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
        let groups = collect(&root, KeepMode::Best);
        // pretend the keeper was deleted after the analysis
        let plan = Plan {
            root: root.clone(),
            groups,
            cache: BTreeMap::new(),
        };
        for loser in &plan.groups[0].losers.clone() {
            let _ = fs::remove_file(&loser.path);
        }
        let before: Vec<PathBuf> = scan_audio(&root);
        let code = run_execute(&plan, None, true);
        assert_eq!(code, 0);
        let after: Vec<PathBuf> = scan_audio(&root);
        assert_eq!(before, after, "nothing must be deleted without the keeper");
        drop(plan);
        fs::remove_dir_all(&root).unwrap();
    }

    fn run_execute(plan: &Plan, trash: Option<&Path>, delete: bool) -> i32 {
        // mirror execute()'s per-group behavior against a borrowed plan
        let trash = trash
            .map(PathBuf::from)
            .unwrap_or_else(|| plan.root.join(".dedup-trash"));
        let mut failed = 0usize;
        for group in &plan.groups {
            if !group.keep.path.is_file() {
                continue;
            }
            for loser in &group.losers {
                if !loser.path.is_file() {
                    continue;
                }
                let res = if delete {
                    fs::remove_file(&loser.path).map(|_| PathBuf::new())
                } else {
                    move_to_trash(&loser.path, &trash, &group.kind)
                };
                if res.is_err() {
                    failed += 1;
                }
            }
        }
        i32::from(failed > 0)
    }
}
