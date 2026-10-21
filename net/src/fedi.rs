//! Reels from the Fediverse, read anonymously through Tor: short videos
//! from Loops (made for them), PeerTube servers and Mastodon-compatible
//! servers' posts.
//!
//! (Pixelfed can't be read this way: it shows its feeds only to people
//! signed in there.)
//!
//! There's no account and nothing is posted: the app only reads public
//! feeds, and which servers it reads stays on the phone. Every request goes
//! through the `Connect` it's given: Tor by default, so servers see a Tor
//! exit, never the phone. (In the opt-in fast mode the app passes
//! [`https::direct`] instead, and servers see the phone's address.)

use crate::https::{self, Connect};
use serde_json::Value;

const MAX_JSON: usize = 4 << 20;
const MAX_IMAGE: usize = 15 << 20;
/// Longest video that counts as a reel, in seconds.
pub const MAX_REEL_S: u32 = 180;
/// Pages read at most for one call, looking for reels among longer videos.
const SCAN_PAGES: usize = 3;
/// Enough reels for one call.
const ENOUGH: usize = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// PeerTube.
    Videos,
    /// Mastodon or another Mastodon-compatible server: posts with videos.
    Social,
    /// Loops: short upright videos, like Reels.
    Loops,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Post {
    pub id: String,
    pub host: String,
    pub author: String,
    /// user@host
    pub handle: String,
    pub text: String,
    /// The post's web page, for sharing.
    pub link: String,
    /// Unix seconds.
    pub created_at: i64,
    pub media: Vec<Item>,
    /// Likes on the site.
    pub likes: u32,
    /// Hashtags, lowercase, without '#'.
    pub tags: Vec<String>,
    /// The site's topic for it (PeerTube categories), if any.
    pub topic: String,
    /// Its language code, if known.
    pub lang: String,
}

impl Post {
    /// What a taste is learned from: who posted it, where, its hashtags,
    /// topic and language. Never sent anywhere.
    pub fn features(&self) -> Vec<String> {
        let mut out = vec![format!("by:{}", self.handle.to_lowercase()), format!("src:{}", self.host)];
        let mut tags: Vec<String> = self.tags.clone();
        // Hashtags written in the text count too.
        for word in self.text.split_whitespace() {
            if let Some(t) = word.strip_prefix('#') {
                let t: String = t.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if t.chars().count() >= 2 {
                    tags.push(t.to_lowercase());
                }
            }
        }
        tags.sort();
        tags.dedup();
        out.extend(tags.into_iter().take(12).map(|t| format!("tag:{t}")));
        if !self.topic.is_empty() {
            out.push(format!("topic:{}", self.topic.to_lowercase()));
        }
        if !self.lang.is_empty() {
            out.push(format!("lang:{}", self.lang.to_lowercase()));
        }
        out
    }

