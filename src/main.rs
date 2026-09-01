use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use clap::{Args as ClapArgs, CommandFactory, Parser, Subcommand};
use id3::TagLike;
use indicatif::{ProgressBar, ProgressStyle};

#[derive(Parser)]
#[command(
    name = "dj-music-suite",
    version,
    about = "a suite of small tools for music files",
    long_about = "dj-music-suite - a suite of small tools for music files\n\nDecryption is built in (ncm_core), no external binaries are needed.\nConverted files are tagged automatically when possible: the cover comes from\n<meta-dir>/track-<musicId>.jpg, then the image embedded in the .ncm file, then\nthe albumPic URL (unless --no-download; downloads are cached into <meta-dir>);\na same-named .lrc file is embedded as unsynced lyrics (USLT)."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// batch convert .ncm files and embed cover art + lyrics
    Convert(ConvertOpts),
}

#[derive(ClapArgs)]
struct ConvertOpts {
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

fn is_ncm(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("ncm"))
        .unwrap_or(false)
}

fn collect_ncm_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.is_dir() {
        return Err(format!("input is not a directory: {}", dir.display()));
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_file() && is_ncm(&path) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

struct NcmMeta {
    music_id: String,
    album_pic: String,
}

fn parse_meta_json(meta_json: &[u8]) -> Option<NcmMeta> {
    let value: serde_json::Value = serde_json::from_slice(meta_json).ok()?;
    let music_id = value
        .get("musicId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let album_pic = value
        .get("albumPic")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if music_id.is_empty() && album_pic.is_empty() {
        return None;
    }
    Some(NcmMeta { music_id, album_pic })
}

fn format_lrc_time(ms: i64) -> String {
    let m = ms / 60_000;
    let s = (ms % 60_000) / 1000;
    let frac = ms % 1000;
    format!("[{m:02}:{s:02}.{frac:03}]")
}

fn normalize_lrc(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('{') {
            let parsed = serde_json::from_str::<serde_json::Value>(line).ok().and_then(|v| {
                let t = v.get("t").and_then(|t| t.as_i64())?;
                let mut text = String::new();
                if let Some(items) = v.get("c").and_then(|c| c.as_array()) {
                    for item in items {
                        if let Some(tx) = item.get("tx").and_then(|t| t.as_str()) {
                            text.push_str(tx);
                        }
                    }
                }
                Some((t, text))
            });
            if let Some((t, text)) = parsed {
                out.push_str(&format_lrc_time(t));
                out.push_str(&text);
                out.push('\n');
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn image_ext_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

fn sniff_mime(data: &[u8]) -> &'static str {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else {
        "application/octet-stream"
    }
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let Some(line_end) = rest.windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_str = String::from_utf8_lossy(&rest[..line_end]);
        let Ok(size) = usize::from_str_radix(size_str.trim().split(';').next().unwrap_or(""), 16)
        else {
            break;
        };
        rest = &rest[line_end + 2..];
        if size == 0 {
            break;
        }
        let take = size.min(rest.len());
        out.extend_from_slice(&rest[..take]);
        rest = &rest[take..];
        if rest.starts_with(b"\r\n") {
            rest = &rest[2..];
        }
        if take < size {
            break;
        }
    }
    out
}

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    use std::io::{Read, Write};
    // No TLS stack on purpose: the NetEase image CDN serves identical content
    // over plain HTTP, so https cover urls are simply downgraded.
    let mut url = url.replacen("https://", "http://", 1);
    for _ in 0..3 {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("unsupported cover url: {url}"))?;
        let (authority, path) = match rest.split_once('/') {
            Some((a, p)) => (a, format!("/{p}")),
            None => (rest, "/".to_string()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (
                h,
                p.parse::<u16>()
                    .map_err(|_| format!("bad port in cover url: {authority}"))?,
            ),
            None => (authority, 80),
        };
        let mut stream = std::net::TcpStream::connect((host, port))
            .map_err(|e| format!("connect to {host}:{port} failed: {e}"))?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(20)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(20)));
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: dj-music-suite\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("send request failed: {e}"))?;
        let mut resp = Vec::new();
        stream
            .read_to_end(&mut resp)
            .map_err(|e| format!("read response failed: {e}"))?;
        let sep = resp
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("malformed http response: no header terminator")?;
        let head = String::from_utf8_lossy(&resp[..sep]).to_string();
        let mut lines = head.lines();
        let status = lines.next().unwrap_or_default();
        let code: u16 = status
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let mut content_length = None;
        let mut chunked = false;
        let mut location = None;
        for line in lines {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = v.trim().parse::<usize>().ok(),
                "transfer-encoding" => chunked = v.trim().to_ascii_lowercase().contains("chunked"),
                "location" => location = Some(v.trim().to_string()),
                _ => {}
            }
        }
        let mut body = resp[sep + 4..].to_vec();
        if (300..400).contains(&code) {
            if let Some(loc) = location {
                url = if loc.starts_with("http") {
                    loc.replacen("https://", "http://", 1)
                } else {
                    format!("http://{authority}{loc}")
                };
                continue;
            }
            return Err(format!("cover download failed: http {code} without Location"));
        }
        if code != 200 {
            return Err(format!("cover download failed: http {code}"));
        }
        if chunked {
            body = dechunk(&body);
        } else if let Some(len) = content_length {
            body.truncate(len.min(body.len()));
        }
        if body.is_empty() {
            return Err("cover download failed: empty body".into());
        }
        return Ok(body);
    }
    Err("cover download failed: too many redirects".into())
}

