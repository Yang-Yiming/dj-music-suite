use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};

use dj_music_core::convert::ConvertOpts;
use dj_music_core::cover as core_cover;
use dj_music_core::import::{self as core_import, Mode as CoreMode};
use dj_music_core::transcode as core_transcode;
use dj_music_core::{dedup as core_dedup, reorg as core_reorg, trash as core_trash, Event};

#[derive(Parser)]
#[command(
    name = "dj-music-suite",
    version,
    about = "a suite of small tools for music files",
    long_about = "dj-music-suite - a suite of small tools for music files\n\nDecryption is built in (ncm_core), no external binaries are needed.\nConverted files are tagged automatically when possible: title, artist and\nalbum come from the .ncm metadata; the cover comes from\n<meta-dir>/track-<musicId>.jpg, then the image embedded in the .ncm file, then\nthe albumPic URL (unless --no-download; downloads are cached into <meta-dir>);\na same-named .lrc file is embedded as unsynced lyrics (USLT).\n\n`import` copies (or moves) new audio files into the music library folder,\ndeduplicating against what is already there: same artist + title and a\nsimilar duration is skipped as a duplicate, a different duration is imported\nas an alternate version.\n\n`reorg` moves the files of a music library folder into a tag-based layout.\nIt only touches the filesystem: analyze first, then let rekordbox relink the\nmoved files via File -> Display All Missing Files -> Relocate.\n\n`transcode` re-encodes audio files into another format and is the only\ncommand that needs an external binary: ffmpeg is looked up on $PATH (or\npassed as --ffmpeg) and the target encoder is checked before anything runs.\nTags, cover art and lyrics are carried over; files that already are the\ntarget codec are skipped rather than re-encoded, and a source that would be\noverwritten by its own conversion is refused unless --replace moves it into\nthe trash folder first.\n\n`cover` finds missing album artwork by searching MusicBrainz/Cover Art Archive and falling back to iTunes. It only sends text queries (artist, title, album); audio files are never uploaded. Analysis writes a reviewable plan and only --execute embeds high-confidence matches into mp3/flac/m4a files."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// batch convert .ncm files and embed cover art + lyrics
    Convert(ConvertArgs),
    /// transcode audio files into another format with ffmpeg
    Transcode(TranscodeArgs),
    /// find and embed missing album artwork (analyze by default)
    Cover(CoverArgs),
    /// copy new audio files into the music library with duplicate detection
    Import(ImportArgs),
    /// reorganize a music library folder on disk (analyze by default)
    Reorg(ReorgArgs),
    /// find and remove duplicate files in a music library (analyze by default)
    Dedup(DedupArgs),
    /// inspect, restore or empty the dedup trash folder
    Trash(TrashArgs),
}

#[derive(clap::Args)]
struct ConvertArgs {
    /// folder containing .ncm files
    #[arg(long, value_name = "DIR")]
    input: PathBuf,

    /// folder to write converted audio files to
    #[arg(long, value_name = "DIR")]
    output: PathBuf,

    /// number of parallel conversions
    #[arg(long, value_name = "N", default_value_t = 8)]
    threads: usize,

    /// folder with track-<musicId> covers (default: <input>/meta); downloaded
    /// covers are cached here
    #[arg(long, value_name = "DIR")]
    meta_dir: Option<PathBuf>,

    /// never fetch missing covers from the albumPic URL
    #[arg(long)]
    no_download: bool,
}

impl From<ConvertArgs> for ConvertOpts {
    fn from(a: ConvertArgs) -> Self {
        ConvertOpts {
            input: a.input,
            output: a.output,
            threads: a.threads,
            meta_dir: a.meta_dir,
            no_download: a.no_download,
        }
    }
}

#[derive(clap::Args)]
struct TranscodeArgs {
    /// target format
    #[arg(long, value_enum)]
    to: TargetArg,

    /// folder with the audio files to convert (scanned recursively,
    /// default: the library root configured in the web UI)
    #[arg(long, value_name = "DIR")]
    input: Option<PathBuf>,

    /// folder to write into (default: next to each source; the tree below
    /// --input is preserved, so same-named files never collide)
    #[arg(long, value_name = "DIR")]
    output: Option<PathBuf>,

