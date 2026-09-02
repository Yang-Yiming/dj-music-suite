use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};

use crate::scan::{file_name, scan_audio};
use crate::tags::{self, TrackMeta};
use crate::template::{RenderValues, Template};

#[derive(Args)]
pub struct ReorgOpts {
    /// music library folder to reorganize
    #[arg(long, value_name = "DIR", required_unless_present = "from")]
    root: Option<PathBuf>,

    /// destination layout relative to the root; placeholders: {artist},
    /// {title}, {album}, {filename} (original name), {ext}
    #[arg(long, value_name = "TEMPLATE", default_value = "{artist}/{filename}.{ext}")]
    template: String,

    /// actually move the files (default: analyze only and write the plan)
    #[arg(long)]
    execute: bool,

    /// execute a previously generated plan json instead of analyzing again
    #[arg(long, value_name = "FILE")]
    from: Option<PathBuf>,

    /// also apply renames (rekordbox relocates files by filename, so renamed
    /// files have to be relinked by hand afterwards)
    #[arg(long)]
    allow_rename: bool,

    /// plan json written by the analysis (and read back with --from)
    #[arg(long, value_name = "FILE", default_value = "reorg-plan.json")]
    plan: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct PlanEntry {
    src: PathBuf,
    dst: Option<PathBuf>,
    /// "move", "rename" or "move+rename" ("none" for skipped entries)
    op: String,
    /// "ready", "in-place", "conflict" or "untagged"
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct DupeFile {
    path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    duration_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct DupeGroup {
    artist: String,
    title: String,
    files: Vec<DupeFile>,
}

#[derive(Serialize, Deserialize)]
struct Plan {
    root: PathBuf,
    template: String,
    entries: Vec<PlanEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    duplicates: Vec<DupeGroup>,
}

pub fn cmd_reorg(opts: ReorgOpts) -> i32 {
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

fn analyze(opts: ReorgOpts) -> i32 {
    let Some(root) = resolve_root(opts.root.as_deref()) else {
        return 2;
    };
    let template = match Template::parse(&opts.template) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bad --template: {e}");
            return 2;
        }
    };
    let plan = build_plan(&root, &template, &opts.template);
    print_report(&plan, &opts.plan);
    match serde_json::to_string_pretty(&plan) {
        Ok(json) => {
            if let Err(e) = fs::write(&opts.plan, json + "\n") {
                eprintln!("[warn] cannot write plan {}: {e}", opts.plan.display());
            }
        }
        Err(e) => eprintln!("[warn] cannot serialize plan: {e}"),
    }
    0
}

fn execute(opts: ReorgOpts) -> i32 {
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
        let template = match Template::parse(&opts.template) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("bad --template: {e}");
                return 2;
            }
        };
        build_plan(&root, &template, &opts.template)
    };

    let actionable: Vec<&PlanEntry> = plan
        .entries
        .iter()
        .filter(|e| e.status == "ready")
        .collect();
    let mut deferred_renames = 0usize;
    if !opts.allow_rename {
        deferred_renames = actionable.iter().filter(|e| e.op.contains("rename")).count();
    }

    let pb = ProgressBar::new(actionable.len() as u64);
    if let Ok(style) = ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{bar:32.cyan/blue}] {pos}/{len} {msg}",
    ) {
        pb.set_style(style.progress_chars("\u{2588}\u{2592}\u{2591}"));
    }

    let mut moved = 0usize;
    let mut renamed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for entry in &actionable {
        let is_rename = entry.op.contains("rename");
        if is_rename && !opts.allow_rename {
            pb.inc(1);
            continue;
        }
        let Some(dst) = entry.dst.as_deref() else {
            pb.inc(1);
            continue;
        };
        let src = entry.src.as_path();
        if !src.is_file() {
            pb.println(format!("[skip] source is gone: {}", src.display()));
            skipped += 1;
        } else if dst.exists() {
            pb.println(format!("[skip] target exists: {}", dst.display()));
            skipped += 1;
        } else if let Err(e) = dst
            .parent()
            .map(fs::create_dir_all)
            .unwrap_or_else(|| Ok(()))
            .and_then(|_| fs::rename(src, dst))
        {
            pb.println(format!("[fail] {}: {e}", src.display()));
            failed += 1;
        } else {
            moved += 1;
            if is_rename {
                renamed += 1;
            }
            pb.println(format!("[ok] {} -> {}", src.display(), dst.display()));
        }
        pb.inc(1);
    }
    pb.finish_and_clear();
    println!(
        "done: {moved} moved ({renamed} renamed), {skipped} skipped, \
         {deferred_renames} renames deferred, {failed} failed"
    );
    println!();
    println!("next step in rekordbox:");
    println!("  File -> Display All Missing Files -> select all -> Relocate ->");
    println!("  point it at {}", plan.root.display());
    if renamed > 0 {
        println!(
            "  rekordbox matches relocated files by filename: relink the {renamed}"
        );
        println!("  renamed file(s) by hand.");
    }
    if failed > 0 {
        1
    } else {
        0
    }
}

