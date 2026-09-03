mod convert;
mod dedup;
mod import;
mod quality;
mod reorg;
mod scan;
mod tags;
mod template;

use std::env;
use std::ffi::OsString;

use clap::{CommandFactory, Parser, Subcommand};

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
    Convert(convert::ConvertOpts),
    /// copy new audio files into the music library with duplicate detection
    Import(import::ImportOpts),
    /// reorganize a music library folder on disk (analyze by default)
    Reorg(reorg::ReorgOpts),
    /// find and remove duplicate files in a music library (analyze by default)
    Dedup(dedup::DedupOpts),
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
    let code = match cli.command {
        Some(Commands::Convert(opts)) => convert::cmd_convert(opts),
        Some(Commands::Import(opts)) => import::cmd_import(opts),
        Some(Commands::Reorg(opts)) => reorg::cmd_reorg(opts),
        Some(Commands::Dedup(opts)) => dedup::cmd_dedup(opts),
        None => {
            let _ = Cli::command().print_help();
            0
        }
    };
    std::process::exit(code);
}