    /// encoder quality: mp3 v0..v9 or e.g. 320k (default v0), m4a e.g. 256k
    /// (default), flac 0..12 (default 8); alac and wav take no quality
    #[arg(long, value_name = "Q")]
    quality: Option<String>,

    /// move each source into the trash folder after it converted (the only
    /// way a conversion that would overwrite its source is allowed)
    #[arg(long)]
    replace: bool,

    /// number of parallel conversions
    #[arg(long, value_name = "N", default_value_t = dj_music_core::transcode::DEFAULT_THREADS)]
    threads: usize,

    /// ffmpeg binary to use (default: `ffmpeg` from $PATH)
    #[arg(long, value_name = "FILE")]
    ffmpeg: Option<PathBuf>,

    /// trash folder for --replace (default: <input>/.dedup-trash)
    #[arg(long, value_name = "DIR")]
    trash: Option<PathBuf>,

    /// actually convert the files (default: analyze only and write the plan)
    #[arg(long)]
    execute: bool,

    /// execute a previously generated plan json instead of analyzing again
    #[arg(long, value_name = "FILE")]
    from: Option<PathBuf>,

    /// plan json written by the analysis (and read back with --from)
    #[arg(long, value_name = "FILE", default_value = "transcode-plan.json")]
    plan: PathBuf,
}

#[derive(Clone, Copy, ValueEnum)]
enum CoverProviderArg {
    Auto,
    Musicbrainz,
    Itunes,
}

impl From<CoverProviderArg> for core_cover::Provider {
    fn from(value: CoverProviderArg) -> Self {
        match value {
            CoverProviderArg::Auto => core_cover::Provider::Auto,
            CoverProviderArg::Musicbrainz => core_cover::Provider::MusicBrainz,
            CoverProviderArg::Itunes => core_cover::Provider::Itunes,
        }
    }
}

#[derive(clap::Args)]
struct CoverArgs {
    /// audio file or folder to inspect (default: the configured library root)
    #[arg(long, value_name = "FILE_OR_DIR")]
    input: Option<PathBuf>,

    /// cover search source; auto tries MusicBrainz first, then iTunes
    #[arg(long, value_enum, default_value_t = CoverProviderArg::Auto)]
    provider: CoverProviderArg,

    /// minimum score for an automatic high-confidence match
    #[arg(long, value_name = "0..1", default_value_t = core_cover::DEFAULT_MIN_CONFIDENCE)]
    min_confidence: f32,

    /// iTunes storefront country code
    #[arg(long, value_name = "CC", default_value = "US")]
    country: String,

    /// replace an existing front cover (default: skip files that have one)
    #[arg(long)]
    overwrite: bool,

    /// actually embed the selected covers (default: analyze only)
    #[arg(long)]
    execute: bool,

    /// execute a previously written plan instead of analyzing again
    #[arg(long, value_name = "FILE")]
    from: Option<PathBuf>,

    /// plan json written by the analysis (and read back with --from)
    #[arg(long, value_name = "FILE", default_value = "cover-plan.json")]
    plan: PathBuf,
}

#[derive(Clone, Copy, ValueEnum)]
enum TargetArg {
    Mp3,
    Flac,
    M4a,
    Alac,
    Wav,
}

impl From<TargetArg> for core_transcode::TargetFormat {
    fn from(t: TargetArg) -> Self {
        match t {
            TargetArg::Mp3 => core_transcode::TargetFormat::Mp3,
            TargetArg::Flac => core_transcode::TargetFormat::Flac,
            TargetArg::M4a => core_transcode::TargetFormat::M4a,
            TargetArg::Alac => core_transcode::TargetFormat::Alac,
            TargetArg::Wav => core_transcode::TargetFormat::Wav,
        }
    }
}

#[derive(clap::Args)]
struct ImportArgs {
    /// folder with new audio files to import (scanned recursively)
    #[arg(long, value_name = "DIR")]
    input: PathBuf,

    /// music library root to import into (default: the library root
    /// configured in the web UI)
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// destination layout relative to the root; placeholders: {artist},
    /// {title}, {album}, {filename} (original name), {ext}
    #[arg(long, value_name = "TEMPLATE", default_value = "{artist}/{filename}.{ext}")]
    template: String,

