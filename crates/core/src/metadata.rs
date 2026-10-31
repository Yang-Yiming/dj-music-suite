//! Read and edit tags on individual audio tracks.
//!
//! Unlike the cover feature, this is track-centric: a scan returns lightweight
//! snapshots, and an update applies only the fields the user changed. Writes
//! use a same-directory temporary copy and replace the original only after the
//! new tags have been read back and verified.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag};
use serde::{Deserialize, Serialize};

use crate::scan::{file_name, scan_audio};
use crate::{usage, Event, Result, Sink};

const SUPPORTED_EXTS: &[&str] = &["mp3", "flac", "m4a"];

/// Everything the web editor needs to display one track and later detect
/// whether it changed on disk between the scan and the save.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackMetadata {
    pub path: PathBuf,
    pub size: u64,
    /// Serialized as a string because nanosecond timestamps exceed JavaScript's
    /// safe integer range and would be rounded when round-tripped by the web UI.
    pub mtime_nanos: String,
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
    pub has_cover: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A sparse update. `None` means "leave the field alone"; `Some("")` clears it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MetadataPatch {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub track_number: Option<String>,
    pub disc_number: Option<String>,
}

impl MetadataPatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.album.is_none()
            && self.album_artist.is_none()
            && self.year.is_none()
            && self.genre.is_none()
            && self.track_number.is_none()
            && self.disc_number.is_none()
    }
}

pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| SUPPORTED_EXTS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Scan one file or a folder recursively. Unreadable supported files are kept
/// in the list with an `error` so one bad file does not hide the rest.
pub fn scan(input: &Path, sink: Sink) -> Result<Vec<TrackMetadata>> {
    let input = fs::canonicalize(input)
        .map_err(|e| crate::Error::Runtime(format!("cannot open {}: {e}", input.display())))?;
    if !input.exists() {
        return Err(crate::Error::Runtime(format!(
            "path does not exist: {}",
            input.display()
        )));
    }

    let mut files = if input.is_file() {
        vec![input.clone()]
    } else {
        scan_audio(&input)
    };
    files.retain(|path| supported(path));
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err(crate::Error::Runtime(format!(
            "no supported audio files (mp3/flac/m4a) found in {}",
            input.display()
        )));
    }

    sink(&Event::Start(files.len() as u64));
    let tracks = files
        .iter()
        .map(|path| {
            let track = read(path).unwrap_or_else(|error| unreadable(path, error.to_string()));
            sink(&Event::Step(file_name(path)));
            track
        })
        .collect();
    Ok(tracks)
}

pub fn read(path: &Path) -> Result<TrackMetadata> {
    let (size, mtime_nanos) = fingerprint(path)?;
    let tagged = Probe::open(path)
        .and_then(|probe| probe.read())
        .map_err(|e| crate::Error::Runtime(format!("cannot read audio metadata: {e}")))?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let duration = tagged.properties().duration();

    Ok(TrackMetadata {
        path: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        size,
        mtime_nanos: mtime_nanos.to_string(),
        format: format_name(path),
        title: tag.and_then(|tag| text(tag, ItemKey::TrackTitle)),
        artist: tag.and_then(|tag| text(tag, ItemKey::TrackArtist)),
        album: tag.and_then(|tag| text(tag, ItemKey::AlbumTitle)),
        album_artist: tag.and_then(|tag| text(tag, ItemKey::AlbumArtist)),
        year: tag.and_then(year),
        genre: tag.and_then(|tag| text(tag, ItemKey::Genre)),
        track_number: tag.and_then(|tag| text(tag, ItemKey::TrackNumber)),
        disc_number: tag.and_then(|tag| text(tag, ItemKey::DiscNumber)),
        duration_secs: (!duration.is_zero()).then_some(duration.as_secs()),
        has_cover: tag.is_some_and(|tag| !tag.pictures().is_empty()),
        error: None,
    })
}

