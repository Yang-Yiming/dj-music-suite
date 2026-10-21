//! Find and embed missing album artwork.
//!
//! The CLI's `cover` command is deliberately two-phase: [`analyze`] reads the
//! files and searches metadata providers, then writes a reviewable plan; only
//! [`execute`] touches the audio files. Network queries never include audio.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{Picture, PictureType};
use lofty::prelude::*;
use lofty::probe::Probe;
use lofty::tag::Tag;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::scan::{file_name, scan_audio};
use crate::{config, usage, Event, Result, Sink};

pub const DEFAULT_MIN_CONFIDENCE: f32 = 0.88;
const READY_MARGIN: f32 = 0.05;
const SEARCH_LIMIT: usize = 10;
const MAX_IMAGE_BYTES: usize = 12 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4096;
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const ARTWORK_FAILURE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
static HTTP_CLIENT: OnceLock<std::result::Result<Client, String>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    Auto,
    #[serde(rename = "musicbrainz")]
    MusicBrainz,
    Itunes,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Auto => "auto",
            Provider::MusicBrainz => "MusicBrainz",
            Provider::Itunes => "iTunes",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Existing,
    Ready,
    Review,
    NotFound,
    Unsupported,
    Error,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Existing => "existing",
            Status::Ready => "ready",
            Status::Review => "review",
            Status::NotFound => "not-found",
            Status::Unsupported => "unsupported",
            Status::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub provider: Provider,
    pub provider_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    pub artwork_url: String,
    pub score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_nanos: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    pub has_cover: bool,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<Candidate>,
    /// Index into `candidates`; only set for high-confidence matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Entry {
    pub fn selected_candidate(&self) -> Option<&Candidate> {
        self.selected.and_then(|i| self.candidates.get(i))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    pub input: PathBuf,
    pub provider: Provider,
    pub min_confidence: f32,
    pub overwrite: bool,
    pub country: String,
    pub entries: Vec<Entry>,
}

impl Plan {
    pub fn count(&self, status: Status) -> usize {
        self.entries.iter().filter(|e| e.status == status).count()
    }

