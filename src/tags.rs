use std::path::Path;
use std::time::Duration;

use lofty::prelude::*;
use lofty::probe::Probe;

pub struct TrackMeta {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

/// Read artist/title/album/duration. Returns None when the file cannot be
/// probed at all (corrupt or unsupported container).
pub fn read_meta(path: &Path) -> Option<TrackMeta> {
    let tagged = Probe::open(path).ok()?.read().ok()?;
    let duration = tagged.properties().duration();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let (mut artist, mut title, mut album) = (None, None, None);
    if let Some(tag) = tag {
        artist = tag.artist().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        title = tag.title().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        album = tag.album().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    }
    let duration = (!duration.is_zero()).then_some(duration);
    Some(TrackMeta {
        artist,
        title,
        album,
        duration,
    })
}

/// Normalize a tag value for identity comparisons: lowercase, collapse whitespace.
pub fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Identity key for duplicate detection: normalized (artist, title).
pub fn identity(meta: &TrackMeta) -> Option<(String, String)> {
    let artist = meta
        .artist
        .as_deref()
        .map(normalize)
        .filter(|s| !s.is_empty())?;
    let title = meta
        .title
        .as_deref()
        .map(normalize)
        .filter(|s| !s.is_empty())?;
    Some((artist, title))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_whitespace_and_case() {
        assert_eq!(normalize("  ARTMS   burn "), "artms burn");
        assert_eq!(normalize("Foo\tBar"), "foo bar");
    }

    #[test]
    fn identity_needs_both_parts() {
        let no_title = TrackMeta {
            artist: Some("A".into()),
            title: None,
            album: None,
            duration: None,
        };
        assert!(identity(&no_title).is_none());
        let both = TrackMeta {
            artist: Some(" A  B ".into()),
            title: Some("c d".into()),
            album: None,
            duration: None,
        };
        assert_eq!(identity(&both), Some(("a b".into(), "c d".into())));
    }
}