struct TagCtx {
    meta_dir: Option<PathBuf>,
    allow_download: bool,
}

/// Persist a freshly downloaded cover next to the local ones so later runs
/// (including --no-download and offline use) find it locally. Returns a
/// note for the log or an empty string if caching was not possible.
fn cache_cover(ctx: &TagCtx, meta: &Option<NcmMeta>, mime: &str, data: &[u8]) -> String {
    let (Some(meta_dir), Some(meta)) = (ctx.meta_dir.as_ref(), meta.as_ref()) else {
        return String::new();
    };
    if meta.music_id.is_empty() {
        return String::new();
    }
    let Some(ext) = image_ext_for_mime(mime) else {
        return String::new();
    };
    let cache_path = meta_dir.join(format!("track-{}.{}", meta.music_id, ext));
    if fs::create_dir_all(meta_dir)
        .and_then(|_| fs::write(&cache_path, data))
        .is_err()
    {
        return String::new();
    }
    format!(" (cached to {})", cache_path.display())
}

fn tag_file(
    produced: &Path,
    src: &Path,
    meta_json: &[u8],
    embedded_image: Option<ncm_core::image::Image>,
    ctx: &TagCtx,
) -> Result<Option<String>, String> {
    let mut lyrics = None;
    let lrc_path = src.with_extension("lrc");
    if lrc_path.is_file() {
        match fs::read_to_string(&lrc_path) {
            Ok(raw) => {
                let normalized = normalize_lrc(&raw);
                if !normalized.is_empty() {
                    lyrics = Some(normalized);
                }
            }
            Err(e) => eprintln!("[warn] cannot read {}: {e}", lrc_path.display()),
        }
    }

    let meta = parse_meta_json(meta_json);

    let mut cover_origin: Option<String> = None;
    let mut cover_mime = String::new();
    let mut cover_data: Option<Vec<u8>> = None;
    if let (Some(meta_dir), Some(meta)) = (ctx.meta_dir.as_ref(), meta.as_ref()) {
        if meta_dir.is_dir() && !meta.music_id.is_empty() {
            for ext in ["jpg", "jpeg", "png", "webp"] {
                let p = meta_dir.join(format!("track-{}.{}", meta.music_id, ext));
                if p.is_file() {
                    if let Ok(data) = fs::read(&p) {
                        cover_mime = sniff_mime(&data).to_string();
                        cover_origin = Some(format!("local {}", p.display()));
                        cover_data = Some(data);
                    }
                    break;
                }
            }
        }
    }
    if cover_data.is_none() {
        if let Some(image) = embedded_image {
            cover_mime = image.mime_type().to_string();
            cover_origin = Some("embedded in ncm".to_string());
            cover_data = Some(image.into_data());
        }
    }
    if cover_data.is_none() && ctx.allow_download {
        if let Some(album_pic) = meta
            .as_ref()
            .map(|m| m.album_pic.as_str())
            .filter(|s| !s.is_empty())
        {
            match http_get(album_pic) {
                Ok(data) => {
                    cover_mime = sniff_mime(&data).to_string();
                    cover_origin = Some(format!(
                        "downloaded {album_pic}{}",
                        cache_cover(ctx, &meta, &cover_mime, &data)
                    ));
                    cover_data = Some(data);
                }
                Err(e) => eprintln!("[warn] {}: {e}", src.display()),
            }
        }
    }

    if cover_data.is_none() && lyrics.is_none() {
        return Ok(None);
    }

    let mut tag =
        id3::Tag::read_from_path(produced).map_err(|e| format!("read id3 tag failed: {e}"))?;
    if let Some(data) = cover_data {
        tag.add_frame(id3::frame::Picture {
            mime_type: cover_mime,
            picture_type: id3::frame::PictureType::CoverFront,
            description: String::new(),
            data,
        });
    }
    if let Some(text) = lyrics {
        tag.add_frame(id3::frame::Lyrics {
            lang: "eng".to_string(),
            description: String::new(),
            text,
        });
    }
    tag.write_to_path(produced, id3::Version::Id3v24)
        .map_err(|e| format!("write id3 tag failed: {e}"))?;

    let mut summary = Vec::new();
    if let Some(origin) = cover_origin {
        summary.push(format!("cover ({origin})"));
    } else {
        summary.push("no cover".to_string());
    }
    if tag.lyrics().next().is_some() {
        summary.push("lyrics".to_string());
    }
    Ok(Some(summary.join(", ")))
}