    pub fn actionable(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.status == Status::Ready)
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub embedded: usize,
    pub replaced: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Per-run choices supplied by the web UI. The CLI can use [`execute`]; the UI
/// sends an explicit subset and may override a review candidate selected by
/// the user in the preview table.
#[derive(Clone, Debug, Default)]
pub struct ExecuteOptions {
    /// Paths to process. None means every actionable entry in the plan.
    pub include: Option<BTreeSet<PathBuf>>,
    /// Path -> candidate index chosen by the user, including review rows.
    pub selections: BTreeMap<PathBuf, usize>,
}

pub struct CoverOpts {
    /// A file or directory. None uses the library root configured by the web UI.
    pub input: Option<PathBuf>,
    pub provider: Provider,
    pub min_confidence: f32,
    pub overwrite: bool,
    pub country: String,
    /// Write the reviewable plan here. None is useful for embedding/tests.
    pub plan: Option<PathBuf>,
    /// Override the artwork/search cache directory (tests use a temp dir).
    pub cache_dir: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct Track {
    artist: Option<String>,
    title: Option<String>,
    album: Option<String>,
    duration_secs: Option<u64>,
    has_cover: bool,
}

#[derive(Clone, Debug)]
struct Query {
    artist: Option<String>,
    title: Option<String>,
    album: Option<String>,
    duration_secs: Option<u64>,
}

#[derive(Clone, Debug)]
struct Group {
    indices: Vec<usize>,
    query: Query,
    album_group: bool,
}

struct Session {
    client: Client,
    cache_dir: PathBuf,
    country: String,
    last_musicbrainz: Option<Instant>,
}

pub fn analyze(opts: &CoverOpts, sink: Sink) -> Result<Plan> {
    if opts.min_confidence <= 0.0 || opts.min_confidence > 1.0 {
        return Err(usage(
            "--min-confidence must be greater than 0 and at most 1",
        ));
    }
    let input = resolve_input(opts.input.as_deref())?;
    let files = collect_files(&input)?;
    if files.is_empty() {
        return Err(crate::Error::Runtime(format!(
            "no supported audio files found in {}",
            input.display()
        )));
    }
    let cache_dir = opts.cache_dir.clone().unwrap_or_else(cache_dir);
    let mut session =
        Session::new(cache_dir, opts.country.clone()).map_err(crate::Error::Runtime)?;

    sink(&Event::Start(files.len() as u64));
    let mut entries = Vec::with_capacity(files.len());
    for path in &files {
        let entry = analyze_file(path, opts.overwrite);
        if let Some(note) = &entry.note {
            if entry.status == Status::Error {
                sink(&Event::Warn(format!("[warn] {}: {note}", path.display())));
            }
        }
        entries.push(entry);
        sink(&Event::Step(file_name(path)));
    }

    let groups = build_groups(&entries);
    sink(&Event::Start(groups.len() as u64));
    for group in &groups {
        let label = group
            .query
            .album
            .as_deref()
            .or(group.query.title.as_deref())
            .unwrap_or("?");
        sink(&Event::Line(format!("[cover] searching {label}")));
        let candidates = search_group(&mut session, group, opts, sink);
        apply_group_result(&mut entries, group, &candidates, &mut session, opts, sink);
        sink(&Event::Step(label.to_string()));
    }

    let plan = Plan {
        input,
        provider: opts.provider,
        min_confidence: opts.min_confidence,
        overwrite: opts.overwrite,
        country: opts.country.clone(),
        entries,
    };
    if let Some(path) = &opts.plan {
        write_plan(&plan, path, sink)?;
    }
    Ok(plan)
}

pub fn execute(plan: &Plan, sink: Sink) -> Result<Summary> {
    execute_with_options(plan, &ExecuteOptions::default(), sink)
}

pub fn execute_with_options(plan: &Plan, options: &ExecuteOptions, sink: Sink) -> Result<Summary> {
    let actionable: Vec<&Entry> = plan
        .entries
        .iter()
        .filter(|entry| {
            options
                .include
                .as_ref()
                .is_none_or(|include| include.contains(&entry.path))
        })
        .collect();
    sink(&Event::Start(actionable.len() as u64));
    let cache_dir = cache_dir();
    let session = Session::new(cache_dir, plan.country.clone())
        .map_err(|e| crate::Error::Runtime(e.to_string()))?;

    let mut summary = Summary::default();
    for entry in actionable {
        let explicit_selection = options.selections.get(&entry.path).copied();
        if entry.status != Status::Ready && explicit_selection.is_none() {
            summary.skipped += 1;
            continue;
        }
        let Some(candidate) = explicit_selection
            .or(entry.selected)
            .and_then(|index| entry.candidates.get(index))
        else {
            if explicit_selection.is_some() {
                summary.failed += 1;
                sink(&Event::Warn(format!(
                    "[fail] {}: selected candidate is out of range",
                    entry.path.display()
                )));
            } else {
                summary.skipped += 1;
            }
            sink(&Event::Step(file_name(&entry.path)));
            continue;
        };

        if file_changed(entry) {
            sink(&Event::Warn(format!(
                "[skip] {} changed since the analysis; run cover again",
                entry.path.display()
            )));
            summary.skipped += 1;
            sink(&Event::Step(file_name(&entry.path)));
            continue;
        }

        let current_has_cover = probe_track(&entry.path)
            .map(|t| t.has_cover)
            .unwrap_or(entry.has_cover);
        if current_has_cover && !plan.overwrite {
            sink(&Event::Line(format!(
                "[skip] {} already has a cover",
                entry.path.display()
            )));
            summary.skipped += 1;
            sink(&Event::Step(file_name(&entry.path)));
            continue;
        }

        let artwork = match session.artwork(candidate) {
            Ok(art) => art,
            Err(e) => {
                sink(&Event::Warn(format!(
                    "[fail] {}: cannot fetch artwork: {e}",
                    entry.path.display()
                )));
                summary.failed += 1;
                sink(&Event::Step(file_name(&entry.path)));
                continue;
            }
        };

        match write_cover(&entry.path, &artwork.bytes) {
            Ok(()) => {
                if current_has_cover {
                    summary.replaced += 1;
                    sink(&Event::Line(format!(
                        "[ok] replaced cover in {}",
                        entry.path.display()
                    )));
                } else {
                    summary.embedded += 1;
                    sink(&Event::Line(format!(
                        "[ok] embedded cover in {}",
                        entry.path.display()
                    )));
                }
            }
            Err(e) => {
                summary.failed += 1;
                sink(&Event::Warn(format!(
                    "[fail] {}: {e}",
                    entry.path.display()
                )));
            }
        }
        sink(&Event::Step(file_name(&entry.path)));
    }
    Ok(summary)
}

/// Fetch (or reuse from the artwork cache) one candidate from an analyzed
/// plan. The web UI uses this as a local image proxy, so the browser only
/// talks to 127.0.0.1 and arbitrary URLs can never be supplied by the caller.
pub fn preview_artwork(plan: &Plan, path: &Path, candidate: usize) -> Result<Artwork> {
    let entry = plan
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| {
            usage(format!(
                "file is not in this cover plan: {}",
                path.display()
            ))
        })?;
    let candidate = entry.candidates.get(candidate).ok_or_else(|| {
        usage(format!(
            "candidate {candidate} is out of range for {}",
            path.display()
        ))
    })?;
    let session = Session::new(cache_dir(), plan.country.clone()).map_err(crate::Error::Runtime)?;
    session.artwork(candidate).map_err(crate::Error::Runtime)
}

pub fn load_plan(path: &Path) -> Result<Plan> {
    let raw = fs::read_to_string(path)
        .map_err(|e| usage(format!("cannot read plan {}: {e}", path.display())))?;
    serde_json::from_str(&raw).map_err(|e| usage(format!("bad plan file {}: {e}", path.display())))
}

fn write_plan(plan: &Plan, path: &Path, sink: Sink) -> Result<()> {
    let raw = serde_json::to_string_pretty(plan)
        .map_err(|e| crate::Error::Runtime(format!("cannot serialize cover plan: {e}")))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| usage(format!("cannot create {}: {e}", parent.display())))?;
    }
    fs::write(path, raw)
        .map_err(|e| crate::Error::Runtime(format!("cannot write {}: {e}", path.display())))?;
    sink(&Event::Line(format!("[plan] {}", path.display())));
    Ok(())
}

fn resolve_input(input: Option<&Path>) -> Result<PathBuf> {
    let raw = match input {
        Some(path) => path.to_path_buf(),
        None => config::resolve_library_root(None)?,
    };
    fs::canonicalize(&raw)
        .map_err(|e| usage(format!("cannot resolve input {}: {e}", raw.display())))
}

fn collect_files(input: &Path) -> Result<Vec<PathBuf>> {
    if input.is_file() {
        return Ok(vec![input.to_path_buf()]);
    }
    if !input.is_dir() {
        return Err(usage(format!(
            "input is not a file or directory: {}",
            input.display()
        )));
    }
    Ok(scan_audio(input))
}

fn analyze_file(path: &Path, overwrite: bool) -> Entry {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mtime_nanos = modified_nanos(path).unwrap_or(0);
    let track = match probe_track(path) {
        Ok(track) => track,
        Err(e) => {
            return Entry {
                path: path.to_path_buf(),
                size,
                mtime_nanos,
                artist: None,
                title: None,
                album: None,
                duration_secs: None,
                has_cover: false,
                status: Status::Error,
                candidates: Vec::new(),
                selected: None,
                note: Some(e),
            }
        }
    };
    let supported = supported_format(path);
    let status = if track.has_cover && !overwrite {
        Status::Existing
    } else if !supported {
        Status::Unsupported
    } else {
        Status::NotFound
    };
    let note = match status {
        Status::Existing => None,
        Status::Unsupported => {
            Some("cover writing currently supports mp3, flac and m4a".to_string())
        }
        _ if track.title.is_none() && track.album.is_none() => {
            Some("missing both title and album; cannot build a search query".to_string())
        }
        _ if track.artist.is_none() => {
            Some("missing artist; candidates will require manual review".to_string())
        }
        _ => None,
    };
    Entry {
        path: path.to_path_buf(),
        size,
        mtime_nanos,
        artist: track.artist,
        title: track.title,
        album: track.album,
        duration_secs: track.duration_secs,
        has_cover: track.has_cover,
        status,
        candidates: Vec::new(),
        selected: None,
        note,
    }
}

