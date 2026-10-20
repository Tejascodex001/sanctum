//! A signed-in device: identity, encrypted store, MLS and temp chats, plus
//! the protocol engine that talks to friends.
//!
//! The engine does no I/O. The network layer feeds it frames from a
//! connection ([`Session::on_frame`]) and writes back what it returns, so
//! whole conversations between phones are tested here by passing frames
//! between two sessions.

use crate::identity::{pair_fingerprint, safety_number, verify, Identity};
use crate::invite::{FriendCode, InviteError};
use crate::mls::{MlsClient, MlsError, Received};
use crate::protocol::{hello_message, Content, Frame, MessageId, TempId};
use crate::store::{
    ChatKind, ChatMeta, OutboxItem, OutboxState, Peer, Store, StoreError, StoredMessage,
};
use crate::temp_chat::{TempChats, TempMessage};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Mls(#[from] MlsError),
    #[error(transparent)]
    Invite(#[from] InviteError),
    #[error("you are already friends")]
    AlreadyFriends,
    #[error("that's your own code")]
    OwnCode,
    #[error("no such chat")]
    NoSuchChat,
    #[error("not a friend")]
    NotAFriend,
    #[error("the other side broke the protocol")]
    Protocol,
}

pub type Result<T> = std::result::Result<T, SessionError>;

/// A friend just added from their code.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub chat_id: i64,
    pub friend: String,
    /// Both phones derive the same sigil from this.
    pub sigil_key: Vec<u8>,
    /// Both phones show this number, to compare on a call.
    pub safety_number: String,
}

/// Something the UI should refresh for.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// New message, or a message was delivered.
    Chat(i64),
    /// A temp chat appeared or changed.
    Temp(u64),
    /// A temp chat was wiped (by the friend) or burned away.
    TempGone(u64),
}

/// A temp message waiting for its friend's acknowledgement. Held in memory
/// only, like everything about temp chats.
struct TempOut {
    id: MessageId,
    peer: [u8; 32],
    content: Content,
    /// Encrypted once the pair's MLS group exists; resent as-is.
    cipher: Option<Vec<u8>>,
}

pub struct Session {
    identity: Identity,
    pub store: Store,
    mls: MlsClient,
    pub temp: TempChats,
    temp_out: Vec<TempOut>,
    events: Vec<Event>,
}

