//! Search across chats, and catching up after time away. See
//! `sanctum_core::recall`.

use super::session;
use sanctum_core::session::Session;
use std::time::{SystemTime, UNIX_EPOCH};

/// When the app was last left (see `catch_up_mark`).
const SINCE: &[u8] = b"catchup:since";

pub struct SearchHit {
    pub chat_id: i64,
    pub seq: i64,
    pub from_me: bool,
    /// "You", or who wrote it.
    pub sender_name: String,
    pub chat_title: String,
    pub text: String,
    pub sent_at: i64,
}

pub struct CatchUpView {
    pub chats: Vec<ChatCatchUpView>,
    pub plans: Vec<PlanAheadView>,
    pub polls: Vec<OpenPollView>,
}

pub struct ChatCatchUpView {
    pub chat_id: i64,
    pub title: String,
    pub count: u32,
    pub senders: Vec<String>,
    pub mentions: u32,
    pub last: String,
}

pub struct PlanAheadView {
    pub chat_id: i64,
    pub seq: i64,
    pub chat_title: String,
    pub title: String,
    pub when: i64,
    pub place: String,
    /// 0 going, 1 maybe, 2 can't go; None: not answered yet.
    pub mine: Option<u8>,
    pub going: u32,
}

pub struct OpenPollView {
    pub chat_id: i64,
    pub seq: i64,
    pub chat_title: String,
    pub question: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Chat titles and member names, for showing results.
struct Names(Vec<(i64, sanctum_core::store::ChatMeta)>);

impl Names {
    fn load(s: &Session) -> anyhow::Result<Self> {
        Ok(Self(s.store.chats()?))
    }

    fn title(&self, chat: i64) -> String {
        self.0.iter().find(|(id, _)| *id == chat).map(|(_, m)| m.title.clone()).unwrap_or_default()
    }

    fn person(&self, chat: i64, key: &[u8; 32]) -> String {
        self.0
            .iter()
            .find(|(id, _)| *id == chat)
            .and_then(|(_, m)| m.peers.iter().find(|p| &p.identity == key))
            .map_or_else(|| "Someone".into(), |p| p.name.clone())
    }
}

/// Messages in every chat with all the words of `query`, newest first.
/// Hidden chats only while they're revealed (`hidden`).
pub fn search_messages(query: String, hidden: bool) -> anyhow::Result<Vec<SearchHit>> {
    session::with(|s| {
        let names = Names::load(s)?;
        Ok(s.search(&query, hidden)?
            .into_iter()
            .map(|h| SearchHit {
                chat_id: h.chat_id,
                seq: h.seq,
                from_me: h.from_me,
                sender_name: match (h.from_me, h.sender) {
                    (true, _) => "You".into(),
                    (false, Some(k)) => names.person(h.chat_id, &k),
                    (false, None) => names.title(h.chat_id),
                },
                chat_title: names.title(h.chat_id),
                text: h.text,
                sent_at: h.sent_at as i64,
            })
            .collect())
    })
}

/// What came in since the app was last left, plans coming up and polls
/// waiting for an answer.
pub fn catch_up(hidden: bool) -> anyhow::Result<CatchUpView> {
    session::with(|s| {
        let since: u64 = s.store.kv_get(SINCE)?.unwrap_or_else(|| now().saturating_sub(24 * 3600));
        let names = Names::load(s)?;
        let up = s.catch_up(since, hidden)?;
        Ok(CatchUpView {
            chats: up
                .chats
                .into_iter()
                .map(|c| ChatCatchUpView {
                    title: names.title(c.chat_id),
                    senders: c.senders.iter().map(|k| names.person(c.chat_id, k)).collect(),
                    chat_id: c.chat_id,
                    count: c.count,
                    mentions: c.mentions,
                    last: c.last,
                })
                .collect(),
            plans: up
                .plans
                .into_iter()
                .map(|p| PlanAheadView {
                    chat_title: names.title(p.chat_id),
                    chat_id: p.chat_id,
                    seq: p.seq,
                    title: p.title,
                    when: p.when as i64,
                    place: p.place,
                    mine: p.mine,
                    going: p.going,
                })
                .collect(),
            polls: up
                .polls
                .into_iter()
                .map(|p| OpenPollView {
                    chat_title: names.title(p.chat_id),
                    chat_id: p.chat_id,
                    seq: p.seq,
                    question: p.question,
                })
                .collect(),
        })
    })
}

/// The app is being left: the next catch-up counts from now.
pub fn catch_up_mark() -> anyhow::Result<()> {
    session::with(|s| Ok(s.store.kv_put(SINCE, &now())?))
}