fn build_groups(entries: &[Entry]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut album_groups: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if !matches!(entry.status, Status::NotFound) {
            continue;
        }
        let Some(_title) = entry.title.as_deref().or(entry.album.as_deref()) else {
            continue;
        };
        let query = Query {
            artist: entry.artist.clone(),
            // An album-level search is about the release, not the first track.
            // Keeping the track title here would make every non-title track
            // look like a worse match for its own album cover.
            title: if entry.album.is_some() {
                entry.album.clone()
            } else {
                entry.title.clone()
            },
            album: entry.album.clone(),
            duration_secs: entry.duration_secs,
        };
        if let (Some(artist), Some(album)) = (&entry.artist, &entry.album) {
            let key = (normalize(artist), normalize(album));
            if let Some(&group_index) = album_groups.get(&key) {
                groups[group_index].indices.push(index);
                continue;
            }
            album_groups.insert(key, groups.len());
            groups.push(Group {
                indices: vec![index],
                query,
                album_group: true,
            });
        } else {
            groups.push(Group {
                indices: vec![index],
                query,
                album_group: false,
            });
        }
    }
    groups
}

fn apply_group_result(
    entries: &mut [Entry],
    group: &Group,
    candidates: &[Candidate],
    session: &mut Session,
    opts: &CoverOpts,
    sink: Sink,
) {
    let mut candidates = candidates.to_vec();
    sort_and_dedupe(&mut candidates);
    let mut top_ready = candidates
        .first()
        .is_some_and(|c| candidate_is_ready(&group.query, c, opts.min_confidence));
    let mut ambiguous = candidates_ambiguous(&group.query, &candidates);

    let mut selected = None;
    let mut note = None;
    if top_ready && !ambiguous {
        selected = select_available(
            &group.query,
            &candidates,
            opts.min_confidence,
            session,
            &mut note,
        );
    } else if ambiguous {
        note = Some(
            "multiple plausible releases; run analyze again after choosing a candidate".into(),
        );
    }

    // A metadata match is useless when its artwork endpoint is unavailable
    // (missing image, Cover Art Archive rate limit, ...). With provider=auto,
    // ask iTunes before giving up. Search results are cached, so the fallback
    // normally costs one request per album.
    if selected.is_none() && opts.provider == Provider::Auto {
        match session.search_itunes(&group.query, group.album_group) {
            Ok(mut found) => candidates.append(&mut found),
            Err(e) => sink(&Event::Warn(format!("[warn] iTunes fallback failed: {e}"))),
        }
        sort_and_dedupe(&mut candidates);
        top_ready = candidates
            .first()
            .is_some_and(|c| candidate_is_ready(&group.query, c, opts.min_confidence));
        ambiguous = candidates_ambiguous(&group.query, &candidates);
        if top_ready && !ambiguous {
            selected = select_available(
                &group.query,
                &candidates,
                opts.min_confidence,
                session,
                &mut note,
            );
        }
    }

    if selected.is_some() {
        note = None;
    }

    let status = if selected.is_some() {
        Status::Ready
    } else if candidates.is_empty() || candidates[0].score < 0.65 {
        Status::NotFound
    } else {
        Status::Review
    };

    for &index in &group.indices {
        let entry = &mut entries[index];
        entry.candidates = candidates.clone();
        entry.selected = selected.filter(|_| status == Status::Ready);
        entry.status = status;
        entry.note = note.clone().or_else(|| {
            if status == Status::NotFound {
                Some("no matching album artwork found".into())
            } else {
                None
            }
        });
        if status == Status::Ready {
            if let Some(candidate) = entry.selected_candidate() {
                sink(&Event::Line(format!(
                    "[match] {} -> {} / {} ({:.2}, {})",
                    file_name(&entry.path),
                    candidate.artist.as_deref().unwrap_or("?"),
                    candidate.album.as_deref().unwrap_or("?"),
                    candidate.score,
                    candidate.provider.label()
                )));
            }
        }
    }
}

fn select_available(
    query: &Query,
    candidates: &[Candidate],
    min_confidence: f32,
    session: &Session,
    note: &mut Option<String>,
) -> Option<usize> {
    for (index, candidate) in candidates.iter().take(3).enumerate() {
        if !candidate_is_ready(query, candidate, min_confidence) {
            continue;
        }
        match session.artwork(candidate) {
            Ok(_) => return Some(index),
            Err(e) => *note = Some(format!("artwork download failed: {e}")),
        }
    }
    None
}

fn candidates_ambiguous(query: &Query, candidates: &[Candidate]) -> bool {
    let (Some(first), Some(second)) = (candidates.first(), candidates.get(1)) else {
        return false;
    };
    if first.score - second.score >= READY_MARGIN {
        return false;
    }
    // Two providers describing the same release are not an ambiguity. This is
    // common when MusicBrainz and iTunes both return the same album.
    let same_artist = similarity(first.artist.as_deref(), second.artist.as_deref()) >= 0.95;
    let same_album = similarity(first.album.as_deref(), second.album.as_deref()) >= 0.90;
    let same_subject = if query.album.is_some() {
        same_album
    } else {
        similarity(first.title.as_deref(), second.title.as_deref()) >= 0.95
    };
    !(same_artist && same_subject)
}

fn search_group(
    session: &mut Session,
    group: &Group,
    opts: &CoverOpts,
    sink: Sink,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    if opts.provider != Provider::Itunes {
        match session.search_musicbrainz(&group.query, group.album_group) {
            Ok(mut found) => candidates.append(&mut found),
            Err(e) => sink(&Event::Warn(format!(
                "[warn] MusicBrainz search failed: {e}"
            ))),
        }
        sort_and_dedupe(&mut candidates);
        if opts.provider == Provider::MusicBrainz
            || has_ready(&group.query, &candidates, opts.min_confidence)
        {
            return candidates;
        }
    }
    if opts.provider != Provider::MusicBrainz {
        match session.search_itunes(&group.query, group.album_group) {
            Ok(mut found) => candidates.append(&mut found),
            Err(e) => sink(&Event::Warn(format!("[warn] iTunes search failed: {e}"))),
        }
    }
    sort_and_dedupe(&mut candidates);
    candidates
}

