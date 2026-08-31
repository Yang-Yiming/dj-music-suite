use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

const USAGE: &str = "\
dj-music-suite - batch convert .ncm files via ncmc

Usage: dj-music-suite --input <DIR> --output <DIR> [--threads <N>] [--ncmc <PATH>]

Options:
    --input <DIR>     folder containing .ncm files
    --output <DIR>    folder to write converted audio files to
    --threads <N>     number of parallel conversions (default: 8)
    --ncmc <PATH>     path to the ncmc binary (default: auto-detect)
    -h, --help        print this help";

struct Args {
    input: PathBuf,
    output: PathBuf,
    threads: usize,
    ncmc: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut input = None;
    let mut output = None;
    let mut threads = 8;
    let mut ncmc = None;
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
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    Ok(Args {
        input: PathBuf::from(input.ok_or("--input is required (e.g. --input test)")?),
        output: PathBuf::from(output.ok_or("--output is required (e.g. --output test-mp3)")?),
        threads,
        ncmc: ncmc.map(PathBuf::from),
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

    let files = Arc::new(files);
    let out_dir = Arc::new(args.output);
    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
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
                    Ok(produced) => println!(
                        "[ok] {} -> {}",
                        src.display(),
                        produced.display()
                    ),
                    Err(e) => {
                        eprintln!("[fail] {}: {e}", src.display());
                        failed.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
    });

    let failed = failed.load(Ordering::SeqCst);
    let ok = files.len() - failed;
    println!("done: {ok} converted, {failed} failed");
    if failed > 0 {
        std::process::exit(1);
    }
}
