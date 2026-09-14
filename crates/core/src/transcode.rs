//! Transcode audio files into another format by driving ffmpeg.
//!
//! Every other command in the suite leaves the bytes it moves (or tags)
//! alone; this one rewrites them. That is why it is its own command with its
//! own analyze → plan → execute pair instead of a reorg mode: a wrong move
//! costs a move back, a wrong encode costs audio, and the analysis has to
//! preflight something reorg never needs — an external encoder.
//!
//! ffmpeg does the decoding, encoding and tag/cover re-mapping (it knows the
//! container quirks, we do not); lofty only *reads* the source to decide
//! whether a file already is the target codec, which is the one mistake this
//! command can make that loses quality for nothing.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use lofty::config::WriteOptions;
use lofty::file::FileType;
use lofty::mp4::Mp4Codec;
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::{Tag, TagType};
use serde::{Deserialize, Serialize};

use crate::scan::{file_name, scan_with_exts};
use crate::trash;
use crate::{usage, Event, Error, Result, Sink};

/// parallel ffmpeg processes; each one is single-threaded per file
pub const DEFAULT_THREADS: usize = 8;

/// Candidate inputs: what ffmpeg can take as a source and a music folder
/// plausibly contains. lofty does not know all of them (ape, wma, dsd, tta),
/// so this list is deliberately wider than [`crate::scan::scan_audio`]'s.
const SOURCE_EXTS: &[&str] = &[
    "mp3", "mp2", "m4a", "m4b", "aac", "flac", "wav", "aif", "aiff", "ape", "wv", "tta", "dsf",
    "dff", "opus", "ogg", "wma", "mpc", "mka", "ac3", "dts", "shn", "caf", "spx",
];

// ---------------------------------------------------------------- targets

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetFormat {
    Mp3,
    Flac,
    M4a,
    Alac,
    Wav,
}

impl TargetFormat {
    pub fn all() -> [TargetFormat; 5] {
        [
            TargetFormat::Mp3,
            TargetFormat::Flac,
            TargetFormat::M4a,
            TargetFormat::Alac,
            TargetFormat::Wav,
        ]
    }

    pub fn parse(raw: &str) -> Option<TargetFormat> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "mp3" => Some(TargetFormat::Mp3),
            "flac" => Some(TargetFormat::Flac),
            // m4a is AAC in an MP4 container; accept the codec name as well
            "m4a" | "aac" => Some(TargetFormat::M4a),
            "alac" => Some(TargetFormat::Alac),
            "wav" => Some(TargetFormat::Wav),
            _ => None,
        }
    }

    pub fn ext(&self) -> &'static str {
        match self {
            TargetFormat::Mp3 => "mp3",
            TargetFormat::Flac => "flac",
            TargetFormat::M4a | TargetFormat::Alac => "m4a",
            TargetFormat::Wav => "wav",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            TargetFormat::Mp3 => "mp3",
            TargetFormat::Flac => "flac",
            TargetFormat::M4a => "m4a (AAC)",
            TargetFormat::Alac => "m4a (ALAC)",
            TargetFormat::Wav => "wav",
        }
    }

    pub fn lossless(&self) -> bool {
        matches!(
            self,
            TargetFormat::Flac | TargetFormat::Alac | TargetFormat::Wav
        )
    }

    /// Whether ffmpeg can put a cover into this container at all. RIFF has no
    /// place for an attached picture, so a wav conversion drops it — worth
    /// saying out loud before the run.
    pub fn carries_cover(&self) -> bool {
        !matches!(self, TargetFormat::Wav)
    }

    /// Default `--quality` for the target, None when it takes none.
    pub fn default_quality(&self) -> Option<&'static str> {
        match self {
            TargetFormat::Mp3 => Some("v0"),
            TargetFormat::M4a => Some("256k"),
            TargetFormat::Flac => Some("8"),
            TargetFormat::Alac | TargetFormat::Wav => None,
        }
    }

    /// Encoders this target can need, for the preflight. A build without them
    /// (ffmpeg-free, for instance) cannot do this target at all.
    pub fn encoders(&self) -> &'static [&'static str] {
        match self {
            TargetFormat::Mp3 => &["libmp3lame"],
            TargetFormat::Flac => &["flac"],
            TargetFormat::M4a => &["aac"],
            TargetFormat::Alac => &["alac"],
            TargetFormat::Wav => &["pcm_s16le", "pcm_s24le", "pcm_s32le"],
        }
    }

    /// Output container ffmpeg should mux into (explicit, so the temp file's
    /// name never decides the format). MP4 goes through `ipod`, the muxer for
    /// `.m4a`: the plain `mp4` muxer writes ftyp `isom` and a cover as a
    /// bogus video track instead of the `covr` atom readers expect.
    fn muxer(&self) -> &'static str {
        match self {
            TargetFormat::Mp3 => "mp3",
            TargetFormat::Flac => "flac",
            TargetFormat::M4a | TargetFormat::Alac => "ipod",
            TargetFormat::Wav => "wav",
        }
    }
}

/// ffmpeg encoder + arguments for one output file.
#[derive(Clone, Debug)]
pub struct CodecPlan {
    pub encoder: String,
    pub args: Vec<String>,
}

fn pcm_encoder(bits: Option<u8>) -> &'static str {
    match bits {
        Some(b) if b > 24 => "pcm_s32le",
        Some(b) if b > 16 => "pcm_s24le",
        _ => "pcm_s16le",
    }
}

/// Resolve the encoder arguments for `target`; `quality` is the raw
/// `--quality` spec, None meaning the target's default.
pub fn codec_plan(
    target: TargetFormat,
    quality: Option<&str>,
    src_bit_depth: Option<u8>,
) -> Result<CodecPlan> {
    let quality = quality.map(str::trim).filter(|q| !q.is_empty());
    let bad_mp3 = || usage(format!("bad --quality {} for mp3: expected v0..v9 or e.g. 320k", quality.unwrap_or("")));
    let codec = |encoder: &str, args: Vec<String>| CodecPlan {
        encoder: encoder.to_string(),
        args,
    };
    match target {
        TargetFormat::Mp3 => {
            let q = quality.unwrap_or("v0");
            if q.len() >= 2 && q[..1].eq_ignore_ascii_case("v") {
                let level: u8 = q[1..].parse().map_err(|_| bad_mp3())?;
                if level > 9 {
                    return Err(bad_mp3());
                }
                Ok(codec("libmp3lame", vec!["-q:a".into(), level.to_string()]))
            } else {
                let kbps = parse_kbps(q)?;
                Ok(codec("libmp3lame", vec!["-b:a".into(), format!("{kbps}k")]))
            }
        }
        TargetFormat::M4a => {
            let kbps = parse_kbps(quality.unwrap_or("256k"))?;
            Ok(codec("aac", vec!["-b:a".into(), format!("{kbps}k")]))
        }
        TargetFormat::Flac => {
            let q = quality.unwrap_or("8");
            let bad = || usage(format!("bad --quality {q} for flac: expected 0..12"));
            let level: u8 = q.parse().map_err(|_| bad())?;
            if level > 12 {
                return Err(bad());
            }
            Ok(codec(
                "flac",
                vec!["-compression_level".into(), level.to_string()],
            ))
        }
        TargetFormat::Alac | TargetFormat::Wav => {
            if let Some(q) = quality {
                return Err(usage(format!(
                    "--quality {q} does not apply to {} (it has no quality knob)",
                    target.label()
                )));
            }
            let encoder = match target {
                TargetFormat::Alac => "alac",
                _ => pcm_encoder(src_bit_depth),
            };
            Ok(codec(encoder, Vec::new()))
        }
    }
}

