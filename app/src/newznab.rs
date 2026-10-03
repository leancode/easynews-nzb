use std::collections::HashMap;

use sha1::{Digest, Sha1};

use crate::models::{EasynewsFile, TicketFile};
use crate::xml::{escape, rfc822};

pub const CAPS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <server version="1.0" title="easynews-nzb" strapline="Easynews Newznab indexer"/>
  <limits max="100" default="50"/>
  <registration available="no" open="no"/>
  <searching>
    <search available="yes" supportedParams="q"/>
    <tv-search available="yes" supportedParams="q,season,ep"/>
    <movie-search available="yes" supportedParams="q,year"/>
    <music-search available="yes" supportedParams="q,artist,album"/>
    <book-search available="yes" supportedParams="q,author,title"/>
  </searching>
  <categories>
    <category id="2000" name="Movies">
      <subcat id="2030" name="Movies/SD"/>
      <subcat id="2040" name="Movies/HD"/>
      <subcat id="2045" name="Movies/UHD"/>
    </category>
    <category id="5000" name="TV">
      <subcat id="5030" name="TV/SD"/>
      <subcat id="5040" name="TV/HD"/>
      <subcat id="5045" name="TV/UHD"/>
    </category>
    <category id="3000" name="Audio">
      <subcat id="3010" name="Audio/MP3"/>
      <subcat id="3040" name="Audio/Lossless"/>
    </category>
    <category id="7000" name="Books">
      <subcat id="7020" name="Books/EBook"/>
    </category>
  </categories>
</caps>
"#;

pub enum SearchMode {
    Search,
    Movie {
        year: Option<String>,
    },
    TvSearch {
        season: Option<u32>,
        ep: Option<u32>,
    },
    Music {
        artist: Option<String>,
        album: Option<String>,
    },
    Book {
        author: Option<String>,
        title: Option<String>,
    },
}

pub struct Release {
    pub title: String,
    pub category: u32,
    pub size: u64,
    pub timestamp: i64,
    pub poster: String,
    pub group: String,
    pub files: Vec<TicketFile>,
}

impl Release {
    pub fn category_name(&self) -> &'static str {
        match self.category {
            2030 | 2040 | 2045 => "Movies",
            5030 | 5040 | 5045 => "TV",
            3010 | 3040 => "Audio",
            7020 => "Books",
            _ => "Other",
        }
    }
}

/// The Easynews `gps` query terms and `fty[]` type filter to use for this search mode.
/// Easynews has no "browse latest" query; it returns a plain-text "No search" for a blank `gps`.
/// Prowlarr (and the apps it syncs to) validate a newly added indexer with a blank-query test per
/// search type and expect a non-empty result, so a genuinely blank query term falls back to the
/// current year — virtually guaranteed to match something — rather than a term Easynews rejects.
fn non_blank(term: String) -> String {
    if term.trim().is_empty() {
        chrono::Utc::now().format("%Y").to_string()
    } else {
        term
    }
}

pub fn easynews_query(mode: &SearchMode, q: Option<&str>) -> (Vec<String>, Vec<&'static str>) {
    match mode {
        SearchMode::Search => (vec![non_blank(q.unwrap_or("").to_string())], vec![]),
        SearchMode::Movie { year } => {
            let mut terms = q.unwrap_or("").to_string();
            if let Some(y) = year {
                terms.push(' ');
                terms.push_str(y);
            }
            (vec![non_blank(terms)], vec!["VIDEO"])
        }
        SearchMode::TvSearch { season, ep } => {
            let base = q.unwrap_or("");
            let mut candidates = Vec::new();
            match (season, ep) {
                (Some(s), Some(e)) => {
                    candidates.push(format!("{base} S{s:02}E{e:02}"));
                    candidates.push(format!("{base} {s}x{e:02}"));
                }
                // A season search with no episode number: Sonarr wants a season pack. Easynews
                // posts each episode as its own file but shares a `setid` across a season, so
                // search broadly for the season and group matching files in build_releases.
                (Some(s), None) => candidates.push(format!("{base} S{s:02}")),
                _ => candidates.push(non_blank(base.to_string())),
            }
            (candidates, vec!["VIDEO"])
        }
        SearchMode::Music { artist, album } => {
            let mut terms = String::new();
            if let Some(a) = artist {
                terms.push_str(a);
            }
            if let Some(a) = album {
                if !terms.is_empty() {
                    terms.push(' ');
                }
                terms.push_str(a);
            }
            if terms.is_empty() {
                terms = q.unwrap_or("").to_string();
            }
            (vec![non_blank(terms)], vec!["AUDIO"])
        }
        SearchMode::Book { author, title } => {
            let mut terms = String::new();
            if let Some(a) = author {
                terms.push_str(a);
            }
            if let Some(t) = title {
                if !terms.is_empty() {
                    terms.push(' ');
                }
                terms.push_str(t);
            }
            if terms.is_empty() {
                terms = q.unwrap_or("").to_string();
            }
            (vec![non_blank(terms)], vec!["DOCUMENT"])
        }
    }
}