fn has_ready(query: &Query, candidates: &[Candidate], min_confidence: f32) -> bool {
    candidates
        .first()
        .is_some_and(|c| candidate_is_ready(query, c, min_confidence))
}

fn candidate_is_ready(query: &Query, candidate: &Candidate, min_confidence: f32) -> bool {
    if query.artist.is_none() {
        return false;
    }
    let artist = similarity(query.artist.as_deref(), candidate.artist.as_deref());
    if artist < 0.80 || candidate.score < min_confidence {
        return false;
    }
    if query.album.is_some() {
        similarity(query.album.as_deref(), candidate.album.as_deref()) >= 0.85
    } else {
        similarity(query.title.as_deref(), candidate.title.as_deref()) >= 0.82
    }
}

fn score_candidate(query: &Query, candidate: &Candidate) -> f32 {
    let artist = similarity(query.artist.as_deref(), candidate.artist.as_deref());
    let title = similarity(query.title.as_deref(), candidate.title.as_deref());
    let album = similarity(query.album.as_deref(), candidate.album.as_deref());
    let duration = duration_similarity(query.duration_secs, candidate.duration_secs);

    let mut weighted = Vec::new();
    if query.artist.is_some() && candidate.artist.is_some() {
        weighted.push((artist, 0.50));
    }
    if query.album.is_some() && candidate.album.is_some() {
        weighted.push((album, 0.40));
    }
    if query.title.is_some() && candidate.title.is_some() {
        weighted.push((title, 0.40));
    }
    if let Some(duration) = duration {
        weighted.push((duration, 0.10));
    }
    if weighted.is_empty() {
        return 0.0;
    }
    let total_weight: f32 = weighted.iter().map(|(_, weight)| weight).sum();
    let mut score = weighted
        .iter()
        .map(|(value, weight)| value * weight)
        .sum::<f32>()
        / total_weight;

    // A candidate with a different mix/version is usually the wrong artwork
    // even when its title is similar. Exact version tokens are a strong hint.
    let wanted = version_tokens(query.title.as_deref().or(query.album.as_deref()));
    let got = version_tokens(candidate.title.as_deref().or(candidate.album.as_deref()));
    if wanted != got && (!wanted.is_empty() || !got.is_empty()) {
        score = (score - 0.12).max(0.0);
    }
    score.clamp(0.0, 1.0)
}

fn duration_similarity(a: Option<u64>, b: Option<u64>) -> Option<f32> {
    let (Some(a), Some(b)) = (a, b) else {
        return None;
    };
    let diff = a.abs_diff(b);
    Some(if diff <= 3 {
        1.0
    } else if diff <= 12 {
        1.0 - diff as f32 / 12.0
    } else {
        0.0
    })
}

fn sort_and_dedupe(candidates: &mut Vec<Candidate>) {
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.artwork_url.cmp(&b.artwork_url))
    });
    let mut seen = BTreeSet::new();
    candidates.retain(|candidate| {
        let key = format!(
            "{}|{}|{}",
            candidate.provider.label(),
            normalize(candidate.album.as_deref().unwrap_or("")),
            normalize(candidate.artist.as_deref().unwrap_or(""))
        );
        if seen.contains(&key) {
            false
        } else {
            seen.insert(key);
            true
        }
    });
    candidates.truncate(SEARCH_LIMIT);
}

fn similarity(a: Option<&str>, b: Option<&str>) -> f32 {
    let (Some(a), Some(b)) = (a, b) else {
        return 0.0;
    };
    let a = normalize(a);
    let b = normalize(b);
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    strsim::normalized_levenshtein(&a, &b) as f32
}

fn normalize(value: &str) -> String {
    let folded: String = value.nfkc().collect::<String>().to_lowercase();
    let replaced = folded.replace('&', " and ");
    let mut out = String::new();
    let mut space = false;
    for ch in replaced.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            space = false;
        } else if !space && !out.is_empty() {
            out.push(' ');
            space = true;
        }
    }
    out.trim().to_string()
}

fn version_tokens(value: Option<&str>) -> BTreeSet<&'static str> {
    let normalized = normalize(value.unwrap_or(""));
    let mut out = BTreeSet::new();
    for token in [
        "remix",
        "live",
        "instrumental",
        "extended",
        "radio edit",
        "acoustic",
        "demo",
        "karaoke",
        "edit",
    ] {
        if normalized.contains(token) {
            out.insert(token);
        }
    }
    out
}

fn probe_track(path: &Path) -> std::result::Result<Track, String> {
    let tagged = Probe::open(path)
        .and_then(|probe| probe.read())
        .map_err(|e| format!("cannot read audio metadata: {e}"))?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let mut artist: Option<String> = tag
        .and_then(|t| t.get_string(ItemKey::AlbumArtist).map(str::to_string))
        .filter(|v| !v.trim().is_empty() && !v.trim().eq_ignore_ascii_case("various artists"));
    if artist.is_none() {
        artist = tag.and_then(|t| t.artist().map(|v| v.into_owned()));
    }
    let mut artist = artist
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("various artists"));
    let mut title = tag
        .and_then(|t| t.title())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let album = tag
        .and_then(|t| t.album())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    // MP4/M4A has no picture type: Lofty exposes its `covr` atom as
    // PictureType::Other. Treat any attached picture as existing artwork there;
    // M4A stores exactly one cover in that atom in practice.
    let has_cover = tag.is_some_and(|t| !t.pictures().is_empty());

    if artist.is_none() || title.is_none() {
        if let Some((file_artist, file_title)) = parse_filename(path) {
            artist = artist.or(Some(file_artist));
            title = title.or(Some(file_title));
        }
    }

    Ok(Track {
        artist: artist.map(|v| first_artist(&v)),
        title,
        album,
        duration_secs: {
            let duration = tagged.properties().duration();
            (!duration.is_zero()).then_some(duration.as_secs())
        },
        has_cover,
    })
}

fn first_artist(value: &str) -> String {
    value
        .split(['/', '、'])
        .map(str::trim)
        .find(|v| !v.is_empty())
        .unwrap_or(value)
        .to_string()
}