fn parse_kbps(raw: &str) -> Result<u32> {
    let digits = raw.trim().trim_end_matches(['k', 'K']);
    let bad = || usage(format!("bad bitrate {raw}: expected e.g. 320k"));
    let kbps: u32 = digits.parse().map_err(|_| bad())?;
    if !(8..=1024).contains(&kbps) {
        return Err(bad());
    }
    Ok(kbps)
}

// ---------------------------------------------------------------- ffmpeg

/// The ffmpeg binary a run drives, plus what it can encode.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    pub path: PathBuf,
    pub version: String,
    encoders: Vec<String>,
}

impl Ffmpeg {
    /// Locate ffmpeg (`explicit` wins, else `$PATH`) and read its version and
    /// encoder list once per run. A missing binary is an environment problem,
    /// not a usage error, so it reports what to install.
    pub fn detect(explicit: Option<&Path>) -> Result<Ffmpeg> {
        let path = match explicit {
            Some(p) if p.is_file() => p.to_path_buf(),
            Some(p) => return Err(usage(format!("--ffmpeg {} is not a file", p.display()))),
            None => PathBuf::from("ffmpeg"),
        };
        let missing = |e: std::io::Error| {
            Error::Runtime(format!(
                "cannot run ffmpeg ({e})\n  transcode needs it: install ffmpeg \
                 (macOS: brew install ffmpeg, Debian/Ubuntu: apt install ffmpeg, \
                 Windows: winget install ffmpeg) or point --ffmpeg at one"
            ))
        };
        let version_out = Command::new(&path)
            .args(["-hide_banner", "-version"])
            .output()
            .map_err(missing)?;
        if !version_out.status.success() {
            return Err(Error::Runtime(format!(
                "ffmpeg -version failed: {}",
                version_out.status
            )));
        }
        let head = String::from_utf8_lossy(&version_out.stdout).to_string();
        let version = head
            .lines()
            .next()
            .and_then(|line| {
                let mut words = line.split_whitespace();
                (words.next() == Some("ffmpeg") && words.next() == Some("version"))
                    .then(|| words.next().unwrap_or("unknown").to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());

        let enc_out = Command::new(&path)
            .args(["-hide_banner", "-encoders"])
            .output()
            .map_err(missing)?;
        let mut encoders = Vec::new();
        for line in String::from_utf8_lossy(&enc_out.stdout).lines() {
            let mut words = line.split_whitespace();
            let (Some(flags), Some(name)) = (words.next(), words.next()) else {
                continue;
            };
            // encoder table rows look like " A....D libmp3lame ..."
            if flags.len() == 6 && flags.chars().all(|c| "AVSFDXB.".contains(c)) {
                encoders.push(name.to_string());
            }
        }
        Ok(Ffmpeg {
            path,
            version,
            encoders,
        })
    }

    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders.iter().any(|e| e == name)
    }

    /// Refuse a target the local ffmpeg cannot encode before any plan is
    /// built: a plan that silently degrades is worse than no plan.
    pub fn preflight(&self, target: TargetFormat) -> Result<()> {
        let missing: Vec<&str> = target
            .encoders()
            .iter()
            .copied()
            .filter(|e| !self.has_encoder(e))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        Err(Error::Runtime(format!(
            "this ffmpeg cannot encode {}: no {} encoder ({}).\n  install a full \
             ffmpeg build (e.g. `brew install ffmpeg`) or pick another target",
            target.label(),
            missing.join(", no "),
            self.path.display()
        )))
    }

    pub fn describe(&self) -> String {
        format!("ffmpeg {} ({})", self.version, self.path.display())
    }
}

// ---------------------------------------------------------------- plan

pub struct TranscodeOpts {
    /// folder to scan for audio files (recursively)
    pub input: PathBuf,
    /// folder to write into; None writes next to each source
    pub output: Option<PathBuf>,
    pub target: TargetFormat,
    pub quality: Option<String>,
    /// move the source into the trash after a successful conversion
    pub replace: bool,
    pub threads: usize,
    /// explicit ffmpeg binary; None searches $PATH
    pub ffmpeg: Option<PathBuf>,
    /// trash folder for `replace` (default: `<input>/.dedup-trash`)
    pub trash: Option<PathBuf>,
    /// plan json written by the analysis (None: don't write a file, e.g. web)
    pub plan: Option<PathBuf>,
    /// execute a previously written plan instead of analyzing again
    pub from: Option<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    pub src: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dst: Option<PathBuf>,
    /// "transcode", or "replace" when the destination *is* the source
    pub op: String,
    /// "ready", "same-codec", "conflict" or "needs-replace"
    pub status: String,
    /// source codec as far as lofty could tell ("FLAC", "AAC", …), for the UI
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_bit_depth: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Plan {
    pub input: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PathBuf>,
    pub target: TargetFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    pub replace: bool,
    pub trash: PathBuf,
    pub entries: Vec<PlanEntry>,
}

impl Plan {
    pub fn count(&self, status: &str) -> usize {
        self.entries.iter().filter(|e| e.status == status).count()
    }

    pub fn actionable(&self) -> Vec<&PlanEntry> {
        self.entries.iter().filter(|e| e.status == "ready").collect()
    }
}

/// Outcome of a transcode run. `bytes_before`/`bytes_after` cover the files
/// that were actually converted, so the two are comparable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeSummary {
    pub converted: usize,
    /// entries the analysis did not mark "ready" (same codec, conflicts)
    pub skipped: usize,
    pub failed: usize,
    /// sources moved into the trash (`--replace`)
    pub trashed: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

// ---------------------------------------------------------------- analysis

/// What lofty can tell about a source file.
#[derive(Clone, PartialEq, Eq, Debug)]
enum SourceCodec {
    Mp3,
    Aac,
    Alac,
    Flac,
    Pcm(Option<u8>),
    /// a format lofty knows by name but has no encoder pairing for
    Other(String),
}

impl SourceCodec {
    fn label(&self) -> String {
        match self {
            SourceCodec::Mp3 => "MP3".to_string(),
            SourceCodec::Aac => "AAC".to_string(),
            SourceCodec::Alac => "ALAC".to_string(),
            SourceCodec::Flac => "FLAC".to_string(),
            SourceCodec::Pcm(Some(bits)) => format!("PCM {bits}-bit"),
            SourceCodec::Pcm(None) => "PCM".to_string(),
            SourceCodec::Other(name) => name.clone(),
        }
    }

    /// ffmpeg encoder that produces exactly this codec, for the "already the
    /// target codec" comparison.
    fn encoder(&self) -> Option<&'static str> {
        match self {
            SourceCodec::Mp3 => Some("libmp3lame"),
            SourceCodec::Aac => Some("aac"),
            SourceCodec::Alac => Some("alac"),
            SourceCodec::Flac => Some("flac"),
            SourceCodec::Pcm(bits) => Some(pcm_encoder(*bits)),
            SourceCodec::Other(_) => None,
        }
    }

