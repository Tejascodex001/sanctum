//! Polls, date polls and plans.
//!
//! A poll is a message ([`Content::Poll`]) like any other, in a friends'
//! chat or a group. Answers ([`Content::Vote`]) go to everyone in the chat,
//! and each phone keeps every person's latest answer with the poll's
//! stored message, so all phones show the same counts. An answer that
//! arrives before its poll (possible in groups, where they travel
//! separately) waits on the side until the poll comes.

use crate::protocol::{Content, MessageId, Poll};
use crate::session::{clip, now, Event, Result, Session, SessionError};
use crate::store::{ChatKind, ChatMeta, OutboxItem, OutboxState, StoredMessage, Vote};

/// Most choices a poll can have.
pub const MAX_OPTIONS: usize = 12;
/// Answers kept waiting for polls that haven't arrived, per poll.
const MAX_PENDING: usize = 200;

/// What a plan's three choices are, in order.
pub const PLAN_GOING: u8 = 0;
pub const PLAN_MAYBE: u8 = 1;
pub const PLAN_NO: u8 = 2;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn pending_name(chat_id: i64, poll: &MessageId) -> Vec<u8> {
    [b"votes:".as_slice(), &chat_id.to_le_bytes(), poll].concat()
}

/// A poll made safe to keep and show: sizes capped, a plan's choices fixed.
/// None if there's nothing to answer.
fn tidy(mut poll: Poll) -> Option<Poll> {
    poll.question = clip(poll.question.trim(), 300);
    if let Some(plan) = &mut poll.plan {
        plan.place = clip(plan.place.trim(), 200);
        poll.options = vec!["Going".into(), "Maybe".into(), "Can't go".into()];
        poll.dates.clear();
        poll.multi = false;
        return (!poll.question.is_empty()).then_some(poll);
    }
    poll.dates.truncate(MAX_OPTIONS);
    if poll.dates.is_empty() {
        poll.options = poll
            .options
            .iter()
            .map(|o| clip(o.trim(), 100))
            .filter(|o| !o.is_empty())
            .take(MAX_OPTIONS)
            .collect();
    } else {
        // Date polls: one label per time (the app may leave them empty).
        poll.options.resize(poll.dates.len(), String::new());
        poll.options = poll.options.iter().map(|o| clip(o.trim(), 100)).collect();
    }
    (!poll.question.is_empty() && poll.options.len() >= 2).then_some(poll)
}

/// Choices that exist, once each, and only one unless the poll allows more.
fn tidy_choices(poll: &Poll, mut choices: Vec<u8>) -> Vec<u8> {
    choices.retain(|c| (*c as usize) < poll.options.len());
    choices.sort_unstable();
    choices.dedup();
    if !poll.multi {
        choices.truncate(1);
    }
    choices
}

/// Records `vote` on the poll message, if it's newer than the voter's last.
fn apply(msg: &mut StoredMessage, vote: Vote) -> bool {
    let Some(poll) = &msg.poll else { return false };
    let vote = Vote {
        choices: tidy_choices(poll, vote.choices),
        ..vote
    };
    match msg.votes.iter_mut().find(|v| v.voter == vote.voter) {
        Some(old) if old.at >= vote.at => false,
        Some(old) => {
            *old = vote;
            true
        }
        None => {
            msg.votes.push(vote);
            true
        }
    }
}

