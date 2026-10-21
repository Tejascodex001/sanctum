//! "Asha is typing…": a signal that lives only on links that are up.
//!
//! While someone writes, their phone sends [`Frame::Typing`] every few
//! seconds to whoever in the chat is connected right now. It is never
//! stored, queued or resent: a friend who isn't connected simply doesn't
//! see it. It carries no text, only which chat (the pair's own, or a
//! group by its id).
//!
//! It travels as a frame of its own, not inside MLS. Links are already
//! private and authenticated (Tor reaches the friend's own onion address,
//! and the signed hello proves who dialed), and an MLS message that was
//! encrypted but never sent would leave a gap in the sender's message
//! counter, which could make their later real messages unreadable.

use crate::protocol::Frame;
use crate::session::{Event, Result, Session};
use crate::store::ChatKind;
use std::time::{Duration, Instant};

/// Signals for one chat go out at most this often.
pub const TYPING_EVERY: Duration = Duration::from_secs(3);
/// Signals from one person in one chat are passed on at most this often.
const HEARD_EVERY: Duration = Duration::from_secs(1);

impl Session {
    /// We're writing in this chat: the signal to send, and who in the chat
    /// should get it if connected. None when there's no one to tell, or
    /// when the last signal for this chat went out less than
    /// [`TYPING_EVERY`] ago.
    pub fn typing(&mut self, chat_id: i64) -> Result<Option<(Frame, Vec<[u8; 32]>)>> {
        let meta = self.chat(chat_id)?;
        if meta.group_id.is_empty() {
            // Notes, a friend who hasn't added us back yet, or a group we
            // left.
            return Ok(None);
        }
        let now = Instant::now();
        if self.typing_sent.get(&chat_id).is_some_and(|at| now.duration_since(*at) < TYPING_EVERY) {
            return Ok(None);
        }
        let (group, to) = match meta.kind {
            ChatKind::Direct => (Vec::new(), meta.peers.iter().map(|p| p.identity).collect()),
            ChatKind::Group => (meta.group_id.clone(), self.others(&meta)?),
            ChatKind::Notes => return Ok(None),
        };
        if to.is_empty() {
            return Ok(None);
        }
        self.typing_sent.insert(chat_id, now);
        Ok(Some((Frame::Typing { group }, to)))
    }

    /// `peer` is typing: in our chat with them (`group` empty), or in a
    /// group we share. Ignored unless they really are in that chat.
    pub(crate) fn on_typing(&mut self, peer: &[u8; 32], group: &[u8]) -> Result<()> {
        let found = if group.is_empty() {
            self.direct_chat(peer)?
                .filter(|(_, meta)| !meta.group_id.is_empty())
                .map(|(chat_id, meta)| (chat_id, meta.title))
        } else {
            match self.group_chat(group)? {
                Some((chat_id, meta)) if self.mls.members(group)?.contains(peer) => meta
                    .peers
                    .iter()
                    .find(|p| &p.identity == peer)
                    .map(|p| (chat_id, p.name.clone())),
                _ => None,
            }
        };
        let Some((chat, name)) = found else {
            return Ok(());
        };
        let now = Instant::now();
        if self.typing_heard.get(&(chat, *peer)).is_some_and(|at| now.duration_since(*at) < HEARD_EVERY) {
            return Ok(());
        }
        self.typing_heard.insert((chat, *peer), now);
        // Forget old entries now and then, so the map stays small.
        if self.typing_heard.len() > 256 {
            self.typing_heard.retain(|_, at| now.duration_since(*at) < HEARD_EVERY);
        }
        self.events.push(Event::Typing { chat, who: *peer, name });
        Ok(())
    }
}
