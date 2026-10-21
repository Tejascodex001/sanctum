//! The Reels tab: short public videos from the Fediverse (Loops, PeerTube
//! and Mastodon-compatible servers), read anonymously through Tor, ranked
//! on this phone by what you like (`sanctum_core::taste`). See
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
use sanctum_core::taste::{Candidate, Signal, Taste};
use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Which servers to read, kept (sealed) on this phone only.
const SOURCES: &[u8] = b"fedi:sources";
/// Reels' fast mode (no Tor) is on. Kept sealed; off unless chosen.
const DIRECT: &[u8] = b"fedi:direct";
/// What Reels has learned you like (sealed, never sent).
const TASTE: &[u8] = b"fedi:taste";
/// Loops was added to a list saved before it existed.
const LOOPS_ADDED: &[u8] = b"fedi:loops-added";
const LOOPS: &str = "loops.video";
/// Until the user picks their own.
const DEFAULTS: &[(FediKind, &str)] = &[
    (FediKind::Loops, LOOPS),
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
    /// Loops: short upright videos.
    Loops,
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
    /// Likes on the site.
    pub likes: u32,
    /// We gave it a heart.
    pub liked: bool,
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
#[derive(Clone, Copy)]
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
        FediKind::Loops => "loops",
    }
}

/// The servers Explore reads.
pub fn fedi_sources() -> anyhow::Result<Vec<FediSource>> {
    let mut stored: Option<Vec<String>> = session::with(|s| Ok(s.store.kv_get(SOURCES)?))?;
    // Lists saved before Loops get it once (it can be removed again).
    if let Some(list) = stored.as_mut() {
        if !session::with(|s| Ok(s.store.kv_get::<bool>(LOOPS_ADDED)?))?.unwrap_or(false) {
            if !list.iter().any(|e| e.starts_with("loops:")) {
                list.insert(0, format!("loops:{LOOPS}"));
                session::with(|s| Ok(s.store.kv_put(SOURCES, &*list)?))?;
            }
            session::with(|s| Ok(s.store.kv_put(LOOPS_ADDED, &true)?))?;
        }
    }
    Ok(match stored {
        Some(list) => list
            .iter()
            .filter_map(|e| {
                let (kind, host) = e.split_once(':')?;
                let kind = match kind {
                    "videos" => FediKind::Videos,
                    "photos" => FediKind::Social,
                    "loops" => FediKind::Loops,
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
    match runtime().block_on(fedi::feed(&connect, net_kind(kind), &host, next.as_deref())) {
        Ok(page) => FediPage {
            posts: page.posts.into_iter().map(|p| post_view(p, false)).collect(),
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

fn net_kind(kind: FediKind) -> Kind {
    match kind {
        FediKind::Videos => Kind::Videos,
        FediKind::Social => Kind::Social,
        FediKind::Loops => Kind::Loops,
    }
}

fn post_view(p: fedi::Post, liked: bool) -> FediPost {
    FediPost {
        id: p.id,
        host: p.host,
        author: p.author,
        handle: p.handle,
        text: p.text,
        link: p.link,
        created_at: p.created_at,
        likes: p.likes,
        liked,
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
    }
}

// ---- The ranked feed ------------------------------------------------------

/// Reels fetched but not shown yet, where each site's feed is up to, and
/// what's known about the reels already shown (for learning from them).
#[flutter_rust_bridge::frb(ignore)]
#[derive(Default)]
struct Reels {
    pool: Vec<fedi::Post>,
    /// Per source: the next page (None: no more).
    cursors: HashMap<String, Option<String>>,
    problems: HashMap<String, FediSourceProblem>,
    shown: HashMap<String, Vec<String>>,
    shown_order: VecDeque<String>,
}

static REELS: LazyLock<Mutex<Reels>> = LazyLock::new(Default::default);
/// Reels shown that are still remembered, for learning from them.
const MAX_SHOWN: usize = 500;

pub struct FediSourceProblem {
    pub kind: FediKind,
    pub host: String,
    pub problem: FediProblem,
}

impl Clone for FediSourceProblem {
    fn clone(&self) -> Self {
        Self { kind: self.kind, host: self.host.clone(), problem: self.problem }
    }
}

pub struct ReelsPage {
    pub posts: Vec<FediPost>,
    /// Sites that couldn't be read, and why.
    pub problems: Vec<FediSourceProblem>,
}

/// What happened with a reel, for learning what you like.
pub enum ReelSignal {
    Like,
    Unlike,
    Share,
    /// Watched `times` its length (1.0: once to the end).
    Watched,
    /// Swiped away within a couple of seconds.
    Skipped,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn key_of(host: &str, id: &str) -> String {
    format!("{host}/{id}")
}

fn source_key(s: &FediSource) -> String {
    format!("{}:{}", kind_code(s.kind), s.host)
}

fn load_taste() -> Taste {
    session::with(|s| Ok(s.store.kv_get::<Taste>(TASTE)?)).ok().flatten().unwrap_or_default()
}

fn save_taste(t: &Taste) {
    let _ = session::with(|s| Ok(s.store.kv_put(TASTE, t)?));
}

/// The next `count` reels, best fits for your taste first, with some new
/// things mixed in. `fresh` starts the sites' feeds again (pull to
/// refresh). Reels shown in the last two days aren't shown again.
pub fn reels_next(count: u32, fresh: bool) -> anyhow::Result<ReelsPage> {
    let count = count.clamp(1, 20) as usize;
    let sources = fedi_sources()?;
    let mut taste = load_taste();
    let at = now();
    if fresh {
        let mut r = REELS.lock().unwrap();
        r.pool.clear();
        r.cursors.clear();
        r.problems.clear();
    }
    let Some(connect) = connect() else {
        return Ok(ReelsPage {
            posts: Vec::new(),
            problems: sources
                .iter()
                .map(|s| FediSourceProblem { kind: s.kind, host: s.host.clone(), problem: FediProblem::Offline })
                .collect(),
        });
    };
    for _ in 0..3 {
        // What to fetch, decided under the lock; fetched without it.
        let due: Vec<(FediSource, Option<String>)> = {
            let r = REELS.lock().unwrap();
            if r.pool.len() >= count * 3 {
                break;
            }
            sources
                .iter()
                .filter_map(|s| match r.cursors.get(&source_key(s)) {
                    Some(None) => None,
                    Some(Some(c)) => Some((s.clone(), Some(c.clone()))),
                    None => Some((s.clone(), None)),
                })
                .collect()
        };
        if due.is_empty() {
            break;
        }
        let pages = runtime().block_on(async {
            let mut tasks = Vec::new();
            for (s, cursor) in &due {
                let (connect, host, cursor, kind) = (connect.clone(), s.host.clone(), cursor.clone(), net_kind(s.kind));
                tasks.push(runtime().spawn(async move { fedi::feed(&connect, kind, &host, cursor.as_deref()).await }));
            }
            let mut out = Vec::new();
            for t in tasks {
                out.push(t.await.unwrap_or_else(|e| Err(FediError::Unreachable(e.to_string()))));
            }
            out
        });
        let mut r = REELS.lock().unwrap();
        for ((s, _), page) in due.iter().zip(pages) {
            let key = source_key(s);
            match page {
                Ok(page) => {
                    r.problems.remove(&key);
                    r.cursors.insert(key, page.next);
                    for p in page.posts {
                        let k = key_of(&p.host, &p.id);
                        let known = r.shown.contains_key(&k) || r.pool.iter().any(|q| q.host == p.host && q.id == p.id);
                        if !known && !taste.seen_recently(&k, at) && !p.media.is_empty() {
                            r.pool.push(p);
                        }
                    }
                }
                Err(e) => {
                    r.cursors.insert(key.clone(), None);
                    r.problems.insert(key, FediSourceProblem { kind: s.kind, host: s.host.clone(), problem: problem(e) });
                }
            }
        }
    }
    let mut r = REELS.lock().unwrap();
    let candidates: Vec<Candidate> = r
        .pool
        .iter()
        .map(|p| Candidate {
            key: key_of(&p.host, &p.id),
            author: p.handle.clone(),
            features: p.features(),
            likes: p.likes,
            upright: p.upright(),
            age_s: at.saturating_sub(p.created_at.max(0) as u64),
        })
        .collect();
    let mut picked = taste.rank(&candidates, count, &mut rand::thread_rng());
    let order = picked.clone();
    picked.sort_unstable_by(|a, b| b.cmp(a));
    let mut taken: HashMap<usize, fedi::Post> = HashMap::new();
    for i in picked {
        taken.insert(i, r.pool.remove(i));
    }
    let mut posts = Vec::new();
    for i in order {
        let p = taken.remove(&i).expect("picked once");
        let c = &candidates[i];
        taste.mark_seen(&c.key, at);
        r.shown.insert(c.key.clone(), c.features.clone());
        r.shown_order.push_back(c.key.clone());
        while r.shown_order.len() > MAX_SHOWN {
            if let Some(old) = r.shown_order.pop_front() {
                r.shown.remove(&old);
            }
        }
        let liked = taste.liked(&c.key);
        posts.push(post_view(p, liked));
    }
    save_taste(&taste);
    Ok(ReelsPage { posts, problems: r.problems.values().cloned().collect() })
}

/// Learns from what you did with a shown reel. For `Watched`, `times` is
/// how many times its length you watched.
pub fn reels_signal(host: String, id: String, signal: ReelSignal, times: f32) -> anyhow::Result<()> {
    let key = key_of(&host, &id);
    let Some(features) = REELS.lock().unwrap().shown.get(&key).cloned() else { return Ok(()) };
    let mut taste = load_taste();
    let signal = match signal {
        ReelSignal::Like => {
            taste.set_liked(&key, true);
            Signal::Like
        }
        ReelSignal::Unlike => {
            taste.set_liked(&key, false);
            Signal::Unlike
        }
        ReelSignal::Share => Signal::Share,
        ReelSignal::Watched => Signal::Watched(times),
        ReelSignal::Skipped => Signal::Skipped,
    };
    taste.learn(&features, signal);
    save_taste(&taste);
    Ok(())
}

/// Forgets what Reels learned you like (Settings).
pub fn reels_forget_taste() -> anyhow::Result<()> {
    session::with(|s| Ok(s.store.kv_put(TASTE, &Taste::default())?))
}

/// A picture, through Tor or directly in fast mode (None if it couldn't
/// be fetched).
pub fn fedi_image(url: String) -> Option<Vec<u8>> {
    let connect = connect()?;
    runtime().block_on(fedi::image(&connect, &url)).ok()
}

/// The address the video player streams from. Normally a local one: the
/// relay fetches the video through Tor. In fast mode, the video's own
/// `https://` address, which the player opens directly. For PeerTube and
/// Loops videos without an address, pass `video_id` (and an empty `url`).
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
