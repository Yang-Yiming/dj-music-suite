use std::path::Path;
use std::time::Duration;

use lofty::prelude::*;
use lofty::probe::Probe;

pub struct TrackMeta {
    pub artist: Option<String>,
    /// album artist (TPE2) when tagged
    pub album_artist: Option<String>,
    /// album artist, or the first of the (slash-separated) artist list
    pub primary_artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

/// Read artist/title/album/duration. Returns None when the file cannot be
pub fn read_meta(path: &Path) -> Option<TrackMeta> {
    let tagged = Probe::open(path).ok()?.read().ok()?;
    let duration = tagged.properties().duration();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let (mut artist, mut album_artist, mut title, mut album) = (None, None, None, None);
    if let Some(tag) = tag {
        artist = tag.artist().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        album_artist = tag
            .get_string(ItemKey::AlbumArtist)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        title = tag.title().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        album = tag.album().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    }
    let primary_artist = primary_artist(album_artist.as_deref(), artist.as_deref());
    let duration = (!duration.is_zero()).then_some(duration);
    Some(TrackMeta {
        artist,
        album_artist,
        primary_artist,
        title,
        album,
        duration,
    })
}

/// The artist a release is filed under: the album artist when present (and
/// not "Various Artists"), else the first of the slash-separated artist list.
fn primary_artist(album_artist: Option<&str>, artist: Option<&str>) -> Option<String> {
    if let Some(aa) = album_artist.map(str::trim).filter(|v| !v.is_empty()) {
        if normalize(aa) != "various artists" {
            return Some(aa.to_string());
        }
    }
    artist?
        .split(['/', '、'])
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_string)
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
            album_artist: None,
            primary_artist: None,
            title: None,
            album: None,
            duration: None,
        };
        assert!(identity(&no_title).is_none());
        let both = TrackMeta {
            artist: Some(" A  B ".into()),
            album_artist: None,
            primary_artist: None,
            title: Some("c d".into()),
            album: None,
            duration: None,
        };
        assert_eq!(identity(&both), Some(("a b".into(), "c d".into())));
    }

    #[test]
    fn primary_artist_prefers_album_artist() {
        assert_eq!(
            primary_artist(Some("Ahadadream"), Some("Ahadadream/Skrillex")),
            Some("Ahadadream".into())
        );
    }

    #[test]
    fn primary_artist_falls_back_to_first_of_list() {
        assert_eq!(
            primary_artist(None, Some("Armin Van Buuren/W&W")),
            Some("Armin Van Buuren".into())
        );
        assert_eq!(
            primary_artist(None, Some(" A 、B ")),
            Some("A".into())
        );
        assert_eq!(primary_artist(None, Some("Solo")), Some("Solo".into()));
        assert_eq!(primary_artist(None, None), None);
    }

    #[test]
    fn primary_artist_ignores_various_artists() {
        assert_eq!(
            primary_artist(Some("Various Artists"), Some("Ida Engberg/David West")),
            Some("Ida Engberg".into())
        );
        assert_eq!(primary_artist(Some("Various Artists"), None), None);
    }
}
