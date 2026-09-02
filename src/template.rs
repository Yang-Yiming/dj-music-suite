//! Path template: e.g. "{artist}/{filename}.{ext}", relative to the library
//! root. Each `/`-separated segment becomes one path component; a segment may
//! mix literal text with `{placeholders}`.

#[derive(Clone, Copy, PartialEq)]
enum Ph {
    /// primary artist: album artist, or first of the artist list
    Artist,
    /// full artist list as stored in the tag
    Artists,
    Title,
    Album,
    Filename,
    Ext,
}

impl Ph {
    fn parse(name: &str) -> Option<Ph> {
        Some(match name {
            "artist" => Ph::Artist,
            "artists" => Ph::Artists,
            "title" => Ph::Title,
            "album" => Ph::Album,
            "filename" => Ph::Filename,
            "ext" => Ph::Ext,
            _ => return None,
        })
    }

    fn name(&self) -> &'static str {
        match self {
            Ph::Artist => "artist",
            Ph::Artists => "artists",
            Ph::Title => "title",
            Ph::Album => "album",
            Ph::Filename => "filename",
            Ph::Ext => "ext",
        }
    }
}

enum Part {
    Text(String),
    Placeholder(Ph),
}

pub struct Template {
    segments: Vec<Vec<Part>>,
}

pub struct RenderValues<'a> {
    pub primary_artist: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub title: Option<&'a str>,
    pub album: Option<&'a str>,
    pub filename: &'a str,
    pub ext: &'a str,
}

impl Template {
    pub fn parse(raw: &str) -> Result<Template, String> {
        if raw.is_empty() {
            return Err("template is empty".into());
        }
        if raw.starts_with('/') {
            return Err("template must be relative to the root (no leading /)".into());
        }
        let mut segments = Vec::new();
        for segment in raw.split('/') {
            if segment.is_empty() {
                return Err("empty path segment (check for duplicate slashes)".into());
            }
            if segment == "." || segment == ".." {
                return Err(format!("refusing path segment \"{segment}\""));
            }
            let mut parts = Vec::new();
            let mut rest = segment;
            while let Some(start) = rest.find('{') {
                if start > 0 {
                    parts.push(Part::Text(rest[..start].to_string()));
                }
                let after = &rest[start + 1..];
                let Some(end) = after.find('}') else {
                    return Err(format!("unclosed '{{' in segment \"{segment}\""));
                };
                let name = after[..end].trim();
                let ph = Ph::parse(name).ok_or_else(|| {
                    format!("unknown placeholder {{{name}}} (valid: artist, artists, title, album, filename, ext)")
                })?;
                parts.push(Part::Placeholder(ph));
                rest = &after[end + 1..];
            }
            if !rest.is_empty() {
                parts.push(Part::Text(rest.to_string()));
            }
            segments.push(parts);
        }
        Ok(Template { segments })
    }

    /// Render to sanitized path components. Err carries the reason (missing
    /// tag or an empty component) so callers can flag the file.
    pub fn render(&self, vals: &RenderValues) -> Result<Vec<String>, String> {
        let mut out = Vec::with_capacity(self.segments.len());
        for parts in &self.segments {
            let mut component = String::new();
            for part in parts {
                match part {
                    Part::Text(text) => component.push_str(text),
                    Part::Placeholder(ph) => {
                        let value = match ph {
                            Ph::Artist => vals.primary_artist,
                            Ph::Artists => vals.artist,
                            Ph::Title => vals.title,
                            Ph::Album => vals.album,
                            Ph::Filename => Some(vals.filename),
                            Ph::Ext => Some(vals.ext),
                        };
                        let Some(value) = value else {
                            return Err(format!("missing {{{}}}", ph.name()));
                        };
                        component.push_str(value.trim());
                    }
                }
            }
            let Some(sanitized) = sanitize_component(&component) else {
                return Err(format!("path component \"{component}\" is empty after sanitizing"));
            };
            out.push(sanitized);
        }
        Ok(out)
    }
}

/// Make a single path component filesystem-safe; None when nothing remains.
pub fn sanitize_component(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if matches!(ch, '/' | '\\' | ':' | '?' | '*' | '"' | '<' | '>' | '|') || ch.is_control() {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim_matches(|c| c == ' ' || c == '.');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals<'a>() -> RenderValues<'a> {
        RenderValues {
            primary_artist: Some("ARTMS"),
            artist: Some("ARTMS"),
            title: Some("BURN"),
            album: None,
            filename: "old name",
            ext: "mp3",
        }
    }

    #[test]
    fn parse_rejects_bad_templates() {
        assert!(Template::parse("").is_err());
        assert!(Template::parse("/abs").is_err());
        assert!(Template::parse("a//b").is_err());
        assert!(Template::parse("a/..").is_err());
        assert!(Template::parse("{nope}").is_err());
        assert!(Template::parse("{artist").is_err());
    }

    #[test]
    fn render_artist_is_primary_artists_is_full_list() {
        let v = RenderValues {
            primary_artist: Some("Ahadadream"),
            artist: Some("Ahadadream/Skrillex/Raf Saperra"),
            title: Some("Bass Dhol"),
            album: None,
            filename: "x",
            ext: "mp3",
        };
        let t = Template::parse("{artists} - {title}.{ext}").unwrap();
        assert_eq!(
            t.render(&v).unwrap(),
            vec!["Ahadadream_Skrillex_Raf Saperra - Bass Dhol.mp3"]
        );
        let t = Template::parse("{artist} - {title}.{ext}").unwrap();
        assert_eq!(t.render(&v).unwrap(), vec!["Ahadadream - Bass Dhol.mp3"]);
    }

    #[test]
    fn render_missing_artists_is_err() {
        let mut v = vals();
        v.artist = None;
        let t = Template::parse("{artists}.{ext}").unwrap();
        let err = t.render(&v).unwrap_err();
        assert!(err.contains("artists"));
    }

    #[test]
    fn render_splits_and_keeps_literals() {
        let t = Template::parse("{artist}/{filename}.{ext}").unwrap();
        assert_eq!(t.render(&vals()).unwrap(), vec!["ARTMS", "old name.mp3"]);
        let t = Template::parse("{artist} - {title}.{ext}").unwrap();
        assert_eq!(t.render(&vals()).unwrap(), vec!["ARTMS - BURN.mp3"]);
    }

    #[test]
    fn render_missing_tag_is_err() {
        let t = Template::parse("{artist}/{album}/{title}.{ext}").unwrap();
        let err = t.render(&vals()).unwrap_err();
        assert!(err.contains("album"));
    }

    #[test]
    fn sanitize_replaces_illegal_characters() {
        assert_eq!(sanitize_component("a/b:c?d").as_deref(), Some("a_b_c_d"));
        assert_eq!(sanitize_component("  .. "), None);
        assert_eq!(sanitize_component(".hidden").as_deref(), Some("hidden"));
    }
}