    /// move files into the library instead of copying
    #[arg(long, value_enum, default_value_t = ModeArg::Copy)]
    mode: ModeArg,

    /// on duplicate/conflict, replace the existing library file with the
    /// incoming one instead of skipping (atomic tmp+rename)
    #[arg(long)]
    overwrite: bool,

    /// actually place the files (default: report only)
    #[arg(long)]
    execute: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Copy,
    Move,
}

impl From<ModeArg> for CoreMode {
    fn from(m: ModeArg) -> Self {
        match m {
            ModeArg::Copy => CoreMode::Copy,
            ModeArg::Move => CoreMode::Move,
        }
    }
}

#[derive(clap::Args)]
struct ReorgArgs {
    /// music library folder to reorganize (default: the library root
    /// configured in the web UI)
    #[arg(long, value_name = "DIR")]
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

#[derive(clap::Args)]
struct DedupArgs {
    /// music library folder to deduplicate (default: the library root
    /// configured in the web UI)
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// actually move/delete the duplicate files (default: analyze only and
    /// write the plan)
    #[arg(long)]
    execute: bool,

    /// execute a previously generated plan json instead of analyzing again
    #[arg(long, value_name = "FILE")]
    from: Option<PathBuf>,

    /// which copy to keep in each group: best scores format, bitrate,
    /// sample rate, cover art, lyrics and tag completeness; first keeps the
    /// first file in scan order
    #[arg(long, value_enum, default_value_t = KeepModeArg::Best)]
    keep: KeepModeArg,

    /// folder to move duplicates into (default: <root>/.dedup-trash)
    #[arg(long, value_name = "DIR")]
    trash: Option<PathBuf>,

    /// delete duplicates outright instead of moving them into the trash
    /// folder (irreversible)
    #[arg(long)]
    delete: bool,

    /// plan json written by the analysis (and read back with --from)
    #[arg(long, value_name = "FILE", default_value = "dedup-plan.json")]
    plan: PathBuf,
}

#[derive(Clone, Copy, ValueEnum)]
enum KeepModeArg {
    Best,
    First,
}

#[derive(clap::Args)]
struct TrashArgs {
    /// music library folder whose trash folder to operate on (default: the
    /// library root configured in the web UI)
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// trash folder (default: <root>/.dedup-trash)
    #[arg(long, value_name = "DIR")]
    trash: Option<PathBuf>,

    /// put these entries back where they came from (entry names as printed by
    /// a plain `trash` run); occupied targets are skipped, never overwritten
    #[arg(long, value_name = "ENTRY", num_args = 1..)]
    restore: Vec<String>,

    /// permanently delete these entries (irreversible)
    #[arg(long, value_name = "ENTRY", num_args = 1..)]
    delete: Vec<String>,

    /// permanently delete everything in the trash folder (irreversible)
    #[arg(long)]
    empty: bool,
}

impl From<KeepModeArg> for core_dedup::KeepMode {
    fn from(k: KeepModeArg) -> Self {
        match k {
            KeepModeArg::Best => core_dedup::KeepMode::Best,
            KeepModeArg::First => core_dedup::KeepMode::First,
        }
    }
}

/// Renders core events as indicatif progress bars plus plain lines, keeping
/// the terminal output the CLI has always shown.
struct TerminalSink {
    bar: Mutex<Option<ProgressBar>>,
}

impl TerminalSink {
    fn sink(&self, event: &Event) {
        match event {
            Event::Start(total) => {
                let mut guard = self.bar.lock().unwrap();
                if let Some(prev) = guard.as_ref() {
                    prev.finish_and_clear();
                }
                let pb = ProgressBar::new(*total);
                if let Ok(style) = ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:32.cyan/blue}] {pos}/{len} ({eta}) {msg}",
                ) {
                    pb.set_style(style.progress_chars("\u{2588}\u{2592}\u{2591}"));
                }
                *guard = Some(pb);
            }
            Event::Step(msg) => {
                if let Some(pb) = self.bar.lock().unwrap().as_ref() {
                    pb.set_message(msg.clone());
                    pb.inc(1);
                }
            }
            Event::Line(text) => {
                let guard = self.bar.lock().unwrap();
                match guard.as_ref() {
                    Some(pb) => pb.println(text),
                    None => println!("{text}"),
                }
            }
            Event::Warn(text) => eprintln!("{text}"),
            // per-file progress: the bar is per file, so show the encoder's
            // position in the current one next to it
            Event::Tick(pct) => {
                if let Some(pb) = self.bar.lock().unwrap().as_ref() {
                    pb.set_message(format!("{} ({pct}%)", pb.message()));
                }
            }
        }
    }

    fn finish(&self) {
        if let Some(pb) = self.bar.lock().unwrap().as_ref() {
            pb.finish_and_clear();
        }
    }
}