/// Junk-size floors below which a result is treated as a sample, trailer, or corrupt/mislabeled
/// post rather than a real release. All four are independently overridable (see `Config`) since
/// what counts as "too small to be real" is a judgment call that varies by taste and by how
/// aggressively a given Usenet group's posters compress things.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// A full-length film below this is almost certainly a sample, trailer, or very low quality
    /// rip. Default 200 MB.
    pub min_movie_size: u64,
    /// Lower than the movie floor on purpose: modern x265 encodes a TV episode noticeably smaller
    /// than x264 at the same visual quality, so a legitimate, efficiently-encoded episode can land
    /// under 200 MB. Default 150 MB.
    pub min_tv_size: u64,
    /// Below a typical short/lower-bitrate track (e.g. "Stay" by Maurice Williams, ~1:38) but
    /// still well above a corrupt or mislabeled post. Default 3 MB.
    pub min_audio_size: u64,
    /// A real epub can be tiny — verified short-story competition winners as small as 4.8 KB are
    /// genuine, complete books, not stubs. Default 4 KB.
    pub min_book_size: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            min_movie_size: 200 * 1024 * 1024,
            min_tv_size: 150 * 1024 * 1024,
            min_audio_size: 3 * 1024 * 1024,
            min_book_size: 4 * 1024,
        }
    }
}

fn is_junk(f: &EasynewsFile, mode: &SearchMode, thresholds: &Thresholds) -> bool {
    if f.passwd || f.virus {
        return true;
    }
    if f.file_name.to_lowercase().contains("sample") {
        return true;
    }
    if matches!(mode, SearchMode::Book { .. }) {
        // Only epub: Easynews' DOCUMENT type also covers pdf/mobi/azw3/comics/etc, which this
        // project deliberately doesn't try to sort out.
        f.file_type != "DOCUMENT"
            || !f.extension.eq_ignore_ascii_case(".epub")
            || f.size < thresholds.min_book_size
    } else {
        match f.file_type.as_str() {
            // Movie-vs-TV isn't known yet here (it depends on the search mode, or a filename
            // heuristic under plain search) — the matching floor is applied once that's resolved,
            // in build_releases' video loop.
            "VIDEO" => false,
            "AUDIO" => f.size < thresholds.min_audio_size,
            _ => true,
        }
    }
}

fn parse_height(fullres: &str) -> Option<u32> {
    let s = fullres.to_lowercase();
    let digits: String = if let Some(idx) = s.find('x') {
        s[idx + 1..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect()
    } else {
        s.chars().take_while(|c| c.is_ascii_digit()).collect()
    };
    digits.parse().ok()
}

fn video_category(fullres: Option<&str>, is_tv: bool) -> u32 {
    let height = fullres.and_then(parse_height).unwrap_or(0);
    let (sd, hd, uhd) = if is_tv {
        (5030, 5040, 5045)
    } else {
        (2030, 2040, 2045)
    };
    if height >= 2000 {
        uhd
    } else if height >= 700 {
        hd
    } else {
        sd
    }
}

fn looks_like_tv(name: &str) -> bool {
    let upper = name.to_uppercase();
    let bytes = upper.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'S' && i + 2 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < bytes.len()
                && bytes[j] == b'E'
                && j + 1 < bytes.len()
                && bytes[j + 1].is_ascii_digit()
            {
                return true;
            }
        }
    }
    false
}

