use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use id3::TagLike;

const USAGE: &str = "\
dj-music-suite - a suite of small tools for music files

Usage: dj-music-suite <COMMAND> [OPTIONS]
       dj-music-suite help [COMMAND]

Commands:
    convert    batch convert .ncm files and embed cover art + lyrics

Run 'dj-music-suite help convert' (or 'convert --help') for command options.
Omitting the command (e.g. 'dj-music-suite --input <DIR>') still runs 'convert'.";

const CONVERT_USAGE: &str = "\
dj-music-suite convert - batch convert .ncm files and embed cover art + lyrics

Usage: dj-music-suite convert --input <DIR> --output <DIR> [--threads <N>]
                              [--meta-dir <DIR>] [--no-download]

Options:
    --input <DIR>     folder containing .ncm files
    --output <DIR>    folder to write converted audio files to
    --threads <N>     number of parallel conversions (default: 8)
    --meta-dir <DIR>  folder with track-<musicId>.jpg covers (default: <input>/meta)
    --no-download     never fetch missing covers from the albumPic URL
    -h, --help        print this help

Decryption is built in (ncm_core), no external binaries are needed.
Converted files are tagged automatically when possible: the cover comes from
<meta-dir>/track-<musicId>.jpg, then the image embedded in the .ncm file, then
the albumPic URL (unless --no-download); a same-named .lrc file is embedded
as unsynced lyrics (USLT).";

struct Args {
    input: PathBuf,
    output: PathBuf,
    threads: usize,
    meta_dir: Option<PathBuf>,
    no_download: bool,
}

fn parse_convert_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut input = None;
    let mut output = None;
    let mut threads = 8;
    let mut meta_dir = None;
    let mut no_download = false;
    let mut iter = args;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{CONVERT_USAGE}");
                std::process::exit(0);
            }
            "--input" => {
                input = Some(iter.next().ok_or("--input requires a value")?);
            }
            "--output" => {
                output = Some(iter.next().ok_or("--output requires a value")?);
            }
            "--threads" => {
                let v = iter.next().ok_or("--threads requires a value")?;
                threads = v.parse().map_err(|_| format!("invalid --threads value: {v}"))?;
                if threads == 0 {
                    return Err("--threads must be at least 1".into());
                }
            }
            "--meta-dir" => {
                meta_dir = Some(iter.next().ok_or("--meta-dir requires a value")?);
            }
            "--no-download" => {
                no_download = true;
            }
            other => return Err(format!("unknown argument: {other}\n\n{CONVERT_USAGE}")),
        }
    }
    Ok(Args {
        input: PathBuf::from(input.ok_or_else(|| {
            format!("--input is required (e.g. --input test)\n\n{CONVERT_USAGE}")
        })?),
        output: PathBuf::from(output.ok_or_else(|| {
            format!("--output is required (e.g. --output test-mp3)\n\n{CONVERT_USAGE}")
        })?),
        threads,
        meta_dir: meta_dir.map(PathBuf::from),
        no_download,
    })
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
        if !meta.music_id.is_empty() {
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
                    cover_origin = Some(format!("downloaded {album_pic}"));
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

fn convert_one(src: &Path, out_dir: &Path, tag_ctx: &TagCtx) -> Result<bool, String> {
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
    println!("[ok] {} -> {}", src.display(), out_path.display());

    let embedded_image = decoder.image.take();
    match tag_file(&out_path, src, &decoder.meta, embedded_image, tag_ctx) {
        Ok(Some(msg)) => {
            println!("[tag] {}: {msg}", out_path.display());
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(e) => {
            eprintln!("[warn] {}: tagging skipped: {e}", out_path.display());
            Ok(false)
        }
    }
}

fn cmd_convert(args: &[String]) -> i32 {
    let args = match parse_convert_args(args.iter().cloned()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let files = match collect_ncm_files(&args.input) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if files.is_empty() {
        eprintln!("no .ncm files found in {}", args.input.display());
        return 1;
    }
    if let Err(e) = fs::create_dir_all(&args.output) {
        eprintln!("cannot create output dir {}: {e}", args.output.display());
        return 2;
    }
    println!(
        "converting {} file(s) from {} to {} with {} thread(s)",
        files.len(),
        args.input.display(),
        args.output.display(),
        args.threads
    );

    let meta_dir = args.meta_dir.unwrap_or_else(|| args.input.join("meta"));
    let meta_dir = if meta_dir.is_dir() { Some(meta_dir) } else { None };
    let tag_ctx = TagCtx {
        meta_dir,
        allow_download: !args.no_download,
    };
    let files = Arc::new(files);
    let out_dir = Arc::new(args.output);
    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let tagged = AtomicUsize::new(0);
    let workers = args.threads.min(files.len());

    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= files.len() {
                    break;
                }
                let src = &files[i];
                match convert_one(src, &out_dir, &tag_ctx) {
                    Ok(true) => {
                        tagged.fetch_add(1, Ordering::SeqCst);
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!("[fail] {}: {e}", src.display());
                        failed.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
    });

    let failed = failed.load(Ordering::SeqCst);
    let tagged = tagged.load(Ordering::SeqCst);
    let ok = files.len() - failed;
    println!("done: {ok} converted, {tagged} tagged, {failed} failed");
    if failed > 0 {
        return 1;
    }
    0
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        println!("{USAGE}");
        std::process::exit(0);
    }
    let verb = match args.first() {
        Some(a) if matches!(a.as_str(), "-h" | "--help") => {
            println!("{USAGE}");
            std::process::exit(0);
        }
        Some(a) if !a.starts_with('-') => args.remove(0),
        _ => "convert".to_string(),
    };
    let code = match verb.as_str() {
        "convert" => cmd_convert(&args),
        "help" => cmd_help(&args),
        other => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

fn cmd_help(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        None => {
            println!("{USAGE}");
            0
        }
        Some("convert") => {
            println!("{CONVERT_USAGE}");
            0
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            2
        }
    }
}