impl Session {
    /// Posts a poll (or date poll, or plan) in a chat. Returns who to reach.
    pub fn send_poll(&mut self, chat_id: i64, poll: Poll) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        let poll = tidy(poll).ok_or(SessionError::Protocol)?;
        let id: MessageId = rand::random();
        let sent_at = now();
        let seq = self.store.add_message(
            chat_id,
            &StoredMessage {
                from_me: true,
                body: poll.question.clone(),
                sent_at,
                delivered: meta.kind == ChatKind::Notes,
                id: Some(id),
                poll: Some(poll.clone()),
                ..Default::default()
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        self.send_to_members(&meta, id, &Content::Poll { id, sent_at, poll }, (chat_id, seq))
    }

    /// Answers the poll at `seq` (no choices: takes the answer back).
    /// Returns who to reach.
    pub fn vote(&mut self, chat_id: i64, seq: i64, choices: Vec<u8>) -> Result<Vec<[u8; 32]>> {
        let Some(mut msg) = self.store.message(chat_id, seq)? else {
            return Err(SessionError::Protocol);
        };
        let (Some(id), Some(poll)) = (msg.id, &msg.poll) else {
            return Err(SessionError::Protocol);
        };
        let choices = tidy_choices(poll, choices);
        let at = now_ms();
        let vote = Vote {
            voter: self.public_key(),
            choices: choices.clone(),
            at,
        };
        if apply(&mut msg, vote) {
            self.store.update_message(chat_id, seq, &msg)?;
            self.events.push(Event::Chat(chat_id));
        }
        self.tell_chat(chat_id, Content::Vote { poll: id, choices, at })
    }

    /// A friend's or member's poll arrived.
    pub(crate) fn on_poll(
        &mut self,
        chat_id: i64,
        peer: &[u8; 32],
        id: MessageId,
        sent_at: u64,
        poll: Poll,
    ) -> Result<()> {
        let Some(poll) = tidy(poll) else { return Ok(()) };
        if self.store.message_by_id(chat_id, &id)?.is_some() {
            return Ok(());
        }
        let mut msg = StoredMessage {
            from_me: false,
            body: poll.question.clone(),
            sent_at: sent_at.min(now()),
            delivered: true,
            id: Some(id),
            sender: Some(*peer),
            poll: Some(poll),
            ..Default::default()
        };
        // Answers that came first.
        let name = pending_name(chat_id, &id);
        if let Some(waiting) = self.store.kv_get::<Vec<Vote>>(&name)? {
            for vote in waiting {
                apply(&mut msg, vote);
            }
            self.store.kv_delete(&name)?;
        }
        self.store.add_message(chat_id, &msg)?;
        self.events.push(Event::Chat(chat_id));
        self.events.push(Event::Message(chat_id));
        Ok(())
    }

    /// A friend's or member's answer to a poll.
    pub(crate) fn on_vote(
        &mut self,
        chat_id: i64,
        peer: &[u8; 32],
        poll: MessageId,
        choices: Vec<u8>,
        at: u64,
    ) -> Result<()> {
        let vote = Vote {
            voter: *peer,
            choices,
            at,
        };
        match self.store.message_by_id(chat_id, &poll)? {
            Some((seq, mut msg)) => {
                if apply(&mut msg, vote) {
                    self.store.update_message(chat_id, seq, &msg)?;
                    self.events.push(Event::Chat(chat_id));
                }
            }
            None => {
                let name = pending_name(chat_id, &poll);
                let mut waiting = self.store.kv_get::<Vec<Vote>>(&name)?.unwrap_or_default();
                waiting.retain(|v| v.voter != vote.voter || v.at > vote.at);
                if waiting.iter().all(|v| v.voter != vote.voter) && waiting.len() < MAX_PENDING {
                    waiting.push(vote);
                }
                self.store.kv_put(&name, &waiting)?;
            }
        }
        Ok(())
    }

    /// Queues a new message's content for the chat's other phones; it
    /// shows as delivered once they have it. Returns who to reach.
    pub(crate) fn send_to_members(
        &mut self,
        meta: &ChatMeta,
        id: MessageId,
        content: &Content,
        message: (i64, i64),
    ) -> Result<Vec<[u8; 32]>> {
        match meta.kind {
            ChatKind::Notes => Ok(Vec::new()),
            ChatKind::Group => {
                if meta.group_id.is_empty() {
                    return Err(SessionError::NotAFriend);
                }
                self.broadcast_message(meta, content, message)
            }
            ChatKind::Direct => {
                let Some(peer) = meta.peers.first().map(|p| p.identity) else {
                    return Ok(Vec::new());
                };
                self.store.outbox_put(
                    &id,
                    &OutboxItem {
                        peer,
                        state: OutboxState::Plain(content.encode()),
                        message: Some(message),
                    },
                )?;
                Ok(vec![peer])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Plan;

    #[test]
    fn polls_are_tidied() {
        let p = tidy(Poll {
            question: "  Dinner?  ".into(),
            options: vec!["Pizza".into(), " ".into(), "Sushi".into()],
            ..Default::default()
        })
        .unwrap();
        assert_eq!((p.question.as_str(), p.options.len()), ("Dinner?", 2));
        // One real choice isn't a poll.
        assert!(tidy(Poll { question: "?".into(), options: vec!["a".into()], ..Default::default() }).is_none());
        // Plans always have the same three choices.
        let plan = tidy(Poll {
            question: "Movie".into(),
            multi: true,
            plan: Some(Plan { when: 5, place: "Cinema".into() }),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(plan.options.len(), 3);
        assert!(!plan.multi);
        // Date polls get a label per date.
        let dates = tidy(Poll { question: "When?".into(), dates: vec![1, 2, 3], ..Default::default() }).unwrap();
        assert_eq!(dates.options.len(), 3);
    }

    #[test]
    fn newer_answers_win() {
        let mut msg = StoredMessage {
            poll: Some(Poll { question: "?".into(), options: vec!["a".into(), "b".into()], ..Default::default() }),
            ..Default::default()
        };
        assert!(apply(&mut msg, Vote { voter: [1; 32], choices: vec![1, 0, 9], at: 5 }));
        // Single choice: only the first valid one counts.
        assert_eq!(msg.votes[0].choices, vec![0]);
        assert!(!apply(&mut msg, Vote { voter: [1; 32], choices: vec![1], at: 4 }));
        assert!(apply(&mut msg, Vote { voter: [1; 32], choices: vec![1], at: 6 }));
        assert_eq!(msg.votes.len(), 1);
        assert_eq!(msg.votes[0].choices, vec![1]);
    }
}