fn main() {
    let raw: Vec<OsString> = env::args_os().skip(1).collect();
    if raw.is_empty() {
        let _ = Cli::command().print_help();
        println!();
        std::process::exit(0);
    }
    // Back-compat: bare flags (e.g. `dj-music-suite --input <DIR> ...`) still
    // run `convert`. Top-level -h/-V are handled by clap as before.
    let first = raw[0].to_string_lossy();
    let cli = if first == "-h" || first == "--help" || first == "-V" || first == "--version" {
        Cli::parse_from(std::iter::once(OsString::from("dj-music-suite")).chain(raw))
    } else if first.starts_with('-') {
        let mut argv = vec![OsString::from("dj-music-suite"), OsString::from("convert")];
        argv.extend(raw);
        Cli::parse_from(argv)
    } else {
        Cli::parse_from(std::iter::once(OsString::from("dj-music-suite")).chain(raw))
    };
    let sink = TerminalSink {
        bar: Mutex::new(None),
    };
    let code = match cli.command {
        Some(Commands::Convert(args)) => cmd_convert(args, &sink),
        Some(Commands::Transcode(args)) => cmd_transcode(args, &sink),
        Some(Commands::Cover(args)) => cmd_cover(args, &sink),
        Some(Commands::Import(args)) => cmd_import(args, &sink),
        Some(Commands::Reorg(args)) => cmd_reorg(args, &sink),
        Some(Commands::Dedup(args)) => cmd_dedup(args, &sink),
        Some(Commands::Trash(args)) => cmd_trash(args, &sink),
        None => {
            let _ = Cli::command().print_help();
            0
        }
    };
    std::process::exit(code);
}

/// Map a core error onto the CLI exit code convention: usage problems exit 2,
/// runtime problems exit 1.
fn error_code(e: dj_music_core::Error) -> i32 {
    eprintln!("{e}");
    match e {
        dj_music_core::Error::Usage(_) => 2,
        dj_music_core::Error::Runtime(_) => 1,
    }
}

fn cmd_convert(args: ConvertArgs, sink: &TerminalSink) -> i32 {
    let summary = match dj_music_core::convert::run(args.into(), &|e: &Event| sink.sink(e)) {
        Ok(s) => s,
        Err(e) => return error_code(e),
    };
    sink.finish();
    let ok = summary.total - summary.failed;
    println!(
        "done: {ok} converted, {} tagged, {} failed",
        summary.tagged, summary.failed
    );
    i32::from(summary.failed > 0)
}