    fn is_lossy(&self) -> bool {
        matches!(self, SourceCodec::Mp3 | SourceCodec::Aac)
    }
}

#[derive(Default, Clone)]
struct SourceInfo {
    codec: Option<SourceCodec>,
    duration_secs: Option<u64>,
    has_cover: bool,
}

impl SourceInfo {
    fn bit_depth(&self) -> Option<u8> {
        match &self.codec {
            Some(SourceCodec::Pcm(bits)) => *bits,
            _ => None,
        }
    }
}

/// Read the source codec + duration + cover. None when lofty cannot parse the
/// container (dsd, wma, ape, …): those are still convertible, we just cannot
/// tell what is inside them.
fn probe_source(path: &Path) -> SourceInfo {
    let Ok(file) = Probe::open(path).and_then(|p| p.read()) else {
        return SourceInfo::default();
    };
    let props = file.properties();
    let duration = props.duration();
    let has_cover = file
        .primary_tag()
        .or_else(|| file.first_tag())
        .is_some_and(|t| !t.pictures().is_empty());
    let codec = match file.file_type() {
        FileType::Mpeg => Some(SourceCodec::Mp3),
        FileType::Flac => Some(SourceCodec::Flac),
        FileType::Wav | FileType::Aiff => Some(SourceCodec::Pcm(props.bit_depth())),
        FileType::Mp4 => mp4_codec(path),
        FileType::Aac => Some(SourceCodec::Aac),
        FileType::Ape => Some(SourceCodec::Other("APE".into())),
        FileType::WavPack => Some(SourceCodec::Other("WavPack".into())),
        FileType::Mpc => Some(SourceCodec::Other("Musepack".into())),
        FileType::Opus => Some(SourceCodec::Other("Opus".into())),
        FileType::Vorbis => Some(SourceCodec::Other("Vorbis".into())),
        FileType::Speex => Some(SourceCodec::Other("Speex".into())),
        _ => None,
    };
    SourceInfo {
        codec,
        duration_secs: (!duration.is_zero()).then_some(duration.as_secs()),
        has_cover,
    }
}

/// MP4 containers hold AAC, ALAC, MP3 or FLAC, and the extension cannot tell
/// them apart — `alac.m4a -> aac m4a` is a real conversion under the same
/// extension, so a false "same codec" here would skip real work.
fn mp4_codec(path: &Path) -> Option<SourceCodec> {
    let mut file = fs::File::open(path).ok()?;
    let mp4 = lofty::mp4::Mp4File::read_from(&mut file, lofty::config::ParseOptions::new()).ok()?;
    match mp4.properties().codec()? {
        Mp4Codec::AAC => Some(SourceCodec::Aac),
        Mp4Codec::ALAC => Some(SourceCodec::Alac),
        Mp4Codec::MP3 => Some(SourceCodec::Mp3),
        Mp4Codec::FLAC => Some(SourceCodec::Flac),
        _ => None,
    }
}

/// Classify every audio file under the input and optionally write the plan
/// json for later `--from` execution.
pub fn analyze(opts: &TranscodeOpts, sink: Sink) -> Result<Plan> {
    let ff = Ffmpeg::detect(opts.ffmpeg.as_deref())?;
    ff.preflight(opts.target)?;
    // a bad --quality is a usage error: report it before any other output
    // (build_plan validates too, so it also holds without an ffmpeg)
    codec_plan(opts.target, opts.quality.as_deref(), None)?;
    sink(&Event::Line(format!("[ok] {}", ff.describe())));
    build_plan(opts, sink)
}

/// The scanning/classifying half of [`analyze`], without the ffmpeg
/// preflight (which the caller has already done).
fn build_plan(opts: &TranscodeOpts, sink: Sink) -> Result<Plan> {
    // fail on a bad --quality now, not once per file
    codec_plan(opts.target, opts.quality.as_deref(), None)?;

    let input = canonical_dir(&opts.input, "input")?;
    let output = match &opts.output {
        Some(out) => {
            fs::create_dir_all(out)
                .map_err(|e| usage(format!("cannot create output dir {}: {e}", out.display())))?;
            Some(canonical_dir(out, "output")?)
        }
        None => None,
    };

    let files = scan_with_exts(&input, SOURCE_EXTS);
    if files.is_empty() {
        return Err(Error::Runtime(format!(
            "no audio files found in {}",
            input.display()
        )));
    }
    let trash = opts
        .trash
        .clone()
        .unwrap_or_else(|| trash::default_dir(&input));

    sink(&Event::Line(format!(
        "[ok] {} -> {}{}",
        input.display(),
        opts.target.label(),
        match &output {
            Some(out) => format!(" in {}", out.display()),
            None => " next to each source".to_string(),
        }
    )));
    sink(&Event::Start(files.len() as u64));

    let mut entries = Vec::with_capacity(files.len());
    for src in &files {
        let info = probe_source(src);
        let dst = target_path(&input, output.as_deref(), src, opts.target);
        entries.push(classify(src, dst, opts.target, opts.replace, &info));
        sink(&Event::Step(file_name(src)));
    }

    let plan = Plan {
        input,
        output,
        target: opts.target,
        quality: opts.quality.clone().filter(|q| !q.trim().is_empty()),
        replace: opts.replace,
        trash,
        entries,
    };
    if let Some(plan_path) = &opts.plan {
        write_plan(&plan, plan_path, sink);
    }
    Ok(plan)
}

/// Read back a plan json written by [`analyze`].
pub fn load_plan(path: &Path) -> Result<Plan> {
    let raw = fs::read_to_string(path)
        .map_err(|e| usage(format!("cannot read plan {}: {e}", path.display())))?;
    serde_json::from_str(&raw).map_err(|e| usage(format!("bad plan file {}: {e}", path.display())))
}

fn canonical_dir(path: &Path, what: &str) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(p) if p.is_dir() => Ok(p),
        Ok(_) => Err(usage(format!("{what} is not a directory: {}", path.display()))),
        Err(e) => Err(usage(format!(
            "cannot resolve {what} {}: {e}",
            path.display()
        ))),
    }
}

/// With an output folder the tree below the input is preserved, so two files
/// with the same name in different folders never collide; without one each
/// file keeps its location and only its extension changes.
fn target_path(
    input: &Path,
    output: Option<&Path>,
    src: &Path,
    target: TargetFormat,
) -> PathBuf {
    match output {
        Some(out) => {
            let rel = src.strip_prefix(input).ok();
            let rel = rel.unwrap_or_else(|| Path::new(src.file_name().unwrap_or(src.as_os_str())));
            out.join(rel).with_extension(target.ext())
        }
        None => src.with_extension(target.ext()),
    }
}

