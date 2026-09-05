use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::scan::{file_name, scan_audio};
use crate::tags;
use crate::template::{RenderValues, Template};
use crate::{usage, Event, Error, Result, Sink};

/// same artist + title with durations within this delta counts as a duplicate
const DUPLICATE_DELTA_SECS: u64 = 3;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Copy,
    Move,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    /// not in the library yet
    New,
    /// same artist + title but a different duration: likely another mix,
    /// imported anyway
    AltVersion,
    /// same artist + title and a similar duration, skipped
    Duplicate,
    /// destination file already exists, skipped
    Conflict,
    /// missing a tag the template needs, skipped
    Untagged,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ImportItem {
    pub src: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dst: Option<PathBuf>,
    /// for duplicates: the matched library file that overwrite would
    /// replace (content refreshed in place, path unchanged)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<PathBuf>,
    pub disposition: Disposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Serializable result of analyzing an import batch: enough to render a
/// preview (web UI) and to drive [`execute`] afterwards.
#[derive(Clone, Serialize, Deserialize)]
pub struct ImportPlan {
    pub input: PathBuf,
    pub root: PathBuf,
    pub template: String,
    pub items: Vec<ImportItem>,
}

/// Outcome of executing an import plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportSummary {
    pub placed: usize,
    pub failed: usize,
}

struct LibEntry {
    path: PathBuf,
    duration_secs: Option<u64>,
}

type Index = BTreeMap<(String, String), Vec<LibEntry>>;

/// Index the library and classify every new file against it. The returned
/// plan is purely informational until [`execute`] is called on it. `root`
/// falls back to the configured library root when omitted.
pub fn analyze(
    input: &Path,
    root: Option<&Path>,
    template_str: &str,
    sink: Sink,
) -> Result<ImportPlan> {
    let input = match fs::canonicalize(input) {
        Ok(p) if p.is_dir() => p,
        Ok(_) => return Err(usage(format!("input is not a directory: {}", input.display()))),
        Err(e) => {
            return Err(usage(format!("cannot resolve input {}: {e}", input.display())))
        }
    };
    let root = config::resolve_library_root(root)?;
    let template =
        Template::parse(template_str).map_err(|e| usage(format!("bad --template: {e}")))?;

    // index the library by (artist, title) for duplicate detection
    let lib_files = scan_audio(&root);
    sink(&Event::Start(lib_files.len() as u64));
    let mut index = Index::new();
    for path in &lib_files {
        if let Some(meta) = tags::read_meta(path) {
            if let Some(identity) = tags::identity(&meta) {
                index
                    .entry(identity)
                    .or_default()
                    .push(LibEntry {
                        path: path.clone(),
                        duration_secs: meta.duration.map(|d| d.as_secs()),
                    });
            }
        }
        sink(&Event::Step(file_name(path)));
    }

    let files = scan_audio(&input);
    if files.is_empty() {
        return Err(Error::Runtime(format!(
            "no audio files found in {}",
            input.display()
        )));
    }
    sink(&Event::Start(files.len() as u64));
    let mut items = Vec::with_capacity(files.len());
    for src in &files {
        items.push(classify_item(src, &root, &template, &mut index));
        sink(&Event::Step(file_name(src)));
    }

    Ok(ImportPlan {
        input,
        root,
        template: template_str.to_string(),
        items,
    })
}

