//! The Reels tab: short public videos from the Fediverse (PeerTube and
//! Mastodon-compatible servers), read anonymously through Tor. See
//! `sanctum_net::fedi`.
//!
//! An opt-in fast mode reads Reels directly instead, without Tor: quicker,
//! but the sites see this phone's IP address. It changes Reels only;
//! friends and updates always go through Tor.

use super::session::{self, runtime};
use super::update::CONNECT;
use sanctum_net::fedi::{self, FediError, Kind};
use sanctum_net::https::{self, Connect};
use sanctum_net::relay::Relay;
use std::sync::{LazyLock, Mutex};

/// Which servers to read, kept (sealed) on this phone only.
const SOURCES: &[u8] = b"fedi:sources";
/// Reels' fast mode (no Tor) is on. Kept sealed; off unless chosen.
const DIRECT: &[u8] = b"fedi:direct";
/// Until the user picks their own.
const DEFAULTS: &[(FediKind, &str)] = &[
    (FediKind::Videos, "tilvids.com"),
    (FediKind::Videos, "makertube.net"),
    (FediKind::Videos, "framatube.org"),
];

static RELAY: LazyLock<Mutex<Option<Relay>>> = LazyLock::new(Default::default);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FediKind {
    /// PeerTube.
    Videos,
    /// Mastodon or another Mastodon-compatible server.
    Social,
}

#[derive(Clone)]
pub struct FediSource {
    pub kind: FediKind,
    pub host: String,
}

pub struct FediPost {
    pub id: String,
    pub host: String,
    pub author: String,
    pub handle: String,
    pub text: String,
    pub link: String,
    pub created_at: i64,
    pub media: Vec<FediItem>,
}

pub struct FediItem {
    pub video: bool,
    pub url: String,
    pub preview: String,
    pub width: u32,
    pub height: u32,
    pub duration_s: u32,
    pub video_id: String,
}

/// Why a server's feed couldn't be shown.
pub enum FediProblem {
    /// Not connected to Tor yet (never in fast mode).
    Offline,
    /// The server shows its feed only to people signed in to it.
    NotPublic,
    /// It didn't answer, or answered with something unexpected.
    Unreachable,
    /// Not a server address.
    BadHost,
}

pub struct FediPage {
    pub posts: Vec<FediPost>,
    pub next: Option<String>,
    pub problem: Option<FediProblem>,
}

fn kind_code(kind: FediKind) -> &'static str {
    match kind {
        FediKind::Videos => "videos",
        // "photos": what these were called before reels.
        FediKind::Social => "photos",
    }
}

/// The servers Explore reads.
pub fn fedi_sources() -> anyhow::Result<Vec<FediSource>> {
    let stored: Option<Vec<String>> = session::with(|s| Ok(s.store.kv_get(SOURCES)?))?;
    Ok(match stored {
        Some(list) => list
            .iter()
            .filter_map(|e| {
                let (kind, host) = e.split_once(':')?;
                let kind = match kind {
                    "videos" => FediKind::Videos,
                    "photos" => FediKind::Social,
                    _ => return None,
                };
                // Pixelfed shows nothing without signing in there.
                if host.contains("pixelfed") {
                    return None;
                }
                Some(FediSource { kind, host: host.into() })
            })
            .collect(),
        None => DEFAULTS
            .iter()
            .map(|(kind, host)| FediSource {
                kind: *kind,
                host: (*host).into(),
            })
            .collect(),
    })
}

/// Adds a server (address as typed). Returns its cleaned-up address, or
/// None if it isn't one.
pub fn fedi_add_source(kind: FediKind, host: String) -> anyhow::Result<Option<String>> {
    let Ok(host) = fedi::clean_host(&host) else { return Ok(None) };
    let mut list = fedi_sources()?;
    if !list.iter().any(|s| s.kind == kind && s.host == host) {
        list.push(FediSource {
            kind,
            host: host.clone(),
        });
        save(&list)?;
    }
    Ok(Some(host))
}

pub fn fedi_remove_source(kind: FediKind, host: String) -> anyhow::Result<()> {
    let mut list = fedi_sources()?;
    list.retain(|s| !(s.kind == kind && s.host == host));
    save(&list)
}

fn save(list: &[FediSource]) -> anyhow::Result<()> {
    let entries: Vec<String> = list.iter().map(|s| format!("{}:{}", kind_code(s.kind), s.host)).collect();
    session::with(|s| Ok(s.store.kv_put(SOURCES, &entries)?))
}

/// Whether Reels use fast mode: straight to the sites, without Tor. Off
/// unless chosen.
pub fn fedi_direct() -> anyhow::Result<bool> {
    Ok(session::with(|s| Ok(s.store.kv_get::<bool>(DIRECT)?))?.unwrap_or(false))
}

/// Turns Reels' fast mode on or off. On, the sites see this phone's IP
/// address; friends and updates still go through Tor either way.
pub fn fedi_set_direct(enabled: bool) -> anyhow::Result<()> {
    session::with(|s| Ok(s.store.kv_put(DIRECT, &enabled)?))
}