fn matches_episode(name: &str, season: u32, ep: u32) -> bool {
    let upper = name.to_uppercase();
    upper.contains(&format!("S{season:02}E{ep:02}")) || upper.contains(&format!("{season}X{ep:02}"))
}

fn matches_season(name: &str, season: u32) -> bool {
    name.to_uppercase().contains(&format!("S{season:02}"))
}

/// A reasonable "Show Name S01"-shaped title for a season-pack release, derived from the longest
/// common prefix of the group's file names (which — for a real set of per-episode files — usually
/// extends up to where the episode number diverges) truncated right after the season marker, so
/// Sonarr's own release-title parser reads it as a full season rather than a single episode.
fn season_pack_title(files: &[&EasynewsFile], season: u32, query: &str) -> String {
    let marker = format!("S{season:02}");
    let prefix = longest_common_prefix(files.iter().map(|f| f.file_name.as_str()));
    if let Some(idx) = prefix.to_uppercase().find(&marker) {
        // Many real uploads name individual episode files without repeating the show (just
        // "S01E01.mkv" in a show-named folder), so the common prefix can be the season marker
        // itself with nothing meaningful before it — not a usable title on its own.
        let before = prefix[..idx].trim_matches(|c: char| !c.is_alphanumeric());
        if !before.is_empty() {
            let end = idx + marker.len();
            let title = prefix[..end].trim_end_matches(['.', '-', '_', ' ']);
            if !title.is_empty() {
                return title.to_string();
            }
        }
    }
    // Fall back to what was actually searched for — the show name Sonarr sent — rather than an
    // uninformative filename-derived fragment. `query` is the Easynews search term, which for a
    // season search already ends in the season marker (see `easynews_query`), so don't double it.
    let query = query.trim();
    if query.is_empty() {
        files[0].file_name.clone()
    } else if query.to_uppercase().contains(&marker) {
        query.to_string()
    } else {
        format!("{query} {marker}")
    }
}

fn shares_letters(a: &str, b: &str) -> bool {
    if b.trim().is_empty() {
        return true;
    }
    let a_letters: std::collections::HashSet<char> = a
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect();
    b.to_lowercase()
        .chars()
        .filter(|c| c.is_alphabetic())
        .any(|c| a_letters.contains(&c))
}

/// True when `file_name` plausibly matches the show that was searched for. A letter-overlap check
/// (see `shares_letters`) is far too weak here — any two real titles of reasonable length share
/// *some* letters — so this requires at least one real word from the query (4+ letters, and not
/// the season marker itself, which is always present and tells us nothing about the show) to
/// actually appear in the file name. TV release names reliably include the show name whenever
/// there's any identifying text in them at all, unlike music track names, which is why this
/// stricter check is only used for season packs and `shares_letters` is left alone for audio.
/// A query with no significant words left after filtering (e.g. a blank-query fallback probe)
/// matches everything, same as before.
fn matches_show_name(file_name: &str, query: &str, season: u32) -> bool {
    let marker = format!("s{season:02}");
    let name = file_name.to_lowercase();
    let query_lower = query.to_lowercase();
    let words: Vec<&str> = query_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4 && *w != marker)
        .collect();
    words.is_empty() || words.iter().any(|w| name.contains(w))
}

fn to_ticket_file(f: &EasynewsFile) -> TicketFile {
    TicketFile {
        hash: f.hash.clone(),
        extension: f.extension.clone(),
        file_name: f.file_name.clone(),
        size: f.size,
        sig: f.sig.clone(),
    }
}

fn longest_common_prefix<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let mut iter = names;
    let mut prefix = match iter.next() {
        Some(s) => s.to_string(),
        None => return String::new(),
    };
    for name in iter {
        let common: usize = prefix
            .chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .count();
        prefix = prefix.chars().take(common).collect();
    }
    prefix
        .trim_end_matches(|c: char| c.is_whitespace() || c == '-' || c == '_' || c.is_ascii_digit())
        .trim_end()
        .to_string()
}

