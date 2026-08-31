use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use id3::TagLike;

const USAGE: &str = "\
dj-music-suite - batch convert .ncm files via ncmc

Usage: dj-music-suite --input <DIR> --output <DIR> [--threads <N>] [--ncmc <PATH>]
                      [--meta-dir <DIR>] [--no-download]

Options:
    --input <DIR>     folder containing .ncm files
    --output <DIR>    folder to write converted audio files to
    --threads <N>     number of parallel conversions (default: 8)
    --ncmc <PATH>     path to the ncmc binary (default: auto-detect)
    --meta-dir <DIR>  folder with track-<musicId>.jpg covers (default: <input>/meta)
    --no-download     never fetch missing covers from the albumPic URL
    -h, --help        print this help

Converted files are tagged automatically when possible: the cover comes from
<meta-dir>/track-<musicId>.jpg (falling back to the albumPic URL stored in
the .ncm metadata unless --no-download) and a same-named .lrc file is
embedded as unsynced lyrics (USLT).";

struct Args {
    input: PathBuf,
    output: PathBuf,
    threads: usize,
    ncmc: Option<PathBuf>,
    meta_dir: Option<PathBuf>,
    no_download: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut input = None;
    let mut output = None;
    let mut threads = 8;
    let mut ncmc = None;
    let mut meta_dir = None;
    let mut no_download = false;
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
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
            "--ncmc" => {
                ncmc = Some(iter.next().ok_or("--ncmc requires a value")?);
            }
            "--meta-dir" => {
                meta_dir = Some(iter.next().ok_or("--meta-dir requires a value")?);
            }
            "--no-download" => {
                no_download = true;
            }
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    Ok(Args {
        input: PathBuf::from(input.ok_or("--input is required (e.g. --input test)")?),
        output: PathBuf::from(output.ok_or("--output is required (e.g. --output test-mp3)")?),
        threads,
        ncmc: ncmc.map(PathBuf::from),
        meta_dir: meta_dir.map(PathBuf::from),
        no_download,
    })
}

fn ncmc_candidates() -> Vec<String> {
    let os = match env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "win32",
        other => other,
    };
    let arch = match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let ext = if env::consts::OS == "windows" { ".exe" } else { "" };
    let name = format!("ncmc-{os}-{arch}{ext}");
    let mut dirs = Vec::new();
    if let Ok(exe) = env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.to_path_buf());
            dirs.push(d.join("bin"));
        }
    }
    if let Ok(cwd) = env::current_dir() {
        dirs.push(cwd.join("bin"));
        dirs.push(cwd);
    }
    dirs.into_iter()
        .map(|d| d.join(&name))
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

fn find_ncmc(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        return Err(format!("ncmc binary not found at {}", p.display()));
    }
    for candidate in ncmc_candidates() {
        let p = Path::new(&candidate);
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
    }
    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            for candidate in ncmc_candidates() {
                let p = dir.join(&candidate);
                if p.is_file() {
                    return Ok(p);
                }
            }
            let bare = dir.join("ncmc");
            if bare.is_file() {
                return Ok(bare);
            }
        }
    }
    Err(format!(
        "could not locate ncmc, looked at: {}\nuse --ncmc <PATH> to point at a ncmc binary",
        ncmc_candidates().join(", ")
    ))
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

fn find_produced(out_dir: &Path, stem: &str) -> Option<PathBuf> {
    fs::read_dir(out_dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.is_file()
            && p.file_stem().and_then(|s| s.to_str()) == Some(stem)
            && !is_ncm(p)
    })
}

const NCM_MAGIC: &[u8; 8] = b"CTENFDAM";
const NCM_META_KEY: [u8; 16] = *b"#14ljk_!\\]&0U<'(";

struct NcmMeta {
    music_id: String,
    album_pic: String,
}

fn le_u32(buf: &[u8], pos: usize) -> Option<(u32, usize)> {
    let end = pos.checked_add(4)?;
    if end > buf.len() {
        return None;
    }
    let mut b = [0u8; 4];
    b.copy_from_slice(&buf[pos..end]);
    Some((u32::from_le_bytes(b), end))
}

fn aes128_ecb_decrypt(data: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, String> {
    use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};
    if data.is_empty() || data.len() % 16 != 0 {
        return Err("data length is not a multiple of the AES block size".into());
    }
    let cipher = aes::Aes128::new(GenericArray::from_slice(key));
    let mut out = data.to_vec();
    for chunk in out.chunks_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }
    if let Some(&pad) = out.last() {
        let pad = pad as usize;
        if (1..=16).contains(&pad)
            && pad <= out.len()
            && out[out.len() - pad..].iter().all(|&b| b == pad as u8)
        {
            out.truncate(out.len() - pad);
        }
    }
    Ok(out)
}