/// How Reels reach the sites: directly in fast mode, else through Tor
/// once it's up.
fn connect() -> Option<Connect> {
    connector(fedi_direct().unwrap_or(false), tor())
}

fn connector(direct: bool, tor: Option<Connect>) -> Option<Connect> {
    if direct {
        Some(https::direct())
    } else {
        tor
    }
}

fn tor() -> Option<Connect> {
    CONNECT.lock().unwrap().clone()
}

/// What the player is given for a video at `url`: the address itself in
/// fast mode, else a local one the relay serves through Tor (`relay`).
/// `https://` only, either way.
fn player_url(direct: bool, url: &str, relay: impl FnOnce(&str) -> Option<String>) -> Option<String> {
    if !url.starts_with("https://") {
        return None;
    }
    if direct {
        Some(url.to_string())
    } else {
        relay(url)
    }
}

fn problem(e: FediError) -> FediProblem {
    match e {
        FediError::NotPublic => FediProblem::NotPublic,
        FediError::BadHost => FediProblem::BadHost,
        FediError::Unreachable(_) => FediProblem::Unreachable,
    }
}

/// One page of a server's feed, through Tor (or directly in fast mode).
/// `next` comes from the previous page.
pub fn fedi_feed(kind: FediKind, host: String, next: Option<String>) -> FediPage {
    let Some(connect) = connect() else {
        return FediPage {
            posts: Vec::new(),
            next: None,
            problem: Some(FediProblem::Offline),
        };
    };
    let kind = match kind {
        FediKind::Videos => Kind::Videos,
        FediKind::Social => Kind::Social,
    };
    match runtime().block_on(fedi::feed(&connect, kind, &host, next.as_deref())) {
        Ok(page) => FediPage {
            posts: page
                .posts
                .into_iter()
                .map(|p| FediPost {
                    id: p.id,
                    host: p.host,
                    author: p.author,
                    handle: p.handle,
                    text: p.text,
                    link: p.link,
                    created_at: p.created_at,
                    media: p
                        .media
                        .into_iter()
                        .map(|m| FediItem {
                            video: m.video,
                            url: m.url,
                            preview: m.preview,
                            width: m.width,
                            height: m.height,
                            duration_s: m.duration_s,
                            video_id: m.video_id,
                        })
                        .collect(),
                })
                .collect(),
            next: page.next,
            problem: None,
        },
        Err(e) => FediPage {
            posts: Vec::new(),
            next: None,
            problem: Some(problem(e)),
        },
    }
}

/// A picture, through Tor or directly in fast mode (None if it couldn't
/// be fetched).
pub fn fedi_image(url: String) -> Option<Vec<u8>> {
    let connect = connect()?;
    runtime().block_on(fedi::image(&connect, &url)).ok()
}

/// The address the video player streams from. Normally a local one: the
/// relay fetches the video through Tor. In fast mode, the video's own
/// `https://` address, which the player opens directly. For PeerTube pass
/// `video_id` (and an empty `url`).
pub fn fedi_video(host: String, url: String, video_id: String) -> Option<String> {
    let direct = fedi_direct().unwrap_or(false);
    let url = if url.is_empty() {
        let connect = connector(direct, tor())?;
        runtime().block_on(fedi::video_file(&connect, &host, &video_id)).ok()?
    } else {
        url
    };
    player_url(direct, &url, relayed)
}

/// A local address for `url`, served by the relay through Tor.
fn relayed(url: &str) -> Option<String> {
    let mut relay = RELAY.lock().unwrap();
    if relay.is_none() {
        // Whatever Tor connection is current when the player asks (Tor may
        // have restarted since). Always Tor, whatever the mode.
        let open: Connect = std::sync::Arc::new(|host: String, port: u16| {
            Box::pin(async move {
                let connect = tor().ok_or_else(|| anyhow::anyhow!("not connected"))?;
                https::tls(connect)(host, port).await
            })
        });
        let _guard = runtime().enter();
        *relay = Some(runtime().block_on(Relay::start(open)).ok()?);
    }
    relay.as_ref()?.url_for(url).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_mode_needs_no_tor_and_normal_mode_waits_for_it() {
        assert!(connector(true, None).is_some(), "fast mode isn't offline without Tor");
        assert!(connector(false, None).is_none(), "normal mode waits for Tor");
        let tor: Connect = std::sync::Arc::new(|_, _| Box::pin(async { anyhow::bail!("unused") }));
        assert!(connector(false, Some(tor)).is_some());
    }

    #[test]
    fn the_player_gets_the_video_directly_only_in_fast_mode() {
        let relay = |url: &str| Some(format!("http://127.0.0.1:1/{}", url.len()));
        let video = "https://tube.example/v.mp4";
        assert_eq!(player_url(true, video, relay).as_deref(), Some(video));
        assert_eq!(player_url(false, video, relay).as_deref(), Some("http://127.0.0.1:1/26"));
        // Nothing but https, in either mode.
        for direct in [true, false] {
            assert_eq!(player_url(direct, "http://tube.example/v.mp4", relay), None);
            assert_eq!(player_url(direct, "file:///etc/passwd", relay), None);
        }
    }
}