/// Build releases from a filtered set of Easynews search results, per docs/DESIGN.md section 3.3.
pub fn build_releases(
    mode: &SearchMode,
    query: &str,
    files: Vec<EasynewsFile>,
    thresholds: &Thresholds,
) -> Vec<Release> {
    let candidates: Vec<EasynewsFile> = files
        .into_iter()
        .filter(|f| !is_junk(f, mode, thresholds))
        .collect();

    if matches!(mode, SearchMode::Book { .. }) {
        // One release per file, same as video: Easynews posts ebooks individually.
        return candidates
            .into_iter()
            .map(|f| Release {
                title: f.file_name.clone(),
                category: 7020,
                size: f.size,
                timestamp: f.timestamp,
                poster: f.poster.clone(),
                group: f.groups.first().cloned().unwrap_or_default(),
                files: vec![to_ticket_file(&f)],
            })
            .collect();
    }

    let mut releases = Vec::new();

    let (video, audio): (Vec<_>, Vec<_>) =
        candidates.into_iter().partition(|f| f.file_type == "VIDEO");

    // Season pack: a season search with no specific episode. Easynews shares one `setid` across
    // a season's episode files (same mechanism as audio albums), so group on that; a lone file
    // with no grouped partners just falls through to the per-file loop below as a single episode.
    let mut packed: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let SearchMode::TvSearch {
        season: Some(season),
        ep: None,
    } = mode
    {
        let season = *season;
        let mut groups: HashMap<String, Vec<&EasynewsFile>> = HashMap::new();
        for f in &video {
            if f.size < thresholds.min_tv_size || !matches_season(&f.file_name, season) {
                continue;
            }
            if let Some(id) = f.setid.as_deref().filter(|id| !id.is_empty()) {
                groups.entry(format!("setid:{id}")).or_default().push(f);
            }
        }
        for files in groups.into_values() {
            if files.len() < 2
                || !files
                    .iter()
                    .any(|f| matches_show_name(&f.file_name, query, season))
            {
                continue;
            }
            for f in &files {
                packed.insert(f.hash.clone());
            }
            let title = season_pack_title(&files, season, query);
            let size: u64 = files.iter().map(|f| f.size).sum();
            let timestamp = files.iter().map(|f| f.timestamp).max().unwrap_or(0);
            let category = video_category(files[0].fullres.as_deref(), true);
            releases.push(Release {
                title,
                category,
                size,
                timestamp,
                poster: files[0].poster.clone(),
                group: files[0].groups.first().cloned().unwrap_or_default(),
                files: files.iter().map(|f| to_ticket_file(f)).collect(),
            });
        }
    }

    // Video: one release per file (per-episode singles, and anything not folded into a pack above).
    for f in video {
        if packed.contains(&f.hash) {
            continue;
        }
        let is_tv = match mode {
            SearchMode::Movie { .. } => false,
            SearchMode::TvSearch { .. } => true,
            SearchMode::Music { .. } | SearchMode::Book { .. } => continue,
            SearchMode::Search => looks_like_tv(&f.file_name),
        };
        let min_size = if is_tv {
            thresholds.min_tv_size
        } else {
            thresholds.min_movie_size
        };
        if f.size < min_size {
            continue;
        }
        match mode {
            SearchMode::Movie { year } => {
                if let Some(y) = year
                    && !f.file_name.contains(y.as_str())
                {
                    continue;
                }
            }
            SearchMode::TvSearch { season, ep } => match (season, ep) {
                (Some(s), Some(e)) if !matches_episode(&f.file_name, *s, *e) => continue,
                (Some(s), None) if !matches_season(&f.file_name, *s) => continue,
                _ => {}
            },
            _ => {}
        }
        let category = video_category(f.fullres.as_deref(), is_tv);
        releases.push(Release {
            title: f.file_name.clone(),
            category,
            size: f.size,
            timestamp: f.timestamp,
            poster: f.poster.clone(),
            group: f.groups.first().cloned().unwrap_or_default(),
            files: vec![to_ticket_file(&f)],
        });
    }

    if matches!(mode, SearchMode::Movie { .. } | SearchMode::TvSearch { .. }) {
        return releases;
    }

    // Audio: group by setid (fallback: poster + prefix of the file name up to the last " - ").
    let mut groups: HashMap<String, Vec<EasynewsFile>> = HashMap::new();
    for f in audio {
        let key = match &f.setid {
            Some(id) if !id.is_empty() => format!("setid:{id}"),
            _ => {
                let prefix = f
                    .file_name
                    .rsplit_once(" - ")
                    .map(|(p, _)| p)
                    .unwrap_or(&f.file_name);
                format!("poster:{}:{}", f.poster, prefix)
            }
        };
        groups.entry(key).or_default().push(f);
    }
    for (_, group_files) in groups {
        if !group_files
            .iter()
            .any(|f| shares_letters(&f.file_name, query))
        {
            continue;
        }
        let title = longest_common_prefix(group_files.iter().map(|f| f.file_name.as_str()));
        let title = if title.is_empty() {
            group_files[0].file_name.clone()
        } else {
            title
        };
        let size: u64 = group_files.iter().map(|f| f.size).sum();
        let has_flac = group_files
            .iter()
            .any(|f| f.extension.eq_ignore_ascii_case(".flac"));
        let timestamp = group_files.iter().map(|f| f.timestamp).max().unwrap_or(0);
        let poster = group_files[0].poster.clone();
        let group_name = group_files[0].groups.first().cloned().unwrap_or_default();
        let files = group_files.iter().map(to_ticket_file).collect();
        releases.push(Release {
            title,
            category: if has_flac { 3040 } else { 3010 },
            size,
            timestamp,
            poster,
            group: group_name,
            files,
        });
    }

    releases
}

