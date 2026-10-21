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
use sanctum_core::taste::{interest_matches, Candidate, Interest, Signal, Taste, INTERESTS};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Which servers to read, kept (sealed) on this phone only.
const SOURCES: &[u8] = b"fedi:sources";
/// Reels' fast mode (no Tor) is on. Kept sealed; off unless chosen.
const DIRECT: &[u8] = b"fedi:direct";
/// What Reels has learned you like (sealed, never sent).
const TASTE: &[u8] = b"fedi:taste";
/// The interests picked before Reels started (absent: not asked yet).
const INTERESTS_KEY: &[u8] = b"fedi:interests";
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
    match runtime().block_on(fedi::feed(&connect, net_kind(kind), &host, next.as_deref(), &fedi::Hints::default())) {
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
/// Sites are read in the background, so a slow one never holds up the rest.
#[flutter_rust_bridge::frb(ignore)]
#[derive(Default)]
struct Reels {
    pool: Vec<fedi::Post>,
    /// Per source: the next page (None: no more).
    cursors: HashMap<String, Option<String>>,
    /// Sources being read right now.
    fetching: HashSet<String>,
    problems: HashMap<String, FediSourceProblem>,
    shown: HashMap<String, Vec<String>>,
    shown_order: VecDeque<String>,
    /// Video files already looked up, by "host/video id".
    files: HashMap<String, String>,
    /// Bumped by a fresh start, so older reads are dropped.
    round: u64,
}

static REELS: LazyLock<Mutex<Reels>> = LazyLock::new(Default::default);
/// Reels shown that are still remembered, for learning from them.
const MAX_SHOWN: usize = 500;
/// Enough to start showing.
const FIRST: usize = 3;
/// How long to wait for the first reels before showing what there is.
const WAIT_DIRECT: Duration = Duration::from_secs(8);
const WAIT_TOR: Duration = Duration::from_secs(25);

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

/// Something to watch reels about, for picking them before Reels starts.
pub struct ReelInterest {
    pub id: String,
    pub label: String,
    pub emoji: String,
    pub chosen: bool,
}

pub struct ReelInterests {
    /// Interests were asked about (picked or skipped).
    pub asked: bool,
    pub items: Vec<ReelInterest>,
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

fn chosen_interests() -> Vec<&'static Interest> {
    let ids: Vec<String> = session::with(|s| Ok(s.store.kv_get(INTERESTS_KEY)?)).ok().flatten().unwrap_or_default();
    INTERESTS.iter().filter(|i| ids.iter().any(|id| id == i.id)).collect()
}

/// The interests to pick from, and which are picked.
pub fn reels_interests() -> anyhow::Result<ReelInterests> {
    let ids: Option<Vec<String>> = session::with(|s| Ok(s.store.kv_get(INTERESTS_KEY)?))?;
    Ok(ReelInterests {
        asked: ids.is_some(),
        items: INTERESTS
            .iter()
            .map(|i| ReelInterest {
                id: i.id.into(),
                label: i.label.into(),
                emoji: i.emoji.into(),
                chosen: ids.as_ref().is_some_and(|ids| ids.iter().any(|id| id == i.id)),
            })
            .collect(),
    })
}

/// Picks what Reels is about (empty: skipped, no preference). The sites
/// are read again with them.
pub fn reels_set_interests(ids: Vec<String>) -> anyhow::Result<()> {
    let ids: Vec<String> = ids.into_iter().filter(|id| INTERESTS.iter().any(|i| i.id == id)).collect();
    session::with(|s| Ok(s.store.kv_put(INTERESTS_KEY, &ids)?))?;
    // Read the sites again with the new interests; what's loaded stays (it's
    // ranked by them anyway).
    let mut r = REELS.lock().unwrap();
    r.round += 1;
    r.cursors.clear();
    r.fetching.clear();
    Ok(())
}

fn hints(chosen: &[&Interest]) -> fedi::Hints {
    fedi::Hints {
        categories: chosen.iter().filter_map(|i| i.peertube).collect(),
        // Each interest's main words: searched on PeerTube, hashtags on
        // Loops and Mastodon.
        tags: chosen.iter().flat_map(|i| i.words.iter().take(4)).map(|w| w.to_string()).collect(),
    }
}

/// Starts reading the next page of every source that has more and isn't
/// being read, unless the pool already has plenty. Results land in the
/// pool as each site answers.
fn refill(sources: &[FediSource], connect: &Connect, target: usize, hints: &fedi::Hints, chosen: &[&Interest]) {
    let mut r = REELS.lock().unwrap();
    if usable(&r.pool, chosen) >= target {
        return;
    }
    let round = r.round;
    for s in sources {
        let key = source_key(s);
        let cursor = match r.cursors.get(&key) {
            Some(None) => continue,
            Some(Some(c)) => Some(c.clone()),
            None => None,
        };
        if !r.fetching.insert(key.clone()) {
            continue;
        }
        let (connect, s, hints) = (connect.clone(), s.clone(), hints.clone());
        runtime().spawn(async move {
            let page = fedi::feed(&connect, net_kind(s.kind), &s.host, cursor.as_deref(), &hints).await;
            let taste = load_taste();
            let at = now();
            let mut r = REELS.lock().unwrap();
            // A fresh start since: these are stale, and new reads already began.
            if r.round != round {
                return;
            }
            r.fetching.remove(&key);
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
        });
    }
}

/// Whether a reel can be shown: with interests picked, only reels about
/// them.
fn fits(p: &fedi::Post, chosen: &[&Interest]) -> bool {
    chosen.is_empty() || interest_matches(chosen, &p.words(), &p.tags) > 0
}

fn usable(pool: &[fedi::Post], chosen: &[&Interest]) -> usize {
    pool.iter().filter(|p| fits(p, chosen)).count()
}

/// The next `count` reels, best fits for your taste and interests first,
/// with some new things mixed in. `fresh` starts the sites' feeds again
/// (pull to refresh). Returns as soon as a few are ready; the rest keep
/// loading in the background. Reels shown in the last two days aren't
/// shown again.
pub fn reels_next(count: u32, fresh: bool) -> anyhow::Result<ReelsPage> {
    let count = count.clamp(1, 20) as usize;
    let sources = fedi_sources()?;
    let chosen = chosen_interests();
    let hints = hints(&chosen);
    if fresh {
        // From the top of every site again, keeping what's already loaded
        // (unless its site was removed).
        let mut r = REELS.lock().unwrap();
        r.round += 1;
        r.pool.retain(|p| sources.iter().any(|s| s.host == p.host));
        r.cursors.clear();
        r.fetching.clear();
        r.problems.clear();
    }
    let direct = fedi_direct().unwrap_or(false);
    let Some(connect) = connector(direct, tor()) else {
        return Ok(ReelsPage {
            posts: Vec::new(),
            problems: sources
                .iter()
                .map(|s| FediSourceProblem { kind: s.kind, host: s.host.clone(), problem: FediProblem::Offline })
                .collect(),
        });
    };
    let target = count * 3;
    let deadline = Instant::now() + if direct { WAIT_DIRECT } else { WAIT_TOR };
    loop {
        refill(&sources, &connect, target, &hints, &chosen);
        let (have, busy) = {
            let r = REELS.lock().unwrap();
            (usable(&r.pool, &chosen), !r.fetching.is_empty())
        };
        // Enough to start, or nothing more coming, or waited long enough.
        if have >= count.min(FIRST) || !busy || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(120));
    }
    // Keep the pool topped up while these are watched.
    refill(&sources, &connect, target, &hints, &chosen);
    let mut taste = load_taste();
    let at = now();
    let mut r = REELS.lock().unwrap();
    // With interests, only reels about them: the others stay in the pool
    // (interests may change) but aren't shown.
    let showable: Vec<usize> = (0..r.pool.len()).filter(|&i| fits(&r.pool[i], &chosen)).collect();
    let candidates: Vec<Candidate> = showable
        .iter()
        .map(|&i| &r.pool[i])
        .map(|p| Candidate {
            key: key_of(&p.host, &p.id),
            author: p.handle.clone(),
            features: p.features(),
            likes: p.likes,
            upright: p.upright(),
            age_s: at.saturating_sub(p.created_at.max(0) as u64),
            interests: interest_matches(&chosen, &p.words(), &p.tags),
        })
        .collect();
    let order = taste.rank(&candidates, count, !chosen.is_empty(), &mut rand::thread_rng());
    // Candidates are numbered among the showable ones; take them out of the
    // pool by their place in it, last first.
    let mut in_pool: Vec<usize> = order.iter().map(|&c| showable[c]).collect();
    in_pool.sort_unstable_by(|a, b| b.cmp(a));
    let mut taken: HashMap<usize, fedi::Post> = HashMap::new();
    for i in in_pool {
        taken.insert(i, r.pool.remove(i));
    }
    let mut posts = Vec::new();
    for i in order {
        let mut p = taken.remove(&showable[i]).expect("picked once");
        let c = &candidates[i];
        taste.mark_seen(&c.key, at);
        r.shown.insert(c.key.clone(), c.features.clone());
        r.shown_order.push_back(c.key.clone());
        while r.shown_order.len() > MAX_SHOWN {
            if let Some(old) = r.shown_order.pop_front() {
                r.shown.remove(&old);
            }
        }
        // Videos whose file is known already play straight away.
        if let Some(m) = p.media.first_mut() {
            if m.url.is_empty() {
                if let Some(url) = r.files.get(&key_of(&p.host, &m.video_id)) {
                    m.url = url.clone();
                    m.video_id.clear();
                }
            }
        }
        let liked = taste.liked(&c.key);
        posts.push(post_view(p, liked));
    }
    let problems = r.problems.values().cloned().collect();
    drop(r);
    save_taste(&taste);
    // Look up the files of the first ones now, so they start sooner.
    for p in posts.iter().take(3) {
        if let Some(m) = p.media.first().filter(|m| m.url.is_empty() && !m.video_id.is_empty()) {
            let (connect, host, id) = (connect.clone(), p.host.clone(), m.video_id.clone());
            runtime().spawn(async move {
                if let Ok(url) = fedi::video_file(&connect, &host, &id).await {
                    REELS.lock().unwrap().files.insert(key_of(&host, &id), url);
                }
            });
        }
    }
    Ok(ReelsPage { posts, problems })
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

/// Forgets what Reels learned you like, and the interests picked
/// (Settings).
pub fn reels_forget_taste() -> anyhow::Result<()> {
    session::with(|s| {
        s.store.kv_put(TASTE, &Taste::default())?;
        s.store.kv_delete(INTERESTS_KEY)?;
        Ok(())
    })
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
    let known = REELS.lock().unwrap().files.get(&key_of(&host, &video_id)).cloned();
    let url = if let Some(known) = known.filter(|_| url.is_empty()) {
        known
    } else if url.is_empty() {
        let connect = connector(direct, tor())?;
        let found = runtime().block_on(fedi::video_file(&connect, &host, &video_id)).ok()?;
        REELS.lock().unwrap().files.insert(key_of(&host, &video_id), found.clone());
        found
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

    #[test]
    fn with_interests_only_reels_about_them_are_shown() {
        let reel = |tags: &[&str], text: &str| fedi::Post {
            tags: tags.iter().map(|t| t.to_string()).collect(),
            text: text.into(),
            ..Default::default()
        };
        let dance: Vec<&Interest> = INTERESTS.iter().filter(|i| i.id == "dance").collect();
        let pool = vec![reel(&["kpopdance"], ""), reel(&[], "Dancing in the rain"), reel(&["cars"], "fast")];
        assert!(fits(&pool[0], &dance) && fits(&pool[1], &dance) && !fits(&pool[2], &dance));
        assert_eq!(usable(&pool, &dance), 2);
        assert_eq!(usable(&pool, &[]), 3, "no interests: everything");
        let h = hints(&dance);
        assert_eq!(h.tags, ["dance", "dancing", "dancer", "kpop"]);
        assert!(h.categories.is_empty());
    }
}