fn parse_filename(path: &Path) -> Option<(String, String)> {
    let mut stem = path.file_stem()?.to_str()?.trim().to_string();
    let bytes = stem.as_bytes();
    let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && digits <= 3 {
        let rest = stem[digits..].trim_start_matches([' ', '.', '-', '_']);
        if !rest.is_empty() {
            stem = rest.to_string();
        }
    }
    let split = [" - ", " – ", " — "]
        .iter()
        .find_map(|needle| stem.split_once(needle))?;
    let artist = split.0.trim();
    let title = split.1.trim();
    if artist.is_empty() || title.is_empty() {
        return None;
    }
    Some((artist.to_string(), title.to_string()))
}

fn supported_format(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| matches!(ext.to_ascii_lowercase().as_str(), "mp3" | "flac" | "m4a"))
        .unwrap_or(false)
}

fn file_changed(entry: &Entry) -> bool {
    let size = fs::metadata(&entry.path).map(|m| m.len()).unwrap_or(0);
    let mtime = modified_nanos(&entry.path).unwrap_or(0);
    size != entry.size || mtime != entry.mtime_nanos
}

fn modified_nanos(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos() as u64)
}

fn write_cover(path: &Path, data: &[u8]) -> std::result::Result<(), String> {
    let original = Probe::open(path)
        .and_then(|probe| probe.read())
        .map_err(|e| format!("cannot read original file: {e}"))?;
    let original_title = original
        .primary_tag()
        .or_else(|| original.first_tag())
        .and_then(|tag| tag.title().map(|v| v.to_string()));
    let original_duration = original.properties().duration();

    let tmp = temp_path(path);
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    fs::copy(path, &tmp).map_err(|e| format!("cannot create temporary copy: {e}"))?;
    let result = (|| -> std::result::Result<(), String> {
        let mut tagged = Probe::open(&tmp)
            .and_then(|probe| probe.read())
            .map_err(|e| format!("cannot parse temporary copy: {e}"))?;
        let tag_type = tagged.primary_tag_type();
        if tagged.primary_tag_mut().is_none() {
            tagged.insert_tag(Tag::new(tag_type));
        }
        let tag = tagged
            .primary_tag_mut()
            .ok_or_else(|| "audio format has no writable primary tag".to_string())?;
        tag.remove_picture_type(PictureType::CoverFront);
        let mut picture = Picture::from_reader(&mut Cursor::new(data))
            .map_err(|e| format!("downloaded artwork is not a supported image: {e}"))?;
        picture.set_pic_type(PictureType::CoverFront);
        tag.push_picture(picture);
        tagged
            .save_to_path(&tmp, WriteOptions::default())
            .map_err(|e| format!("cannot write artwork tag: {e}"))?;

        let verify = Probe::open(&tmp)
            .and_then(|probe| probe.read())
            .map_err(|e| format!("cannot verify written file: {e}"))?;
        let verify_tag = verify.primary_tag().or_else(|| verify.first_tag());
        if verify_tag.is_none_or(|t| t.pictures().is_empty()) {
            return Err("artwork was not present after writing".into());
        }
        if !original_duration.is_zero() {
            let after = verify.properties().duration();
            if after.abs_diff(original_duration) > Duration::from_secs(1) {
                return Err("audio duration changed unexpectedly; refusing to replace file".into());
            }
        }
        if let Some(title) = &original_title {
            let after = verify_tag
                .and_then(|t| t.title())
                .map(|v| v.to_string())
                .unwrap_or_default();
            if &after != title {
                return Err("title tag changed unexpectedly; refusing to replace file".into());
            }
        }
        Ok(())
    })();

    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace original file: {e}")
    })
}

fn temp_path(path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_file_name(format!(
        ".djms-cover-{}-{stamp}-{}",
        std::process::id(),
        file_name(path)
    ))
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("dj-music-suite")
        .join("cover-cache")
}