pub fn ticket_token(files: &[TicketFile]) -> String {
    let mut hashes: Vec<&str> = files.iter().map(|f| f.hash.as_str()).collect();
    hashes.sort_unstable();
    let mut hasher = Sha1::new();
    hasher.update(hashes.join(",").as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)[..24].to_string()
}

pub struct RssItem<'a> {
    pub release: &'a Release,
    pub token: &'a str,
    pub link: String,
}

pub fn build_rss(items: &[RssItem], total: usize, offset: usize) -> String {
    let mut out = String::new();
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push('\n');
    out.push_str(r#"<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/" xmlns:atom="http://www.w3.org/2005/Atom">"#);
    out.push_str("<channel>");
    out.push_str("<title>easynews-nzb</title>");
    out.push_str(&format!(
        r#"<newznab:response offset="{offset}" total="{total}"/>"#
    ));
    for item in items {
        let r = item.release;
        out.push_str("<item>");
        out.push_str(&format!("<title>{}</title>", escape(&r.title)));
        out.push_str(&format!(
            r#"<guid isPermaLink="true">{}</guid>"#,
            escape(&item.link)
        ));
        out.push_str(&format!("<link>{}</link>", escape(&item.link)));
        out.push_str(&format!("<pubDate>{}</pubDate>", rfc822(r.timestamp)));
        out.push_str(&format!(
            "<category>{}</category>",
            escape(r.category_name())
        ));
        out.push_str(&format!(
            r#"<enclosure url="{}" length="{}" type="application/x-nzb"/>"#,
            escape(&item.link),
            r.size
        ));
        out.push_str(&format!(
            r#"<newznab:attr name="category" value="{}"/>"#,
            r.category
        ));
        out.push_str(&format!(
            r#"<newznab:attr name="size" value="{}"/>"#,
            r.size
        ));
        out.push_str(&format!(
            r#"<newznab:attr name="guid" value="{}"/>"#,
            escape(item.token)
        ));
        out.push_str(&format!(
            r#"<newznab:attr name="files" value="{}"/>"#,
            r.files.len()
        ));
        out.push_str(&format!(
            r#"<newznab:attr name="poster" value="{}"/>"#,
            escape(&r.poster)
        ));
        out.push_str(&format!(
            r#"<newznab:attr name="group" value="{}"/>"#,
            escape(&r.group)
        ));
        out.push_str(&format!(
            r#"<newznab:attr name="usenetdate" value="{}"/>"#,
            rfc822(r.timestamp)
        ));
        out.push_str(r#"<newznab:attr name="grabs" value="0"/>"#);
        out.push_str("</item>");
    }
    out.push_str("</channel></rss>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SearchResponse;
    use serde_json::json;

    /// Synthetic search response shaped like docs/DESIGN.md section 1.1 (masked/fake hash and
    /// sig values, not captured from a live account).
    fn movie_search_fixture() -> serde_json::Value {
        json!({
            "sid": "fake-sid",
            "dlFarm": "auto",
            "dlPort": "443",
            "downURL": "https://members.easynews.com/dl",
            "data": [
                {
                    "0": "deadbeef0001", "hash": "deadbeef0001",
                    "10": "Some.Movie.2011.1080p", "fn": "Some.Movie.2011.1080p",
                    "11": ".mkv", "extension": ".mkv",
                    "rawSize": 900_000_000u64,
                    "ts": 1_700_000_000i64,
                    "7": "poster1", "poster": "poster1",
                    "9": ["alt.binaries.movies"], "group_list": ["alt.binaries.movies"],
                    "type": "VIDEO",
                    "fullres": "1920x1080",
                    "passwd": false, "virus": false,
                    "sig": "fakesig1",
                },
                {
                    "0": "deadbeef0002", "hash": "deadbeef0002",
                    "10": "Some.Movie.2011.sample", "fn": "Some.Movie.2011.sample",
                    "11": ".mkv", "extension": ".mkv",
                    "rawSize": 50_000_000u64,
                    "ts": 1_700_000_000i64,
                    "type": "VIDEO",
                    "fullres": "1280x720",
                    "passwd": false, "virus": false,
                    "sig": "fakesig2",
                },
                {
                    "0": "deadbeef0003", "hash": "deadbeef0003",
                    "10": "Some.Movie.2011.infected", "fn": "Some.Movie.2011.infected",
                    "11": ".mkv", "extension": ".mkv",
                    "rawSize": 900_000_000u64,
                    "ts": 1_700_000_000i64,
                    "type": "VIDEO",
                    "fullres": "1920x1080",
                    "passwd": false, "virus": true,
                    "sig": "fakesig3",
                },
            ],
        })
    }

    fn album_search_fixture() -> serde_json::Value {
        json!({
            "data": [
                {
                    "0": "aaaa0001", "10": "Artist - Album - 01 - Track One", "11": ".flac",
                    "rawSize": 40_000_000u64, "ts": 1_700_000_000i64, "7": "poster2",
                    "19": "set-1", "type": "AUDIO", "passwd": false, "virus": false, "sig": "s1",
                },
                {
                    "0": "aaaa0002", "10": "Artist - Album - 02 - Track Two", "11": ".flac",
                    "rawSize": 42_000_000u64, "ts": 1_700_000_000i64, "7": "poster2",
                    "19": "set-1", "type": "AUDIO", "passwd": false, "virus": false, "sig": "s2",
                },
            ],
        })
    }

    #[test]
    fn movie_release_drops_junk_and_picks_uhd_or_hd_category() {
        let resp = SearchResponse::from_json(&movie_search_fixture()).unwrap();
        assert_eq!(resp.data.len(), 3);
        let mode = SearchMode::Movie {
            year: Some("2011".into()),
        };
        let releases = build_releases(&mode, "Some Movie", resp.data, &Thresholds::default());
        assert_eq!(
            releases.len(),
            1,
            "sample and virus-flagged files must be dropped"
        );
        assert_eq!(releases[0].category, 2040);
        assert_eq!(releases[0].title, "Some.Movie.2011.1080p");
    }

    #[test]
    fn music_files_group_by_setid_into_one_lossless_release() {
        let resp = SearchResponse::from_json(&album_search_fixture()).unwrap();
        let mode = SearchMode::Music {
            artist: Some("Artist".into()),
            album: Some("Album".into()),
        };
        let releases = build_releases(&mode, "Artist Album", resp.data, &Thresholds::default());
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].category, 3040);
        assert_eq!(releases[0].files.len(), 2);
        assert_eq!(releases[0].size, 82_000_000);
    }

    fn season_pack_fixture() -> serde_json::Value {
        json!({
            "data": [
                {
                    "0": "ss110001", "10": "Some.Show.S01E01.Pilot.1080p", "11": ".mkv",
                    "rawSize": 200_000_000u64, "ts": 1_700_000_000i64, "7": "poster4",
                    "19": "season-set-1", "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "ss1",
                },
                {
                    "0": "ss110002", "10": "Some.Show.S01E02.Second.1080p", "11": ".mkv",
                    "rawSize": 210_000_000u64, "ts": 1_700_000_000i64, "7": "poster4",
                    "19": "season-set-1", "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "ss2",
                },
                {
                    "0": "ss110003", "10": "Some.Show.S01E03.Third.1080p", "11": ".mkv",
                    "rawSize": 190_000_000u64, "ts": 1_700_000_000i64, "7": "poster4",
                    "19": "season-set-1", "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "ss3",
                },
                {
                    "0": "ss110004", "10": "Some.Show.S02E01.Lone.Episode.1080p", "11": ".mkv",
                    "rawSize": 200_000_000u64, "ts": 1_700_000_000i64, "7": "poster4",
                    "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "ss4",
                },
            ],
        })
    }

    #[test]
    fn season_search_groups_shared_setid_into_one_pack_and_leaves_singles_alone() {
        let resp = SearchResponse::from_json(&season_pack_fixture()).unwrap();
        let mode = SearchMode::TvSearch {
            season: Some(1),
            ep: None,
        };
        let releases = build_releases(&mode, "Some Show", resp.data, &Thresholds::default());
        assert_eq!(
            releases.len(),
            1,
            "the S02 single shouldn't match a S01 search at all"
        );
        assert_eq!(releases[0].files.len(), 3);
        assert_eq!(releases[0].size, 600_000_000);
        assert_eq!(releases[0].title, "Some.Show.S01");
        assert_eq!(releases[0].category, 5040);
    }

    #[test]
    fn season_pack_title_falls_back_to_query_when_filenames_omit_the_show_name() {
        // Real-world case found live (2026-10-02): episode files inside a show-named folder,
        // named just "S01E01.mkv" etc, with no show name of their own to derive a title from.
        // Unit-tests `season_pack_title` directly: such files no longer group at all (see
        // `season_search_does_not_group_files_with_no_show_name_match` below), but the title
        // function itself should still fall back sensibly if it's ever called with them.
        let resp = SearchResponse::from_json(&json!({
            "data": [
                {
                    "0": "nn110001", "10": "S01E01", "11": ".mkv",
                    "rawSize": 200_000_000u64, "ts": 1_700_000_000i64, "7": "poster5",
                    "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "n1",
                },
                {
                    "0": "nn110002", "10": "S01E02", "11": ".mkv",
                    "rawSize": 210_000_000u64, "ts": 1_700_000_000i64, "7": "poster5",
                    "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "n2",
                },
            ],
        }))
        .unwrap();
        let files: Vec<&EasynewsFile> = resp.data.iter().collect();
        assert_eq!(season_pack_title(&files, 1, "Friends"), "Friends S01");
    }

    #[test]
    fn season_pack_title_does_not_double_the_season_marker() {
        // Regression (found live 2026-10-02): `query` passed to build_releases is the Easynews
        // search term, which for a season search already ends in "S01" (see easynews_query) --
        // appending the marker again produced titles like "Friends S01 S01".
        let resp = SearchResponse::from_json(&json!({
            "data": [
                {
                    "0": "qq110001", "10": "S01E01", "11": ".mkv",
                    "rawSize": 200_000_000u64, "ts": 1_700_000_000i64, "7": "poster6",
                    "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "q1",
                },
            ],
        }))
        .unwrap();
        let files: Vec<&EasynewsFile> = resp.data.iter().collect();
        assert_eq!(season_pack_title(&files, 1, "Friends S01"), "Friends S01");
    }

    #[test]
    fn season_search_does_not_group_files_with_no_show_name_match() {
        // The real false positive found live (2026-10-02): unrelated "Homicide" episode files
        // matched a "Friends S01" search purely because they happened to share some letters with
        // the query -- any two real titles of reasonable length do. Fixed by requiring a real
        // word from the query to actually appear in the file name; these files should now fall
        // through as separate singles instead of being bundled into a wrongly-labeled pack.
        let resp = SearchResponse::from_json(&json!({
            "data": [
                {
                    "0": "hh110001", "10": "[S01.E01] Homicide - Carnegie Deli Massacre", "11": ".mkv",
                    "rawSize": 200_000_000u64, "ts": 1_700_000_000i64, "7": "poster7",
                    "19": "homicide-set-1", "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "h1",
                },
                {
                    "0": "hh110002", "10": "[S01.E03] Homicide - Vanished On Wall Street", "11": ".mkv",
                    "rawSize": 210_000_000u64, "ts": 1_700_000_000i64, "7": "poster7",
                    "19": "homicide-set-1", "type": "VIDEO", "fullres": "1920x1080",
                    "passwd": false, "virus": false, "sig": "h2",
                },
            ],
        }))
        .unwrap();
        let mode = SearchMode::TvSearch {
            season: Some(1),
            ep: None,
        };
        let releases = build_releases(&mode, "Friends S01", resp.data, &Thresholds::default());
        assert_eq!(
            releases.len(),
            2,
            "unrelated files sharing letters but no real word with the query must not be bundled into one pack"
        );
        assert!(releases.iter().all(|r| r.files.len() == 1));
    }

    /// Shaped like a real Easynews DOCUMENT response (verified 2026-10-02): small epub files,
    /// plus a non-epub document and an undersized one that must both be dropped.
    fn book_search_fixture() -> serde_json::Value {
        json!({
            "data": [
                {
                    "0": "bb110001", "10": "King of Pride - Ana Huang", "11": ".epub",
                    "rawSize": 1_968_470u64, "ts": 1_700_000_000i64, "7": "poster3",
                    "type": "DOCUMENT", "passwd": false, "virus": false, "sig": "b1",
                },
                {
                    "0": "bb110002", "10": "King of Wrath - Ana Huang", "11": ".epub",
                    "rawSize": 754_935u64, "ts": 1_700_000_000i64, "7": "poster3",
                    "type": "DOCUMENT", "passwd": false, "virus": false, "sig": "b2",
                },
                {
                    "0": "bb110003", "10": "King of Wrath - Ana Huang", "11": ".pdf",
                    "rawSize": 2_000_000u64, "ts": 1_700_000_000i64, "7": "poster3",
                    "type": "DOCUMENT", "passwd": false, "virus": false, "sig": "b3",
                },
                {
                    "0": "bb110004", "10": "corrupt-empty-post", "11": ".epub",
                    "rawSize": 100u64, "ts": 1_700_000_000i64, "7": "poster3",
                    "type": "DOCUMENT", "passwd": false, "virus": false, "sig": "b4",
                },
            ],
        })
    }

    #[test]
    fn book_search_keeps_only_epub_above_the_size_floor() {
        let resp = SearchResponse::from_json(&book_search_fixture()).unwrap();
        let mode = SearchMode::Book {
            author: Some("Ana Huang".into()),
            title: None,
        };
        let releases = build_releases(&mode, "Ana Huang", resp.data, &Thresholds::default());
        assert_eq!(releases.len(), 2, "pdf and undersized epub must be dropped");
        assert!(releases.iter().all(|r| r.category == 7020));
        assert!(releases.iter().all(|r| r.files.len() == 1));
    }

    #[test]
    fn ticket_token_is_stable_regardless_of_file_order() {
        let files = vec![
            TicketFile {
                hash: "b".into(),
                extension: ".mkv".into(),
                file_name: "b".into(),
                size: 1,
                sig: "x".into(),
            },
            TicketFile {
                hash: "a".into(),
                extension: ".mkv".into(),
                file_name: "a".into(),
                size: 1,
                sig: "x".into(),
            },
        ];
        let reversed: Vec<TicketFile> = files.iter().cloned().rev().collect();
        assert_eq!(ticket_token(&files), ticket_token(&reversed));
        assert_eq!(ticket_token(&files).len(), 24);
    }

    #[test]
    fn parses_height_from_common_fullres_shapes() {
        assert_eq!(parse_height("1920x1080"), Some(1080));
        assert_eq!(parse_height("2160p"), Some(2160));
        assert_eq!(parse_height("720"), Some(720));
    }
}