fn ext_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn classify(
    src: &Path,
    dst: PathBuf,
    target: TargetFormat,
    replace: bool,
    info: &SourceInfo,
) -> PlanEntry {
    let size = fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    let ext = ext_of(src);
    let bases = PlanEntry {
        src: src.to_path_buf(),
        dst: Some(dst.clone()),
        op: String::new(),
        status: String::new(),
        src_codec: info.codec.as_ref().map(SourceCodec::label),
        src_bit_depth: info.bit_depth(),
        duration_secs: info.duration_secs,
        size,
        note: None,
    };

    // is it already the target codec?
    let same = match &info.codec {
        Some(codec) => {
            let planned = codec_plan(target, None, info.bit_depth()).ok();
            ext == target.ext()
                && codec
                    .encoder()
                    .is_some_and(|enc| planned.is_some_and(|p| p.encoder == enc))
        }
        // lofty could not read the container, so the extension is all there
        // is: re-encoding a file that already is the target only loses
        // quality, so take the extension at its word
        None => ext == target.ext(),
    };
    if same {
        return PlanEntry {
            op: "transcode".into(),
            status: "same-codec".into(),
            note: Some(match &info.codec {
                Some(codec) => format!("already {} ({})", target.label(), codec.label()),
                None => format!(
                    "already {} (codec not readable, assumed from the extension)",
                    target.label()
                ),
            }),
            ..bases
        };
    }

    let replaces_source = dst == src;
    let op = if replaces_source { "replace" } else { "transcode" };
    if replaces_source && !replace {
        // the file would be overwritten by its own conversion: without the
        // trash the original is gone for good, so refuse instead
        return PlanEntry {
            op: op.into(),
            status: "needs-replace".into(),
            note: Some(
                "same file name: the source has to be replaced; \
                 enable replace to move it into the trash first"
                    .into(),
            ),
            ..bases
        };
    }
    if !replaces_source && dst.exists() {
        return PlanEntry {
            op: op.into(),
            status: "conflict".into(),
            note: Some("target already exists, left untouched".into()),
            ..bases
        };
    }

    let note = match &info.codec {
        Some(codec) if target.lossless() && codec.is_lossy() => Some(format!(
            "{} -> {}: no quality gain, the file only gets bigger",
            codec.label(),
            target.label()
        )),
        // a source whose tags we cannot read may come out untagged
        None => Some("cannot read the source's tags; the output may be untagged".into()),
        _ => None,
    };
    let note = match note {
        Some(note) if info.has_cover && !target.carries_cover() => {
            Some(format!("{note}; the cover art is dropped ({})", target.label()))
        }
        Some(note) => Some(note),
        None if info.has_cover && !target.carries_cover() => {
            Some(format!("the cover art is dropped ({})", target.label()))
        }
        None => None,
    };
    PlanEntry {
        op: op.into(),
        status: "ready".into(),
        note,
        ..bases
    }
}

fn write_plan(plan: &Plan, plan_path: &Path, sink: Sink) {
    match serde_json::to_string_pretty(plan) {
        Ok(json) => {
            if let Err(e) = fs::write(plan_path, json + "\n") {
                sink(&Event::Warn(format!(
                    "[warn] cannot write plan {}: {e}",
                    plan_path.display()
                )));
            } else {
                sink(&Event::Line(format!("plan written to {}", plan_path.display())));
            }
        }
        Err(e) => sink(&Event::Warn(format!("[warn] cannot serialize plan: {e}"))),
    }
}

// ---------------------------------------------------------------- execution

/// Analyze (or read back a plan) and convert the entries marked "ready".
pub fn execute(opts: &TranscodeOpts, sink: Sink) -> Result<TranscodeSummary> {
    let plan = match &opts.from {
        Some(from) => load_plan(from)?,
        None => analyze(opts, sink)?,
    };
    execute_plan(&plan, opts.threads, opts.ffmpeg.as_deref(), sink)
}

/// Convert the entries of an already analyzed plan (CLI: through [`execute`];
/// web: the plan kept in memory). Only "ready" entries are touched.
pub fn execute_plan(
    plan: &Plan,
    threads: usize,
    ffmpeg: Option<&Path>,
    sink: Sink,
) -> Result<TranscodeSummary> {
    if threads == 0 {
        return Err(usage("--threads must be at least 1"));
    }
    let ff = Ffmpeg::detect(ffmpeg)?;
    ff.preflight(plan.target)?;

    let actionable: Vec<PlanEntry> = plan.actionable().into_iter().cloned().collect();
    sink(&Event::Start(actionable.len() as u64));
    let skipped = plan.entries.len() - actionable.len();

    let ff = Arc::new(ff);
    let plan = Arc::new(plan.clone());
    let next = AtomicUsize::new(0);
    let converted = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    let trashed = AtomicUsize::new(0);
    let before = AtomicU64::new(0);
    let after = AtomicU64::new(0);
    let workers = threads.min(actionable.len());

    thread::scope(|s| {
        let actionable = &actionable;
        for _ in 0..workers {
            let (ff, plan) = (Arc::clone(&ff), Arc::clone(&plan));
            let (next, converted, failed) = (&next, &converted, &failed);
            let (trashed, before, after) = (&trashed, &before, &after);
            s.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= actionable.len() {
                    break;
                }
                let entry = &actionable[i];
                match transcode_one(&ff, &plan, entry, sink) {
                    Ok(outcome) => {
                        converted.fetch_add(1, Ordering::SeqCst);
                        before.fetch_add(entry.size, Ordering::SeqCst);
                        after.fetch_add(outcome.bytes_after, Ordering::SeqCst);
                        if outcome.trashed {
                            trashed.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                    Err(e) => {
                        sink(&Event::Line(format!("[fail] {}: {e}", entry.src.display())));
                        failed.fetch_add(1, Ordering::SeqCst);
                    }
                }
                sink(&Event::Step(file_name(&entry.src)));
            });
        }
    });

    // "skipped" is what the analysis ruled out (same codec, conflicts);
    // "failed" is what ffmpeg or the filesystem refused at execution time
    Ok(TranscodeSummary {
        converted: converted.load(Ordering::SeqCst),
        skipped,
        failed: failed.load(Ordering::SeqCst),
        trashed: trashed.load(Ordering::SeqCst),
        bytes_before: before.load(Ordering::SeqCst),
        bytes_after: after.load(Ordering::SeqCst),
    })
}

