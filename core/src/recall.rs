//! Finding things again, on the phone: searching every chat, and catching
//! up on what came in while you were away.
//!
//! Messages are sealed on disk, so both read them through the store and
//! look at them in memory; nothing is indexed in the clear.

use crate::protocol::MediaKind;
use crate::session::{clip, now, Result, Session};
use crate::store::{ChatKind, StoredMessage};

/// Most search results.
const MAX_HITS: usize = 100;
/// Plans shown in a catch-up: from a few hours ago to two weeks ahead.
const PLANS_FROM: u64 = 3 * 3600;
const PLANS_TO: u64 = 14 * 24 * 3600;
/// Unanswered polls this recent are shown.
const POLLS_FROM: u64 = 7 * 24 * 3600;

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub chat_id: i64,
    pub seq: i64,
    pub from_me: bool,
    pub sender: Option<[u8; 32]>,
    /// The message, or what matched in it (a poll's question, a file's name).
    pub text: String,
    pub sent_at: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CatchUp {
    /// Chats with new messages, busiest first.
    pub chats: Vec<ChatCatchUp>,
    /// Plans coming up, soonest first.
    pub plans: Vec<PlanAhead>,
    /// Others' polls we haven't answered, newest first.
    pub polls: Vec<OpenPoll>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatCatchUp {
    pub chat_id: i64,
    pub count: u32,
    /// Who wrote, in order of their first new message.
    pub senders: Vec<[u8; 32]>,
    /// New messages that say our name.
    pub mentions: u32,
    /// The newest message, in short.
    pub last: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanAhead {
    pub chat_id: i64,
    pub seq: i64,
    pub title: String,
    pub when: u64,
    pub place: String,
    /// 0 going, 1 maybe, 2 can't go; None: not answered.
    pub mine: Option<u8>,
    pub going: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenPoll {
    pub chat_id: i64,
    pub seq: i64,
    pub question: String,
    pub sent_at: u64,
}

/// Everything about a message that search looks at.
fn searchable(m: &StoredMessage) -> String {
    let mut text = m.body.clone();
    if let Some(p) = &m.poll {
        text.push('\n');
        text.push_str(&p.options.join("\n"));
        if let Some(plan) = &p.plan {
            text.push('\n');
            text.push_str(&plan.place);
        }
    }
    if let Some(media) = &m.media {
        if media.kind == MediaKind::File {
            text.push('\n');
            text.push_str(&media.name);
        }
    }
    if let Some(a) = &m.album {
        text.push('\n');
        text.push_str(&a.title);
    }
    text
}

/// What a message is, in a few words.
pub fn summary(m: &StoredMessage) -> String {
    if let Some(p) = &m.poll {
        return clip(&format!("{}: {}", if p.plan.is_some() { "Plan" } else { "Poll" }, p.question), 120);
    }
    if !m.body.trim().is_empty() {
        return clip(m.body.trim(), 120);
    }
    match m.media.as_ref().map(|x| x.kind) {
        Some(MediaKind::Image) => "Photo".into(),
        Some(MediaKind::Video) => "Video".into(),
        Some(MediaKind::Audio) => "Voice note".into(),
        Some(MediaKind::File) => m.media.as_ref().map(|x| x.name.clone()).unwrap_or_default(),
        None => String::new(),
    }
}

/// The part of `text` around the first match, so a long message shows
/// where it matched.
fn excerpt(text: &str, first_word: &str) -> String {
    let lower = text.to_lowercase();
    let Some(at) = lower.find(first_word) else { return clip(text, 140) };
    let chars_before = lower[..at].chars().count();
    let start = chars_before.saturating_sub(40);
    let piece: String = text.chars().skip(start).take(140).collect();
    if start > 0 {
        format!("…{}", piece.trim_start())
    } else {
        piece
    }
}

fn says_name(text: &str, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    if name.chars().count() < 2 {
        return false;
    }
    let text = text.to_lowercase();
    text.match_indices(&name).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

impl Session {
    /// Messages in every chat containing all the words of `query` (any
    /// case), newest first. Hidden chats only when `hidden` is true.
    pub fn search(&self, query: &str, hidden: bool) -> Result<Vec<Hit>> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        for (chat_id, meta) in self.store.chats()? {
            if meta.hidden && !hidden {
                continue;
            }
            for (seq, m) in self.store.messages_indexed(chat_id, None)? {
                let text = searchable(&m);
                let lower = text.to_lowercase();
                if words.iter().all(|w| lower.contains(w.as_str())) {
                    hits.push(Hit {
                        chat_id,
                        seq,
                        from_me: m.from_me,
                        sender: m.sender,
                        text: excerpt(&text, &words[0]),
                        sent_at: m.sent_at,
                    });
                }
            }
        }
        hits.sort_by_key(|h| std::cmp::Reverse((h.sent_at, h.seq)));
        hits.truncate(MAX_HITS);
        Ok(hits)
    }

    /// What came in since `since` (Unix seconds), plans coming up and polls
    /// waiting for our answer. Hidden chats only when `hidden` is true.
    pub fn catch_up(&self, since: u64, hidden: bool) -> Result<CatchUp> {
        let me = self.public_key();
        let at = now();
        let mut out = CatchUp::default();
        for (chat_id, meta) in self.store.chats()? {
            if (meta.hidden && !hidden) || meta.kind == ChatKind::Notes {
                continue;
            }
            let messages = self.store.messages_indexed(chat_id, None)?;
            let mut chat = ChatCatchUp {
                chat_id,
                count: 0,
                senders: Vec::new(),
                mentions: 0,
                last: String::new(),
            };
            for (seq, m) in &messages {
                if !m.from_me && m.sent_at > since {
                    chat.count += 1;
                    if let Some(s) = m.sender {
                        if !chat.senders.contains(&s) {
                            chat.senders.push(s);
                        }
                    }
                    if says_name(&m.body, &self.my_name) {
                        chat.mentions += 1;
                    }
                    chat.last = summary(m);
                }
                let Some(poll) = &m.poll else { continue };
                let mine = m.votes.iter().find(|v| v.voter == me && !v.choices.is_empty());
                match &poll.plan {
                    Some(plan) if plan.when + PLANS_FROM >= at && plan.when <= at + PLANS_TO => {
                        out.plans.push(PlanAhead {
                            chat_id,
                            seq: *seq,
                            title: poll.question.clone(),
                            when: plan.when,
                            place: plan.place.clone(),
                            mine: mine.and_then(|v| v.choices.first().copied()),
                            going: m.votes.iter().filter(|v| v.choices.first() == Some(&crate::poll::PLAN_GOING)).count()
                                as u32,
                        });
                    }
                    None if !m.from_me && mine.is_none() && m.sent_at + POLLS_FROM >= at => {
                        out.polls.push(OpenPoll {
                            chat_id,
                            seq: *seq,
                            question: poll.question.clone(),
                            sent_at: m.sent_at,
                        });
                    }
                    _ => {}
                }
            }
            if chat.count > 0 {
                out.chats.push(chat);
            }
        }
        out.chats.sort_by_key(|c| (std::cmp::Reverse(c.mentions), std::cmp::Reverse(c.count)));
        out.plans.sort_by_key(|p| p.when);
        out.polls.sort_by_key(|p| std::cmp::Reverse(p.sent_at));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_whole_words() {
        assert!(says_name("Are you coming, Anna?", "anna"));
        assert!(!says_name("Hannah's here", "anna"));
        assert!(!says_name("a b c", "a"), "too short to mean anything");
    }

    #[test]
    fn excerpts_show_the_match() {
        let long = format!("{} the beach on sunday {}", "x".repeat(200), "y".repeat(200));
        let e = excerpt(&long, "beach");
        assert!(e.starts_with('…') && e.contains("beach"));
        assert_eq!(excerpt("short beach", "beach"), "short beach");
    }
}