fn convert_one(src: &Path, out_dir: &Path, tag_ctx: &TagCtx, pb: &ProgressBar) -> Result<bool, String> {
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("bad file name: {}", src.display()))?
        .to_string();

    let file = fs::File::open(src).map_err(|e| format!("cannot open {}: {e}", src.display()))?;
    let mut decoder =
        ncm_core::decoder::Decoder::decode(file).map_err(|e| format!("ncm decode failed: {e}"))?;

    let out_path = out_dir.join(format!("{stem}.{}", decoder.ext()));
    let mut out = fs::File::create(&out_path)
        .map_err(|e| format!("cannot create {}: {e}", out_path.display()))?;
    std::io::copy(&mut decoder.audio, &mut out)
        .map_err(|e| format!("writing {} failed: {e}", out_path.display()))?;
    pb.println(format!("[ok] {} -> {}", src.display(), out_path.display()));

    let embedded_image = decoder.image.take();
    match tag_file(&out_path, src, &decoder.meta, embedded_image, tag_ctx) {
        Ok(Some(msg)) => {
            pb.println(format!("[tag] {}: {msg}", out_path.display()));
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(e) => {
            pb.println(format!("[warn] {}: tagging skipped: {e}", out_path.display()));
            Ok(false)
        }
    }
}

fn cmd_convert(opts: ConvertOpts) -> i32 {
    if opts.threads == 0 {
        eprintln!("--threads must be at least 1");
        return 2;
    }
    let files = match collect_ncm_files(&opts.input) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if files.is_empty() {
        eprintln!("no .ncm files found in {}", opts.input.display());
        return 1;
    }
    if let Err(e) = fs::create_dir_all(&opts.output) {
        eprintln!("cannot create output dir {}: {e}", opts.output.display());
        return 2;
    }

    // meta_dir is always resolved (even when the folder does not exist yet) so
    // downloaded covers can be cached into it on the first run.
    let meta_dir = Some(opts.meta_dir.unwrap_or_else(|| opts.input.join("meta")));
    let tag_ctx = TagCtx {
        meta_dir,
        allow_download: !opts.no_download,
    };

    let pb = ProgressBar::new(files.len() as u64);
    if let Ok(style) = ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{bar:32.cyan/blue}] {pos}/{len} ({eta}) {msg}",
    ) {
        pb.set_style(style.progress_chars("\u{2588}\u{2592}\u{2591}"));
    }

    let files = Arc::new(files);
    let out_dir = Arc::new(opts.output);
    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let tagged = AtomicUsize::new(0);
    let workers = opts.threads.min(files.len());

    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= files.len() {
                    break;
                }
                let src = &files[i];
                pb.set_message(src.display().to_string());
                match convert_one(src, &out_dir, &tag_ctx, &pb) {
                    Ok(true) => {
                        tagged.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        pb.println(format!("[fail] {}: {e}", src.display()));
                        failed.fetch_add(1, Ordering::SeqCst);
                    }
                }
                pb.inc(1);
            });
        }
    });

    let failed = failed.load(Ordering::SeqCst);
    let tagged = tagged.load(Ordering::SeqCst);
    let ok = files.len() - failed;
    pb.finish_and_clear();
    println!("done: {ok} converted, {tagged} tagged, {failed} failed");
    if failed > 0 {
        return 1;
    }
    0
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
        Some(Commands::Convert(opts)) => cmd_convert(opts),
        None => {
            let _ = Cli::command().print_help();
            0
        }
    };
    std::process::exit(code);
}