fn resolve_root(root: Option<&Path>) -> Option<PathBuf> {
    let Some(raw) = root else {
        eprintln!("--root is required (or pass --from <PLAN> together with --execute)");
        return None;
    };
    match fs::canonicalize(raw) {
        Ok(p) if p.is_dir() => Some(p),
        Ok(_) => {
            eprintln!("root is not a directory: {}", raw.display());
            None
        }
        Err(e) => {
            eprintln!("cannot resolve root {}: {e}", raw.display());
            None
        }
    }
}

fn build_plan(root: &Path, template: &Template, template_str: &str) -> Plan {
    let files = scan_audio(root);
    let pb = ProgressBar::new(files.len() as u64);
    if let Ok(style) = ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{bar:32.cyan/blue}] {pos}/{len} {msg}",
    ) {
        pb.set_style(style.progress_chars("\u{2588}\u{2592}\u{2591}"));
    }

    let mut entries = Vec::with_capacity(files.len());
    let mut dup_map: BTreeMap<(String, String), Vec<DupeFile>> = BTreeMap::new();
    for src in &files {
        pb.set_message(file_name(src));
        let meta = tags::read_meta(src);
        if let Some(meta) = &meta {
            if let Some((artist, title)) = tags::identity(meta) {
                dup_map.entry((artist, title)).or_default().push(DupeFile {
                    path: src.clone(),
                    duration_secs: meta.duration.map(|d| d.as_secs()),
                    size: fs::metadata(src).ok().map(|m| m.len()),
                });
            }
        }
        let entry = match render_target(root, template, src, meta.as_ref()) {
            Ok(dst) => classify(src, &dst),
            Err(reason) => PlanEntry {
                src: src.clone(),
                dst: None,
                op: "none".to_string(),
                status: "untagged".to_string(),
                note: Some(reason),
            },
        };
        entries.push(entry);
        pb.inc(1);
    }
    pb.finish_and_clear();

    let duplicates = dup_map
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|((artist, title), files)| DupeGroup { artist, title, files })
        .collect();
    Plan {
        root: root.to_path_buf(),
        template: template_str.to_string(),
        entries,
        duplicates,
    }
}

fn render_target(
    root: &Path,
    template: &Template,
    src: &Path,
    meta: Option<&TrackMeta>,
) -> Result<PathBuf, String> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let filename = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let vals = RenderValues {
        primary_artist: meta.and_then(|m| m.primary_artist.as_deref()),
        artist: meta.and_then(|m| m.artist.as_deref()),
        title: meta.and_then(|m| m.title.as_deref()),
        album: meta.and_then(|m| m.album.as_deref()),
        filename: &filename,
        ext: &ext,
    };
    let components = template.render(&vals)?;
    Ok(components
        .iter()
        .fold(root.to_path_buf(), |p, c| p.join(c)))
}

fn classify(src: &Path, dst: &Path) -> PlanEntry {
    if src == dst {
        return in_place(src, dst);
    }
    if dst.exists() {
        return PlanEntry {
            src: src.to_path_buf(),
            dst: Some(dst.to_path_buf()),
            op: "none".to_string(),
            status: "conflict".to_string(),
            note: Some("target already exists".to_string()),
        };
    }
    let same_dir = src.parent().zip(dst.parent()).map(|(a, b)| a == b).unwrap_or(false);
    let same_name = src.file_name() == dst.file_name();
    let op = match (same_dir, same_name) {
        (true, true) => {
            return in_place(src, dst);
        }
        (false, true) => "move",
        (true, false) => "rename",
        (false, false) => "move+rename",
    };
    PlanEntry {
        src: src.to_path_buf(),
        dst: Some(dst.to_path_buf()),
        op: op.to_string(),
        status: "ready".to_string(),
        note: None,
    }
}

fn in_place(src: &Path, dst: &Path) -> PlanEntry {
    PlanEntry {
        src: src.to_path_buf(),
        dst: Some(dst.to_path_buf()),
        op: "none".to_string(),
        status: "in-place".to_string(),
        note: None,
    }
}