    /// Its words, for matching interests: hashtags, topic and the words of
    /// its text, lowercase.
    pub fn words(&self) -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> = self.tags.iter().cloned().collect();
        let text = format!("{} {}", self.topic, self.text).to_lowercase();
        for w in text.split(|c: char| !c.is_alphanumeric()) {
            if w.chars().count() >= 2 {
                out.insert(w.to_string());
            }
        }
        out
    }

    /// Taller than wide, like Reels.
    pub fn upright(&self) -> bool {
        self.media.first().is_some_and(|m| m.height > m.width && m.width > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    pub video: bool,
    /// The photo or video file; empty for PeerTube videos until asked for
    /// (see [`video_file`]).
    pub url: String,
    /// A still picture of it.
    pub preview: String,
    pub width: u32,
    pub height: u32,
    pub duration_s: u32,
    /// PeerTube or Loops: the video's id, to find its file (Loops ids start
    /// with "loops:").
    pub video_id: String,
}

/// What to look for first, from the interests picked: PeerTube
/// categories, and hashtags (for Loops' trending ones and Mastodon's).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Hints {
    pub categories: Vec<u32>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Page {
    pub posts: Vec<Post>,
    /// Where the next page starts.
    pub next: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FediError {
    /// The server only shows its feed to people signed in to it.
    #[error("this server doesn't share its feed publicly")]
    NotPublic,
    #[error("couldn't reach the server: {0}")]
    Unreachable(String),
    #[error("not a server address")]
    BadHost,
}

/// A server address as typed ("https://Pixelfed.social/" → "pixelfed.social").
pub fn clean_host(input: &str) -> Result<String, FediError> {
    let host = input.trim().trim_start_matches("https://").trim_start_matches("http://");
    let host = host.split('/').next().unwrap_or("").trim_start_matches('@').to_ascii_lowercase();
    // "user@server" means the server.
    let host = host.rsplit('@').next().unwrap_or("").to_string();
    let ok = host.contains('.')
        && host.len() <= 253
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
        && !host.starts_with('.')
        && !host.ends_with(".onion");
    if ok {
        Ok(host)
    } else {
        Err(FediError::BadHost)
    }
}

async fn json(connect: &Connect, url: &str) -> Result<Value, FediError> {
    let body = https::get(connect, url, MAX_JSON, &|_, _| {}).await.map_err(|e| {
        let e = e.to_string();
        if e.contains("HTTP 401") || e.contains("HTTP 403") || e.contains("HTTP 422") {
            FediError::NotPublic
        } else {
            FediError::Unreachable(e)
        }
    })?;
    serde_json::from_slice(&body).map_err(|e| FediError::Unreachable(e.to_string()))
}

/// One page of reels from a server. `next` comes from the previous page.
pub async fn feed(
    connect: &Connect,
    kind: Kind,
    host: &str,
    next: Option<&str>,
    hints: &Hints,
) -> Result<Page, FediError> {
    let host = clean_host(host)?;
    let mut out = Page::default();
    let mut cursor = next.map(str::to_string);
    for _ in 0..SCAN_PAGES {
        let page = match kind {
            Kind::Videos => peertube_page(connect, &host, cursor.as_deref(), &hints.categories).await?,
            Kind::Loops => return loops_page(connect, &host, cursor.as_deref(), &hints.tags).await,
            Kind::Social => social_page(connect, &host, cursor.as_deref(), &hints.tags).await?,
        };
        out.posts.extend(page.posts);
        out.next = page.next;
        cursor = out.next.clone();
        if out.posts.len() >= ENOUGH || cursor.is_none() {
            break;
        }
    }
    Ok(out)
}

/// Mastodon: with interests, their hashtags' timelines in turn
/// ("h:<n>[:<max id>]" cursors); else the server's own public timeline.
async fn social_page(connect: &Connect, host: &str, cursor: Option<&str>, tags: &[String]) -> Result<Page, FediError> {
    if tags.is_empty() {
        let max = cursor.map(|n| format!("&max_id={}", urlencode(n))).unwrap_or_default();
        let url = format!("https://{host}/api/v1/timelines/public?local=true&only_media=true&limit=40{max}");
        return Ok(parse_statuses(host, &json(connect, &url).await?));
    }
    let rest = cursor.and_then(|c| c.strip_prefix("h:")).unwrap_or("0");
    let (n, max) = rest.split_once(':').map_or((rest, None), |(n, m)| (n, Some(m)));
    let n: usize = n.parse().unwrap_or(0);
    let Some(tag) = tags.get(n) else { return Ok(Page::default()) };
    let max_q = max.map(|m| format!("&max_id={}", urlencode(m))).unwrap_or_default();
    let url = format!("https://{host}/api/v1/timelines/tag/{}?only_media=true&limit=40{max_q}", urlencode(tag));
    let page = parse_statuses(host, &json(connect, &url).await?);
    // A hashtag's first few pages, then the next hashtag.
    let deeper = max.map_or(0, |_| 1);
    let next = match &page.next {
        Some(id) if deeper < 1 => Some(format!("h:{n}:{id}")),
        _ => (n + 1 < tags.len()).then(|| format!("h:{}", n + 1)),
    };
    Ok(Page { posts: page.posts, next })
}

/// PeerTube: its search finds short videos directly ("s:" cursors); servers
/// whose search won't, are read page by page for them ("l:" cursors). With
/// interests, only their categories.
async fn peertube_page(connect: &Connect, host: &str, cursor: Option<&str>, categories: &[u32]) -> Result<Page, FediError> {
    let only: String = categories.iter().map(|c| format!("&categoryOneOf={c}")).collect();
    let (search, start) = match cursor.and_then(|c| c.split_once(':')) {
        Some(("s", n)) => (true, n.parse().unwrap_or(0)),
        Some((_, n)) => (false, n.parse().unwrap_or(0)),
        None => (true, 0),
    };
    let list = |start: u32| {
        format!("https://{host}/api/v1/videos?sort=-publishedAt&count=100&start={start}&nsfw=false&isLive=false{only}")
    };
    if search {
        let url = format!(
            "https://{host}/api/v1/search/videos?durationMax={MAX_REEL_S}&sort=-publishedAt&count=30&start={start}\
             &nsfw=false&isLive=false&searchTarget=local{only}"
        );
        match json(connect, &url).await.map(|v| parse_videos(host, &v, start)) {
            Ok(page) if start > 0 || !page.posts.is_empty() || page.next.is_some() => return Ok(with_mode(page, "s")),
            Err(e) if start > 0 => return Err(e),
            // Nothing found, or no search here: read the list instead.
            _ => {}
        }
    }
    let start = if search { 0 } else { start };
    Ok(with_mode(parse_videos(host, &json(connect, &list(start)).await?, start), "l"))
}

/// Loops, as a guest: first its picks of popular videos, then the
/// videos of each trending hashtag ("t:<n>" cursors).
async fn loops_page(connect: &Connect, host: &str, cursor: Option<&str>, interests: &[String]) -> Result<Page, FediError> {
    let Some(n) = cursor.and_then(|c| c.strip_prefix("t:")).and_then(|n| n.parse::<usize>().ok()) else {
        let v = json(connect, &format!("https://{host}/api/web/feed")).await?;
        return Ok(Page { posts: parse_loops(host, &v, None), next: Some("t:0".into()) });
    };
    let tags = json(connect, &format!("https://{host}/api/v1/explore/tags")).await?;
    let mut names: Vec<String> = list_of(&tags).iter().map(|t| str_of(t, "name")).filter(|t| !t.is_empty()).collect();
    // Hashtags matching the interests first.
    names.sort_by_key(|t| !interests.contains(&t.to_lowercase()));
    let Some(tag) = names.get(n) else { return Ok(Page::default()) };
    let v = json(connect, &format!("https://{host}/api/v1/explore/tag-feed/{}", urlencode(tag))).await?;
    Ok(Page {
        posts: parse_loops(host, &v, Some(tag)),
        next: (n + 1 < names.len()).then(|| format!("t:{}", n + 1)),
    })
}

/// A response's list: `{"data": [...]}` or the list itself.
fn list_of(v: &Value) -> Vec<Value> {
    v.get("data").unwrap_or(v).as_array().cloned().unwrap_or_default()
}

/// Loops videos. Hashtag feeds leave out the file's address: those are
/// looked up when played (`video_file`, with a "loops:" id).
pub fn parse_loops(host: &str, v: &Value, tag: Option<&str>) -> Vec<Post> {
    list_of(v)
        .iter()
        .filter(|d| !d.get("is_sensitive").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|d| {
            let id = non_empty(str_of(d, "id"), d.get("id").and_then(Value::as_u64).map(|n| n.to_string()).unwrap_or_default());
            if id.is_empty() {
                return None;
            }
            let media = d.get("media").cloned().unwrap_or_default();
            let duration_s = media.get("duration").and_then(Value::as_f64).unwrap_or(0.0) as u32;
            if duration_s > MAX_REEL_S {
                return None;
            }
            let src = str_of(&media, "src_url");
            let account = d.get("account").cloned().unwrap_or_default();
            let user = str_of(&account, "username");
            let mut tags: Vec<String> = d
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_lowercase).collect())
                .unwrap_or_default();
            tags.extend(tag.map(str::to_lowercase));
            let dim = |k: &str| media.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
            Some(Post {
                host: host.into(),
                author: non_empty(str_of(&account, "name"), user.clone()),
                handle: if user.is_empty() { host.to_string() } else { format!("{user}@{host}") },
                text: non_empty(str_of(d, "caption"), str_of(d, "captionText")),
                link: non_empty(str_of(d, "url"), format!("https://{host}/v/{}", str_of(d, "hid"))),
                created_at: parse_time(&str_of(d, "created_at")),
                media: vec![Item {
                    video: true,
                    preview: str_of(&media, "thumbnail"),
                    width: dim("width"),
                    height: dim("height"),
                    duration_s,
                    video_id: if src.starts_with("https://") { String::new() } else { format!("loops:{id}") },
                    url: if src.starts_with("https://") { src } else { String::new() },
                }],
                likes: d.get("likes").and_then(Value::as_u64).unwrap_or(0) as u32,
                tags,
                lang: str_of(d, "lang"),
                id,
                ..Default::default()
            })
        })
        .collect()
}