fn classify_item(src: &Path, root: &Path, template: &Template, index: &mut Index) -> ImportItem {
    let meta = tags::read_meta(src);
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
        primary_artist: meta.as_ref().and_then(|m| m.primary_artist.as_deref()),
        artist: meta.as_ref().and_then(|m| m.artist.as_deref()),
        title: meta.as_ref().and_then(|m| m.title.as_deref()),
        album: meta.as_ref().and_then(|m| m.album.as_deref()),
        filename: &filename,
        ext: &ext,
    };
    let components = match template.render(&vals) {
        Ok(c) => c,
        Err(reason) => {
            return ImportItem {
                src: src.to_path_buf(),
                dst: None,
                replace: None,
                disposition: Disposition::Untagged,
                note: Some(reason),
            };
        }
    };
    let dst = components
        .iter()
        .fold(root.to_path_buf(), |p, c| p.join(c));
    if dst.exists() {
        return ImportItem {
            src: src.to_path_buf(),
            dst: Some(dst),
            replace: None,
            disposition: Disposition::Conflict,
            note: Some("target already exists (imported before?)".to_string()),
        };
    }

    if let Some(meta) = &meta {
        if let Some(identity) = tags::identity(meta) {
            let dur = meta.duration.map(|d| d.as_secs());
            if let Some(entries) = index.get(&identity) {
                let fmt_dur = |s: Option<u64>| {
                    s.map(|s| format!("{}:{:02}", s / 60, s % 60))
                        .unwrap_or_else(|| "?".to_string())
                };
                return match entries.iter().find(|e| close(e.duration_secs, dur)) {
                    Some(entry) => ImportItem {
                        src: src.to_path_buf(),
                        dst: Some(dst),
                        replace: Some(entry.path.clone()),
                        disposition: Disposition::Duplicate,
                        note: Some(format!(
                            "library already has {} ({})",
                            entry.path.display(),
                            fmt_dur(entry.duration_secs)
                        )),
                    },
                    None => {
                        let entry = &entries[0];
                        ImportItem {
                            src: src.to_path_buf(),
                            dst: Some(dst),
                            replace: None,
                            disposition: Disposition::AltVersion,
                            note: Some(format!(
                                "library has {} ({}), duration differs; \
                                 importing as alternate version",
                                entry.path.display(),
                                fmt_dur(entry.duration_secs)
                            )),
                        }
                    }
                };
            }
            // projected import: register so later files in the same batch
            // deduplicate against this one
            index.entry(identity).or_default().push(LibEntry {
                path: dst.clone(),
                duration_secs: dur,
            });
        }
    }

    ImportItem {
        src: src.to_path_buf(),
        dst: Some(dst),
        replace: None,
        disposition: Disposition::New,
        note: None,
    }
}

fn close(a: Option<u64>, b: Option<u64>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => x.abs_diff(y) <= DUPLICATE_DELTA_SECS,
        // unknown durations must not mask a duplicate
        _ => true,
    }
}

/// Place the plan's actionable files into the library. Replacements
/// (duplicates/conflicts) only happen when `overwrite` is set; each placement
/// is atomic (tmp file + rename) so a failure never corrupts a library file.
pub fn execute(plan: &ImportPlan, mode: Mode, overwrite: bool, sink: Sink) -> ImportSummary {
    let actionable: Vec<&ImportItem> = plan
        .items
        .iter()
        .filter(|i| match i.disposition {
            Disposition::New | Disposition::AltVersion => true,
            Disposition::Conflict | Disposition::Duplicate => overwrite,
            Disposition::Untagged => false,
        })
        .collect();
    sink(&Event::Start(actionable.len() as u64));
    let verb = match mode {
        Mode::Copy => "copied",
        Mode::Move => "moved",
    };

    let mut placed = 0usize;
    let mut failed = 0usize;
    for item in &actionable {
        let target = match item.disposition {
            // replacements land at the matched library file's path
            Disposition::Duplicate => item.replace.as_deref(),
            _ => item.dst.as_deref(),
        };
        let Some(target) = target else {
            sink(&Event::Step(String::new()));
            continue;
        };
        if item.src == target {
            sink(&Event::Line(format!(
                "[skip] {} is already the library file",
                item.src.display()
            )));
            sink(&Event::Step(String::new()));
            continue;
        }
        let result = target
            .parent()
            .map(fs::create_dir_all)
            .unwrap_or_else(|| Ok(()))
            .and_then(|_| atomic_place(&item.src, target));
        let placed_ok = result.is_ok();
        if let Err(e) = result {
            failed += 1;
            sink(&Event::Line(format!("[fail] {}: {e}", item.src.display())));
        }
        if placed_ok {
            if mode == Mode::Move {
                if let Err(e) = fs::remove_file(&item.src) {
                    failed += 1;
                    sink(&Event::Line(format!(
                        "[warn] copied but cannot remove {}: {e}",
                        item.src.display()
                    )));
                }
            }
            placed += 1;
            let what = if matches!(item.disposition, Disposition::Conflict | Disposition::Duplicate)
            {
                "replaced"
            } else {
                verb
            };
            sink(&Event::Line(format!(
                "[ok] {} {} -> {}",
                what,
                item.src.display(),
                target.display()
            )));
        }
        sink(&Event::Step(file_name(&item.src)));
    }
    ImportSummary { placed, failed }
}

