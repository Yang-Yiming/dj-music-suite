use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};

use crate::scan::{file_name, scan_audio};
use crate::tags;
use crate::template::{RenderValues, Template};

/// same artist + title with durations within this delta counts as a duplicate
const DUPLICATE_DELTA_SECS: u64 = 3;

#[derive(Args)]
pub struct ImportOpts {
    /// folder with new audio files to import (scanned recursively)
    #[arg(long, value_name = "DIR")]
    input: PathBuf,

    /// music library root to import into
    #[arg(long, value_name = "DIR")]
    root: PathBuf,

    /// destination layout relative to the root; placeholders: {artist},
    /// {title}, {album}, {filename} (original name), {ext}
    #[arg(long, value_name = "TEMPLATE", default_value = "{artist}/{filename}.{ext}")]
    template: String,

    /// move files into the library instead of copying
    #[arg(long, value_enum, default_value_t = Mode::Copy)]
    mode: Mode,

    /// actually place the files (default: report only)
    #[arg(long)]
    execute: bool,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Mode {
    Copy,
    Move,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Disposition {
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

struct Item {
    src: PathBuf,
    dst: Option<PathBuf>,
    disposition: Disposition,
    note: Option<String>,
}

struct LibEntry {
    path: PathBuf,
    duration_secs: Option<u64>,
}

type Index = BTreeMap<(String, String), Vec<LibEntry>>;

pub fn cmd_import(opts: ImportOpts) -> i32 {
    let input = match fs::canonicalize(&opts.input) {
        Ok(p) if p.is_dir() => p,
        Ok(_) => {
            eprintln!("input is not a directory: {}", opts.input.display());
            return 2;
        }
        Err(e) => {
            eprintln!("cannot resolve input {}: {e}", opts.input.display());
            return 2;
        }
    };
    let root = match fs::canonicalize(&opts.root) {
        Ok(p) if p.is_dir() => p,
        Ok(_) => {
            eprintln!("root is not a directory: {}", opts.root.display());
            return 2;
        }
        Err(e) => {
            eprintln!("cannot resolve root {}: {e}", opts.root.display());
            return 2;
        }
    };
    let template = match Template::parse(&opts.template) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("bad --template: {e}");
            return 2;
        }
    };

    // index the library by (artist, title) for duplicate detection
    let lib_files = scan_audio(&root);
    let pb = progress_bar(lib_files.len() as u64);
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
        pb.inc(1);
    }
    pb.finish_and_clear();

    let files = scan_audio(&input);
    if files.is_empty() {
        eprintln!("no audio files found in {}", input.display());
        return 1;
    }
    let pb = progress_bar(files.len() as u64);
    let mut items = Vec::with_capacity(files.len());
    for src in &files {
        pb.set_message(file_name(src));
        items.push(classify_item(src, &root, &template, &mut index));
        pb.inc(1);
    }
    pb.finish_and_clear();

    print_report(&items, &input, &root, &opts.template, opts.mode);
    if opts.execute {
        place(&items, opts.mode)
    } else {
        0
    }
}