/// Apply a sparse patch to one file. The size/mtime pair comes from the scan
/// that produced the editor state, so a file changed in the meantime is never
/// overwritten silently.
pub fn update(
    path: &Path,
    expected_size: u64,
    expected_mtime_nanos: u64,
    patch: &MetadataPatch,
) -> Result<TrackMetadata> {
    if patch.is_empty() {
        return Err(usage("no metadata fields were changed"));
    }
    if !supported(path) {
        return Err(usage(format!(
            "metadata editing supports mp3, flac and m4a (got {})",
            path.display()
        )));
    }
    let path = fs::canonicalize(path)
        .map_err(|e| crate::Error::Runtime(format!("cannot open {}: {e}", path.display())))?;
    let (size, mtime_nanos) = fingerprint(&path)?;
    if size != expected_size || mtime_nanos != expected_mtime_nanos {
        return Err(crate::Error::Runtime(
            "file changed since it was scanned; rescan before saving".into(),
        ));
    }

    let before = read(&path)?;
    let tmp = temp_path(&path);
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    fs::copy(&path, &tmp)
        .map_err(|e| crate::Error::Runtime(format!("cannot create temporary copy: {e}")))?;

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
        apply_patch(tag, patch)?;
        tagged
            .save_to_path(&tmp, WriteOptions::default())
            .map_err(|e| format!("cannot write metadata: {e}"))?;

        let after = read(&tmp).map_err(|e| e.to_string())?;
        if before.duration_secs != after.duration_secs {
            return Err("audio duration changed unexpectedly; refusing to replace file".into());
        }
        if before.has_cover != after.has_cover {
            return Err("cover art changed unexpectedly; refusing to replace file".into());
        }
        verify_untouched(&before, &after, patch)?;
        verify_patch(&after, patch)?;
        Ok(())
    })();

    if let Err(message) = result {
        let _ = fs::remove_file(&tmp);
        return Err(crate::Error::Runtime(message));
    }
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        crate::Error::Runtime(format!("cannot replace original file: {e}"))
    })?;
    read(&path)
}

fn apply_patch(tag: &mut Tag, patch: &MetadataPatch) -> std::result::Result<(), String> {
    if let Some(value) = &patch.title {
        set_text(tag, ItemKey::TrackTitle, value);
    }
    if let Some(value) = &patch.artist {
        set_text(tag, ItemKey::TrackArtist, value);
    }
    if let Some(value) = &patch.album {
        set_text(tag, ItemKey::AlbumTitle, value);
    }
    if let Some(value) = &patch.album_artist {
        set_text(tag, ItemKey::AlbumArtist, value);
    }
    if let Some(value) = &patch.genre {
        set_text(tag, ItemKey::Genre, value);
    }
    if let Some(value) = &patch.track_number {
        set_text(tag, ItemKey::TrackNumber, value);
    }
    if let Some(value) = &patch.disc_number {
        set_text(tag, ItemKey::DiscNumber, value);
    }
    if let Some(value) = &patch.year {
        tag.remove_key(ItemKey::Year);
        tag.remove_key(ItemKey::RecordingDate);
        let value = value.trim();
        if !value.is_empty() {
            tag.insert_text(ItemKey::RecordingDate, value.to_string());
        }
    }
    Ok(())
}

fn set_text(tag: &mut Tag, key: ItemKey, value: &str) {
    tag.remove_key(key);
    let value = value.trim();
    if !value.is_empty() {
        tag.insert_text(key, value.to_string());
    }
}

fn verify_patch(after: &TrackMetadata, patch: &MetadataPatch) -> std::result::Result<(), String> {
    check_patch("title", &after.title, &patch.title)?;
    check_patch("artist", &after.artist, &patch.artist)?;
    check_patch("album", &after.album, &patch.album)?;
    check_patch("album artist", &after.album_artist, &patch.album_artist)?;
    check_patch("year", &after.year, &patch.year)?;
    check_patch("genre", &after.genre, &patch.genre)?;
    check_patch("track number", &after.track_number, &patch.track_number)?;
    check_patch("disc number", &after.disc_number, &patch.disc_number)?;
    Ok(())
}

fn check_patch(
    name: &str,
    actual: &Option<String>,
    patch: &Option<String>,
) -> std::result::Result<(), String> {
    let Some(expected) = patch else {
        return Ok(());
    };
    let expected = {
        let value = expected.trim();
        (!value.is_empty()).then(|| value.to_string())
    };
    if actual != &expected {
        return Err(format!(
            "{name} was not written correctly; refusing to replace file"
        ));
    }
    Ok(())
}

fn verify_untouched(
    before: &TrackMetadata,
    after: &TrackMetadata,
    patch: &MetadataPatch,
) -> std::result::Result<(), String> {
    check_untouched("title", &before.title, &after.title, &patch.title)?;
    check_untouched("artist", &before.artist, &after.artist, &patch.artist)?;
    check_untouched("album", &before.album, &after.album, &patch.album)?;
    check_untouched(
        "album artist",
        &before.album_artist,
        &after.album_artist,
        &patch.album_artist,
    )?;
    check_untouched("year", &before.year, &after.year, &patch.year)?;
    check_untouched("genre", &before.genre, &after.genre, &patch.genre)?;
    check_untouched(
        "track number",
        &before.track_number,
        &after.track_number,
        &patch.track_number,
    )?;
    check_untouched(
        "disc number",
        &before.disc_number,
        &after.disc_number,
        &patch.disc_number,
    )?;
    Ok(())
}