impl Session {
    pub fn open(
        identity_secret: [u8; 32],
        path: impl AsRef<Path>,
        store_key: &[u8; 32],
    ) -> Result<Self> {
        let identity = Identity::from_secret(identity_secret);
        let store = Store::open(path, store_key)?;
        let mls = MlsClient::load(&identity, &store)?;
        Ok(Self {
            identity,
            store,
            mls,
            temp: TempChats::default(),
            temp_out: Vec::new(),
            events: Vec::new(),
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.identity.public_key()
    }

    /// Your friend code, to show as a QR or share.
    pub fn my_code(&self, name: &str) -> String {
        FriendCode::new(self.public_key(), name).encode()
    }

    // ---- Friends -----------------------------------------------------

    /// Adds the friend whose code was scanned, pasted or typed. Each friend
    /// adds the other's code; the encrypted session is set up on first
    /// contact.
    pub fn add_friend(&mut self, code: &str) -> Result<Paired> {
        let friend = FriendCode::decode(code)?;
        if friend.identity == self.public_key() {
            return Err(SessionError::OwnCode);
        }
        if self.direct_chat(&friend.identity)?.is_some() {
            return Err(SessionError::AlreadyFriends);
        }
        let sigil_key = pair_fingerprint(&self.public_key(), &friend.identity).to_vec();
        let name = if friend.name.is_empty() {
            "Friend".to_string()
        } else {
            friend.name
        };
        let chat_id = self.store.create_chat(&ChatMeta {
            kind: ChatKind::Direct,
            title: name.clone(),
            sigil_key: sigil_key.clone(),
            pinned: false,
            created_at: now(),
            group_id: Vec::new(),
            peers: vec![Peer {
                identity: friend.identity,
                name: name.clone(),
                onion: None,
            }],
        })?;
        Ok(Paired {
            chat_id,
            friend: name,
            safety_number: safety_number(&sigil_key),
            sigil_key,
        })
    }

    /// Friends, as (chat id, friend).
    pub fn friends(&self) -> Result<Vec<(i64, Peer)>> {
        Ok(self
            .store
            .chats()?
            .into_iter()
            .filter(|(_, m)| m.kind == ChatKind::Direct)
            .filter_map(|(id, m)| m.peers.into_iter().next().map(|p| (id, p)))
            .collect())
    }

    pub fn chat(&self, chat_id: i64) -> Result<ChatMeta> {
        self.store
            .chats()?
            .into_iter()
            .find(|(id, _)| *id == chat_id)
            .map(|(_, m)| m)
            .ok_or(SessionError::NoSuchChat)
    }

    fn direct_chat(&self, peer: &[u8; 32]) -> Result<Option<(i64, ChatMeta)>> {
        Ok(self.store.chats()?.into_iter().find(|(_, m)| {
            m.kind == ChatKind::Direct && m.peers.iter().any(|p| &p.identity == peer)
        }))
    }

    fn friend_name(&self, peer: &[u8; 32]) -> String {
        self.direct_chat(peer)
            .ok()
            .flatten()
            .map_or_else(|| "Friend".into(), |(_, m)| m.title)
    }

    // ---- Sending -----------------------------------------------------

    /// Saves our message and queues it for the friend. Returns the friend
    /// to reach, or `None` for notes to self.
    pub fn send_text(&mut self, chat_id: i64, body: &str) -> Result<Option<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        let sent_at = now();
        let note = meta.kind == ChatKind::Notes;
        let seq = self.store.add_message(
            chat_id,
            &StoredMessage {
                from_me: true,
                body: body.into(),
                sent_at,
                delivered: note,
            },
        )?;
        let Some(peer) = meta.peers.first().map(|p| p.identity) else {
            return Ok(None);
        };
        let content = Content::Text {
            body: body.into(),
            sent_at,
        };
        self.store.outbox_put(
            &rand::random(),
            &OutboxItem {
                peer,
                state: OutboxState::Plain(content.encode()),
                message: Some((chat_id, seq)),
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        Ok(Some(peer))
    }

    /// Friends with something waiting to be delivered.
    pub fn pending_peers(&self) -> Result<Vec<[u8; 32]>> {
        let mut peers = self.store.outbox_peers()?;
        for out in &self.temp_out {
            if !peers.contains(&out.peer) {
                peers.push(out.peer);
            }
        }
        Ok(peers)
    }

    // ---- Temp chats --------------------------------------------------

    /// Starts a temp chat, with a friend (by their chat id) or yourself.
    pub fn temp_new(&mut self, friend_chat: Option<i64>) -> Result<u64> {
        self.purge();
        let peer = match friend_chat {
            Some(chat_id) => Some(
                self.chat(chat_id)?
                    .peers
                    .first()
                    .map(|p| p.identity)
                    .ok_or(SessionError::NotAFriend)?,
            ),
            None => None,
        };
        let title = peer.map_or_else(|| "Just me".into(), |p| self.friend_name(&p));
        let chat = self.temp.create(peer, title);
        let (id, remote) = (chat.id, chat.remote);
        if let Some(peer) = peer {
            self.queue_temp(peer, Content::TempOpen { temp: remote });
        }
        Ok(id)
    }

    /// Sends a temp message. Returns the friend to reach, if any.
    pub fn temp_send(&mut self, temp_id: u64, body: &str) -> Result<Option<[u8; 32]>> {
        self.purge();
        let chat = self.temp.get_mut(temp_id).ok_or(SessionError::NoSuchChat)?;
        let sent_at = now();
        let id = chat.peer.map(|_| rand::random::<MessageId>());
        chat.messages.push(TempMessage {
            from_me: true,
            body: body.into(),
            sent_at,
            id,
            delivered: false,
        });
        let (peer, remote) = (chat.peer, chat.remote);
        if let (Some(peer), Some(id)) = (peer, id) {
            self.temp_out.push(TempOut {
                id,
                peer,
                content: Content::TempText {
                    temp: remote,
                    body: body.into(),
                    sent_at,
                },
                cipher: None,
            });
        }
        self.events.push(Event::Temp(temp_id));
        Ok(peer)
    }

    /// Wipes a temp chat here and asks the friend's phone to wipe it too.
    pub fn temp_wipe(&mut self, temp_id: u64) -> Option<[u8; 32]> {
        let chat = self.temp.wipe(temp_id)?;
        self.forget_temp_outbox(chat.peer, chat.remote);
        let peer = chat.peer?;
        self.queue_temp(peer, Content::TempWipe { temp: chat.remote });
        Some(peer)
    }

    /// Wipes every temp chat; returns friends to tell.
    pub fn temp_wipe_all(&mut self) -> Vec<[u8; 32]> {
        let mut peers = Vec::new();
        for chat in self.temp.wipe_all() {
            self.forget_temp_outbox(chat.peer, chat.remote);
            if let Some(peer) = chat.peer {
                self.queue_temp(peer, Content::TempWipe { temp: chat.remote });
                if !peers.contains(&peer) {
                    peers.push(peer);
                }
            }
        }
        peers
    }

    /// Burns expired temp messages. Call on a timer.
    pub fn purge(&mut self) {
        for id in self.temp.purge_expired(now()) {
            self.events.push(Event::TempGone(id));
        }
    }

    fn queue_temp(&mut self, peer: [u8; 32], content: Content) {
        self.temp_out.push(TempOut {
            id: rand::random(),
            peer,
            content,
            cipher: None,
        });
    }

    /// Drops undelivered messages of a wiped chat; only the wipe itself
    /// still needs to reach the friend.
    fn forget_temp_outbox(&mut self, peer: Option<[u8; 32]>, remote: TempId) {
        self.temp_out.retain(|o| {
            Some(o.peer) != peer
                || !matches!(&o.content, Content::TempOpen { temp } | Content::TempText { temp, .. } if *temp == remote)
        });
    }

    /// Events since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    // ---- Connections -------------------------------------------------

    /// We were reached: the challenge to send first.
    pub fn challenge(&self) -> ([u8; 32], Frame) {
        let nonce = rand::random();
        (nonce, Frame::Challenge { nonce })
    }

    /// We reached `peer` and got their challenge: our signed hello.
    pub fn hello(&self, peer: &[u8; 32], challenge: &Frame) -> Result<Frame> {
        let Frame::Challenge { nonce } = challenge else {
            return Err(SessionError::Protocol);
        };
        let signature = self.identity.sign(&hello_message(peer, nonce)).to_vec();
        Ok(Frame::Hello {
            from: self.public_key(),
            signature,
        })
    }

    /// We were reached and got their hello: who they are, if a friend.
    pub fn accept_hello(&self, nonce: &[u8; 32], hello: &Frame) -> Result<[u8; 32]> {
        let Frame::Hello { from, signature } = hello else {
            return Err(SessionError::Protocol);
        };
        let signature: [u8; 64] = signature
            .as_slice()
            .try_into()
            .map_err(|_| SessionError::Protocol)?;
        if !verify(from, &hello_message(&self.public_key(), nonce), &signature) {
            return Err(SessionError::Protocol);
        }
        if self.direct_chat(from)?.is_none() {
            return Err(SessionError::NotAFriend);
        }
        Ok(*from)
    }

    /// A connection with `peer` is ready: frames to send first.
    pub fn on_link_up(&mut self, peer: &[u8; 32]) -> Result<Vec<Frame>> {
        let (_, meta) = self.direct_chat(peer)?.ok_or(SessionError::NotAFriend)?;
        if meta.group_id.is_empty() {
            // The friend with the smaller key sets up the group.
            return Ok(if self.public_key() < *peer {
                vec![Frame::KeyPackageRequest]
            } else {
                Vec::new()
            });
        }
        self.flush(peer)
    }

    /// Handles a frame from `peer`; returns frames to send back.
    pub fn on_frame(&mut self, peer: &[u8; 32], frame: Frame) -> Result<Vec<Frame>> {
        let (chat_id, mut meta) = self.direct_chat(peer)?.ok_or(SessionError::NotAFriend)?;
        let initiator = self.public_key() < *peer;
        match frame {
            Frame::KeyPackageRequest if !initiator => {
                if !meta.group_id.is_empty() {
                    return Ok(Vec::new());
                }
                let kp = self.mls.key_package()?;
                self.mls.persist(&self.store)?;
                Ok(vec![Frame::KeyPackage(kp)])
            }
            Frame::KeyPackage(kp) if initiator => {
                if !meta.group_id.is_empty() {
                    return self.flush(peer);
                }
                let group = self.mls.create_group()?;
                let invite = self.mls.invite(&group, &kp)?;
                self.mls.persist(&self.store)?;
                // The key package must be this friend's, nobody else's.
                if !self.pair_members_ok(&group, peer)? {
                    return Err(SessionError::Protocol);
                }
                meta.group_id = group;
                self.store.update_chat(chat_id, &meta)?;
                let mut out = vec![Frame::Welcome(invite.welcome)];
                out.extend(self.flush(peer)?);
                Ok(out)
            }
            Frame::Welcome(welcome) if !initiator => {
                if !meta.group_id.is_empty() {
                    return Ok(Vec::new());
                }
                let group = self.mls.join(&welcome)?;
                self.mls.persist(&self.store)?;
                if !self.pair_members_ok(&group, peer)? {
                    return Err(SessionError::Protocol);
                }
                meta.group_id = group;
                self.store.update_chat(chat_id, &meta)?;
                self.flush(peer)
            }
            Frame::Mls { id, bytes } => {
                if meta.group_id.is_empty() {
                    return Err(SessionError::Protocol);
                }
                // A resend after our acknowledgement got lost.
                if self.store.is_seen(&id)? {
                    return Ok(vec![Frame::Ack { id }]);
                }
                match self.mls.receive(&meta.group_id, &bytes) {
                    Ok(Received::Message { sender, plaintext }) if sender == *peer => {
                        self.mls.persist(&self.store)?;
                        if let Some(content) = Content::decode(&plaintext) {
                            self.deliver(chat_id, peer, content)?;
                        }
                    }
                    Ok(_) => self.mls.persist(&self.store)?,
                    // Undecryptable (e.g. stale after a reinstall): drop it,
                    // and acknowledge so the sender stops retrying.
                    Err(_) => {}
                }
                self.store.mark_seen(&id)?;
                Ok(vec![Frame::Ack { id }])
            }
            Frame::Ack { id } => {
                if let Some(item) = self.store.outbox_remove(&id)? {
                    if let Some((chat, seq)) = item.message {
                        self.store.set_delivered(chat, seq)?;
                        self.events.push(Event::Chat(chat));
                    }
                } else if let Some(i) = self.temp_out.iter().position(|o| o.id == id) {
                    self.temp_out.remove(i);
                    if let Some(temp) = self.temp.mark_delivered(&id) {
                        self.events.push(Event::Temp(temp));
                    }
                }
                Ok(Vec::new())
            }
            _ => Err(SessionError::Protocol),
        }
    }

    /// Applies a friend's message.
    fn deliver(&mut self, chat_id: i64, peer: &[u8; 32], content: Content) -> Result<()> {
        let name = self.friend_name(peer);
        self.purge();
        match content {
            Content::Text { body, sent_at } => {
                self.store.add_message(
                    chat_id,
                    &StoredMessage {
                        from_me: false,
                        body,
                        sent_at: sent_at.min(now()),
                        delivered: true,
                    },
                )?;
                self.events.push(Event::Chat(chat_id));
            }
            Content::TempOpen { temp } => {
                let id = self.temp.for_remote(*peer, temp, &name).id;
                self.events.push(Event::Temp(id));
            }
            Content::TempText {
                temp,
                body,
                sent_at,
            } => {
                let chat = self.temp.for_remote(*peer, temp, &name);
                chat.messages.push(TempMessage {
                    from_me: false,
                    body,
                    sent_at: sent_at.min(now()),
                    id: None,
                    delivered: true,
                });
                let id = chat.id;
                self.purge();
                self.events.push(Event::Temp(id));
            }
            Content::TempWipe { temp } => {
                if let Some(id) = self.temp.wipe_remote(*peer, temp) {
                    self.forget_temp_outbox(Some(*peer), temp);
                    self.events.push(Event::TempGone(id));
                }
            }
        }
        Ok(())
    }

    /// Everything waiting for `peer`, encrypted for the pair's group.
    fn flush(&mut self, peer: &[u8; 32]) -> Result<Vec<Frame>> {
        let Some((_, meta)) = self.direct_chat(peer)? else {
            return Ok(Vec::new());
        };
        if meta.group_id.is_empty() {
            return Ok(Vec::new());
        }
        let mut frames = Vec::new();
        for (id, mut item) in self.store.outbox_for(peer)? {
            if let OutboxState::Plain(plain) = &item.state {
                item.state = OutboxState::Cipher(self.mls.encrypt(&meta.group_id, plain)?);
                self.mls.persist(&self.store)?;
                self.store.outbox_put(&id, &item)?;
            }
            if let OutboxState::Cipher(bytes) = item.state {
                frames.push(Frame::Mls { id, bytes });
            }
        }
        for out in self.temp_out.iter_mut().filter(|o| &o.peer == peer) {
            if out.cipher.is_none() {
                out.cipher = Some(self.mls.encrypt(&meta.group_id, &out.content.encode())?);
                self.mls.persist(&self.store)?;
            }
            frames.push(Frame::Mls {
                id: out.id,
                bytes: out.cipher.clone().unwrap(),
            });
        }
        Ok(frames)
    }

    /// The pair's group must be exactly us and this friend.
    fn pair_members_ok(&self, group: &[u8], peer: &[u8; 32]) -> Result<bool> {
        let mut members = self.mls.members(group)?;
        members.sort();
        let mut expected = vec![self.public_key(), *peer];
        expected.sort();
        Ok(members == expected)
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Phone {
        s: Session,
        secret: [u8; 32],
        key: [u8; 32],
        dir: TempDir,
    }

    impl Phone {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let secret = Identity::generate().secret();
            let key = rand::random();
            let s = Session::open(secret, dir.path().join("s.db"), &key).unwrap();
            Self {
                s,
                secret,
                key,
                dir,
            }
        }

        fn pk(&self) -> [u8; 32] {
            self.s.public_key()
        }

        /// A restart: everything in memory (including temp chats) is gone.
        fn restart(&mut self) {
            self.s = Session::open(self.secret, self.dir.path().join("s.db"), &self.key).unwrap();
        }

        fn texts(&self, chat: i64) -> Vec<(bool, String, bool)> {
            self.s
                .store
                .messages(chat, None)
                .unwrap()
                .into_iter()
                .map(|m| (m.from_me, m.body, m.delivered))
                .collect()
        }
    }

    /// Both add each other's codes. Returns (alice's chat with bob, bob's chat with alice).
    fn befriend(a: &mut Phone, b: &mut Phone) -> (i64, i64) {
        let ab = a.s.add_friend(&b.s.my_code("Bob")).unwrap().chat_id;
        let ba = b.s.add_friend(&a.s.my_code("Alice")).unwrap().chat_id;
        (ab, ba)
    }

    /// `dialer` connects to `reached`: handshake, then frames flow both ways
    /// until both sides are quiet. Returns how many frames were exchanged.
    fn connect(dialer: &mut Phone, reached: &mut Phone) -> usize {
        let (nonce, challenge) = reached.s.challenge();
        let hello = dialer.s.hello(&reached.pk(), &challenge).unwrap();
        let who = reached.s.accept_hello(&nonce, &hello).unwrap();
        assert_eq!(who, dialer.pk());
        let (dpk, rpk) = (dialer.pk(), reached.pk());
        let mut to_reached = dialer.s.on_link_up(&rpk).unwrap();
        let mut to_dialer = reached.s.on_link_up(&dpk).unwrap();
        let mut count = 0;
        while !to_reached.is_empty() || !to_dialer.is_empty() {
            let mut next_to_dialer = Vec::new();
            for f in to_reached.drain(..) {
                count += 1;
                next_to_dialer.extend(reached.s.on_frame(&dpk, f).unwrap());
            }
            let mut next_to_reached = Vec::new();
            for f in to_dialer.drain(..) {
                count += 1;
                next_to_reached.extend(dialer.s.on_frame(&rpk, f).unwrap());
            }
            to_dialer.extend(next_to_dialer);
            to_reached.extend(next_to_reached);
            assert!(count < 1000, "frames never settle");
        }
        count
    }

    #[test]
    fn first_contact_sets_up_encryption_and_delivers_waiting_messages() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        // Written before the phones ever connected.
        assert_eq!(alice.s.send_text(ab, "hi bob").unwrap(), Some(bob.pk()));
        assert_eq!(alice.s.pending_peers().unwrap(), vec![bob.pk()]);
        assert_eq!(alice.texts(ab), [(true, "hi bob".into(), false)]);

        connect(&mut alice, &mut bob);
        assert_eq!(bob.texts(ba), [(false, "hi bob".into(), true)]);
        assert_eq!(
            alice.texts(ab),
            [(true, "hi bob".into(), true)],
            "delivered tick"
        );
        assert!(alice.s.pending_peers().unwrap().is_empty());
        assert!(bob.s.take_events().contains(&Event::Chat(ba)));
        assert!(!alice.s.chat(ab).unwrap().group_id.is_empty());
        assert_eq!(
            alice.s.chat(ab).unwrap().group_id,
            bob.s.chat(ba).unwrap().group_id
        );
    }

    #[test]
    fn either_side_may_connect_and_messages_flow_both_ways() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        bob.s.send_text(ba, "from bob, before setup").unwrap();
        connect(&mut bob, &mut alice); // bob dials this time
        assert_eq!(
            alice.texts(ab),
            [(false, "from bob, before setup".into(), true)]
        );
        for i in 0..5 {
            alice.s.send_text(ab, &format!("a{i}")).unwrap();
            bob.s.send_text(ba, &format!("b{i}")).unwrap();
            if i % 2 == 0 {
                connect(&mut alice, &mut bob)
            } else {
                connect(&mut bob, &mut alice)
            };
        }
        assert_eq!(alice.texts(ab).len(), 11);
        assert!(alice.texts(ab).iter().all(|m| m.2), "all delivered");
        assert_eq!(bob.texts(ba).iter().filter(|m| !m.0).count(), 5);
    }

    #[test]
    fn queued_messages_survive_restarts_and_lost_acks_do_not_duplicate() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob); // set up encryption
        alice.s.send_text(ab, "one").unwrap();
        alice.restart();
        bob.restart();