fn cmd_transcode(args: TranscodeArgs, sink: &TerminalSink) -> i32 {
    if args.replace && args.output.is_some() {
        eprintln!("--replace only makes sense without --output (it moves the sources that were converted)");
        return 2;
    }
    if args.from.is_some() && !args.execute {
        eprintln!("--from only makes sense together with --execute");
        return 2;
    }
    let input = match args.input.clone() {
        Some(input) => input,
        // default: the library root, resolved exactly like reorg/dedup do
        None => match dj_music_core::config::resolve_library_root(None) {
            Ok(root) => root,
            Err(e) => return error_code(e),
        },
    };
    let opts = core_transcode::TranscodeOpts {
        input: input.clone(),
        output: args.output,
        target: args.to.into(),
        quality: args.quality,
        replace: args.replace,
        threads: args.threads,
        ffmpeg: args.ffmpeg,
        trash: args.trash,
        plan: Some(args.plan.clone()),
        from: args.from.clone(),
    };
    let sink_fn = |e: &Event| sink.sink(e);
    if !args.execute {
        match core_transcode::analyze(&opts, &sink_fn) {
            Ok(plan) => {
                sink.finish();
                print_transcode_report(&plan);
                0
            }
            Err(e) => error_code(e),
        }
    } else {
        match core_transcode::execute(&opts, &sink_fn) {
            Ok(summary) => {
                sink.finish();
                let before = summary.bytes_before as f64 / (1024.0 * 1024.0);
                let after = summary.bytes_after as f64 / (1024.0 * 1024.0);
                println!(
                    "done: {} converted ({before:.1} MB -> {after:.1} MB), {} skipped, {} failed",
                    summary.converted, summary.skipped, summary.failed
                );
                if summary.trashed > 0 {
                    println!(
                        "{} source(s) moved to {} - review with `trash`, then restore or delete",
                        summary.trashed,
                        core_trash::default_dir(&input).display()
                    );
                }
                println!();
                println!("next step in rekordbox: refresh the library root folder so the new");
                println!("files are picked up (replacing a file in place keeps its path).");
                i32::from(summary.failed > 0)
            }
            Err(e) => error_code(e),
        }
    }
}

fn cmd_cover(args: CoverArgs, sink: &TerminalSink) -> i32 {
    if args.from.is_some() && !args.execute {
        eprintln!("--from only makes sense together with --execute");
        return 2;
    }
    let sink_fn = |e: &Event| sink.sink(e);
    let plan = match args.from.as_deref() {
        Some(path) => match core_cover::load_plan(path) {
            Ok(plan) => plan,
            Err(e) => return error_code(e),
        },
        None => {
            let opts = core_cover::CoverOpts {
                input: args.input,
                provider: args.provider.into(),
                min_confidence: args.min_confidence,
                overwrite: args.overwrite,
                country: args.country,
                plan: Some(args.plan),
                cache_dir: None,
            };
            match core_cover::analyze(&opts, &sink_fn) {
                Ok(plan) => plan,
                Err(e) => return error_code(e),
            }
        }
    };
    sink.finish();
    print_cover_report(&plan);
    if !args.execute {
        return 0;
    }
    let summary = match core_cover::execute(&plan, &sink_fn) {
        Ok(summary) => summary,
        Err(e) => return error_code(e),
    };
    sink.finish();
    println!(
        "done: {} embedded, {} replaced, {} skipped, {} failed",
        summary.embedded, summary.replaced, summary.skipped, summary.failed
    );
    println!();
    println!("next step in rekordbox: refresh the affected tracks/album so the new");
    println!("artwork is read from the embedded tags.");
    i32::from(summary.failed > 0)
}

fn print_cover_report(plan: &core_cover::Plan) {
    println!("cover analysis");
    println!("  input: {}", plan.input.display());
    println!("  provider: {}", plan.provider.label());
    println!("  confidence: {:.2}", plan.min_confidence);
    if plan.overwrite {
        println!("  existing covers: replaced");
    } else {
        println!("  existing covers: skipped");
    }
    println!();
    for (status, label) in [
        (core_cover::Status::Ready, "ready to embed"),
        (core_cover::Status::Existing, "already have a cover"),
        (core_cover::Status::Review, "need manual review"),
        (core_cover::Status::NotFound, "not found"),
        (core_cover::Status::Unsupported, "format not supported"),
        (core_cover::Status::Error, "unreadable/error"),
    ] {
        let count = plan.count(status);
        if count > 0 {
            println!("  {label:<28} {count}");
        }
    }
    if plan.entries.is_empty() {
        println!("  (no audio files found)");
    }

    for entry in &plan.entries {
        match entry.status {
            core_cover::Status::Ready => {
                if let Some(candidate) = entry.selected_candidate() {
                    println!(
                        "  - {} -> {} / {} ({}, {:.2})",
                        dj_music_core::scan::file_name(&entry.path),
                        candidate.artist.as_deref().unwrap_or("?"),
                        candidate.album.as_deref().unwrap_or("?"),
                        candidate.provider.label(),
                        candidate.score
                    );
                }
            }
            core_cover::Status::Review => {
                let best = entry.candidates.first();
                let detail = best
                    .map(|candidate| {
                        format!(
                            "best: {} / {} ({}, {:.2})",
                            candidate.artist.as_deref().unwrap_or("?"),
                            candidate.album.as_deref().unwrap_or("?"),
                            candidate.provider.label(),
                            candidate.score
                        )
                    })
                    .or_else(|| entry.note.clone())
                    .unwrap_or_else(|| "ambiguous match".into());
                println!(
                    "  - {} [review] {detail}",
                    dj_music_core::scan::file_name(&entry.path)
                );
            }
            core_cover::Status::Unsupported | core_cover::Status::Error => {
                println!(
                    "  - {} [{}] {}",
                    dj_music_core::scan::file_name(&entry.path),
                    entry.status.as_str(),
                    entry.note.as_deref().unwrap_or("")
                );
            }
            core_cover::Status::Existing | core_cover::Status::NotFound => {}
        }
    }

    let ready = plan.actionable().len();
    if ready == 0 {
        println!();
        println!("nothing is ready to embed");
    } else {
        println!();
        println!("with --execute: {ready} file(s) would get a front cover");
    }
}


