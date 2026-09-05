use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Mutex;

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};

use dj_music_core::convert::ConvertOpts;
use dj_music_core::dedup::DedupOpts;
use dj_music_core::import::{self as core_import, Mode as CoreMode};
use dj_music_core::reorg::ReorgOpts;
use dj_music_core::{dedup as core_dedup, reorg as core_reorg, Event};

#[derive(Parser)]
#[command(
    name = "dj-music-suite",
    version,
    about = "a suite of small tools for music files",
    long_about = "dj-music-suite - a suite of small tools for music files\n\nDecryption is built in (ncm_core), no external binaries are needed.\nConverted files are tagged automatically when possible: title, artist and\nalbum come from the .ncm metadata; the cover comes from\n<meta-dir>/track-<musicId>.jpg, then the image embedded in the .ncm file, then\nthe albumPic URL (unless --no-download; downloads are cached into <meta-dir>);\na same-named .lrc file is embedded as unsynced lyrics (USLT).\n\n`import` copies (or moves) new audio files into the music library folder,\ndeduplicating against what is already there: same artist + title and a\nsimilar duration is skipped as a duplicate, a different duration is imported\nas an alternate version.\n\n`reorg` moves the files of a music library folder into a tag-based layout.\nIt only touches the filesystem: analyze first, then let rekordbox relink the\nmoved files via File -> Display All Missing Files -> Relocate."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// batch convert .ncm files and embed cover art + lyrics
    Convert(ConvertArgs),
    /// copy new audio files into the music library with duplicate detection
    Import(ImportArgs),
    /// reorganize a music library folder on disk (analyze by default)
    Reorg(ReorgArgs),
    /// find and remove duplicate files in a music library (analyze by default)
    Dedup(DedupArgs),
    /// start the local web UI in the browser
    Serve(ServeArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    /// port to listen on (127.0.0.1 only)
    #[arg(long, value_name = "PORT", default_value_t = 8765)]
    port: u16,

    /// do not open the browser automatically
    #[arg(long)]
    no_open: bool,
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
struct ImportArgs {
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

#[derive(clap::Args)]
struct DedupArgs {
    /// music library folder to deduplicate
    #[arg(long, value_name = "DIR", required_unless_present = "from")]
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
        Some(Commands::Import(args)) => cmd_import(args, &sink),
        Some(Commands::Reorg(args)) => cmd_reorg(args),
        Some(Commands::Dedup(args)) => cmd_dedup(args),
        Some(Commands::Serve(args)) => cmd_serve(args),
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

fn cmd_import(args: ImportArgs, sink: &TerminalSink) -> i32 {
    let mode: CoreMode = args.mode.into();
    let plan = match core_import::analyze(&args.input, &args.root, &args.template, &|e: &Event| sink.sink(e)) {
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

fn cmd_reorg(args: ReorgArgs) -> i32 {
    core_reorg::run(ReorgOpts {
        root: args.root,
        template: args.template,
        execute: args.execute,
        from: args.from,
        allow_rename: args.allow_rename,
        plan: args.plan,
    })
}

fn cmd_dedup(args: DedupArgs) -> i32 {
    core_dedup::run(DedupOpts {
        root: args.root,
        execute: args.execute,
        from: args.from,
        keep: args.keep.into(),
        trash: args.trash,
        delete: args.delete,
        plan: args.plan,
    })
}

fn cmd_serve(args: ServeArgs) -> i32 {
    match dj_music_server::serve(dj_music_server::ServeOpts {
        port: args.port,
        open_browser: !args.no_open,
    }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
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