fn with_mode(mut page: Page, mode: &str) -> Page {
    page.next = page.next.map(|n| format!("{mode}:{n}"));
    page
}

/// A video's file: Loops' (`video_id` "loops:<id>"), or PeerTube's at a
/// size that streams well over Tor.
pub async fn video_file(connect: &Connect, host: &str, video_id: &str) -> Result<String, FediError> {
    let host = clean_host(host)?;
    if let Some(id) = video_id.strip_prefix("loops:") {
        let v = json(connect, &format!("https://{host}/api/v1/video/{}", urlencode(id))).await?;
        let v = v.get("data").unwrap_or(&v);
        let src = v.get("media").map(|m| str_of(m, "src_url")).unwrap_or_default();
        return if src.starts_with("https://") { Ok(src) } else { Err(FediError::Unreachable("no playable file".into())) };
    }
    let url = format!("https://{host}/api/v1/videos/{}", urlencode(video_id));
    pick_file(&json(connect, &url).await?).ok_or_else(|| FediError::Unreachable("no playable file".into()))
}

/// A picture (preview or photo), through Tor.
pub async fn image(connect: &Connect, url: &str) -> Result<Vec<u8>, FediError> {
    https::get(connect, url, MAX_IMAGE, &|_, _| {}).await.map_err(|e| FediError::Unreachable(e.to_string()))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

/// A server-relative path or a full https address, as a full address.
fn absolute(host: &str, path: &str) -> String {
    if path.starts_with("https://") {
        path.to_string()
    } else if path.starts_with('/') {
        format!("https://{host}{path}")
    } else {
        String::new()
    }
}

pub fn parse_videos(host: &str, v: &Value, start: u32) -> Page {
    let data = v.get("data").and_then(Value::as_array).cloned().unwrap_or_default();
    let posts: Vec<Post> = data
        .iter()
        .filter(|d| !d.get("nsfw").and_then(Value::as_bool).unwrap_or(false))
        .filter(|d| !d.get("isLive").and_then(Value::as_bool).unwrap_or(false))
        .filter(|d| (1..=MAX_REEL_S as u64).contains(&d.get("duration").and_then(Value::as_u64).unwrap_or(0)))
        .filter_map(|d| {
            let uuid = str_of(d, "uuid");
            if uuid.is_empty() {
                return None;
            }
            let account = d.get("account").cloned().unwrap_or_default();
            let preview = absolute(host, &str_of(d, "previewPath"));
            let preview = if preview.is_empty() { absolute(host, &str_of(d, "thumbnailPath")) } else { preview };
            let short = str_of(d, "shortUUID");
            Some(Post {
                id: uuid.clone(),
                host: host.into(),
                author: non_empty(str_of(&account, "displayName"), str_of(&account, "name")),
                handle: format!("{}@{}", str_of(&account, "name"), non_empty(str_of(&account, "host"), host.into())),
                text: str_of(d, "name"),
                link: format!("https://{host}/w/{}", if short.is_empty() { &uuid } else { &short }),
                created_at: parse_time(&str_of(d, "publishedAt")),
                media: vec![Item {
                    video: true,
                    preview,
                    duration_s: d.get("duration").and_then(Value::as_u64).unwrap_or(0) as u32,
                    video_id: uuid,
                    // Width over height, when the server says (newer ones do).
                    width: d.get("aspectRatio").and_then(Value::as_f64).map_or(0, |r| (r * 1000.0) as u32),
                    height: if d.get("aspectRatio").and_then(Value::as_f64).is_some() { 1000 } else { 0 },
                    ..Default::default()
                }],
                likes: d.get("likes").and_then(Value::as_u64).unwrap_or(0) as u32,
                topic: d.get("category").map(|c| str_of(c, "label")).unwrap_or_default(),
                lang: d.get("language").map(|c| str_of(c, "id")).unwrap_or_default(),
                ..Default::default()
            })
        })
        .collect();
    let total = v.get("total").and_then(Value::as_u64).unwrap_or(0);
    let after = start + data.len() as u32;
    Page {
        posts,
        next: ((after as u64) < total && !data.is_empty()).then(|| after.to_string()),
    }
}

/// The file closest to 480p (not above it, unless that's all there is),
/// from plain files or the HLS playlist's.
pub fn pick_file(v: &Value) -> Option<String> {
    let mut files: Vec<&Value> = v.get("files").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    if let Some(lists) = v.get("streamingPlaylists").and_then(Value::as_array) {
        for list in lists {
            if let Some(more) = list.get("files").and_then(Value::as_array) {
                files.extend(more.iter());
            }
        }
    }
    let height = |f: &Value| f.get("resolution").and_then(|r| r.get("id")).and_then(Value::as_u64).unwrap_or(0);
    let url = |f: &Value| non_empty(str_of(f, "fileUrl"), str_of(f, "fileDownloadUrl"));
    let usable: Vec<&Value> = files.into_iter().filter(|f| height(f) > 0 && url(f).starts_with("https://")).collect();
    let best = usable
        .iter()
        .filter(|f| height(f) <= 480)
        .max_by_key(|f| height(f))
        .or_else(|| usable.iter().min_by_key(|f| height(f)))?;
    Some(url(best))
}

pub fn parse_statuses(host: &str, v: &Value) -> Page {
    let list = v.as_array().cloned().unwrap_or_default();
    let posts = list
        .iter()
        .filter(|s| !s.get("sensitive").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|s| {
            // Boosts show the original.
            let s = s.get("reblog").filter(|r| r.is_object()).unwrap_or(s);
            let media: Vec<Item> = s
                .get("media_attachments")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(attachment).collect())
                .unwrap_or_default();
            if media.is_empty() {
                return None;
            }
            let account = s.get("account").cloned().unwrap_or_default();
            let acct = str_of(&account, "acct");
            Some(Post {
                id: str_of(s, "id"),
                host: host.into(),
                author: non_empty(str_of(&account, "display_name"), str_of(&account, "username")),
                handle: if acct.contains('@') { acct } else { format!("{acct}@{host}") },
                text: strip_html(&str_of(s, "content")),
                link: non_empty(str_of(s, "url"), str_of(s, "uri")),
                created_at: parse_time(&str_of(s, "created_at")),
                media,
                likes: s.get("favourites_count").and_then(Value::as_u64).unwrap_or(0) as u32,
                tags: s
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|t| str_of(t, "name").to_lowercase()).filter(|t| !t.is_empty()).collect())
                    .unwrap_or_default(),
                lang: str_of(s, "language"),
                ..Default::default()
            })
        })
        .collect();
    Page {
        posts,
        next: list.last().map(|s| str_of(s, "id")).filter(|id| !id.is_empty()),
    }
}