fn print_transcode_report(plan: &core_transcode::Plan) {
    let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
    let ready: Vec<&core_transcode::PlanEntry> = plan.actionable();
    let convertable: u64 = ready.iter().map(|e| e.size).sum();
    println!("transcode analysis");
    println!("  input: {}", plan.input.display());
    println!(
        "  target: {}{}",
        plan.target.label(),
        match &plan.quality {
            Some(q) => format!(" (quality {q})"),
            None => String::new(),
        }
    );
    println!(
        "  output: {}",
        match &plan.output {
            Some(out) => out.display().to_string(),
            None => "next to each source".to_string(),
        }
    );
    if plan.replace {
        println!("  sources: moved to {} after converting", plan.trash.display());
    }
    println!();
    for (status, label) in [
        ("ready", "convert"),
        ("same-codec", "already the target codec"),
        ("conflict", "conflict (target exists)"),
        ("needs-replace", "needs --replace (same file name)"),
    ] {
        let n = plan.count(status);
        if n > 0 {
            println!("  {label:<32} {n}");
        }
    }
    if plan.entries.is_empty() {
        println!("  (no audio files found)");
    }
    for entry in &plan.entries {
        if let Some(note) = &entry.note {
            let codec = entry.src_codec.as_deref().unwrap_or("unknown codec");
            println!(
                "  - {} [{codec}, {:.1} MB]: {note}",
                dj_music_core::scan::file_name(&entry.src),
                mb(entry.size)
            );
        }
    }
    if ready.is_empty() {
        println!();
        println!("nothing to convert");
    } else {
        println!();
        println!(
            "with --execute: {} file(s) ({:.1} MB of sources) would be converted",
            ready.len(),
            mb(convertable)
        );
        if plan.count("same-codec") > 0 {
            println!("files already in the target codec are skipped: re-encoding them");
            println!("only loses quality.");
        }
    }
}

fn cmd_import(args: ImportArgs, sink: &TerminalSink) -> i32 {
    let mode: CoreMode = args.mode.into();
    let sink_fn = |e: &Event| sink.sink(e);
    let plan = match core_import::analyze(&args.input, args.root.as_deref(), &args.template, &sink_fn)
    {
        Ok(p) => p,
        Err(e) => return error_code(e),
    };
    sink.finish();
    print_import_report(&plan, mode, args.overwrite);
    if args.execute {
        let summary = core_import::execute(&plan, mode, args.overwrite, &|e: &Event| sink.sink(e));
        sink.finish();
        println!("done: {} placed, {} failed", summary.placed, summary.failed);
        println!();
        println!("next step in rekordbox: import/refresh the library root folder -");
        println!("new tracks come in with tags and artwork already embedded.");
        i32::from(summary.failed > 0)
    } else {
        0
    }
}