/// The source's lyrics, whichever key its container uses (ID3v2 USLT, Vorbis
/// LYRICS/UNSYNCEDLYRICS, MP4 ©lyr).
fn read_lyrics(src: &Path) -> Option<String> {
    let file = Probe::open(src).ok()?.read().ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag())?;
    for key in [ItemKey::Lyrics, ItemKey::UnsyncLyrics] {
        if let Some(text) = tag.get_string(key).map(str::to_string) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// Copy the source's lyrics into the converted file under the key its
/// container actually defines. Best effort: the audio is already written, so
/// a container that cannot hold lyrics (or a failed tag write) warns instead
/// of failing the conversion.
fn copy_lyrics(src: &Path, tmp: &Path, sink: Sink) {
    let Some(text) = read_lyrics(src) else {
        return;
    };
    let result = (|| -> std::result::Result<(), String> {
        let file = Probe::open(tmp)
            .and_then(|p| p.read())
            .map_err(|e| format!("cannot re-read {}: {e}", tmp.display()))?;
        let tag_type = file.file_type().primary_tag_type();
        // start from the tag ffmpeg just wrote (everything else in it must
        // survive); a container with only a secondary tag (RIFF INFO) gets a
        // fresh primary one rather than a remap that would drop its items
        let mut tag = match file.primary_tag() {
            Some(tag) => tag.clone(),
            None => Tag::new(tag_type),
        };
        tag.remove_key(ItemKey::Lyrics);
        tag.remove_key(ItemKey::UnsyncLyrics);
        // ID3v2 has no plain `Lyrics` item; its unsynced lyrics are USLT
        let key = if tag_type == TagType::Id3v2 {
            ItemKey::UnsyncLyrics
        } else {
            ItemKey::Lyrics
        };
        if !tag.insert_text(key, text) {
            return Err(format!("{tag_type:?} cannot store lyrics"));
        }
        tag.save_to_path(tmp, WriteOptions::default())
            .map_err(|e| e.to_string())
    })();
    if let Err(e) = result {
        sink(&Event::Warn(format!(
            "[warn] lyrics not carried over into {}: {e}",
            file_name(tmp)
        )));
    }
}

struct Outcome {
    bytes_after: u64,
    trashed: bool,
}

fn transcode_one(
    ff: &Ffmpeg,
    plan: &Plan,
    entry: &PlanEntry,
    sink: Sink,
) -> std::result::Result<Outcome, String> {
    let dst = entry
        .dst
        .as_deref()
        .ok_or_else(|| "entry has no destination".to_string())?;
    let replaces_source = dst == entry.src.as_path();
    let codec = codec_plan(plan.target, plan.quality.as_deref(), entry.src_bit_depth)
        .map_err(|e| e.to_string())?;

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let tmp = tmp_path(dst);

    let mut cmd = Command::new(&ff.path);
    cmd.arg("-nostdin")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(&entry.src)
        // metadata as ffmpeg reads it, plus the cover: 0:v? is the attached
        // picture, and RIFF has no slot for one, so wav gets audio only
        .args(["-map_metadata", "0", "-map", "0:a:0"]);
    if plan.target.carries_cover() {
        cmd.args([
            "-map",
            "0:v?",
            "-c:v",
            "copy",
            "-disposition:v",
            "attached_pic",
        ]);
    }
    cmd.arg("-c:a")
        .arg(&codec.encoder)
        .args(&codec.args)
        .args(["-f", plan.target.muxer()])
        // progress on stdout (frame by frame), errors on stderr
        .args(["-progress", "pipe:1", "-nostats"])
        .arg(&tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Err(e) = run_ffmpeg(&mut cmd, entry.duration_secs, sink) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    // ffmpeg's own lyrics mapping is not the one DJ software reads (a USLT
    // copied into flac becomes a LYRICS-<lang> comment, into mp3 a TXXX
    // frame), so the text is re-written in the key each container defines.
    // Still on the temp file: the tags land with the rename.
    copy_lyrics(&entry.src, &tmp, sink);

    // the source is disposed of before the destination is written over it, so
    // a failure here can never lose the only copy of the audio
    let mut did_trash = false;
    if plan.replace {
        match trash::move_into(&entry.src, &plan.trash, "transcoded") {
            Ok(moved) => {
                if let Some(rel) = trash::relative(&plan.trash, &moved) {
                    trash::append(
                        &plan.trash,
                        &[trash::Record {
                            trash: rel,
                            original: trash::original_key(&plan.input, &entry.src),
                            kind: "transcoded".into(),
                            size: entry.size,
                            sha256: None,
                            moved_at: trash::now(),
                        }],
                        sink,
                    );
                }
                did_trash = true;
            }
            Err(e) => sink(&Event::Warn(format!(
                "[warn] cannot move the source into the trash, kept in place: {}: {e}",
                entry.src.display()
            ))),
        }
    }

    if let Err(e) = place(&tmp, dst) {
        let _ = fs::remove_file(&tmp);
        let extra = if did_trash {
            format!(" (the source is in the trash: {})", plan.trash.display())
        } else {
            String::new()
        };
        return Err(format!("cannot write {}: {e}{extra}", dst.display()));
    }
    if replaces_source {
        sink(&Event::Line(format!(
            "[ok] {} re-encoded as {}",
            dst.display(),
            plan.target.label()
        )));
    } else {
        sink(&Event::Line(format!(
            "[ok] {} -> {}",
            entry.src.display(),
            dst.display()
        )));
    }
    Ok(Outcome {
        bytes_after: fs::metadata(dst).map(|m| m.len()).unwrap_or(0),
        trashed: did_trash,
    })
}

/// Hidden scratch file next to the destination (the same convention as
/// import's atomic placement), so a failed encode leaves nothing behind.
fn tmp_path(dst: &Path) -> PathBuf {
    dst.with_file_name(format!(".djms-tmp-{}-{}", std::process::id(), file_name(dst)))
}

/// Move the finished temp file onto the destination. A destination that
/// already exists is only ever behind an explicit replacement (its source is
/// in the trash by then), so removing it here is safe.
fn place(tmp: &Path, dst: &Path) -> std::io::Result<()> {
    match fs::rename(tmp, dst) {
        Ok(()) => Ok(()),
        Err(e) if dst.exists() => fs::remove_file(dst)
            .and_then(|_| fs::rename(tmp, dst))
            .map_err(|e2| {
                std::io::Error::new(
                    e2.kind(),
                    format!("{e}; after removing the destination: {e2}"),
                )
            }),
        Err(e) => Err(e),
    }
}

/// Run ffmpeg, forwarding its `-progress` stream as [`Event::Tick`] so a long
/// encode is not a frozen progress bar, and keeping the last error lines for
/// the failure message.
fn run_ffmpeg(
    cmd: &mut Command,
    duration_secs: Option<u64>,
    sink: Sink,
) -> std::result::Result<(), String> {
    let mut child = cmd.spawn().map_err(|e| format!("cannot run ffmpeg: {e}"))?;
    let stdout = child.stdout.take().ok_or("ffmpeg stdout is not piped")?;
    let stderr = child.stderr.take().ok_or("ffmpeg stderr is not piped")?;
    let errors = thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        text
    });

    let mut last = u8::MAX;
    for line in BufReader::new(stdout)
        .lines()
        .map_while(std::result::Result::ok)
    {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key != "out_time_us" {
            continue;
        }
        let Ok(micros) = value.trim().parse::<u64>() else {
            continue;
        };
        let Some(total) = duration_secs.filter(|t| *t > 0) else {
            continue;
        };
        let pct = (micros.saturating_mul(100) / total.saturating_mul(1_000_000)).min(100) as u8;
        if pct != last {
            last = pct;
            sink(&Event::Tick(pct));
        }
    }

    let status = child.wait().map_err(|e| format!("ffmpeg failed: {e}"))?;
    let errors = errors.join().unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    Err(ffmpeg_error(&errors, &status))
}