impl Session {
    fn new(cache_dir: PathBuf, country: String) -> std::result::Result<Self, String> {
        fs::create_dir_all(cache_dir.join("artwork"))
            .and_then(|_| fs::create_dir_all(cache_dir.join("search")))
            .map_err(|e| format!("cannot create cover cache {}: {e}", cache_dir.display()))?;
        let client = HTTP_CLIENT
            .get_or_init(|| {
                Client::builder()
                    .timeout(Duration::from_secs(25))
                    .redirect(reqwest::redirect::Policy::limited(6))
                    .user_agent(format!(
                        "dj-music-suite/{} (https://github.com/Yang-Yiming/dj-music-suite)",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .build()
                    .map_err(|e| format!("cannot build HTTP client: {e}"))
            })
            .clone()
            .map_err(|e| e.clone())?;
        Ok(Session {
            client,
            cache_dir,
            country: if country.trim().is_empty() {
                "US".to_string()
            } else {
                country.trim().to_ascii_uppercase()
            },
            last_musicbrainz: None,
        })
    }

    fn search_musicbrainz(
        &mut self,
        query: &Query,
        album_group: bool,
    ) -> std::result::Result<Vec<Candidate>, String> {
        let cache_key = format!(
            "musicbrainz|{}|{}|{}|{}",
            if album_group { "album" } else { "track" },
            query.artist.as_deref().unwrap_or(""),
            query.album.as_deref().unwrap_or(""),
            query.title.as_deref().unwrap_or("")
        );
        if let Some(value) = self.read_search_cache(&cache_key) {
            return parse_musicbrainz(query, &value);
        }

        let (endpoint, mb_query) = if let (true, Some(artist), Some(album)) =
            (album_group, query.artist.as_deref(), query.album.as_deref())
        {
            (
                "https://musicbrainz.org/ws/2/release",
                format!(
                    "release:\"{}\" AND artist:\"{}\"",
                    query_escape(album),
                    query_escape(artist)
                ),
            )
        } else {
            let mut parts = Vec::new();
            if let Some(title) = query.title.as_deref() {
                parts.push(format!("recording:\"{}\"", query_escape(title)));
            }
            if let Some(artist) = query.artist.as_deref() {
                parts.push(format!("artist:\"{}\"", query_escape(artist)));
            }
            (
                "https://musicbrainz.org/ws/2/recording",
                parts.join(" AND "),
            )
        };
        if mb_query.is_empty() {
            return Ok(Vec::new());
        }

        let value = self.musicbrainz_json(endpoint, &mb_query)?;
        self.write_search_cache(&cache_key, &value);
        parse_musicbrainz(query, &value)
    }

    fn musicbrainz_json(
        &mut self,
        endpoint: &str,
        query: &str,
    ) -> std::result::Result<serde_json::Value, String> {
        if let Some(last) = self.last_musicbrainz {
            let elapsed = last.elapsed();
            if elapsed < Duration::from_millis(1100) {
                std::thread::sleep(Duration::from_millis(1100) - elapsed);
            }
        }
        self.last_musicbrainz = Some(Instant::now());
        for attempt in 0..3 {
            let response = self
                .client
                .get(endpoint)
                .query(&[("query", query), ("fmt", "json"), ("limit", "10")])
                .send()
                .map_err(|e| format!("request failed: {e}"))?;
            let status = response.status();
            if status.is_success() {
                return response
                    .json::<serde_json::Value>()
                    .map_err(|e| format!("bad JSON response: {e}"));
            }
            if attempt < 2 && (status.as_u16() == 429 || status.is_server_error()) {
                std::thread::sleep(Duration::from_secs(1 + attempt as u64));
                continue;
            }
            return Err(format!("HTTP {status}"));
        }
        Err("request failed after retries".into())
    }

    fn search_itunes(
        &mut self,
        query: &Query,
        album_group: bool,
    ) -> std::result::Result<Vec<Candidate>, String> {
        let cache_key = format!(
            "itunes|{}|{}|{}|{}|{}",
            if album_group { "album" } else { "track" },
            self.country,
            query.artist.as_deref().unwrap_or(""),
            query.album.as_deref().unwrap_or(""),
            query.title.as_deref().unwrap_or("")
        );
        if let Some(value) = self.read_search_cache(&cache_key) {
            return parse_itunes(query, &value);
        }
        let term = if album_group {
            [query.artist.as_deref(), query.album.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            [query.artist.as_deref(), query.title.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
        };
        if term.is_empty() {
            return Ok(Vec::new());
        }
        let mut value = self.itunes_json(&term, album_group, &self.country)?;
        if value
            .get("resultCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            == 0
            && self.country != "US"
        {
            // A more specific storefront should not make the command a dead
            // end when the release is only listed in the US catalogue.
            value = self.itunes_json(&term, album_group, "US")?;
        }
        self.write_search_cache(&cache_key, &value);
        parse_itunes(query, &value)
    }

    fn itunes_json(
        &self,
        term: &str,
        album_group: bool,
        country: &str,
    ) -> std::result::Result<serde_json::Value, String> {
        let response = self
            .client
            .get("https://itunes.apple.com/search")
            .query(&[
                ("term", term),
                ("media", "music"),
                ("entity", if album_group { "album" } else { "song" }),
                ("limit", "10"),
                ("country", country),
            ])
            .send()
            .map_err(|e| format!("request failed: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        response
            .json::<serde_json::Value>()
            .map_err(|e| format!("bad JSON response: {e}"))
    }

    fn artwork(&self, candidate: &Candidate) -> std::result::Result<Artwork, String> {
        if self.artwork_recently_failed(&candidate.artwork_url) {
            return Err("download failed recently; using another provider".into());
        }
        let path = self.artwork_path(&candidate.artwork_url);
        if let Ok(data) = fs::read(&path) {
            if let Some(meta) = image_meta(&data).filter(|meta| {
                meta.width <= MAX_IMAGE_DIMENSION && meta.height <= MAX_IMAGE_DIMENSION
            }) {
                return Ok(Artwork {
                    bytes: data,
                    mime: meta.mime,
                    width: meta.width,
                    height: meta.height,
                });
            }
            let _ = fs::remove_file(&path);
        }
        let mut data = Vec::new();
        for attempt in 0..3 {
            let mut response = match self.client.get(&candidate.artwork_url).send() {
                Ok(response) => response,
                Err(e) => {
                    self.remember_artwork_failure(&candidate.artwork_url);
                    return Err(format!("download failed: {e}"));
                }
            };
            let status = response.status();
            if status.is_success() {
                response
                    .by_ref()
                    .take((MAX_IMAGE_BYTES + 1) as u64)
                    .read_to_end(&mut data)
                    .map_err(|e| format!("download failed: {e}"))?;
                break;
            }
            if attempt < 2 && (status.as_u16() == 429 || status.is_server_error()) {
                std::thread::sleep(Duration::from_secs(1 + attempt as u64));
                continue;
            }
            self.remember_artwork_failure(&candidate.artwork_url);
            return Err(format!("HTTP {status}"));
        }
        if data.len() > MAX_IMAGE_BYTES {
            self.remember_artwork_failure(&candidate.artwork_url);
            return Err(format!(
                "image is larger than {} MB",
                MAX_IMAGE_BYTES / 1024 / 1024
            ));
        }
        let meta = image_meta(&data).ok_or_else(|| {
            self.remember_artwork_failure(&candidate.artwork_url);
            "downloaded file is not a JPEG or PNG image".to_string()
        })?;
        if meta.width > MAX_IMAGE_DIMENSION || meta.height > MAX_IMAGE_DIMENSION {
            self.remember_artwork_failure(&candidate.artwork_url);
            return Err(format!(
                "image is {}x{}, larger than the {}px limit",
                meta.width, meta.height, MAX_IMAGE_DIMENSION
            ));
        }
        fs::write(&path, &data).map_err(|e| format!("cannot cache artwork: {e}"))?;
        let _ = fs::remove_file(self.artwork_failure_path(&candidate.artwork_url));
        Ok(Artwork {
            bytes: data,
            mime: meta.mime,
            width: meta.width,
            height: meta.height,
        })
    }

    fn artwork_path(&self, url: &str) -> PathBuf {
        let key = sha256_hex(url.as_bytes());
        self.cache_dir.join("artwork").join(format!("{key}.img"))
    }

    fn artwork_failure_path(&self, url: &str) -> PathBuf {
        let key = sha256_hex(url.as_bytes());
        self.cache_dir.join("artwork").join(format!("{key}.failed"))
    }

    fn artwork_recently_failed(&self, url: &str) -> bool {
        fs::metadata(self.artwork_failure_path(url))
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed <= ARTWORK_FAILURE_TTL)
    }

    fn remember_artwork_failure(&self, url: &str) {
        let _ = fs::write(self.artwork_failure_path(url), b"failed\n");
    }

    fn read_search_cache(&self, key: &str) -> Option<serde_json::Value> {
        let path = self
            .cache_dir
            .join("search")
            .join(format!("{}.json", sha256_hex(key.as_bytes())));
        let meta = fs::metadata(&path).ok()?;
        let modified = meta.modified().ok()?;
        if modified.elapsed().ok()? > SEARCH_CACHE_TTL {
            return None;
        }
        let raw = fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    }

    fn write_search_cache(&self, key: &str, value: &serde_json::Value) {
        let path = self
            .cache_dir
            .join("search")
            .join(format!("{}.json", sha256_hex(key.as_bytes())));
        if let Ok(raw) = serde_json::to_vec(value) {
            let _ = fs::write(path, raw);
        }
    }
}

pub struct Artwork {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

struct ImageMeta {
    mime: &'static str,
    width: u32,
    height: u32,
}

fn image_meta(data: &[u8]) -> Option<ImageMeta> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") && data.len() >= 24 {
        let width = u32::from_be_bytes(data[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(data[20..24].try_into().ok()?);
        return Some(ImageMeta {
            mime: "image/png",
            width,
            height,
        });
    }
    if data.starts_with(b"\xff\xd8") {
        let (width, height) = jpeg_dimensions(data)?;
        return Some(ImageMeta {
            mime: "image/jpeg",
            width,
            height,
        });
    }
    None
}

fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut index = 2usize;
    while index + 4 <= data.len() {
        if data[index] != 0xff {
            index += 1;
            continue;
        }
        while index < data.len() && data[index] == 0xff {
            index += 1;
        }
        if index >= data.len() {
            return None;
        }
        let marker = data[index];
        index += 1;
        if matches!(marker, 0xd8 | 0xd9) {
            continue;
        }
        if index + 2 > data.len() {
            return None;
        }
        let length = u16::from_be_bytes([data[index], data[index + 1]]) as usize;
        if length < 2 || index + length > data.len() {
            return None;
        }
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            if length < 7 {
                return None;
            }
            let height = u16::from_be_bytes([data[index + 3], data[index + 4]]) as u32;
            let width = u16::from_be_bytes([data[index + 5], data[index + 6]]) as u32;
            return Some((width, height));
        }
        index += length;
    }
    None
}

fn parse_musicbrainz(
    query: &Query,
    value: &serde_json::Value,
) -> std::result::Result<Vec<Candidate>, String> {
    let mut out = Vec::new();
    if let Some(releases) = value.get("releases").and_then(|v| v.as_array()) {
        for release in releases {
            push_musicbrainz_release(query, release, None, &mut out);
        }
    }
    if let Some(recordings) = value.get("recordings").and_then(|v| v.as_array()) {
        for recording in recordings {
            let title = recording.get("title").and_then(|v| v.as_str());
            let artist = musicbrainz_artist(recording);
            let duration = recording
                .get("length")
                .and_then(|v| v.as_u64())
                .map(|ms| ms / 1000);
            if let Some(releases) = recording.get("releases").and_then(|v| v.as_array()) {
                for release in releases {
                    push_musicbrainz_release(
                        query,
                        release,
                        Some((title, artist.clone(), duration)),
                        &mut out,
                    );
                }
            }
        }
    }
    for candidate in &mut out {
        candidate.score = score_candidate(query, candidate);
    }
    Ok(out)
}

fn push_musicbrainz_release(
    query: &Query,
    release: &serde_json::Value,
    recording: Option<(Option<&str>, Option<String>, Option<u64>)>,
    out: &mut Vec<Candidate>,
) {
    let release_group = release.get("release-group");
    let group_id = release_group
        .and_then(|g| g.get("id"))
        .and_then(|v| v.as_str());
    let release_id = release.get("id").and_then(|v| v.as_str());
    let id = group_id.or(release_id);
    let Some(id) = id else {
        return;
    };
    let album = release
        .get("title")
        .and_then(|v| v.as_str())
        .or_else(|| group_id.and_then(|_| release_group?.get("title")?.as_str()))
        .map(str::to_string);
    let artist = release
        .get("artist-credit")
        .and_then(musicbrainz_artist_credit_from_value)
        .or_else(|| recording.as_ref().and_then(|(_, artist, _)| artist.clone()));
    let title = recording
        .as_ref()
        .and_then(|(title, _, _)| *title)
        .map(str::to_string)
        .or_else(|| album.clone());
    let year = release
        .get("date")
        .and_then(|v| v.as_str())
        .map(|date| date.chars().take(4).collect::<String>());
    let primary_type = release_group
        .and_then(|g| g.get("primary-type"))
        .and_then(|v| v.as_str());
    let artwork_url = if let Some(group_id) = group_id {
        format!("https://coverartarchive.org/release-group/{group_id}/front")
    } else {
        format!("https://coverartarchive.org/release/{id}/front")
    };
    let mut candidate = Candidate {
        provider: Provider::MusicBrainz,
        provider_id: id.to_string(),
        artist,
        title,
        album,
        year,
        duration_secs: recording.as_ref().and_then(|(_, _, duration)| *duration),
        artwork_url,
        score: 0.0,
        note: primary_type.map(|kind| format!("release type: {kind}")),
    };
    candidate.score = score_candidate(query, &candidate);
    out.push(candidate);
}

fn musicbrainz_artist(value: &serde_json::Value) -> Option<String> {
    value
        .get("artist-credit")
        .and_then(musicbrainz_artist_credit_from_value)
}

fn musicbrainz_artist_credit_from_value(value: &serde_json::Value) -> Option<String> {
    let array = value.as_array()?;
    let names: Vec<&str> = array
        .iter()
        .filter_map(|credit| credit.get("name").and_then(|v| v.as_str()))
        .collect();
    (!names.is_empty()).then(|| names.join(" / "))
}

fn parse_itunes(
    query: &Query,
    value: &serde_json::Value,
) -> std::result::Result<Vec<Candidate>, String> {
    let mut out = Vec::new();
    for result in value
        .get("results")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let provider_id = result
            .get("collectionId")
            .or_else(|| result.get("trackId"))
            .map(|v| v.to_string())
            .unwrap_or_default();
        let artwork = result
            .get("artworkUrl100")
            .or_else(|| result.get("artworkUrl60"))
            .and_then(|v| v.as_str())
            .map(highres_artwork);
        let Some(artwork_url) = artwork else {
            continue;
        };
        let collection = result
            .get("collectionName")
            .and_then(|v| v.as_str())
            .map(clean_release_title);
        let track = result
            .get("trackName")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut candidate = Candidate {
            provider: Provider::Itunes,
            provider_id,
            artist: result
                .get("artistName")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            title: track.or_else(|| collection.clone()),
            album: collection,
            year: result
                .get("releaseDate")
                .and_then(|v| v.as_str())
                .map(|date| date.chars().take(4).collect::<String>()),
            duration_secs: result
                .get("trackTimeMillis")
                .and_then(|v| v.as_u64())
                .map(|ms| ms / 1000),
            artwork_url,
            score: 0.0,
            note: None,
        };
        candidate.score = score_candidate(query, &candidate);
        out.push(candidate);
    }
    Ok(out)
}

fn clean_release_title(value: &str) -> String {
    let mut out = value.trim().to_string();
    for suffix in [" - Single", " - EP", " - Album", " - Compilation"] {
        let start = out.len().saturating_sub(suffix.len());
        if out
            .get(start..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
        {
            out.truncate(out.len() - suffix.len());
            break;
        }
    }
    out.trim().to_string()
}

fn highres_artwork(url: &str) -> String {
    url.replace("100x100bb", "1200x1200bb")
        .replace("60x60bb", "1200x1200bb")
}

fn query_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_handles_case_width_and_punctuation() {
        assert_eq!(normalize("  ARTMS — BURN "), "artms burn");
        assert_eq!(normalize("AC/DC & Friends"), "ac dc and friends");
        let wide = normalize("ＡＲＴＭＳ");
        assert_eq!(wide, "artms");
    }

    #[test]
    fn filename_fallback_is_conservative() {
        assert_eq!(
            parse_filename(Path::new("ARTMS - BURN.mp3")),
            Some(("ARTMS".into(), "BURN".into()))
        );
        assert_eq!(
            parse_filename(Path::new("01. ARTMS - BURN.flac")),
            Some(("ARTMS".into(), "BURN".into()))
        );
        assert_eq!(parse_filename(Path::new("untitled.mp3")), None);
    }

    #[test]
    fn exact_match_scores_high_and_artist_mismatch_is_not_ready() {
        let query = Query {
            artist: Some("ARTMS".into()),
            title: Some("BURN".into()),
            album: Some("BURN".into()),
            duration_secs: Some(180),
        };
        let mut candidate = Candidate {
            provider: Provider::MusicBrainz,
            provider_id: "abc".into(),
            artist: Some("ARTMS".into()),
            title: Some("BURN".into()),
            album: Some("BURN".into()),
            year: None,
            duration_secs: Some(181),
            artwork_url: "https://example.test/cover.jpg".into(),
            score: 0.0,
            note: None,
        };
        candidate.score = score_candidate(&query, &candidate);
        assert!(candidate.score > 0.95);
        assert!(candidate_is_ready(
            &query,
            &candidate,
            DEFAULT_MIN_CONFIDENCE
        ));

        candidate.artist = Some("Someone Else".into());
        candidate.score = score_candidate(&query, &candidate);
        assert!(!candidate_is_ready(
            &query,
            &candidate,
            DEFAULT_MIN_CONFIDENCE
        ));
    }

    #[test]
    fn version_mismatch_is_penalized() {
        let query = Query {
            artist: Some("ARTMS".into()),
            title: Some("BURN".into()),
            album: None,
            duration_secs: None,
        };
        let candidate = Candidate {
            provider: Provider::Itunes,
            provider_id: "1".into(),
            artist: Some("ARTMS".into()),
            title: Some("BURN (Live)".into()),
            album: Some("Live".into()),
            year: None,
            duration_secs: None,
            artwork_url: "https://example.test/live.jpg".into(),
            score: 0.0,
            note: None,
        };
        assert!(score_candidate(&query, &candidate) < 0.95);
    }

    #[test]
    fn highres_itunes_url_is_upgraded() {
        assert_eq!(
            highres_artwork("https://is1.mzstatic.com/x/100x100bb.jpg"),
            "https://is1.mzstatic.com/x/1200x1200bb.jpg"
        );
    }

    #[test]
    fn png_dimensions_are_read() {
        let mut data = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        data.extend_from_slice(&600u32.to_be_bytes());
        data.extend_from_slice(&600u32.to_be_bytes());
        let meta = image_meta(&data).unwrap();
        assert_eq!(
            (meta.mime, meta.width, meta.height),
            ("image/png", 600, 600)
        );
    }

    #[test]
    fn clean_release_title_removes_itunes_suffixes() {
        assert_eq!(clean_release_title("<BURN> - Single"), "<BURN>");
        assert_eq!(clean_release_title("Album - EP"), "Album");
        assert_eq!(clean_release_title("Live Album"), "Live Album");
    }

    #[test]
    fn write_cover_works_for_supported_formats() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("djms-cover-write-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let cover = dir.join("cover.jpg");
        let status = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=0x336699:s=64x64:d=1",
                "-frames:v",
                "1",
            ])
            .arg(&cover)
            .status()
            .unwrap();
        assert!(status.success(), "cannot generate JPEG fixture");
        let artwork = fs::read(&cover).unwrap();

        for (ext, encoder) in [("mp3", "libmp3lame"), ("flac", "flac"), ("m4a", "aac")] {
            let path = dir.join(format!("track.{ext}"));
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=1",
                    "-c:a",
                ])
                .arg(encoder)
                .args([
                    "-metadata",
                    "artist=ARTMS",
                    "-metadata",
                    "title=BURN",
                    "-metadata",
                    "album=BURN",
                ])
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success(), "cannot generate {ext} fixture");

            write_cover(&path, &artwork).unwrap();
            let track = probe_track(&path).unwrap();
            assert!(track.has_cover, "{ext} cover was not written");
            assert_eq!(track.title.as_deref(), Some("BURN"));
            assert_eq!(track.artist.as_deref(), Some("ARTMS"));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
