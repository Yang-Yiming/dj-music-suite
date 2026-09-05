//! `dj-music-suite-web`: the local web UI as its own binary.

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "dj-music-suite-web",
    version,
    about = "local web UI for dj-music-suite",
    long_about = "dj-music-suite-web - local web UI for dj-music-suite\n\nServes the dj-music-suite web interface on 127.0.0.1 and opens it in the\nbrowser. Files are uploaded into a local staging folder; nothing leaves the\nmachine. Set the music library root once on the page (stored in the app\nconfig dir), then convert .ncm files and import into the library with\nduplicate detection."
)]
struct ServeArgs {
    /// port to listen on (127.0.0.1 only)
    #[arg(long, value_name = "PORT", default_value_t = 8765)]
    port: u16,

    /// do not open the browser automatically
    #[arg(long)]
    no_open: bool,
}

fn main() {
    let args = ServeArgs::parse();
    match dj_music_server::serve(dj_music_server::ServeOpts {
        port: args.port,
        open_browser: !args.no_open,
    }) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