fn check_untouched(
    name: &str,
    before: &Option<String>,
    after: &Option<String>,
    patch: &Option<String>,
) -> std::result::Result<(), String> {
    if patch.is_none() && before != after {
        return Err(format!(
            "unmodified {name} changed unexpectedly; refusing to replace file"
        ));
    }
    Ok(())
}

fn text(tag: &Tag, key: ItemKey) -> Option<String> {
    tag.get_string(key)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn year(tag: &Tag) -> Option<String> {
    text(tag, ItemKey::RecordingDate).or_else(|| text(tag, ItemKey::Year))
}

fn unreadable(path: &Path, error: String) -> TrackMetadata {
    let (size, mtime_nanos) = fingerprint(path).unwrap_or((0, 0));
    TrackMetadata {
        path: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        size,
        mtime_nanos: mtime_nanos.to_string(),
        format: format_name(path),
        title: None,
        artist: None,
        album: None,
        album_artist: None,
        year: None,
        genre: None,
        track_number: None,
        disc_number: None,
        duration_secs: None,
        has_cover: false,
        error: Some(error),
    }
}

fn format_name(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("?")
        .to_ascii_lowercase()
}

fn fingerprint(path: &Path) -> Result<(u64, u64)> {
    let metadata = fs::metadata(path)
        .map_err(|e| crate::Error::Runtime(format!("cannot stat {}: {e}", path.display())))?;
    let mtime_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    Ok((metadata.len(), mtime_nanos))
}

fn temp_path(path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_file_name(format!(
        ".djms-metadata-{}-{stamp}-{}",
        std::process::id(),
        file_name(path)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn fixture_dir() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("djms-metadata-test-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn scan_folder_and_update_fields() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let dir = fixture_dir();
        fs::create_dir_all(dir.join("album")).unwrap();
        for (name, encoder) in [
            ("track.mp3", "libmp3lame"),
            ("track.flac", "flac"),
            ("track.m4a", "aac"),
        ] {
            let path = dir.join("album").join(name);
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=1",
                    "-c:a",
                    encoder,
                    "-metadata",
                    "artist=Old Artist",
                    "-metadata",
                    "title=Old Title",
                    "-metadata",
                    "album=Old Album",
                ])
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success(), "cannot generate {name} fixture");
        }

        let no_sink: Sink = &|_| {};
        let tracks = scan(&dir, no_sink).unwrap();
        assert_eq!(tracks.len(), 3);
        for scanned in &tracks {
            assert_eq!(scanned.title.as_deref(), Some("Old Title"));
            assert_eq!(scanned.artist.as_deref(), Some("Old Artist"));

            let patch = MetadataPatch {
                title: Some(format!("New Title {}", scanned.format)),
                artist: Some("New Artist".into()),
                genre: Some("House".into()),
                track_number: Some("3".into()),
                ..Default::default()
            };
            let saved = update(
                &scanned.path,
                scanned.size,
                scanned.mtime_nanos.parse().unwrap(),
                &patch,
            )
            .unwrap();
            assert_eq!(
                saved.title.as_deref(),
                Some(format!("New Title {}", scanned.format).as_str())
            );
            assert_eq!(saved.artist.as_deref(), Some("New Artist"));
            assert_eq!(saved.album.as_deref(), Some("Old Album"));
            assert_eq!(saved.genre.as_deref(), Some("House"));
            assert_eq!(saved.track_number.as_deref(), Some("3"));

            let cleared = update(
                &saved.path,
                saved.size,
                saved.mtime_nanos.parse().unwrap(),
                &MetadataPatch {
                    genre: Some(String::new()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(cleared.genre, None);
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_rejects_a_changed_file() {
        let dir = fixture_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("track.mp3");
        fs::write(&path, b"not a real mp3").unwrap();
        let before = fs::metadata(&path).unwrap();
        let mtime = before
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        fs::write(&path, b"changed after scan").unwrap();
        let err = update(
            &path,
            before.len(),
            mtime,
            &MetadataPatch {
                title: Some("New".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("changed since"));
        let _ = fs::remove_dir_all(&dir);
    }
}