/// Copy `src` onto `dst` atomically: write a hidden temp file next to `dst`,
/// then rename over it, so a failed copy never corrupts an existing library
/// file (this includes overwrite placements).
fn atomic_place(src: &Path, dst: &Path) -> std::io::Result<()> {
    let tmp = dst.with_file_name(format!(".djms-tmp-{}-{}", std::process::id(), file_name(dst)));
    let result = fs::copy(src, &tmp).and_then(|_| fs::rename(&tmp, dst));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::Template;
    use id3::TagLike;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "djms-import-test-{tag}-{}",
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

    fn setup(tag: &str) -> (PathBuf, PathBuf, Template, Index) {
        let lib = tmpdir(&format!("{tag}-lib"));
        let staging = tmpdir(&format!("{tag}-in"));
        write_tagged(&lib.join("exists.mp3"), "A", "T", 100);
        let mut index = Index::new();
        index
            .entry(("a".into(), "t".into()))
            .or_default()
            .push(LibEntry {
                path: lib.join("exists.mp3"),
                duration_secs: Some(2),
            });
        (lib, staging, Template::parse("{filename}.{ext}").unwrap(), index)
    }

    fn no_sink(_: &Event) {}

    fn place_item(
        item: &ImportItem,
        input: &Path,
        root: &Path,
        mode: Mode,
        overwrite: bool,
    ) -> ImportSummary {
        let plan = ImportPlan {
            input: input.to_path_buf(),
            root: root.to_path_buf(),
            template: "{filename}.{ext}".to_string(),
            items: vec![item.clone()],
        };
        execute(&plan, mode, overwrite, &no_sink)
    }

    #[test]
    fn new_duplicate_and_conflict() {
        let (lib, staging, template, mut index) = setup("ndc");
        // same identity + same duration -> duplicate
        write_tagged(&staging.join("same.mp3"), "A", "T", 100);
        // different title -> new
        write_tagged(&staging.join("other.mp3"), "A", "T2", 100);
        // same filename as the library file -> conflict
        write_tagged(&staging.join("exists.mp3"), "A", "T", 100);

        let files = scan_audio(&staging);
        let items: Vec<ImportItem> = files
            .iter()
            .map(|f| classify_item(f, &lib, &template, &mut index))
            .collect();
        let d = |name: &str| {
            items
                .iter()
                .find(|i| i.src.file_name().unwrap() == name)
                .unwrap()
                .disposition
        };
        assert_eq!(d("same.mp3"), Disposition::Duplicate);
        assert_eq!(d("other.mp3"), Disposition::New);
        assert_eq!(d("exists.mp3"), Disposition::Conflict);
    }

    #[test]
    fn different_duration_is_alt_version() {
        let (lib, staging, template, mut index) = setup("alt");
        write_tagged(&staging.join("extended-mix.mp3"), "A", "T", 300);
        let item = classify_item(&staging.join("extended-mix.mp3"), &lib, &template, &mut index);
        assert_eq!(item.disposition, Disposition::AltVersion);
        assert!(item.note.as_deref().unwrap().contains("alternate version"));
    }

    #[test]
    fn untagged_is_skipped() {
        let (lib, staging, _template, mut index) = setup("untag");
        // this template needs the artist tag, which the junk file lacks
        let template = Template::parse("{artist}/{filename}.{ext}").unwrap();
        fs::write(staging.join("junk.mp3"), b"junk").unwrap();
        let item = classify_item(&staging.join("junk.mp3"), &lib, &template, &mut index);
        assert_eq!(item.disposition, Disposition::Untagged);
    }

    #[test]
    fn batch_internal_duplicates_are_caught() {
        let (lib, staging, template, mut index) = setup("batch");
        // a fresh artist: the first file is New, the second one dedupes
        // against the first's projected import
        write_tagged(&staging.join("a-copy.mp3"), "Z", "T", 100);
        write_tagged(&staging.join("b-copy.mp3"), "Z", "T", 100);
        let files = scan_audio(&staging);
        let items: Vec<ImportItem> = files
            .iter()
            .map(|f| classify_item(f, &lib, &template, &mut index))
            .collect();
        let kinds: Vec<Disposition> = items.iter().map(|i| i.disposition).collect();
        assert_eq!(kinds, vec![Disposition::New, Disposition::Duplicate]);
    }

    #[test]
    fn move_execution_then_rerun_conflicts() {
        let (lib, staging, template, mut index) = setup("exec");
        write_tagged(&staging.join("fresh.mp3"), "B", "T", 100);
        let item = classify_item(&staging.join("fresh.mp3"), &lib, &template, &mut index);
        assert_eq!(item.disposition, Disposition::New);
        let summary = place_item(&item, &staging, &lib, Mode::Move, false);
        assert_eq!(summary, ImportSummary { placed: 1, failed: 0 });

        let dst = item.dst.clone().unwrap();
        assert!(dst.is_file());
        assert!(!staging.join("fresh.mp3").exists());

        // re-importing the same content now hits the conflict branch
        let files = scan_audio(&lib);
        let rerun = files
            .iter()
            .map(|f| classify_item(f, &lib, &template, &mut index))
            .find(|i| i.src == dst)
            .unwrap();
        assert_eq!(rerun.disposition, Disposition::Conflict);
    }

    #[test]
    fn conflict_overwrite_replaces_target() {
        let (lib, staging, template, mut index) = setup("ovr-conflict");
        write_tagged(&staging.join("exists.mp3"), "A", "T", 300);
        let item = classify_item(&staging.join("exists.mp3"), &lib, &template, &mut index);
        assert_eq!(item.disposition, Disposition::Conflict);

        let dst = item.dst.clone().unwrap();
        let summary = place_item(&item, &staging, &lib, Mode::Copy, true);
        assert_eq!(summary, ImportSummary { placed: 1, failed: 0 });
        // the library file now carries the incoming content, byte for byte
        assert_eq!(
            fs::read(&dst).unwrap(),
            fs::read(staging.join("exists.mp3")).unwrap()
        );
        // copy mode: the source is untouched
        assert!(staging.join("exists.mp3").is_file());
    }

    #[test]
    fn duplicate_overwrite_replaces_matched_library_file() {
        let (lib, staging, template, mut index) = setup("ovr-dup");
        // 101 frames -> same duration bucket as the 100-frame library file,
        // but a different size so the replacement is observable
        write_tagged(&staging.join("renamed.mp3"), "A", "T", 101);
        let item = classify_item(&staging.join("renamed.mp3"), &lib, &template, &mut index);
        assert_eq!(item.disposition, Disposition::Duplicate);
        let matched = item.replace.clone().unwrap();
        assert_eq!(matched, lib.join("exists.mp3"));
        let incoming = fs::read(staging.join("renamed.mp3")).unwrap();

        let summary = place_item(&item, &staging, &lib, Mode::Move, true);
        assert_eq!(summary, ImportSummary { placed: 1, failed: 0 });
        // the library keeps its path but carries the incoming content
        assert_eq!(fs::read(&matched).unwrap(), incoming);
        // move mode + replacement: the incoming file is consumed, and no
        // extra copy under its own name was created
        assert!(!staging.join("renamed.mp3").exists());
        assert_eq!(scan_audio(&lib).len(), 1);
    }

    #[test]
    fn self_import_is_skipped_not_corrupted() {
        let (lib, staging, template, mut index) = setup("self");
        // import the library file itself (e.g. --input inside --root):
        // conflict, and overwriting it must be refused
        let item = classify_item(&lib.join("exists.mp3"), &lib, &template, &mut index);
        assert_eq!(item.disposition, Disposition::Conflict);
        let before = fs::read(lib.join("exists.mp3")).unwrap();
        let summary = place_item(&item, &staging, &lib, Mode::Copy, true);
        assert_eq!(summary, ImportSummary { placed: 0, failed: 0 });
        assert_eq!(fs::read(lib.join("exists.mp3")).unwrap(), before);
    }

    #[test]
    fn plan_serialization_round_trips() {
        let (lib, staging, template, mut index) = setup("serde");
        write_tagged(&staging.join("fresh.mp3"), "B", "T", 100);
        write_tagged(&staging.join("same.mp3"), "A", "T", 100);
        let files = scan_audio(&staging);
        let items: Vec<ImportItem> = files
            .iter()
            .map(|f| classify_item(f, &lib, &template, &mut index))
            .collect();
        let plan = ImportPlan {
            input: staging.clone(),
            root: lib.clone(),
            template: "{filename}.{ext}".to_string(),
            items,
        };
        let json = serde_json::to_string(&plan).unwrap();
        let back: ImportPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.items.len(), plan.items.len());
        assert_eq!(back.items[0].disposition, plan.items[0].disposition);
        // and the round-tripped plan is executable
        let summary = execute(&back, Mode::Copy, false, &no_sink);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.placed, 1);
    }
}