fn classify_item(src: &Path, root: &Path, template: &Template, index: &mut Index) -> Item {
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
        artist: meta.as_ref().and_then(|m| m.artist.as_deref()),
        title: meta.as_ref().and_then(|m| m.title.as_deref()),
        album: meta.as_ref().and_then(|m| m.album.as_deref()),
        filename: &filename,
        ext: &ext,
    };
    let components = match template.render(&vals) {
        Ok(c) => c,
        Err(reason) => {
            return Item {
                src: src.to_path_buf(),
                dst: None,
                disposition: Disposition::Untagged,
                note: Some(reason),
            };
        }
    };
    let dst = components
        .iter()
        .fold(root.to_path_buf(), |p, c| p.join(c));
    if dst.exists() {
        return Item {
            src: src.to_path_buf(),
            dst: Some(dst),
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
                    Some(entry) => Item {
                        src: src.to_path_buf(),
                        dst: Some(dst),
                        disposition: Disposition::Duplicate,
                        note: Some(format!(
                            "library already has {} ({})",
                            entry.path.display(),
                            fmt_dur(entry.duration_secs)
                        )),
                    },
                    None => {
                        let entry = &entries[0];
                        Item {
                            src: src.to_path_buf(),
                            dst: Some(dst),
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

    Item {
        src: src.to_path_buf(),
        dst: Some(dst),
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

fn print_report(items: &[Item], input: &Path, root: &Path, template: &str, mode: Mode) {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for item in items {
        let key = match item.disposition {
            Disposition::New => "new",
            Disposition::AltVersion => "alt-version",
            Disposition::Duplicate => "duplicate",
            Disposition::Conflict => "conflict",
            Disposition::Untagged => "untagged",
        };
        *counts.entry(key).or_default() += 1;
    }
    println!("import analysis");
    println!("  input: {}", input.display());
    println!("  library: {}", root.display());
    println!("  template: {template}");
    println!();
    for (key, label, hint) in [
        ("new", "new", ""),
        ("alt-version", "alt-version", "imported: same song, different duration"),
        ("duplicate", "duplicate", "skipped: same artist + title in library"),
        ("conflict", "conflict", "skipped: target exists"),
        ("untagged", "untagged", "skipped: missing tags for template"),
    ] {
        let n = counts.get(key).copied().unwrap_or(0);
        if n > 0 {
            if hint.is_empty() {
                println!("  {label:<12} {n}");
            } else {
                println!("  {label:<12} {n}  ({hint})");
            }
        }
    }
    for item in items {
        let detail = matches!(
            item.disposition,
            Disposition::AltVersion | Disposition::Duplicate | Disposition::Conflict | Disposition::Untagged
        );
        if detail {
            println!(
                "  - {}: {}",
                file_name(&item.src),
                item.note.as_deref().unwrap_or("")
            );
        }
    }
    let n = counts.get("new").copied().unwrap_or(0)
        + counts.get("alt-version").copied().unwrap_or(0);
    let verb = match mode {
        Mode::Copy => "copied",
        Mode::Move => "moved",
    };
    if n == 0 {
        println!();
        println!("nothing to import");
    } else {
        println!();
        println!("with --execute: {n} file(s) would be {verb} into the library");
    }
}

fn place(items: &[Item], mode: Mode) -> i32 {
    let actionable: Vec<&Item> = items
        .iter()
        .filter(|i| matches!(i.disposition, Disposition::New | Disposition::AltVersion))
        .collect();
    let pb = progress_bar(actionable.len() as u64);
    let verb = match mode {
        Mode::Copy => "copied",
        Mode::Move => "moved",
    };

    let mut placed = 0usize;
    let mut failed = 0usize;
    for item in &actionable {
        let Some(dst) = item.dst.as_deref() else {
            pb.inc(1);
            continue;
        };
        let result = dst
            .parent()
            .map(fs::create_dir_all)
            .unwrap_or_else(|| Ok(()))
            .and_then(|_| match mode {
                Mode::Copy => fs::copy(&item.src, dst).map(|_| ()),
                Mode::Move => move_file(&item.src, dst),
            });
        match result {
            Ok(()) => {
                placed += 1;
                pb.println(format!(
                    "[ok] {verb} {} -> {}",
                    item.src.display(),
                    dst.display()
                ));
            }
            Err(e) => {
                failed += 1;
                pb.println(format!("[fail] {}: {e}", item.src.display()));
            }
        }
        pb.inc(1);
    }
    pb.finish_and_clear();
    println!("done: {placed} placed, {failed} failed");
    println!();
    println!("next step in rekordbox: import/refresh the library root folder -");
    println!("new tracks come in with tags and artwork already embedded.");
    if failed > 0 {
        1
    } else {
        0
    }
}

/// move across devices: rename fails with an error, fall back to copy + delete
fn move_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::rename(src, dst).or_else(|_| {
        fs::copy(src, dst).and_then(|_| fs::remove_file(src)).map(|_| ())
    })
}

fn progress_bar(len: u64) -> ProgressBar {
    let pb = ProgressBar::new(len);
    if let Ok(style) = ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{bar:32.cyan/blue}] {pos}/{len} {msg}",
    ) {
        pb.set_style(style.progress_chars("\u{2588}\u{2592}\u{2591}"));
    }
    pb
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
        let items: Vec<Item> = files
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
        let items: Vec<Item> = files
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
        assert_eq!(place(std::slice::from_ref(&item), Mode::Move), 0);

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
}