fn print_report(plan: &Plan, plan_path: &Path) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &plan.entries {
        // ready entries are grouped by their op (move/rename/move+rename),
        // everything else by its status
        let key = if e.status == "ready" {
            e.op.as_str()
        } else {
            e.status.as_str()
        };
        *counts.entry(key).or_default() += 1;
    }
    println!("reorg analysis");
    println!("  root: {}", plan.root.display());
    println!("  template: {}", plan.template);
    println!();
    for (status, label) in [
        ("move", "move"),
        ("rename", "rename"),
        ("move+rename", "move+rename"),
        ("in-place", "in place"),
        ("conflict", "conflict"),
        ("untagged", "untagged"),
    ] {
        let n = counts.get(status).copied().unwrap_or(0);
        if n > 0 {
            println!("  {label:<12} {n}");
        }
    }
    if counts.is_empty() {
        println!("  (no audio files found)");
    }
    let renames = counts.get("rename").copied().unwrap_or(0)
        + counts.get("move+rename").copied().unwrap_or(0);
    if renames > 0 {
        println!();
        println!("  note: {renames} file(s) would be RENAMED. rekordbox relocates files by");
        println!("  filename, so renames are skipped unless --allow-rename (then they");
        println!("  must be relinked by hand in rekordbox).");
    }
    println!();
    println!("plan written to {}", plan_path.display());
    if !plan.duplicates.is_empty() {
        println!();
        println!("possible duplicates (same normalized artist + title):");
        for g in &plan.duplicates {
            println!("  {} / {} ({} files)", g.artist, g.title, g.files.len());
            for f in &g.files {
                let dur = f
                    .duration_secs
                    .map(|s| format!(" ({}:{:02})", s / 60, s % 60))
                    .unwrap_or_default();
                let size = f
                    .size
                    .map(|b| format!(", {:.1} MB", b as f64 / (1024.0 * 1024.0)))
                    .unwrap_or_default();
                println!("    - {}{}{}", f.path.display(), dur, size);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::Template;
    use id3::TagLike;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "djms-reorg-test-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_tagged(path: &Path, artist: &str, title: Option<&str>) {
        // ten 417-byte MPEG1 Layer3 frames (128 kbps / 44.1 kHz) so lofty can
        // parse the audio stream; the tag itself carries the metadata.
        let frame = [0xFFu8, 0xFB, 0x90, 0x00]
            .into_iter()
            .chain(std::iter::repeat(0u8).take(413))
            .collect::<Vec<u8>>();
        let audio = frame.repeat(10);
        fs::write(path, &audio).unwrap();
        let mut tag = id3::Tag::new();
        tag.set_artist(artist);
        if let Some(title) = title {
            tag.set_title(title);
        }
        tag.write_to_path(path, id3::Version::Id3v24).unwrap();
    }

    #[test]
    fn untagged_file_is_flagged() {
        let root = tmpdir("untagged");
        fs::write(root.join("song one.mp3"), b"junk").unwrap();
        let template = Template::parse("{artist}/{filename}.{ext}").unwrap();
        let plan = build_plan(&root, &template, "{artist}/{filename}.{ext}");
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].status, "untagged");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tagged_file_is_classified_as_move() {
        let root = tmpdir("tagged");
        write_tagged(&root.join("song.mp3"), "Artist", None);
        let template = Template::parse("{artist}/{filename}.{ext}").unwrap();
        let plan = build_plan(&root, &template, "x");
        assert_eq!(plan.entries[0].status, "ready");
        assert_eq!(plan.entries[0].op, "move");
        let dst = plan.entries[0].dst.clone().unwrap();
        assert_eq!(dst, root.join("Artist/song.mp3"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn occupied_target_is_conflict_and_same_path_is_in_place() {
        let root = tmpdir("conflict");
        fs::create_dir_all(root.join("Artist")).unwrap();
        write_tagged(&root.join("Artist/song.mp3"), "Artist", None);
        write_tagged(&root.join("song.mp3"), "Artist", None);
        let template = Template::parse("{artist}/{filename}.{ext}").unwrap();
        let plan = build_plan(&root, &template, "x");
        let statuses: Vec<&str> = plan.entries.iter().map(|e| e.status.as_str()).collect();
        assert!(statuses.contains(&"conflict"));
        assert!(statuses.contains(&"in-place"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rename_template_marks_renames() {
        let root = tmpdir("rename");
        fs::create_dir_all(root.join("Artist")).unwrap();
        write_tagged(&root.join("Artist/song.mp3"), "Artist", Some("Title"));
        let template = Template::parse("{artist}/{artist} - {title}.{ext}").unwrap();
        let plan = build_plan(&root, &template, "x");
        assert_eq!(plan.entries[0].op, "rename");
        assert_eq!(plan.entries[0].status, "ready");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn duplicate_identity_is_reported() {
        let root = tmpdir("dupe");
        for name in ["a.mp3", "b.mp3"] {
            write_tagged(&root.join(name), "Artist", Some("Same Song"));
        }
        let template = Template::parse("{artist}/{filename}.{ext}").unwrap();
        let plan = build_plan(&root, &template, "x");
        assert_eq!(plan.duplicates.len(), 1);
        assert_eq!(plan.duplicates[0].files.len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }
}