fn cmd_reorg(args: ReorgArgs, sink: &TerminalSink) -> i32 {
    if args.from.is_some() && !args.execute {
        eprintln!("--from only makes sense together with --execute");
        return 2;
    }
    let opts = core_reorg::ReorgOpts {
        root: args.root,
        template: args.template,
        execute: args.execute,
        from: args.from,
        allow_rename: args.allow_rename,
        plan: Some(args.plan),
    };
    let sink_fn = |e: &Event| sink.sink(e);
    if !args.execute {
        match core_reorg::analyze(&opts, &sink_fn) {
            Ok(plan) => {
                sink.finish();
                print_reorg_report(&plan);
                0
            }
            Err(e) => error_code(e),
        }
    } else {
        match core_reorg::execute(&opts, &sink_fn) {
            Ok(summary) => {
                sink.finish();
                println!(
                    "done: {} moved ({} renamed), {} skipped, {} renames deferred, {} failed",
                    summary.moved, summary.renamed, summary.skipped, summary.deferred_renames,
                    summary.failed
                );
                println!();
                println!("next step in rekordbox:");
                println!("  File -> Display All Missing Files -> select all -> Relocate ->");
                println!("  point it at {}", summary.root.display());
                if summary.renamed > 0 {
                    println!(
                        "  rekordbox matches relocated files by filename: relink the {}",
                        summary.renamed
                    );
                    println!("  renamed file(s) by hand.");
                }
                i32::from(summary.failed > 0)
            }
            Err(e) => error_code(e),
        }
    }
}