/// A short video attached to a post (photos and long videos are left out).
fn attachment(a: &Value) -> Option<Item> {
    if !matches!(str_of(a, "type").as_str(), "video" | "gifv") {
        return None;
    }
    let url = str_of(a, "url");
    if !url.starts_with("https://") {
        return None;
    }
    let original = a.get("meta").and_then(|m| m.get("original")).cloned().unwrap_or_default();
    let dim = |k: &str| original.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
    let duration_s = original.get("duration").and_then(Value::as_f64).unwrap_or(0.0) as u32;
    if duration_s > MAX_REEL_S {
        return None;
    }
    Some(Item {
        video: true,
        preview: str_of(a, "preview_url"),
        width: dim("width"),
        height: dim("height"),
        duration_s,
        url,
        video_id: String::new(),
    })
}

fn non_empty(a: String, b: String) -> String {
    if a.trim().is_empty() {
        b
    } else {
        a
    }
}

/// Post HTML as plain text: paragraphs and line breaks kept, tags and
/// common entities resolved.
pub fn strip_html(html: &str) -> String {
    let spaced = html.replace("</p><p>", "\n\n").replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n");
    let mut out = String::new();
    let mut in_tag = false;
    for c in spaced.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

/// "2024-05-01T12:34:56.000Z" as Unix seconds (0 if it isn't one).
pub fn parse_time(s: &str) -> i64 {
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|t| t.parse::<i64>().ok());
    let (Some(y), Some(m), Some(d), Some(h), Some(mi), Some(sec)) =
        (num(0..4), num(5..7), num(8..10), num(11..13), num(14..16), num(17..19))
    else {
        return 0;
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400 + h * 3600 + mi * 60 + sec
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hosts_are_cleaned() {
        assert_eq!(clean_host(" https://Pixelfed.Social/explore ").unwrap(), "pixelfed.social");
        assert_eq!(clean_host("@anna@tube.example").unwrap(), "tube.example");
        assert!(clean_host("localhost").is_err());
        assert!(clean_host("evil.example/../x y").is_ok_and(|h| h == "evil.example"));
        assert!(clean_host("abc.onion").is_err());
    }

    #[test]
    fn times_and_html() {
        assert_eq!(parse_time("1970-01-02T00:00:01.000Z"), 86_401);
        assert_eq!(parse_time("2024-03-01T00:00:00Z"), 1_709_251_200);
        assert_eq!(parse_time("soon"), 0);
        assert_eq!(strip_html("<p>Sunset &amp; sea</p><p>#goa</p>"), "Sunset & sea\n\n#goa");
    }

    #[test]
    fn peertube_pages_and_files() {
        let v = json!({"total": 30, "data": [
            {"uuid": "u1", "shortUUID": "s1", "name": "Hills", "duration": 61, "previewPath": "/lazy-static/previews/p.jpg",
             "publishedAt": "2024-03-01T00:00:00Z", "account": {"name": "ann", "displayName": "Ann", "host": "tube.example"}},
            {"uuid": "u2", "name": "Adult", "nsfw": true, "duration": 30},
            {"uuid": "u3", "name": "Live now", "isLive": true, "duration": 30},
            {"uuid": "u4", "name": "A long talk", "duration": 3600}
        ]});
        let page = parse_videos("tube.example", &v, 0);
        assert_eq!(page.posts.len(), 1, "only short videos");
        let p = &page.posts[0];
        assert_eq!((p.author.as_str(), p.handle.as_str(), p.link.as_str()), ("Ann", "ann@tube.example", "https://tube.example/w/s1"));
        assert_eq!(p.media[0].preview, "https://tube.example/lazy-static/previews/p.jpg");
        assert_eq!(page.next.as_deref(), Some("4"));
        assert_eq!(with_mode(page, "s").next.as_deref(), Some("s:4"));

        let detail = json!({"files": [], "streamingPlaylists": [{"files": [
            {"resolution": {"id": 1080}, "fileUrl": "https://cdn.example/1080.mp4"},
            {"resolution": {"id": 480}, "fileUrl": "https://cdn.example/480.mp4"},
            {"resolution": {"id": 240}, "fileUrl": "https://cdn.example/240.mp4"},
            {"resolution": {"id": 0}, "fileUrl": "https://cdn.example/audio.mp4"}
        ]}]});
        assert_eq!(pick_file(&detail).as_deref(), Some("https://cdn.example/480.mp4"));
        let only_big = json!({"files": [{"resolution": {"id": 720}, "fileUrl": "https://cdn.example/720.mp4"}]});
        assert_eq!(pick_file(&only_big).as_deref(), Some("https://cdn.example/720.mp4"));
    }

    #[test]
    fn short_videos_from_posts() {
        let v = json!([
            {"id": "9", "content": "<p>Morning</p>", "url": "https://so.example/@ann/9", "created_at": "2024-03-01T00:00:00Z",
             "account": {"acct": "ann", "display_name": "", "username": "ann"},
             "media_attachments": [
                {"type": "image", "url": "https://so.example/m/1.jpg"},
                {"type": "video", "url": "https://so.example/m/2.mp4", "preview_url": "https://so.example/m/2s.jpg",
                 "meta": {"original": {"width": 720, "height": 1280, "duration": 14.5}}}]},
            {"id": "8", "sensitive": true, "media_attachments": [{"type": "video", "url": "https://so.example/m/3.mp4"}]},
            {"id": "7", "media_attachments": [{"type": "video", "url": "https://so.example/m/4.mp4",
                                                "meta": {"original": {"duration": 900.0}}}]},
            {"id": "6", "content": "photo only", "media_attachments": [{"type": "image", "url": "https://so.example/m/5.jpg"}]}
        ]);
        let page = parse_statuses("so.example", &v);
        assert_eq!(page.posts.len(), 1);
        let p = &page.posts[0];
        assert_eq!((p.author.as_str(), p.handle.as_str(), p.text.as_str()), ("ann", "ann@so.example", "Morning"));
        assert_eq!(p.media.len(), 1);
        assert_eq!((p.media[0].video, p.media[0].height, p.media[0].duration_s), (true, 1280, 14));
        assert_eq!(page.next.as_deref(), Some("6"));
    }

    #[test]
    fn loops_feeds_and_tag_feeds() {
        let feed = json!({"data": [
            {"id": "7", "caption": "Sunset run #goa", "url": "https://loops.example/v/abc", "likes": 120,
             "tags": ["Travel"], "created_at": "2024-03-01T00:00:00+00:00",
             "account": {"name": "Ann", "username": "ann"},
             "media": {"src_url": "https://cdn.example/7.mp4", "thumbnail": "https://cdn.example/7.jpg",
                       "width": 720, "height": 1280, "duration": 14}},
            {"id": "8", "is_sensitive": true, "media": {"src_url": "https://cdn.example/8.mp4"}},
            {"id": "9", "media": {"src_url": "https://cdn.example/9.mp4", "duration": 900}}
        ]});
        let posts = parse_loops("loops.example", &feed, None);
        assert_eq!(posts.len(), 1, "sensitive and long videos are left out");
        let p = &posts[0];
        assert_eq!((p.author.as_str(), p.handle.as_str(), p.likes), ("Ann", "ann@loops.example", 120));
        assert_eq!(p.media[0].url, "https://cdn.example/7.mp4");
        assert!(p.media[0].video_id.is_empty() && p.upright());
        let f = p.features();
        for want in ["by:ann@loops.example", "src:loops.example", "tag:travel", "tag:goa"] {
            assert!(f.contains(&want.to_string()), "{want} in {f:?}");
        }
        // Hashtag feeds have no file address: it's looked up when played.
        let tagged = json!({"data": [{"id": "10", "media": {"thumbnail": "https://cdn.example/10.jpg", "width": 1080, "height": 1920}}]});
        let posts = parse_loops("loops.example", &tagged, Some("Cats"));
        assert_eq!(posts[0].media[0].video_id, "loops:10");
        assert!(posts[0].media[0].url.is_empty());
        assert!(posts[0].features().contains(&"tag:cats".to_string()));
    }
}
