use std::borrow::Cow;
use std::path::Path;

use lofty::prelude::*;
use lofty::probe::Probe;

/// Lossless / archival formats first; higher rank wins.
fn format_rank(ext: &str) -> u32 {
    match ext.to_ascii_lowercase().as_str() {
        "flac" => 6,
        "wav" => 5,
        "aiff" | "aif" => 4,
        "m4a" => 3,
        "aac" => 2,
        "mp3" => 1,
        _ => 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quality {
    pub format_rank: u32,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub bits_per_sample: Option<u32>,
    pub has_cover: bool,
    pub has_lyrics: bool,
    pub tags_complete: bool,
    pub score: i64,
}

impl Quality {
    /// Deterministic composite score. Weights are step-separated so a higher
    /// tier can never be outweighed by lower ones:
    /// format (1e10 step) > bitrate (1e7) > sample rate (1e4) > bit depth
    /// (1e2) > cover > lyrics > tag completeness.
    pub fn new(
        ext: &str,
        bitrate: Option<u32>,
        sample_rate: Option<u32>,
        bits_per_sample: Option<u32>,
        has_cover: bool,
        has_lyrics: bool,
        tags_complete: bool,
    ) -> Self {
        let format_rank = format_rank(ext);
        let bitrate_bucket = bitrate.unwrap_or(0).min(2000) / 32;
        let khz = sample_rate.unwrap_or(0).min(199_000) / 1000;
        let bits = bits_per_sample.unwrap_or(0).min(64);
        let score = format_rank as i64 * 10_000_000_000
            + bitrate_bucket as i64 * 10_000_000
            + khz as i64 * 10_000
            + bits as i64 * 100
            + i64::from(has_cover) * 10
            + i64::from(has_lyrics) * 5
            + i64::from(tags_complete) * 2;
        Quality {
            format_rank,
            bitrate,
            sample_rate,
            bits_per_sample,
            has_cover,
            has_lyrics,
            tags_complete,
            score,
        }
    }
}

/// Probe `path` with lofty and score what the format reports. Returns None
/// when the file cannot be probed at all (then it cannot be scored either).
pub fn measure(path: &Path) -> Option<Quality> {
    let ext = path.extension()?.to_str()?;
    let tagged = Probe::open(path).ok()?.read().ok()?;
    let props = tagged.properties();
    let bitrate = props.overall_bitrate().filter(|v| *v != 0);
    let sample_rate = props.sample_rate().filter(|v| *v != 0);
    let bits = props.bit_depth().filter(|v| *v != 0).map(u32::from);
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let has_cover = tag.is_some_and(|t| !t.pictures().is_empty());
    let has_lyrics = tag
        .is_some_and(|t| t.get_string(ItemKey::Lyrics).is_some_and(|v| !v.trim().is_empty()));
    let non_empty = |v: Option<Cow<'_, str>>| v.is_some_and(|v| !v.trim().is_empty());
    let tags_complete = tag
        .is_some_and(|t| non_empty(t.artist()) && non_empty(t.title()) && non_empty(t.album()));
    Some(Quality::new(
        ext,
        bitrate,
        sample_rate,
        bits,
        has_cover,
        has_lyrics,
        tags_complete,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_format_beats_higher_bitrate_lossy() {
        // 320kbps mp3 must not outrank any flac, whatever the other knobs say
        let flac_low = Quality::new("flac", Some(800), Some(44100), Some(16), false, false, false);
        let mp3_max = Quality::new("mp3", Some(320), Some(48000), None, true, true, true);
        assert!(flac_low.score > mp3_max.score);
    }

    #[test]
    fn cover_wins_within_same_format_and_bitrate() {
        let with = Quality::new("mp3", Some(320), Some(44100), None, true, false, true);
        let without = Quality::new("mp3", Some(320), Some(44100), None, false, true, true);
        assert!(with.score > without.score);
    }

    #[test]
    fn lyrics_rank_below_cover() {
        let cover = Quality::new("mp3", Some(320), Some(44100), None, true, false, false);
        let lyrics = Quality::new("mp3", Some(320), Some(44100), None, false, true, false);
        assert!(cover.score > lyrics.score);
    }

    #[test]
    fn bitrate_buckets_in_32kbps_steps() {
        let a = Quality::new("mp3", Some(320), Some(44100), None, false, false, false);
        let b = Quality::new("mp3", Some(321), Some(44100), None, false, false, false);
        assert_eq!(a.score, b.score, "321kbps buckets to the same as 320");
        // within one bucket, cover outranks lyrics + tag completeness
        let cover = Quality::new("mp3", Some(320), Some(44100), None, true, false, false);
        let minor_flags = Quality::new("mp3", Some(321), Some(44100), None, false, true, true);
        assert!(cover.score > minor_flags.score);
    }

    #[test]
    fn unknown_ext_still_scores_zero_rank() {
        let q = Quality::new("xyz", None, None, None, false, false, false);
        assert_eq!(q.format_rank, 0);
        assert_eq!(q.score, 0);
    }
}