fn print_reorg_report(plan: &core_reorg::Plan) {
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

fn cmd_dedup(args: DedupArgs, sink: &TerminalSink) -> i32 {
    if args.from.is_some() && !args.execute {
        eprintln!("--from only makes sense together with --execute");
        return 2;
    }
    let opts = core_dedup::DedupOpts {
        root: args.root,
        execute: args.execute,
        from: args.from,
        keep: args.keep.into(),
        trash: args.trash,
        delete: args.delete,
        plan: Some(args.plan),
    };
    let sink_fn = |e: &Event| sink.sink(e);
    if !args.execute {
        match core_dedup::analyze(&opts, &sink_fn) {
            Ok((plan, stats)) => {
                sink.finish();
                print_dedup_report(&plan.groups, stats.unreadable, stats.scanned);
                0
            }
            Err(e) => error_code(e),
        }
    } else {
        match core_dedup::execute(&opts, &sink_fn) {
            Ok(summary) => {
                sink.finish();
                let freed_mb = summary.freed_bytes as f64 / (1024.0 * 1024.0);
                println!(
                    "done: {} duplicates removed ({freed_mb:.1} MB freed), {} skipped, {} failed",
                    summary.removed, summary.skipped, summary.failed
                );
                if !args.delete && summary.removed > 0 {
                    let trash = core_trash::default_dir(&summary.root);
                    println!("duplicates were moved to {}", trash.display());
                    println!("review with `trash`, then restore or delete from there.");
                }
                i32::from(summary.failed > 0)
            }
            Err(e) => error_code(e),
        }
    }
}

fn cmd_trash(args: TrashArgs, sink: &TerminalSink) -> i32 {
    let actions = usize::from(args.empty) + usize::from(!args.restore.is_empty()) + usize::from(!args.delete.is_empty());
    if actions > 1 {
        eprintln!("--restore, --delete and --empty are mutually exclusive");
        return 2;
    }
    let root = match dj_music_core::config::resolve_library_root(args.root.as_deref()) {
        Ok(root) => root,
        Err(e) => return error_code(e),
    };
    let trash = args
        .trash
        .clone()
        .unwrap_or_else(|| core_trash::default_dir(&root));
    let sink_fn = |e: &Event| sink.sink(e);

    if args.empty {
        let summary = core_trash::clear(&trash, &sink_fn);
        sink.finish();
        let mb = summary.bytes as f64 / (1024.0 * 1024.0);
        println!(
            "done: {} files deleted ({mb:.1} MB freed), {} failed",
            summary.removed, summary.failed
        );
        return i32::from(summary.failed > 0);
    }
    if !args.restore.is_empty() || !args.delete.is_empty() {
        let (summary, verb) = if !args.restore.is_empty() {
            match core_trash::restore(&root, &trash, &args.restore, &sink_fn) {
                Ok(s) => (s, "restored"),
                Err(e) => return error_code(e),
            }
        } else {
            match core_trash::remove(&trash, &args.delete, &sink_fn) {
                Ok(s) => (s, "deleted"),
                Err(e) => return error_code(e),
            }
        };
        sink.finish();
        let mb = summary.bytes as f64 / (1024.0 * 1024.0);
        println!(
            "done: {} {verb} ({mb:.1} MB), {} skipped, {} failed",
            if verb == "restored" { summary.restored } else { summary.removed },
            summary.skipped,
            summary.failed
        );
        if summary.skipped > 0 {
            println!("skipped entries stay in {}", trash.display());
        }
        return i32::from(summary.failed > 0);
    }

    print_trash_list(&root, &trash);
    0
}

fn print_trash_list(root: &std::path::Path, trash: &std::path::Path) {
    let entries = core_trash::list(root, trash);
    println!("dedup trash");
    println!("  folder: {}", trash.display());
    if entries.is_empty() {
        println!();
        println!("  (empty)");
        return;
    }
    let total: u64 = entries.iter().map(|e| e.size).sum();
    println!(
        "  {} files, {:.1} MB",
        entries.len(),
        total as f64 / (1024.0 * 1024.0)
    );
    println!();
    for entry in &entries {
        let origin = match &entry.original {
            Some(path) => path.display().to_string(),
            None => "(unknown origin, can only be deleted)".to_string(),
        };
        let when = entry
            .moved_at
            .map(|t| {
                // days since the move, computed from the unix timestamp
                let days = core_trash::now().saturating_sub(t) / 86_400;
                match days {
                    0 => "today".to_string(),
                    1 => "yesterday".to_string(),
                    n => format!("{n} days ago"),
                }
            })
            .unwrap_or_default();
        println!("  {}", entry.rel);
        println!("    from: {origin}");
        if !when.is_empty() {
            println!("    moved: {when}");
        }
    }
    println!();
    println!("  --restore <ENTRY>...  put files back   --delete <ENTRY>...  delete permanently");
    println!("  --empty               delete everything");
}

fn describe(score: &core_dedup::ScoreDetail) -> String {
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

fn print_dedup_report(
    groups: &[core_dedup::Group],
    unreadable: usize,
    scanned: usize,
) {
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

fn print_import_report(plan: &core_import::ImportPlan, mode: CoreMode, overwrite: bool) {
    use core_import::Disposition;
    let items = &plan.items;
    let mut counts: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
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
    let (dup_hint, conflict_hint) = if overwrite {
        (
            "will replace the matched library file",
            "will overwrite the target file",
        )
    } else {
        (
            "skipped: same artist + title in library",
            "skipped: target exists",
        )
    };
    println!("import analysis");
    println!("  input: {}", plan.input.display());
    println!("  library: {}", plan.root.display());
    println!("  template: {}", plan.template);
    println!();
    for (key, label, hint) in [
        ("new", "new", ""),
        ("alt-version", "alt-version", "imported: same song, different duration"),
        ("duplicate", "duplicate", dup_hint),
        ("conflict", "conflict", conflict_hint),
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
                dj_music_core::scan::file_name(&item.src),
                item.note.as_deref().unwrap_or("")
            );
        }
    }
    let placed =
        counts.get("new").copied().unwrap_or(0) + counts.get("alt-version").copied().unwrap_or(0);
    let replaced = if overwrite {
        counts.get("duplicate").copied().unwrap_or(0) + counts.get("conflict").copied().unwrap_or(0)
    } else {
        0
    };
    let verb = match mode {
        CoreMode::Copy => "copied",
        CoreMode::Move => "moved",
    };
    let total = placed + replaced;
    if total == 0 {
        println!();
        println!("nothing to import");
    } else if replaced > 0 {
        println!();
        println!(
            "with --execute: {placed} file(s) and {replaced} replacement(s) would be {verb} into the library"
        );
    } else {
        println!();
        println!("with --execute: {total} file(s) would be {verb} into the library");
    }
}