        // Deliver, but lose bob's acknowledgement.
        let (nonce, challenge) = bob.s.challenge();
        let hello = alice.s.hello(&bob.pk(), &challenge).unwrap();
        bob.s.accept_hello(&nonce, &hello).unwrap();
        let (apk, bpk) = (alice.pk(), bob.pk());
        let frames = alice.s.on_link_up(&bpk).unwrap();
        bob.s.on_link_up(&apk).unwrap();
        for f in frames {
            bob.s.on_frame(&apk, f).unwrap(); // acks dropped
        }
        assert_eq!(alice.texts(ab), [(true, "one".into(), false)]);

        // Next time it is resent; bob acknowledges without a duplicate.
        connect(&mut alice, &mut bob);
        assert_eq!(bob.texts(ba), [(false, "one".into(), true)]);
        assert_eq!(alice.texts(ab), [(true, "one".into(), true)]);
    }

    #[test]
    fn strangers_and_forged_hellos_are_refused() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        befriend(&mut alice, &mut bob);
        let eve = Phone::new();
        let (nonce, challenge) = bob.s.challenge();
        // Eve is not bob's friend.
        let hello = eve.s.hello(&bob.pk(), &challenge).unwrap();
        assert!(matches!(
            bob.s.accept_hello(&nonce, &hello),
            Err(SessionError::NotAFriend)
        ));
        // Eve claims to be alice but can't sign as her.
        let Frame::Hello { signature, .. } = hello else {
            unreachable!()
        };
        let forged = Frame::Hello {
            from: alice.pk(),
            signature,
        };
        assert!(matches!(
            bob.s.accept_hello(&nonce, &forged),
            Err(SessionError::Protocol)
        ));
        // A hello signed for a different challenge (replay).
        let (_, other) = bob.s.challenge();
        let replay = alice.s.hello(&bob.pk(), &other).unwrap();
        assert!(bob.s.accept_hello(&nonce, &replay).is_err());
    }

    #[test]
    fn temp_chat_with_a_friend_appears_on_both_phones_and_never_touches_disk() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        befriend(&mut alice, &mut bob);
        let (ab, _) = (alice.s.friends().unwrap()[0].0, ());
        let t = alice.s.temp_new(Some(ab)).unwrap();
        alice.s.temp_send(t, "secret plan").unwrap();
        connect(&mut alice, &mut bob);

        let on_bob = &bob.s.temp.all()[0];
        assert_eq!(on_bob.title, "Alice");
        assert_eq!(on_bob.messages[0].body, "secret plan");
        assert!(alice.s.temp.get(t).unwrap().messages[0].delivered);

        // Bob replies; alice sees it in the same temp chat.
        let bt = on_bob.id;
        bob.s.temp_send(bt, "got it").unwrap();
        connect(&mut bob, &mut alice);
        let bodies: Vec<_> = alice
            .s
            .temp
            .get(t)
            .unwrap()
            .messages
            .iter()
            .map(|m| m.body.clone())
            .collect();
        assert_eq!(bodies, ["secret plan", "got it"]);

        // Nothing of it is on disk, on either phone.
        for p in [&alice, &bob] {
            let mut bytes = std::fs::read(p.dir.path().join("s.db")).unwrap();
            if let Ok(wal) = std::fs::read(p.dir.path().join("s.db-wal")) {
                bytes.extend(wal);
            }
            assert!(!bytes.windows(11).any(|w| w == b"secret plan"));
        }
        // A restart ends temp chats.
        bob.restart();
        assert!(bob.s.temp.all().is_empty());
    }

    #[test]
    fn wiping_a_temp_chat_wipes_it_on_both_phones() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        befriend(&mut alice, &mut bob);
        let ab = alice.s.friends().unwrap()[0].0;
        let t = alice.s.temp_new(Some(ab)).unwrap();
        alice.s.temp_send(t, "soon gone").unwrap();
        connect(&mut alice, &mut bob);
        let bt = bob.s.temp.all()[0].id;
        bob.s.take_events();

        assert_eq!(bob.s.temp_wipe(bt), Some(alice.pk()));
        connect(&mut bob, &mut alice);
        assert!(alice.s.temp.get(t).is_none());
        assert!(alice.s.take_events().contains(&Event::TempGone(t)));
    }

    #[test]
    fn wiping_before_delivery_sends_only_the_wipe() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        befriend(&mut alice, &mut bob);
        let ab = alice.s.friends().unwrap()[0].0;
        let t = alice.s.temp_new(Some(ab)).unwrap();
        alice.s.temp_send(t, "never mind").unwrap();
        alice.s.temp_wipe(t);
        connect(&mut alice, &mut bob);
        assert!(bob.s.temp.all().is_empty(), "the message never arrives");
    }

    #[test]
    fn temp_chat_with_yourself_stays_local() {
        let mut alice = Phone::new();
        let t = alice.s.temp_new(None).unwrap();
        assert_eq!(alice.s.temp_send(t, "note").unwrap(), None);
        assert!(alice.s.pending_peers().unwrap().is_empty());
        assert_eq!(alice.s.temp.get(t).unwrap().title, "Just me");
    }

    #[test]
    fn notes_are_never_queued() {
        let mut alice = Phone::new();
        let notes = alice
            .s
            .store
            .create_chat(&ChatMeta {
                kind: ChatKind::Notes,
                title: "Notes to self".into(),
                sigil_key: alice.pk().to_vec(),
                pinned: false,
                created_at: 1,
                group_id: vec![],
                peers: vec![],
            })
            .unwrap();
        assert_eq!(alice.s.send_text(notes, "x").unwrap(), None);
        assert!(alice.s.pending_peers().unwrap().is_empty());
    }

    #[test]
    fn friend_codes_and_safety_numbers() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let on_alice = alice.s.add_friend(&bob.s.my_code("Bob")).unwrap();
        let on_bob = bob.s.add_friend(&alice.s.my_code("Alice")).unwrap();
        assert_eq!(
            (on_alice.friend.as_str(), on_bob.friend.as_str()),
            ("Bob", "Alice")
        );
        assert_eq!(on_alice.safety_number, on_bob.safety_number);
        assert!(matches!(
            alice.s.add_friend(&bob.s.my_code("Bob")),
            Err(SessionError::AlreadyFriends)
        ));
        assert!(matches!(
            alice.s.add_friend(&alice.s.my_code("me")),
            Err(SessionError::OwnCode)
        ));
    }
}