/// ffmpeg puts the cause on the last non-empty stderr lines; only those are
/// worth a log line.
fn ffmpeg_error(stderr: &str, status: &std::process::ExitStatus) -> String {
    let tail: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rev()
        .take(3)
        .collect();
    let text = tail.into_iter().rev().collect::<Vec<_>>().join(" | ");
    let text = if text.chars().count() > 400 {
        text.chars().take(400).collect::<String>() + "…"
    } else {
        text
    };
    if text.is_empty() {
        format!("ffmpeg exited with {status}")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "djms-transcode-test-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(&dir).unwrap_or(dir)
    }

    fn no_sink(_: &Event) {}

    fn opts(input: &Path, target: TargetFormat) -> TranscodeOpts {
        TranscodeOpts {
            input: input.to_path_buf(),
            output: None,
            target,
            quality: None,
            replace: false,
            threads: 2,
            ffmpeg: None,
            trash: None,
            plan: None,
            from: None,
        }
    }

    fn find<'a>(plan: &'a Plan, name: &str) -> &'a PlanEntry {
        plan.entries
            .iter()
            .find(|e| file_name(&e.src) == name)
            .unwrap_or_else(|| panic!("no entry for {name}"))
    }

    /// real MPEG frames are parseable by lofty, junk bytes are not
    fn write_mp3(path: &Path, frames: usize) {
        let frame = [0xFFu8, 0xFB, 0x90, 0x00]
            .into_iter()
            .chain(std::iter::repeat_n(0u8, 413))
            .collect::<Vec<u8>>();
        fs::write(path, frame.repeat(frames)).unwrap();
    }

    fn fake_ffmpeg(encoders: &[&str]) -> Ffmpeg {
        Ffmpeg {
            path: PathBuf::from("ffmpeg"),
            version: "test".into(),
            encoders: encoders.iter().map(|e| e.to_string()).collect(),
        }
    }

    #[test]
    fn target_parse_covers_aliases_and_rejects_junk() {
        assert_eq!(TargetFormat::parse("MP3"), Some(TargetFormat::Mp3));
        assert_eq!(TargetFormat::parse("aac"), Some(TargetFormat::M4a));
        assert_eq!(TargetFormat::parse(" alac "), Some(TargetFormat::Alac));
        assert_eq!(TargetFormat::parse("wav"), Some(TargetFormat::Wav));
        assert_eq!(TargetFormat::parse("ogg"), None);
        assert_eq!(TargetFormat::Mp3.ext(), "mp3");
        assert_eq!(TargetFormat::Alac.ext(), "m4a");
        assert_eq!(TargetFormat::Wav.ext(), "wav");
        assert!(!TargetFormat::Mp3.lossless());
        assert!(TargetFormat::Wav.lossless());
    }

    #[test]
    fn preflight_refuses_a_build_without_the_encoder() {
        let partial = fake_ffmpeg(&["flac", "pcm_s16le", "pcm_s24le", "pcm_s32le", "aac", "alac"]);
        assert!(partial.preflight(TargetFormat::Flac).is_ok());
        let err = partial.preflight(TargetFormat::Mp3).unwrap_err();
        assert!(err.to_string().contains("libmp3lame"), "{err}");
        assert!(fake_ffmpeg(&[]).preflight(TargetFormat::Wav).is_err());
    }

    #[test]
    fn codec_plan_applies_defaults_and_target_specific_quality() {
        let mp3 = codec_plan(TargetFormat::Mp3, None, None).unwrap();
        assert_eq!(mp3.encoder, "libmp3lame");
        assert_eq!(mp3.args, ["-q:a", "0"]);
        assert_eq!(
            codec_plan(TargetFormat::Mp3, Some("320k"), None).unwrap().args,
            ["-b:a", "320k"]
        );
        assert_eq!(
            codec_plan(TargetFormat::Mp3, Some("v2"), None).unwrap().args,
            ["-q:a", "2"]
        );
        assert_eq!(
            codec_plan(TargetFormat::M4a, None, None).unwrap().args,
            ["-b:a", "256k"]
        );
        assert_eq!(
            codec_plan(TargetFormat::M4a, Some("512k"), None).unwrap().args,
            ["-b:a", "512k"]
        );
        assert_eq!(
            codec_plan(TargetFormat::Flac, None, None).unwrap().args,
            ["-compression_level", "8"]
        );
        assert_eq!(
            codec_plan(TargetFormat::Alac, None, None).unwrap().encoder,
            "alac"
        );

        // wav keeps the source bit depth instead of silently truncating 24-bit
        assert_eq!(
            codec_plan(TargetFormat::Wav, None, Some(16)).unwrap().encoder,
            "pcm_s16le"
        );
        assert_eq!(
            codec_plan(TargetFormat::Wav, None, Some(24)).unwrap().encoder,
            "pcm_s24le"
        );
        assert_eq!(
            codec_plan(TargetFormat::Wav, None, Some(32)).unwrap().encoder,
            "pcm_s32le"
        );
        assert_eq!(
            codec_plan(TargetFormat::Wav, None, None).unwrap().encoder,
            "pcm_s16le"
        );

        // quality knobs that do not exist are rejected, never ignored
        for bad in [
            codec_plan(TargetFormat::Alac, Some("320k"), None),
            codec_plan(TargetFormat::Wav, Some("8"), None),
            codec_plan(TargetFormat::Mp3, Some("v99"), None),
            codec_plan(TargetFormat::Mp3, Some("v"), None),
            codec_plan(TargetFormat::Flac, Some("13"), None),
            codec_plan(TargetFormat::M4a, Some("loud"), None),
            codec_plan(TargetFormat::M4a, Some("4k"), None),
        ] {
            assert!(matches!(bad, Err(Error::Usage(_))), "accepted a bad quality");
        }
    }

    #[test]
    fn same_codec_needs_container_and_encoder_to_agree() {
        // mirrors target_path: the destination carries the *target* extension
        let status = |target: TargetFormat, src: &SourceCodec| {
            let src_path = PathBuf::from(format!("/lib/x.{}", match src {
                SourceCodec::Mp3 => "mp3",
                SourceCodec::Aac => "m4a",
                SourceCodec::Alac => "m4a",
                SourceCodec::Flac => "flac",
                SourceCodec::Pcm(_) => "wav",
                SourceCodec::Other(_) => "ape",
            }));
            let dst = src_path.with_extension(target.ext());
            let info = SourceInfo {
                codec: Some(src.clone()),
                duration_secs: None,
                has_cover: false,
            };
            classify(&src_path, dst, target, false, &info).status
        };
        // plain matches are skipped
        assert_eq!(status(TargetFormat::Mp3, &SourceCodec::Mp3), "same-codec");
        assert_eq!(status(TargetFormat::Flac, &SourceCodec::Flac), "same-codec");
        assert_eq!(status(TargetFormat::M4a, &SourceCodec::Aac), "same-codec");
        assert_eq!(status(TargetFormat::Alac, &SourceCodec::Alac), "same-codec");
        assert_eq!(
            status(TargetFormat::Wav, &SourceCodec::Pcm(Some(24))),
            "same-codec"
        );
        // an ALAC .m4a is not an AAC .m4a: same extension, different codec,
        // and the conversion happens in place
        assert_eq!(status(TargetFormat::M4a, &SourceCodec::Alac), "needs-replace");
        assert_eq!(status(TargetFormat::Alac, &SourceCodec::Aac), "needs-replace");
        // lossy -> lossless converts, it is just not an upgrade
        assert_eq!(status(TargetFormat::Flac, &SourceCodec::Mp3), "ready");
        assert_eq!(status(TargetFormat::Wav, &SourceCodec::Flac), "ready");
        assert_eq!(
            status(TargetFormat::Flac, &SourceCodec::Other("APE".into())),
            "ready"
        );
    }

    #[test]
    fn lossy_to_lossless_is_flagged_but_not_blocked() {
        let info = SourceInfo {
            codec: Some(SourceCodec::Mp3),
            duration_secs: Some(200),
            has_cover: false,
        };
        let entry = classify(
            Path::new("/lib/a.mp3"),
            PathBuf::from("/lib/a.flac"),
            TargetFormat::Flac,
            false,
            &info,
        );
        assert_eq!(entry.status, "ready");
        assert!(entry.note.unwrap().contains("no quality gain"));
        assert_eq!(entry.duration_secs, Some(200));
        assert_eq!(entry.src_codec.as_deref(), Some("MP3"));
    }

    #[test]
    fn analyze_classifies_ready_same_codec_and_conflict() {
        let root = tmpdir("analyze");
        write_mp3(&root.join("song.mp3"), 20);
        fs::write(root.join("junk.flac"), b"not audio").unwrap();
        fs::write(root.join("other.wav"), b"junk").unwrap();
        fs::write(root.join("other.mp3"), b"junk").unwrap();

        let plan = build_plan(&opts(&root, TargetFormat::Mp3), &no_sink).unwrap();
        assert_eq!(plan.count("same-codec"), 2); // song.mp3 + other.mp3 (junk)
        assert_eq!(plan.count("ready"), 1);
        assert_eq!(plan.count("conflict"), 1);
        let junk = find(&plan, "junk.flac");
        assert_eq!(junk.dst.as_deref(), Some(root.join("junk.mp3").as_path()));
        assert_eq!(junk.op, "transcode");
        assert_eq!(find(&plan, "other.wav").status, "conflict");
        assert_eq!(plan.trash, root.join(".dedup-trash"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn analyze_marks_same_codec_by_extension_when_lofty_cannot_read_it() {
        let root = tmpdir("unreadable");
        // neither file is a real container: the extension is all there is
        fs::write(root.join("honest.mp3"), b"junk").unwrap();
        fs::write(root.join("mystery.ape"), b"junk").unwrap();
        let plan = build_plan(&opts(&root, TargetFormat::Mp3), &no_sink).unwrap();
        let honest = find(&plan, "honest.mp3");
        assert_eq!(honest.status, "same-codec");
        assert!(honest.note.as_deref().unwrap().contains("assumed"));
        let mystery = find(&plan, "mystery.ape");
        assert_eq!(mystery.status, "ready");
        assert!(mystery.note.as_deref().unwrap().contains("may be untagged"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn analyze_preserves_the_tree_under_the_output_folder() {
        let root = tmpdir("tree");
        fs::create_dir_all(root.join("in/disc1")).unwrap();
        let out = root.join("out");
        let mut o = opts(&root.join("in"), TargetFormat::Flac);
        o.output = Some(out.clone());
        fs::write(root.join("in/disc1/a.wav"), b"x").unwrap();
        let plan = build_plan(&o, &no_sink).unwrap();
        assert_eq!(
            find(&plan, "a.wav").dst.as_deref(),
            Some(out.join("disc1/a.flac").as_path())
        );
        assert_eq!(plan.output.as_deref(), Some(out.as_path()));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn analyze_rejects_a_quality_the_target_has_no_knob_for() {
        let root = tmpdir("quality");
        fs::write(root.join("a.flac"), b"x").unwrap();
        let mut o = opts(&root, TargetFormat::Wav);
        o.quality = Some("320k".into());
        assert!(matches!(build_plan(&o, &no_sink), Err(Error::Usage(_))));
        let mut o = opts(&root, TargetFormat::Mp3);
        o.quality = Some("v99".into());
        assert!(matches!(build_plan(&o, &no_sink), Err(Error::Usage(_))));

        let empty = tmpdir("quality-empty");
        assert!(matches!(
            build_plan(&opts(&empty, TargetFormat::Mp3), &no_sink),
            Err(Error::Runtime(_))
        ));
        let file = root.join("not-a-dir");
        let mut o = opts(&root, TargetFormat::Mp3);
        o.input = file.clone();
        assert!(matches!(
            build_plan(&o, &no_sink),
            Err(Error::Usage(_))
        ));
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&empty).unwrap();
    }

    fn ffmpeg_available() -> bool {
        Ffmpeg::detect(None).is_ok()
    }

    fn run_ffmpeg_or_panic(args: &[&str]) {
        let status = Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg {args:?} failed");
    }

    /// Synthesize a tagged, cover-carrying flac with ffmpeg itself. The lyrics
    /// use the standard `LYRICS` comment (what a real library carries), not
    /// ffmpeg's own nonstandard `LYRICS-<lang>` key.
    fn make_tagged_flac(dir: &Path) -> PathBuf {
        let cover = dir.join("cover.jpg");
        run_ffmpeg_or_panic(&[
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=64x64:d=1",
            "-frames:v",
            "1",
            cover.to_str().unwrap(),
        ]);
        let flac = dir.join("tagged.flac");
        run_ffmpeg_or_panic(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=2",
            "-i",
            cover.to_str().unwrap(),
            "-map",
            "0:a:0",
            "-map",
            "1:v:0",
            "-c:a",
            "flac",
            "-c:v",
            "copy",
            "-disposition:v",
            "attached_pic",
            "-metadata",
            "title=Test Title",
            "-metadata",
            "artist=Test Artist",
            "-metadata",
            "album=Test Album",
            "-metadata",
            "lyrics=[00:00.000]line one",
            flac.to_str().unwrap(),
        ]);
        flac
    }

    /// The shape the suite's own `convert` produces: an mp3 with USLT lyrics
    /// and an APIC cover.
    fn make_tagged_mp3(dir: &Path) -> PathBuf {
        let cover = dir.join("cover.jpg");
        if !cover.is_file() {
            run_ffmpeg_or_panic(&[
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=64x64:d=1",
                "-frames:v",
                "1",
                cover.to_str().unwrap(),
            ]);
        }
        let mp3 = dir.join("tagged.mp3");
        run_ffmpeg_or_panic(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=550:duration=2",
            "-i",
            cover.to_str().unwrap(),
            "-map",
            "0:a:0",
            "-map",
            "1:v:0",
            "-c:a",
            "libmp3lame",
            "-q:a",
            "4",
            "-c:v",
            "copy",
            "-disposition:v",
            "attached_pic",
            "-metadata",
            "title=Source Title",
            "-metadata",
            "artist=Source Artist",
            "-metadata",
            "lyrics=[00:00.000]line one",
            mp3.to_str().unwrap(),
        ]);
        mp3
    }

    /// The whole point of the command: the audio is re-encoded *and* the tags,
    /// lyrics and cover survive the trip into the other container.
    #[test]
    fn ffmpeg_end_to_end_keeps_tags_lyrics_and_cover() {
        if !ffmpeg_available() {
            eprintln!("skipping: no ffmpeg on PATH");
            return;
        }
        let root = tmpdir("e2e");
        let src = make_tagged_flac(&root);
        let mut o = opts(&root, TargetFormat::Mp3);
        o.output = Some(root.join("out"));
        o.quality = Some("v2".into());
        let plan = analyze(&o, &no_sink).unwrap();
        assert_eq!(plan.count("ready"), 1);
        let entry = find(&plan, "tagged.flac").clone();
        assert_eq!(entry.src_codec.as_deref(), Some("FLAC"));
        assert!(entry.note.is_none());

        let summary = execute_plan(&plan, 1, None, &no_sink).unwrap();
        assert_eq!(summary.converted, 1);
        assert_eq!(summary.failed, 0);
        assert!(summary.bytes_after > 0);

        let out = plan.output.clone().unwrap().join("tagged.mp3");
        assert!(out.is_file());
        let tagged = Probe::open(&out).unwrap().read().unwrap();
        assert_eq!(tagged.file_type(), FileType::Mpeg);
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("Test Title"));
        assert_eq!(tag.artist().as_deref(), Some("Test Artist"));
        assert_eq!(tag.album().as_deref(), Some("Test Album"));
        // ID3v2 spells unsynced lyrics USLT — the key DJ software reads
        let lyrics = tag.get_string(ItemKey::UnsyncLyrics).unwrap_or_default();
        assert!(lyrics.contains("line one"), "lyrics lost: {lyrics:?}");
        assert!(!tag.pictures().is_empty(), "cover lost");

        // no scratch files left behind, and the source is untouched
        let leftovers: Vec<String> = fs::read_dir(plan.output.as_deref().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".djms-tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
        assert!(src.is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    /// The same trip into an MP4 container: cover, tags and lyrics under the
    /// MP4 keys, and the codec really changes.
    #[test]
    fn ffmpeg_end_to_end_into_alac_carries_cover_and_tags() {
        if !ffmpeg_available() {
            eprintln!("skipping: no ffmpeg on PATH");
            return;
        }
        let root = tmpdir("e2e-m4a");
        make_tagged_flac(&root);
        let mut o = opts(&root, TargetFormat::Alac);
        o.output = Some(root.join("out"));
        let plan = analyze(&o, &no_sink).unwrap();
        let summary = execute_plan(&plan, 1, None, &no_sink).unwrap();
        assert_eq!(summary.converted, 1);
        assert_eq!(summary.failed, 0);

        let out = plan.output.unwrap().join("tagged.m4a");
        let tagged = Probe::open(&out).unwrap().read().unwrap();
        assert_eq!(tagged.file_type(), FileType::Mp4);
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("Test Title"));
        assert_eq!(tag.artist().as_deref(), Some("Test Artist"));
        assert!(tag
            .get_string(ItemKey::Lyrics)
            .unwrap_or_default()
            .contains("line one"));
        assert!(!tag.pictures().is_empty(), "cover lost");
        assert_eq!(mp4_codec(&out), Some(SourceCodec::Alac));
        fs::remove_dir_all(&root).unwrap();
    }

    /// The direction the suite's own files take: an mp3 whose lyrics are USLT
    /// becomes a flac whose lyrics are a real Vorbis `LYRICS` comment.
    #[test]
    fn ffmpeg_uslt_lyrics_become_vorbis_lyrics() {
        if !ffmpeg_available() {
            eprintln!("skipping: no ffmpeg on PATH");
            return;
        }
        let root = tmpdir("mp3-flac");
        make_tagged_mp3(&root);
        let mut o = opts(&root, TargetFormat::Flac);
        o.output = Some(root.join("out"));
        let plan = analyze(&o, &no_sink).unwrap();
        let entry = find(&plan, "tagged.mp3").clone();
        assert_eq!(entry.src_codec.as_deref(), Some("MP3"));
        let summary = execute_plan(&plan, 1, None, &no_sink).unwrap();
        assert_eq!(summary.converted, 1);
        assert_eq!(summary.failed, 0);

        let out = plan.output.unwrap().join("tagged.flac");
        let tagged = Probe::open(&out).unwrap().read().unwrap();
        assert_eq!(tagged.file_type(), FileType::Flac);
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("Source Title"));
        assert_eq!(tag.artist().as_deref(), Some("Source Artist"));
        assert!(tag
            .get_string(ItemKey::Lyrics)
            .unwrap_or_default()
            .contains("line one"));
        assert!(!tag.pictures().is_empty(), "cover lost");
        fs::remove_dir_all(&root).unwrap();
    }

    /// WAV cannot hold an attached picture: it must convert with the audio
    /// only, and the analysis says so up front instead of dropping it quietly.
    #[test]
    fn ffmpeg_wav_target_drops_the_cover_with_a_note() {
        if !ffmpeg_available() {
            eprintln!("skipping: no ffmpeg on PATH");
            return;
        }
        let root = tmpdir("wav");
        make_tagged_flac(&root);
        let plan = build_plan(&opts(&root, TargetFormat::Wav), &no_sink).unwrap();
        let entry = find(&plan, "tagged.flac");
        let note = entry.note.clone().unwrap_or_default();
        assert!(note.contains("cover art is dropped"), "{note}");

        let mut o = opts(&root, TargetFormat::Wav);
        o.replace = true;
        let plan = build_plan(&o, &no_sink).unwrap();
        let summary = execute_plan(&plan, 1, None, &no_sink).unwrap();
        assert_eq!(summary.converted, 1);
        assert_eq!(summary.failed, 0);
        assert!(root.join("tagged.wav").is_file());
        fs::remove_dir_all(&root).unwrap();
    }

    /// `--replace` is the promise that nothing is lost: the source ends up in
    /// the trash with a manifest record, so `trash --restore` can put it back.
    #[test]
    fn ffmpeg_replace_moves_the_source_into_the_trash() {
        if !ffmpeg_available() {
            eprintln!("skipping: no ffmpeg on PATH");
            return;
        }
        let root = tmpdir("replace");
        let src = root.join("song.flac");
        run_ffmpeg_or_panic(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=330:duration=1",
            src.to_str().unwrap(),
        ]);
        let original = fs::read(&src).unwrap();

        let mut o = opts(&root, TargetFormat::Mp3);
        o.replace = true;
        let plan = analyze(&o, &no_sink).unwrap();
        let entry = find(&plan, "song.flac").clone();
        assert_eq!(entry.status, "ready");
        assert_eq!(entry.op, "transcode");
        assert_eq!(plan.trash, root.join(".dedup-trash"));

        let summary = execute_plan(&plan, 1, None, &no_sink).unwrap();
        assert_eq!(summary.converted, 1);
        assert_eq!(summary.trashed, 1);
        assert!(!src.exists());
        assert!(root.join("song.mp3").is_file());

        // restorable, byte for byte
        let listed = trash::list(&plan.input, &plan.trash);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].original.as_deref(), Some(src.as_path()));
        assert_eq!(listed[0].kind, "transcoded");
        let rel = listed[0].rel.clone();
        trash::restore(&plan.input, &plan.trash, &[rel], &no_sink).unwrap();
        assert_eq!(fs::read(&src).unwrap(), original);
        fs::remove_dir_all(&root).unwrap();
    }

    /// Replacing in place (m4a ALAC -> m4a AAC) must never leave the file half
    /// written: the old one is already in the trash, the new one arrives by
    /// rename.
    #[test]
    fn ffmpeg_in_place_replacement_needs_and_keeps_the_trash_copy() {
        if !ffmpeg_available() {
            eprintln!("skipping: no ffmpeg on PATH");
            return;
        }
        let root = tmpdir("inplace");
        let src = root.join("track.m4a");
        run_ffmpeg_or_panic(&[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=220:duration=1",
            "-c:a",
            "alac",
            src.to_str().unwrap(),
        ]);

        // without replace the analysis refuses to touch the file at all
        let plan = build_plan(&opts(&root, TargetFormat::M4a), &no_sink).unwrap();
        assert_eq!(find(&plan, "track.m4a").status, "needs-replace");
        assert_eq!(execute_plan(&plan, 1, None, &no_sink).unwrap().converted, 0);

        let mut o = opts(&root, TargetFormat::M4a);
        o.replace = true;
        let plan = analyze(&o, &no_sink).unwrap();
        let entry = find(&plan, "track.m4a");
        assert_eq!(entry.status, "ready");
        assert_eq!(entry.op, "replace");
        assert_eq!(entry.src_codec.as_deref(), Some("ALAC"));
        let summary = execute_plan(&plan, 1, None, &no_sink).unwrap();
        assert_eq!(summary.converted, 1);
        assert_eq!(summary.trashed, 1);

        let tagged = Probe::open(&src).unwrap().read().unwrap();
        assert_eq!(tagged.file_type(), FileType::Mp4);
        assert_eq!(mp4_codec(&src), Some(SourceCodec::Aac));
        fs::remove_dir_all(&root).unwrap();
    }
}
