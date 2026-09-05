use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::scan::{file_name, scan_audio};
use crate::tags::{self, TrackMeta};
use crate::template::{RenderValues, Template};
use crate::{usage, Event, Result, Sink};

pub struct ReorgOpts {
    pub root: Option<PathBuf>,
    pub template: String,
    pub execute: bool,
    pub from: Option<PathBuf>,
    pub allow_rename: bool,
    /// plan json written by the analysis (None: don't write a file, e.g. web)
    pub plan: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
pub struct PlanEntry {
    pub src: PathBuf,
    pub dst: Option<PathBuf>,
    /// "move", "rename" or "move+rename" ("none" for skipped entries)
    pub op: String,
    /// "ready", "in-place", "conflict" or "untagged"
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct DupeFile {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Serialize, Deserialize)]
pub struct DupeGroup {
    pub artist: String,
    pub title: String,
    pub files: Vec<DupeFile>,
}

#[derive(Serialize, Deserialize)]
pub struct Plan {
    pub root: PathBuf,
    pub template: String,
    pub entries: Vec<PlanEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duplicates: Vec<DupeGroup>,
}

/// Outcome of applying a reorg plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReorgSummary {
    pub root: PathBuf,
    pub moved: usize,
    pub renamed: usize,
    pub skipped: usize,
    /// rename entries not applied because allow_rename is off
    pub deferred_renames: usize,
    pub failed: usize,
}

/// Classify every audio file under the root and optionally write the plan
/// json for later `--from` execution.
pub fn analyze(opts: &ReorgOpts, sink: Sink) -> Result<Plan> {
    let root = config::resolve_library_root(opts.root.as_deref())?;
    let template = Template::parse(&opts.template).map_err(|e| usage(format!("bad --template: {e}")))?;
    let plan = build_plan(&root, &template, &opts.template, sink);
    if let Some(plan_path) = &opts.plan {
        write_plan(&plan, plan_path, sink);
    }
    Ok(plan)
}

/// Read back a plan json written by [`analyze`].
pub fn load_plan(path: &Path) -> Result<Plan> {
    let raw = fs::read_to_string(path)
        .map_err(|e| usage(format!("cannot read plan {}: {e}", path.display())))?;
    serde_json::from_str(&raw).map_err(|e| usage(format!("bad plan file {}: {e}", path.display())))
}

/// Apply a plan (freshly analyzed or loaded with [`load_plan`]). Only "ready"
/// entries move; renames need `allow_rename`.
pub fn execute(opts: &ReorgOpts, sink: Sink) -> Result<ReorgSummary> {
    let plan = if let Some(from) = &opts.from {
        load_plan(from)?
    } else {
        let root = config::resolve_library_root(opts.root.as_deref())?;
        let template =
            Template::parse(&opts.template).map_err(|e| usage(format!("bad --template: {e}")))?;
        build_plan(&root, &template, &opts.template, sink)
    };

    let actionable: Vec<&PlanEntry> = plan.entries.iter().filter(|e| e.status == "ready").collect();
    let mut deferred_renames = 0usize;
    if !opts.allow_rename {
        deferred_renames = actionable.iter().filter(|e| e.op.contains("rename")).count();
    }

    sink(&Event::Start(actionable.len() as u64));
    let mut moved = 0usize;
    let mut renamed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for entry in &actionable {
        let is_rename = entry.op.contains("rename");
        if is_rename && !opts.allow_rename {
            sink(&Event::Step(String::new()));
            continue;
        }
        let Some(dst) = entry.dst.as_deref() else {
            sink(&Event::Step(String::new()));
            continue;
        };
        let src = entry.src.as_path();
        if !src.is_file() {
            sink(&Event::Line(format!("[skip] source is gone: {}", src.display())));
            skipped += 1;
        } else if dst.exists() {
            sink(&Event::Line(format!("[skip] target exists: {}", dst.display())));
            skipped += 1;
        } else if let Err(e) = dst
            .parent()
            .map(fs::create_dir_all)
            .unwrap_or_else(|| Ok(()))
            .and_then(|_| fs::rename(src, dst))
        {
            sink(&Event::Line(format!("[fail] {}: {e}", src.display())));
            failed += 1;
        } else {
            moved += 1;
            if is_rename {
                renamed += 1;
            }
            sink(&Event::Line(format!("[ok] {} -> {}", src.display(), dst.display())));
        }
        sink(&Event::Step(file_name(src)));
    }
    Ok(ReorgSummary {
        root: plan.root,
        moved,
        renamed,
        skipped,
        deferred_renames,
        failed,
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

fn build_plan(root: &Path, template: &Template, template_str: &str, sink: Sink) -> Plan {
    let files = scan_audio(root);
    sink(&Event::Start(files.len() as u64));

    let mut entries = Vec::with_capacity(files.len());
    let mut dup_map: BTreeMap<(String, String), Vec<DupeFile>> = BTreeMap::new();
    for src in &files {
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
        sink(&Event::Step(file_name(src)));
    }

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
) -> std::result::Result<PathBuf, String> {
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
        // analyze() canonicalizes the root; match that so path comparisons
        // in the tests line up (/var -> /private/var on macOS)
        fs::canonicalize(&dir).unwrap_or(dir)
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

    fn no_sink(_: &Event) {}

    fn opts(root: &Path, template: &str) -> ReorgOpts {
        ReorgOpts {
            root: Some(root.to_path_buf()),
            template: template.to_string(),
            execute: false,
            from: None,
            allow_rename: false,
            plan: None,
        }
    }

    #[test]
    fn untagged_file_is_flagged() {
        let root = tmpdir("untagged");
        fs::write(root.join("song one.mp3"), b"junk").unwrap();
        let plan = analyze(&opts(&root, "{artist}/{filename}.{ext}"), &no_sink).unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].status, "untagged");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tagged_file_is_classified_as_move() {
        let root = tmpdir("tagged");
        write_tagged(&root.join("song.mp3"), "Artist", None);
        let plan = analyze(&opts(&root, "{artist}/{filename}.{ext}"), &no_sink).unwrap();
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
        let plan = analyze(&opts(&root, "{artist}/{filename}.{ext}"), &no_sink).unwrap();
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
        let plan = analyze(
            &opts(&root, "{artist}/{artist} - {title}.{ext}"),
            &no_sink,
        )
        .unwrap();
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
        let plan = analyze(&opts(&root, "{artist}/{filename}.{ext}"), &no_sink).unwrap();
        assert_eq!(plan.duplicates.len(), 1);
        assert_eq!(plan.duplicates[0].files.len(), 2);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn analyze_writes_plan_and_execute_from_plan_moves() {
        let root = tmpdir("roundtrip");
        write_tagged(&root.join("song.mp3"), "Artist", None);
        let plan_path = root.join(".plan.json");
        let mut o = opts(&root, "{artist}/{filename}.{ext}");
        o.plan = Some(plan_path.clone());
        analyze(&o, &no_sink).unwrap();
        assert!(plan_path.is_file());

        let mut exec = opts(&root, "{artist}/{filename}.{ext}");
        exec.execute = true;
        exec.from = Some(plan_path.clone());
        let summary = execute(&exec, &no_sink).unwrap();
        assert_eq!(summary.moved, 1);
        assert_eq!(summary.failed, 0);
        assert!(root.join("Artist/song.mp3").is_file());
        fs::remove_dir_all(&root).unwrap();
    }
}