fn parse_ncm_meta(path: &Path) -> Result<Option<NcmMeta>, String> {
    use std::io::Read;
    let mut file =
        fs::File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut head = Vec::new();
    file.by_ref()
        .take(256 * 1024)
        .read_to_end(&mut head)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if head.len() < 10 || &head[..8] != NCM_MAGIC {
        return Ok(None);
    }
    let mut pos = 10;
    let (klen, p) = le_u32(&head, pos).ok_or("truncated ncm header")?;
    pos = p + klen as usize;
    let (mlen, p) = le_u32(&head, pos).ok_or("truncated ncm header")?;
    pos = p;
    if mlen == 0 {
        return Ok(None);
    }
    if pos + mlen as usize > head.len() {
        return Err("ncm metadata section exceeds the 256 KiB read window".into());
    }
    let decoded: Vec<u8> = head[pos..pos + mlen as usize].iter().map(|b| b ^ 0x63).collect();
    let json_bytes: Vec<u8> = if decoded.starts_with(b"163 key(Don't modify):") {
        let mut b64 = decoded[22..].to_vec();
        let rem = b64.len() % 4;
        if rem > 0 {
            b64.extend(std::iter::repeat(b'=').take(4 - rem));
        }
        use base64::Engine as _;
        let cipher = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .map_err(|e| format!("ncm metadata base64 decode failed: {e}"))?;
        let plain = aes128_ecb_decrypt(&cipher, &NCM_META_KEY)?;
        plain.into_iter().skip(6).collect()
    } else {
        decoded
    };
    let end = json_bytes
        .iter()
        .rposition(|&b| b == b'}')
        .ok_or("ncm metadata json not found")?
        + 1;
    let value: serde_json::Value = serde_json::from_slice(&json_bytes[..end])
        .map_err(|e| format!("ncm metadata json parse failed: {e}"))?;
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
        return Ok(None);
    }
    Ok(Some(NcmMeta { music_id, album_pic }))
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

fn tag_produced(src: &Path, produced: &Path, ctx: &TagCtx) -> Result<Option<String>, String> {
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

    let mut cover_origin: Option<String> = None;
    let mut cover_mime = String::new();
    let mut cover_data: Option<Vec<u8>> = None;
    if let Some(meta) = parse_ncm_meta(src)? {
        if let Some(meta_dir) = ctx.meta_dir.as_ref() {
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
        if cover_data.is_none() && ctx.allow_download && !meta.album_pic.is_empty() {
            match http_get(&meta.album_pic) {
                Ok(data) => {
                    cover_mime = sniff_mime(&data).to_string();
                    cover_origin = Some(format!("downloaded {}", meta.album_pic));
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

fn convert_one(ncmc: &Path, src: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let file_name = src
        .file_name()
        .ok_or_else(|| format!("bad file name: {}", src.display()))?;
    let staged = out_dir.join(file_name);
    fs::copy(src, &staged).map_err(|e| format!("copy into output dir failed: {e}"))?;
    let result = Command::new(ncmc)
        .arg(&staged)
        .output()
        .map_err(|e| format!("failed to run ncmc: {e}"))
        .and_then(|out| {
            if out.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let stdout = String::from_utf8_lossy(&out.stdout);
                let msg = if stderr.trim().is_empty() {
                    stdout
                } else {
                    stderr
                };
                Err(format!("ncmc failed: {}", msg.trim()))
            }
        });
    let _ = fs::remove_file(&staged);
    let stem = staged
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let produced = find_produced(out_dir, &stem);
    match (result, produced) {
        (Ok(()), Some(p)) => Ok(p),
        (Ok(()), None) => Err("ncmc reported success but no audio file was produced".into()),
        (Err(e), _) => Err(e),
    }
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let ncmc = match find_ncmc(args.ncmc.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let files = match collect_ncm_files(&args.input) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if files.is_empty() {
        eprintln!("no .ncm files found in {}", args.input.display());
        std::process::exit(1);
    }
    if let Err(e) = fs::create_dir_all(&args.output) {
        eprintln!("cannot create output dir {}: {e}", args.output.display());
        std::process::exit(2);
    }
    println!(
        "converting {} file(s) from {} to {} with {} thread(s), using ncmc: {}",
        files.len(),
        args.input.display(),
        args.output.display(),
        args.threads,
        ncmc.display()
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
                match convert_one(&ncmc, src, &out_dir) {
                    Ok(produced) => {
                        println!("[ok] {} -> {}", src.display(), produced.display());
                        match tag_produced(src, &produced, &tag_ctx) {
                            Ok(Some(msg)) => {
                                println!("[tag] {}: {msg}", produced.display());
                                tagged.fetch_add(1, Ordering::SeqCst);
                            }
                            Ok(None) => {}
                            Err(e) => {
                                eprintln!("[warn] {}: tagging skipped: {e}", produced.display());
                            }
                        }
                    }
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
        std::process::exit(1);
    }
}
