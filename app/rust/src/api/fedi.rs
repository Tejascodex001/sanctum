//! The Explore tab: public Fediverse feeds (PeerTube videos, Pixelfed
//! photos), read anonymously through Tor. See `sanctum_net::fedi`.

use super::session::{self, runtime};
use super::update::CONNECT;
use sanctum_net::fedi::{self, FediError, Kind};
use sanctum_net::https::{self, Connect};
use sanctum_net::relay::Relay;
use std::sync::{LazyLock, Mutex};

/// Which servers to read, kept (sealed) on this phone only.
const SOURCES: &[u8] = b"fedi:sources";
/// Until the user picks their own.
const DEFAULTS: &[(FediKind, &str)] = &[
    (FediKind::Videos, "framatube.org"),
    (FediKind::Videos, "tilvids.com"),
    (FediKind::Photos, "pixelfed.social"),
];

static RELAY: LazyLock<Mutex<Option<Relay>>> = LazyLock::new(Default::default);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FediKind {
    /// PeerTube.
    Videos,
    /// Pixelfed (or any Mastodon-compatible server).
    Photos,
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
    /// Not connected to Tor yet.
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
        FediKind::Photos => "photos",
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
                    "photos" => FediKind::Photos,
                    _ => return None,
                };
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

fn connect() -> Option<Connect> {
    CONNECT.lock().unwrap().clone()
}

fn problem(e: FediError) -> FediProblem {
    match e {
        FediError::NotPublic => FediProblem::NotPublic,
        FediError::BadHost => FediProblem::BadHost,
        FediError::Unreachable(_) => FediProblem::Unreachable,
    }
}

/// One page of a server's feed, through Tor. `next` comes from the
/// previous page.
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
        FediKind::Photos => Kind::Photos,
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

/// A picture, through Tor (None if it couldn't be fetched).
pub fn fedi_image(url: String) -> Option<Vec<u8>> {
    let connect = connect()?;
    runtime().block_on(fedi::image(&connect, &url)).ok()
}

/// A local address the video player can stream from; the relay fetches it
/// through Tor. For PeerTube pass `video_id` (and an empty `url`).
pub fn fedi_video(host: String, url: String, video_id: String) -> Option<String> {
    let url = if url.is_empty() {
        let connect = connect()?;
        runtime().block_on(fedi::video_file(&connect, &host, &video_id)).ok()?
    } else {
        url
    };
    let mut relay = RELAY.lock().unwrap();
    if relay.is_none() {
        // Whatever Tor connection is current when the player asks (Tor may
        // have restarted since).
        let open: Connect = std::sync::Arc::new(|host: String, port: u16| {
            Box::pin(async move {
                let connect = connect().ok_or_else(|| anyhow::anyhow!("not connected"))?;
                https::tls(connect)(host, port).await
            })
        });
        let _guard = runtime().enter();
        *relay = Some(runtime().block_on(Relay::start(open)).ok()?);
    }
    relay.as_ref()?.url_for(&url).ok()
}
