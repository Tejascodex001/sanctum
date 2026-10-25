//! A signed-in device: identity, encrypted store, MLS and temp chats, plus
//! the protocol engine that talks to friends.
//!
//! The engine does no I/O. The network layer feeds it frames from a
//! connection ([`Session::on_frame`]) and writes back what it returns, so
//! whole conversations between phones are tested here by passing frames
//! between two sessions.

use crate::call::{now_ms, CallView, Calls, MediaHub};
use crate::group_sync::{GroupSync, Offers};
use crate::identity::{pair_fingerprint, safety_number, verify, Identity};
use crate::invite::{FriendCode, InviteError};
use crate::mls::{MlsClient, MlsError, Received};
use crate::protocol::{hello_message, Content, Frame, MessageId, Quote, TempId};
use crate::store::{
    ChatKind, ChatMeta, OutboxItem, OutboxState, Peer, Store, StoreError, StoredMessage,
};
use crate::temp_chat::{TempChats, TempMessage};
use std::collections::HashSet;
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
    #[error("only the group's admin can do that")]
    NotAdmin,
    /// A group rule said no; the words are for the person to read.
    #[error("{0}")]
    GroupRule(&'static str),
    #[error("too large to send ({0} MB at most for this kind)")]
    TooLarge(u64),
    #[error("storage: {0}")]
    Io(String),
    #[error("the other side broke the protocol")]
    Protocol,
    #[error("already in a call")]
    InCall,
    #[error("groups can have up to {0} people")]
    GroupFull(usize),
}

pub type Result<T> = std::result::Result<T, SessionError>;

/// A friend just added from their code.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub chat_id: i64,
    pub friend: String,
    /// The friend's identity key (their own sigil comes from it).
    pub friend_key: [u8; 32],
    /// Added by scanning their QR in person.
    pub verified: bool,
    /// Both phones derive the same sigil from this.
    pub sigil_key: Vec<u8>,
    /// Both phones show this number, to compare on a call.
    pub safety_number: String,
}

/// Something the UI should refresh for.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// New message, or a message was delivered, changed or reacted to.
    Chat(i64),
    /// A temp chat appeared or changed.
    Temp(u64),
    /// A temp chat was wiped (by the friend) or burned away.
    TempGone(u64),
    /// A friend request arrived.
    Request,
    /// A friend's message arrived in this chat (also sent as `Chat`):
    /// worth a notification, unlike a delivery receipt.
    Message(i64),
    /// A friend's message arrived in this temp chat.
    TempMessage(u64),
    /// Today's moments changed.
    Moments,
    /// `who` (called `name` here) is writing in this chat right now. Only
    /// from connected friends; nothing is stored. Show it for a few
    /// seconds unless another one comes.
    Typing { chat: i64, who: [u8; 32], name: String },
    /// A call rang, was answered or ended (see `call`).
    Call(CallView),
    /// A call to us went unanswered: its record is in this chat. Worth a
    /// notification.
    MissedCall(i64),
}

/// Someone who added your code and wants you to add them back. Held in
/// memory only: they keep asking until you answer.
#[derive(Debug, Clone, PartialEq)]
pub struct FriendRequest {
    pub identity: [u8; 32],
    pub name: String,
    pub at: u64,
}

/// More pending requests than this and the oldest are dropped.
const MAX_REQUESTS: usize = 20;
const MAX_NAME: usize = 64;

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
    pub(crate) identity: Identity,
    pub store: Store,
    pub(crate) mls: MlsClient,
    pub temp: TempChats,
    temp_out: Vec<TempOut>,
    pub(crate) events: Vec<Event>,
    /// Your name, sent with friend requests.
    pub(crate) my_name: String,
    /// Group messages we couldn't read yet (e.g. a membership change is
    /// still on its way), by frame id: retried a few times, then dropped.
    pub(crate) unreadable: std::collections::HashMap<MessageId, u8>,
    /// Peers with something new to send, found while handling another's
    /// frame (e.g. a group change everyone must apply): the network reaches
    /// them ([`Session::take_pokes`]).
    pub(crate) pokes: Vec<[u8; 32]>,
    /// Friends we've sent our nearby key to this run.
    pub(crate) nearby_offered: HashSet<[u8; 32]>,
    /// Attachments' encrypted files.
    pub(crate) media_dir: std::path::PathBuf,
    /// Attachments sent on the current link, by (peer, outbox id).
    pub(crate) blobs_sent: std::collections::HashMap<([u8; 32], MessageId), std::time::Instant>,
    requests: Vec<FriendRequest>,
    /// Declined this run; not shown again until a restart.
    declined: HashSet<[u8; 32]>,
    /// When we last said we're typing, per chat (see `typing`).
    pub(crate) typing_sent: std::collections::HashMap<i64, std::time::Instant>,
    /// When we last passed on someone's typing, per (chat, person).
    pub(crate) typing_heard: std::collections::HashMap<(i64, [u8; 32]), std::time::Instant>,
    /// The call going on now (see `call`); memory only.
    pub(crate) calls: Calls,
    /// Its sealed sound, shared with the network and the phone's audio.
    pub(crate) media: MediaHub,
    /// Members passing group messages and files on (see `group_sync`).
    pub(crate) sync: GroupSync,
}

impl Session {
    pub fn open(
        identity_secret: [u8; 32],
        path: impl AsRef<Path>,
        store_key: &[u8; 32],
    ) -> Result<Self> {
        let identity = Identity::from_secret(identity_secret);
        let media_dir = path.as_ref().parent().unwrap_or(Path::new(".")).join("media");
        let store = Store::open(path, store_key)?;
        let mls = MlsClient::load(&identity, &store)?;
        Ok(Self {
            identity,
            store,
            mls,
            temp: TempChats::default(),
            temp_out: Vec::new(),
            events: Vec::new(),
            my_name: String::new(),
            unreadable: Default::default(),
            pokes: Vec::new(),
            nearby_offered: HashSet::new(),
            media_dir,
            blobs_sent: Default::default(),
            requests: Vec::new(),
            declined: HashSet::new(),
            typing_sent: Default::default(),
            typing_heard: Default::default(),
            calls: Calls::default(),
            media: MediaHub::default(),
            sync: GroupSync::default(),
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.identity.public_key()
    }

    /// The name your friend requests carry.
    pub fn set_my_name(&mut self, name: &str) {
        self.my_name = clip(name.trim(), MAX_NAME);
    }

    /// Your friend code, to show as a QR or share.
    pub fn my_code(&self, name: &str) -> String {
        FriendCode::new(self.public_key(), name).encode()
    }

    // ---- Friends -----------------------------------------------------

    /// Adds the friend whose code was scanned, pasted or typed. Each friend
    /// adds the other's code; the encrypted session is set up on first
    /// contact. A code `scanned` in person from their screen counts as
    /// verified; a typed or pasted one is verified later by comparing the
    /// safety number.
    pub fn add_friend(&mut self, code: &str, scanned: bool) -> Result<Paired> {
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
                verified: scanned,
            }],
            hidden: false,
            admin: None,
            admins: Vec::new(),
        })?;
        Ok(Paired {
            chat_id,
            friend: name,
            friend_key: friend.identity,
            verified: scanned,
            safety_number: safety_number(&sigil_key),
            sigil_key,
        })
    }

    /// Renames a chat; for a friend's chat, also what you call the friend
    /// (temp chats with them use it too). Only on this phone.
    pub fn rename_chat(&mut self, chat_id: i64, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let mut meta = self.chat(chat_id)?;
        meta.title = name.to_string();
        if meta.kind == ChatKind::Direct {
            for p in &mut meta.peers {
                p.name = name.to_string();
            }
        }
        self.store.update_chat(chat_id, &meta)?;
        Ok(())
    }

    /// Hides a chat from the list (or shows it again). Only on this phone.
    pub fn set_hidden(&mut self, chat_id: i64, hidden: bool) -> Result<()> {
        let mut meta = self.chat(chat_id)?;
        meta.hidden = hidden;
        self.store.update_chat(chat_id, &meta)?;
        Ok(())
    }

    /// Marks a friend verified after comparing the safety number.
    pub fn set_verified(&mut self, chat_id: i64, verified: bool) -> Result<()> {
        let mut meta = self.chat(chat_id)?;
        for p in &mut meta.peers {
            p.verified = verified;
        }
        self.store.update_chat(chat_id, &meta)?;
        Ok(())
    }

    /// The safety number for a friend's chat.
    pub fn safety_number(&self, chat_id: i64) -> Result<String> {
        Ok(safety_number(&self.chat(chat_id)?.sigil_key))
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

    pub(crate) fn direct_chat(&self, peer: &[u8; 32]) -> Result<Option<(i64, ChatMeta)>> {
        Ok(self.store.chats()?.into_iter().find(|(_, m)| {
            m.kind == ChatKind::Direct && m.peers.iter().any(|p| &p.identity == peer)
        }))
    }

    pub(crate) fn friend_name(&self, peer: &[u8; 32]) -> String {
        self.direct_chat(peer)
            .ok()
            .flatten()
            .map_or_else(|| "Friend".into(), |(_, m)| m.title)
    }

    // ---- Sending -----------------------------------------------------

    /// Saves our message and queues it for the friend. Returns the friend
    /// to reach, or `None` for notes to self.
    pub fn send_text(&mut self, chat_id: i64, body: &str) -> Result<Option<[u8; 32]>> {
        self.send_message(chat_id, body, None)
    }

    /// Like [`Session::send_text`], optionally answering the message at
    /// `reply_to` (its sequence number in the chat), which it quotes.
    pub fn send_message(
        &mut self,
        chat_id: i64,
        body: &str,
        reply_to: Option<i64>,
    ) -> Result<Option<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        let sent_at = now();
        let note = meta.kind == ChatKind::Notes;
        let id: MessageId = rand::random();
        let quote = match reply_to {
            Some(seq) => self.quote(&meta, chat_id, seq)?,
            None => None,
        };
        let seq = self.store.add_message(
            chat_id,
            &StoredMessage {
                from_me: true,
                body: body.into(),
                sent_at,
                delivered: note,
                id: Some(id),
                reply: quote.clone(),
                sender: None,
                media: None,
                ..Default::default()
            },
        )?;
        let Some(peer) = meta.peers.first().map(|p| p.identity) else {
            return Ok(None);
        };
        let content = match quote {
            Some(quote) => Content::Reply {
                body: body.into(),
                sent_at,
                quote,
            },
            None => Content::Text {
                body: body.into(),
                sent_at,
            },
        };
        self.store.outbox_put(
            &id,
            &OutboxItem {
                peer,
                state: OutboxState::Plain(content.encode()),
                message: Some((chat_id, seq)),
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        Ok(Some(peer))
    }

    /// What a reply to the message at `seq` shows of it.
    pub(crate) fn quote(&self, meta: &ChatMeta, chat_id: i64, seq: i64) -> Result<Option<Quote>> {
        let Some(m) = self.store.message(chat_id, seq)? else {
            return Ok(None);
        };
        let author = if m.from_me {
            self.public_key()
        } else {
            m.sender
                .or_else(|| meta.peers.first().map(|p| p.identity))
                .unwrap_or_default()
        };
        Ok(Some(Quote {
            id: m.id.unwrap_or_default(),
            author,
            snippet: clip(&m.body, 140),
        }))
    }

    /// Who the network should reach now (see `pokes`).
    pub fn take_pokes(&mut self) -> Vec<[u8; 32]> {
        std::mem::take(&mut self.pokes)
    }

    /// Friends with something waiting to be delivered.
    pub fn pending_peers(&self) -> Result<Vec<[u8; 32]>> {
        let mut peers = self.store.outbox_peers()?;
        // Friends we haven't set up encryption with: our friend request
        // goes out until they add us back.
        for (_, meta) in self.store.chats()? {
            if meta.kind == ChatKind::Direct && meta.group_id.is_empty() {
                for p in meta.peers {
                    if !peers.contains(&p.identity) {
                        peers.push(p.identity);
                    }
                }
            }
        }
        for peer in self.pending_invitees()? {
            if !peers.contains(&peer) {
                peers.push(peer);
            }
        }
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

    /// Burns expired temp messages, and ends calls nobody answered. Call on
    /// a timer.
    pub fn purge(&mut self) {
        for id in self.temp.purge_expired(now()) {
            self.events.push(Event::TempGone(id));
        }
        let _ = self.purge_moments();
        let _ = self.call_tick(now_ms(), std::time::Instant::now());
        let _ = self.prune_kept(now(), false);
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
        if *from == self.public_key() {
            return Err(SessionError::Protocol);
        }
        // Strangers get this far too: they may only send a friend request
        // ([`Session::on_stranger_frame`]).
        Ok(*from)
    }

    pub fn is_friend(&self, peer: &[u8; 32]) -> Result<bool> {
        Ok(self.direct_chat(peer)?.is_some())
    }

    /// The chat with this friend.
    pub fn chat_id_for(&self, peer: &[u8; 32]) -> Result<Option<i64>> {
        Ok(self.direct_chat(peer)?.map(|(id, _)| id))
    }

    /// Every friend's key: the network keeps a link open to each.
    pub fn friend_keys(&self) -> Result<Vec<[u8; 32]>> {
        Ok(self.friends()?.into_iter().map(|(_, p)| p.identity).collect())
    }

    /// The one frame a stranger may send: a friend request.
    pub fn on_stranger_frame(&mut self, peer: &[u8; 32], frame: Frame) -> Result<()> {
        let Frame::FriendRequest { name } = frame else {
            return Err(SessionError::NotAFriend);
        };
        if self.is_friend(peer)? || self.declined.contains(peer) {
            return Ok(());
        }
        let name = clip(name.trim(), MAX_NAME);
        let name = if name.is_empty() { "Someone".into() } else { name };
        self.requests.retain(|r| r.identity != *peer);
        self.requests.push(FriendRequest { identity: *peer, name, at: now() });
        if self.requests.len() > MAX_REQUESTS {
            self.requests.remove(0);
        }
        self.events.push(Event::Request);
        Ok(())
    }

    /// Pending friend requests, newest first.
    pub fn requests(&self) -> Vec<FriendRequest> {
        self.requests.iter().rev().cloned().collect()
    }

    /// Adds the friend who asked. Unverified: compare the safety number to
    /// be sure. Returns the friend to reach so encryption gets set up.
    pub fn accept_request(&mut self, identity: &[u8; 32]) -> Result<Paired> {
        let i = self
            .requests
            .iter()
            .position(|r| r.identity == *identity)
            .ok_or(SessionError::NoSuchChat)?;
        let request = self.requests.remove(i);
        let code = FriendCode::new(request.identity, &request.name).encode();
        self.add_friend(&code, false)
    }

    pub fn decline_request(&mut self, identity: &[u8; 32]) {
        self.requests.retain(|r| r.identity != *identity);
        self.declined.insert(*identity);
    }

    /// A connection with `peer` is ready: frames to send first.
    pub fn on_link_up(&mut self, peer: &[u8; 32]) -> Result<Vec<Frame>> {
        let mut out = match self.direct_chat(peer)? {
            // The friend with the smaller key sets up the pair's group. The
            // other asks too: if the friend still has a group from before
            // (we deleted and re-added them), that tells them to start
            // over. The request comes first in case they haven't added us.
            Some((_, meta)) if meta.group_id.is_empty() => vec![
                Frame::FriendRequest { name: self.my_name.clone() },
                Frame::KeyPackageRequest,
            ],
            Some(_) => {
                self.offer_nearby_key(peer)?;
                self.flush(peer)?
            }
            // Not a friend, but in a group with us; or a former friend we
            // still owe a goodbye ("removed on both phones").
            None if self.shares_group(peer)? || self.store.has_outbox_for(peer)? => self.flush(peer)?,
            None => return Err(SessionError::NotAFriend),
        };
        out.extend(self.group_invites_for(peer)?);
        Ok(out)
    }

    /// Whether `peer` may connect: a friend, or someone in a group with us.
    pub fn knows(&self, peer: &[u8; 32]) -> Result<bool> {
        Ok(self.is_friend(peer)? || self.shares_group(peer)?)
    }

    /// Handles a frame from `peer`; returns frames to send back.
    pub fn on_frame(&mut self, peer: &[u8; 32], frame: Frame) -> Result<Vec<Frame>> {
        // Frames anyone we know may send, friend or fellow group member.
        let frame = match frame {
            Frame::Ping => return Ok(Vec::new()),
            Frame::Ack { id } => {
                self.on_ack(id)?;
                return Ok(Vec::new());
            }
            Frame::GroupKeyPackageRequest { .. }
            | Frame::GroupKeyPackage { .. }
            | Frame::GroupWelcome { .. }
            | Frame::GroupMls { .. }
            | Frame::GroupHave { .. }
            | Frame::GroupWant { .. }
            | Frame::BlobWant { .. } => return self.on_group_frame(peer, frame),
            Frame::BlobChunk {
                id,
                blob,
                index,
                total,
                bytes,
            } => return self.on_blob_chunk(peer, id, blob, index, total, bytes),
            // Never answered, stored or acknowledged.
            Frame::Typing { group } => {
                self.on_typing(peer, &group)?;
                return Ok(Vec::new());
            }
            // A call's sound: only from the friend in the call, never kept.
            // (The network hands these to the media directly; see `call`.)
            Frame::CallMedia { call, seq, sealed } => {
                self.media.receive(peer, &call, seq, &sealed);
                return Ok(Vec::new());
            }
            other => other,
        };
        let Some((chat_id, mut meta)) = self.direct_chat(peer)? else {
            // A former friend we still owe a goodbye: whatever they sent
            // before getting it is moot. Keep the link for their ack.
            if self.store.has_outbox_for(peer)? {
                return Ok(Vec::new());
            }
            return Err(SessionError::NotAFriend);
        };
        let initiator = self.public_key() < *peer;
        match frame {
            // Already friends.
            Frame::FriendRequest { .. } => Ok(Vec::new()),
            Frame::KeyPackageRequest if initiator => {
                if meta.group_id.is_empty() {
                    // First contact: our own request is already on its way.
                    return Ok(Vec::new());
                }
                // They lost the group (deleted and re-added us): rebuild.
                meta.group_id = Vec::new();
                self.store.update_chat(chat_id, &meta)?;
                self.drop_stale_ciphers(peer)?;
                Ok(vec![Frame::KeyPackageRequest])
            }
            Frame::KeyPackageRequest => {
                // Answered even if we have a group: theirs is gone, and the
                // Welcome that follows replaces ours.
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
                // No longer "waiting for them to accept".
                self.events.push(Event::Chat(chat_id));
                let mut out = vec![Frame::Welcome(invite.welcome)];
                out.extend(self.flush(peer)?);
                Ok(out)
            }
            Frame::Welcome(welcome) if !initiator => {
                if !meta.group_id.is_empty() {
                    self.drop_stale_ciphers(peer)?;
                }
                let group = self.mls.join(&welcome)?;
                self.mls.persist(&self.store)?;
                if !self.pair_members_ok(&group, peer)? {
                    return Err(SessionError::Protocol);
                }
                meta.group_id = group;
                self.store.update_chat(chat_id, &meta)?;
                self.events.push(Event::Chat(chat_id));
                self.flush(peer)
            }
            Frame::Mls { id, bytes } => {
                if meta.group_id.is_empty() {
                    // Sent under a group we no longer have; it's being
                    // rebuilt. Unacknowledged, so it's not marked delivered.
                    return Ok(Vec::new());
                }
                // A resend after our acknowledgement got lost.
                if self.store.is_seen(&id)? {
                    return Ok(vec![Frame::Ack { id }]);
                }
                match self.mls.receive(&meta.group_id, &bytes) {
                    Ok(Received::Message { sender, plaintext }) if sender == *peer => {
                        self.mls.persist(&self.store)?;
                        if let Some(content) = Content::decode(&plaintext) {
                            self.deliver(chat_id, peer, id, content)?;
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
            _ => Err(SessionError::Protocol),
        }
    }

    /// The friend (or group member) has it. A group message counts as
    /// delivered once every member has it.
    fn on_ack(&mut self, id: MessageId) -> Result<()> {
        self.call_delivered(&id);
        if let Some(item) = self.store.outbox_remove(&id)? {
            if let Some((chat, seq)) = item.message {
                if !self.store.outbox_has_message(chat, seq)? {
                    self.store.set_delivered(chat, seq)?;
                }
                self.events.push(Event::Chat(chat));
            }
        } else if let Some(i) = self.temp_out.iter().position(|o| o.id == id) {
            self.temp_out.remove(i);
            if let Some(temp) = self.temp.mark_delivered(&id) {
                self.events.push(Event::Temp(temp));
            }
        }
        Ok(())
    }

    /// Applies a friend's message.
    fn deliver(
        &mut self,
        chat_id: i64,
        peer: &[u8; 32],
        id: MessageId,
        content: Content,
    ) -> Result<()> {
        let name = self.friend_name(peer);
        self.purge();
        match content {
            Content::Text { .. } | Content::Reply { .. } => {
                let (body, sent_at, reply) = match content {
                    Content::Text { body, sent_at } => (body, sent_at, None),
                    Content::Reply {
                        body,
                        sent_at,
                        mut quote,
                    } => {
                        quote.snippet = clip(&quote.snippet, 140);
                        (body, sent_at, Some(quote))
                    }
                    _ => unreachable!(),
                };
                self.add_received(
                    chat_id,
                    StoredMessage {
                        from_me: false,
                        body,
                        sent_at: sent_at.min(now()),
                        delivered: true,
                        id: Some(id),
                        reply,
                        sender: Some(*peer),
                        media: None,
                        ..Default::default()
                    },
                )?;
                self.events.push(Event::Chat(chat_id));
                self.events.push(Event::Message(chat_id));
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
                self.events.push(Event::TempMessage(id));
            }
            // Group content belongs in group frames, not a pair's chat.
            Content::HistoryBatch { group, items } => self.on_history(peer, group, items)?,
            Content::GroupText { .. }
            | Content::GroupInfo(_)
            | Content::GroupLeave
            | Content::GroupRoles { .. }
            | Content::GroupRequest(_)
            | Content::HistoryShared { .. } => {}
            Content::DeleteMessage { id } => {
                // Only their own messages.
                if let Some((seq, m)) = self.store.message_by_id(chat_id, &id)? {
                    if !m.from_me {
                        if let Some(media) = &m.media {
                            self.delete_blobs(&[media.blob]);
                        }
                        self.store.delete_message(chat_id, seq)?;
                        self.events.push(Event::Chat(chat_id));
                    }
                }
            }
            Content::ClearChat => {
                self.delete_blobs(&self.store.blobs_in(chat_id)?);
                self.store.clear_messages(chat_id)?;
                self.events.push(Event::Chat(chat_id));
            }
            Content::Unfriend => {
                self.delete_chat(chat_id)?;
                self.events.push(Event::Chat(chat_id));
            }
            Content::NearbyKey(key) => self.on_nearby_key(peer, key)?,
            Content::Media {
                id,
                body,
                sent_at,
                quote,
                media,
            } => self.receive_media(chat_id, peer, id, body, sent_at, quote, media, None)?,
            Content::AlbumMedia {
                id,
                body,
                sent_at,
                media,
                album,
            } => self.receive_media(chat_id, peer, id, body, sent_at, None, media, Some(album))?,
            Content::Poll { id, sent_at, poll } => self.on_poll(chat_id, peer, id, sent_at, poll)?,
            Content::Vote { poll, choices, at } => self.on_vote(chat_id, peer, poll, choices, at)?,
            Content::Moment {
                id,
                sent_at,
                caption,
                media,
            } => self.on_moment(peer, id, sent_at, caption, media)?,
            Content::MomentGone { id } => self.on_moment_gone(peer, id)?,
            Content::React { id, emoji, at } => self.on_react(chat_id, peer, id, emoji, at)?,
            Content::CallInvite(offer) => self.on_call_invite(chat_id, peer, offer)?,
            Content::CallAnswer(answer) => self.on_call_answer(peer, answer)?,
            Content::CallEnd { call, reason } => self.on_call_end(peer, call, reason)?,
            Content::TempWipe { temp } => {
                if let Some(id) = self.temp.wipe_remote(*peer, temp) {
                    self.forget_temp_outbox(Some(*peer), temp);
                    self.events.push(Event::TempGone(id));
                }
            }
        }
        Ok(())
    }

    // ---- Deleting ----------------------------------------------------

    /// Deletes one message, on this phone or (our own messages) on every
    /// phone in the chat. Returns who to reach.
    pub fn delete_message(&mut self, chat_id: i64, seq: i64, for_everyone: bool) -> Result<Vec<[u8; 32]>> {
        let Some(message) = self.store.message(chat_id, seq)? else {
            return Ok(Vec::new());
        };
        self.store.delete_message(chat_id, seq)?;
        if let Some(media) = &message.media {
            self.delete_blobs(&[media.blob]);
        }
        self.events.push(Event::Chat(chat_id));
        match (for_everyone && message.from_me, message.id) {
            (true, Some(id)) => self.tell_chat(chat_id, Content::DeleteMessage { id }),
            _ => Ok(Vec::new()),
        }
    }

    /// Clears a chat's messages, here or on both phones (friends' chats).
    pub fn clear_chat(&mut self, chat_id: i64, for_everyone: bool) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        self.delete_blobs(&self.store.blobs_in(chat_id)?);
        self.store.clear_messages(chat_id)?;
        self.events.push(Event::Chat(chat_id));
        if for_everyone && meta.kind == ChatKind::Direct {
            return self.tell_chat(chat_id, Content::ClearChat);
        }
        Ok(Vec::new())
    }

    /// Removes a friend: from this phone, or from both (their phone deletes
    /// its chat with us too). Returns who to reach.
    pub fn remove_friend(&mut self, chat_id: i64, both_phones: bool) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        if meta.kind != ChatKind::Direct {
            return Err(SessionError::NoSuchChat);
        }
        let peer = meta.peers.first().map(|p| p.identity);
        // The goodbye is encrypted now: once the chat is gone, the pair's
        // group can't be looked up for it.
        let goodbye = match (both_phones, meta.group_id.is_empty()) {
            (true, false) => Some(self.mls.encrypt(&meta.group_id, &Content::Unfriend.encode())?),
            _ => None,
        };
        self.mls.persist(&self.store)?;
        self.delete_chat(chat_id)?;
        let (Some(peer), Some(cipher)) = (peer, goodbye) else {
            return Ok(Vec::new());
        };
        self.store.outbox_put(
            &rand::random(),
            &OutboxItem {
                peer,
                state: OutboxState::Sealed { plain: Vec::new(), cipher },
                message: None,
            },
        )?;
        Ok(vec![peer])
    }

    /// Queues `content` for the chat's other phones.
    pub(crate) fn tell_chat(&mut self, chat_id: i64, content: Content) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        match meta.kind {
            ChatKind::Group if !meta.group_id.is_empty() => self.broadcast_content(&meta, &content),
            ChatKind::Direct => {
                let Some(peer) = meta.peers.first().map(|p| p.identity) else {
                    return Ok(Vec::new());
                };
                self.store.outbox_put(
                    &rand::random(),
                    &OutboxItem {
                        peer,
                        state: OutboxState::Plain(content.encode()),
                        message: None,
                    },
                )?;
                Ok(vec![peer])
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Deletes a chat, its messages and anything still waiting to be sent
    /// in it. Deleting a friend's chat removes the friend: they can no
    /// longer reach this phone until you pair again.
    pub fn delete_chat(&mut self, chat_id: i64) -> Result<()> {
        if let Ok(blobs) = self.store.blobs_in(chat_id) {
            self.delete_blobs(&blobs);
        }
        if let Ok(meta) = self.chat(chat_id) {
            for peer in meta.peers.iter().filter(|_| meta.kind == ChatKind::Direct) {
                self.forget_nearby_key(&peer.identity)?;
            }
            for peer in &meta.peers {
                for (id, _) in self.store.outbox_for(&peer.identity)? {
                    self.store.outbox_remove(&id)?;
                }
            }
            if meta.kind == ChatKind::Group && !meta.group_id.is_empty() {
                self.sync_forget(&meta.group_id)?;
            }
        }
        self.store.delete_chat(chat_id)?;
        Ok(())
    }

    /// The pair's group is being replaced: messages already encrypted for
    /// the old one go back to plaintext, to be encrypted again for the new
    /// group. (Ciphertext from older builds can't be; it's dropped, and the
    /// message stays undelivered.)
    fn drop_stale_ciphers(&mut self, peer: &[u8; 32]) -> Result<()> {
        for (id, mut item) in self.store.outbox_for(peer)? {
            match item.state {
                OutboxState::Plain(_) => {}
                OutboxState::Sealed { plain, .. } => {
                    item.state = OutboxState::Plain(plain);
                    self.store.outbox_put(&id, &item)?;
                }
                OutboxState::Cipher(_) => {
                    self.store.outbox_remove(&id)?;
                }
                // Other groups' messages, and files (encrypted with their own
                // keys), are unaffected.
                OutboxState::Group { .. } | OutboxState::Frame(_) | OutboxState::Blob { .. } => {}
            }
        }
        for out in self.temp_out.iter_mut().filter(|o| &o.peer == peer) {
            out.cipher = None;
        }
        Ok(())
    }

    /// Everything waiting for `peer`, encrypted for the pair's group, then
    /// (for a member whose phone swaps lists) what we have of the groups we
    /// share and what we owe them there (see `group_sync`).
    pub(crate) fn flush(&mut self, peer: &[u8; 32]) -> Result<Vec<Frame>> {
        let mut offers = Offers::default();
        let mut frames = self.flush_outbox(peer, &mut offers)?;
        frames.extend(self.sync_frames(peer, offers)?);
        Ok(frames)
    }

    fn flush_outbox(&mut self, peer: &[u8; 32], offers: &mut Offers) -> Result<Vec<Frame>> {
        // The pair's group, if set up: needed for direct messages.
        let pair_group = self
            .direct_chat(peer)?
            .map(|(_, meta)| meta.group_id)
            .filter(|g| !g.is_empty());
        let mut frames = Vec::new();
        for (id, mut item) in self.store.outbox_for(peer)? {
            match &item.state {
                // Group messages and welcomes go out as they are (or are
                // offered first, if the member may have them already).
                OutboxState::Group { group, cipher } => {
                    if self.sync_send_message(peer, &id, group, cipher, offers)? {
                        frames.push(Frame::GroupMls {
                            group: group.clone(),
                            id,
                            bytes: cipher.clone(),
                        });
                    }
                    continue;
                }
                OutboxState::Frame(bytes) => {
                    frames.extend(Frame::from_bytes(bytes));
                    continue;
                }
                OutboxState::Blob { blob, .. } => {
                    let blob = *blob;
                    if self.sync_send_blob(peer, &id, &blob, item.message, offers)? {
                        frames.extend(self.blob_frames(peer, id, &blob)?);
                    }
                    continue;
                }
                _ => {}
            }
            // Already encrypted: goes out as is.
            if let OutboxState::Sealed { cipher: bytes, .. } | OutboxState::Cipher(bytes) = &item.state {
                frames.push(Frame::Mls { id, bytes: bytes.clone() });
                continue;
            }
            let Some(group) = &pair_group else { continue };
            if let OutboxState::Plain(plain) = &item.state {
                let cipher = self.mls.encrypt(group, plain)?;
                item.state = OutboxState::Sealed { plain: plain.clone(), cipher };
                self.mls.persist(&self.store)?;
                self.store.outbox_put(&id, &item)?;
            }
            match item.state {
                OutboxState::Cipher(bytes) | OutboxState::Sealed { cipher: bytes, .. } => {
                    frames.push(Frame::Mls { id, bytes });
                }
                OutboxState::Plain(_) | OutboxState::Group { .. } | OutboxState::Frame(_) | OutboxState::Blob { .. } => {}
            }
        }
        let Some(group) = pair_group else {
            return Ok(frames);
        };
        for out in self.temp_out.iter_mut().filter(|o| &o.peer == peer) {
            if out.cipher.is_none() {
                out.cipher = Some(self.mls.encrypt(&group, &out.content.encode())?);
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

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub(crate) fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;

    pub(crate) struct Phone {
        pub(crate) s: Session,
        secret: [u8; 32],
        key: [u8; 32],
        pub(crate) dir: TempDir,
        /// What its build can do: 0 plays an older build, which never
        /// swaps lists of group messages.
        pub(crate) features: u32,
    }

    impl Phone {
        pub(crate) fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let secret = Identity::generate().secret();
            let key = rand::random();
            let s = Session::open(secret, dir.path().join("s.db"), &key).unwrap();
            Self {
                s,
                secret,
                key,
                dir,
                features: crate::protocol::FEATURES,
            }
        }

        pub(crate) fn pk(&self) -> [u8; 32] {
            self.s.public_key()
        }

        /// A restart: everything in memory (including temp chats) is gone.
        pub(crate) fn restart(&mut self) {
            self.s = Session::open(self.secret, self.dir.path().join("s.db"), &self.key).unwrap();
        }

        pub(crate) fn texts(&self, chat: i64) -> Vec<(bool, String, bool)> {
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
    pub(crate) fn befriend(a: &mut Phone, b: &mut Phone) -> (i64, i64) {
        let ab = a.s.add_friend(&b.s.my_code("Bob"), true).unwrap().chat_id;
        let ba = b.s.add_friend(&a.s.my_code("Alice"), true).unwrap().chat_id;
        (ab, ba)
    }

    /// `dialer` connects to `reached`: handshake, then frames flow both ways
    /// until both sides are quiet. Returns how many frames were exchanged.
    pub(crate) fn connect(dialer: &mut Phone, reached: &mut Phone) -> usize {
        link(dialer, reached).frames
    }

    /// What went over one connection.
    #[derive(Default)]
    pub(crate) struct Traffic {
        pub(crate) frames: usize,
        /// Bytes each side sent, as framed on the wire.
        pub(crate) dialer_sent: usize,
        pub(crate) reached_sent: usize,
        /// Every frame each side sent.
        pub(crate) from_dialer: Vec<Frame>,
        pub(crate) from_reached: Vec<Frame>,
    }

    /// Like [`connect`], counting what each side sent.
    pub(crate) fn link(dialer: &mut Phone, reached: &mut Phone) -> Traffic {
        use crate::protocol::encode_frame;
        let (nonce, challenge) = reached.s.challenge();
        let hello = dialer.s.hello(&reached.pk(), &challenge).unwrap();
        let who = reached.s.accept_hello(&nonce, &hello).unwrap();
        assert_eq!(who, dialer.pk());
        let (dpk, rpk) = (dialer.pk(), reached.pk());
        // Each side learns what the other's build can do (an older build
        // tells nothing, and ignores what it's told).
        let features = dialer.features & reached.features;
        dialer.s.link_started(&rpk);
        dialer.s.link_features(&rpk, features).unwrap();
        reached.s.link_started(&dpk);
        reached.s.link_features(&dpk, features).unwrap();
        let mut to_reached = dialer.s.on_link_up(&rpk).unwrap();
        let mut to_dialer = reached.s.on_link_up(&dpk).unwrap();
        let mut t = Traffic::default();
        while !to_reached.is_empty() || !to_dialer.is_empty() {
            let mut next_to_dialer = Vec::new();
            for f in to_reached.drain(..) {
                t.frames += 1;
                t.dialer_sent += encode_frame(&f).len();
                t.from_dialer.push(f.clone());
                next_to_dialer.extend(reached.s.on_frame(&dpk, f).unwrap());
            }
            let mut next_to_reached = Vec::new();
            for f in to_dialer.drain(..) {
                t.frames += 1;
                t.reached_sent += encode_frame(&f).len();
                t.from_reached.push(f.clone());
                next_to_reached.extend(dialer.s.on_frame(&rpk, f).unwrap());
            }
            to_dialer.extend(next_to_dialer);
            to_reached.extend(next_to_reached);
            assert!(t.frames < 20_000, "frames never settle");
        }
        t
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
        let events = bob.s.take_events();
        assert!(events.contains(&Event::Chat(ba)) && events.contains(&Event::Message(ba)));
        assert!(!alice.s.take_events().iter().any(|e| matches!(e, Event::Message(_))), "a receipt is no message");
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
    fn deleting_a_friend_and_pairing_again_rebuilds_encryption() {
        for initiator_forgets in [true, false] {
            let (mut alice, mut bob) = (Phone::new(), Phone::new());
            let (ab, ba) = befriend(&mut alice, &mut bob);
            connect(&mut alice, &mut bob);
            let swap = (alice.pk() < bob.pk()) != initiator_forgets;
            let (forgets, keeps, forgets_chat, keeps_chat) = if swap {
                (&mut bob, &mut alice, ba, ab)
            } else {
                (&mut alice, &mut bob, ab, ba)
            };
            // Encrypted for the old group but never sent.
            keeps.s.send_text(keeps_chat, "lost").unwrap();
            keeps.s.flush(&forgets.pk()).unwrap();

            forgets.s.delete_chat(forgets_chat).unwrap();
            assert!(forgets.s.friends().unwrap().is_empty());
            let code = keeps.s.my_code("Them");
            let forgets_chat = forgets.s.add_friend(&code, true).unwrap().chat_id;
            connect(forgets, keeps);

            forgets.s.send_text(forgets_chat, "back").unwrap();
            keeps.s.send_text(keeps_chat, "welcome back").unwrap();
            connect(keeps, forgets);
            assert_eq!(keeps.texts(keeps_chat).last().unwrap(), &(false, "back".into(), true));
            assert_eq!(
                forgets.texts(forgets_chat),
                [
                    (false, "lost".into(), true),
                    (true, "back".into(), true),
                    (false, "welcome back".into(), true)
                ]
            );
            // Encrypted for the old group, re-encrypted for the new one.
            assert!(keeps.texts(keeps_chat).contains(&(true, "lost".into(), true)));
        }
    }

    /// `dialer` reaches a phone that may not know it yet; returns whether
    /// it was a stranger (and its friend request was handed over).
    fn reach(dialer: &mut Phone, reached: &mut Phone) -> bool {
        let (nonce, challenge) = reached.s.challenge();
        let hello = dialer.s.hello(&reached.pk(), &challenge).unwrap();
        let who = reached.s.accept_hello(&nonce, &hello).unwrap();
        if reached.s.is_friend(&who).unwrap() {
            connect(dialer, reached);
            return false;
        }
        let first = dialer.s.on_link_up(&reached.pk()).unwrap().remove(0);
        reached.s.on_stranger_frame(&who, first).unwrap();
        true
    }

    #[test]
    fn adding_one_code_sends_a_friend_request() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        alice.s.set_my_name("Alice");
        // Only Alice enters a code.
        let ab = alice.s.add_friend(&bob.s.my_code("Bob"), false).unwrap().chat_id;
        assert_eq!(alice.s.pending_peers().unwrap(), vec![bob.pk()], "keeps asking");

        assert!(reach(&mut alice, &mut bob));
        assert!(reach(&mut alice, &mut bob), "asking again doesn't duplicate");
        let requests = bob.s.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!((requests[0].identity, requests[0].name.as_str()), (alice.pk(), "Alice"));
        assert!(bob.s.take_events().contains(&Event::Request));
        assert!(bob.s.friends().unwrap().is_empty());

        let paired = bob.s.accept_request(&alice.pk()).unwrap();
        assert!(!paired.verified);
        assert!(bob.s.requests().is_empty());
        // Bob's phone now reaches out to set up encryption.
        assert_eq!(bob.s.pending_peers().unwrap(), vec![alice.pk()]);
        assert!(!reach(&mut bob, &mut alice));
        alice.s.send_text(ab, "you got my request").unwrap();
        connect(&mut alice, &mut bob);
        assert_eq!(bob.texts(paired.chat_id), [(false, "you got my request".into(), true)]);
        assert_eq!(
            alice.s.safety_number(ab).unwrap(),
            bob.s.safety_number(paired.chat_id).unwrap()
        );
        assert!(alice.s.pending_peers().unwrap().is_empty());
        assert!(bob.s.pending_peers().unwrap().is_empty());
    }

    #[test]
    fn declined_requests_stay_away_until_restart() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        alice.s.add_friend(&bob.s.my_code("Bob"), false).unwrap();
        reach(&mut alice, &mut bob);
        let request = bob.s.requests()[0].clone();
        assert_eq!(request.name, "Someone", "no name set");
        bob.s.decline_request(&alice.pk());
        reach(&mut alice, &mut bob);
        assert!(bob.s.requests().is_empty());
        assert!(bob.s.accept_request(&alice.pk()).is_err());
        bob.restart();
        reach(&mut alice, &mut bob);
        assert_eq!(bob.s.requests().len(), 1);
    }

    #[test]
    fn friend_requests_are_capped() {
        let mut bob = Phone::new();
        for _ in 0..MAX_REQUESTS + 5 {
            let mut eve = Phone::new();
            eve.s.set_my_name(&"e".repeat(500));
            eve.s.add_friend(&bob.s.my_code("Bob"), false).unwrap();
            reach(&mut eve, &mut bob);
        }
        let requests = bob.s.requests();
        assert_eq!(requests.len(), MAX_REQUESTS);
        assert!(requests.iter().all(|r| r.name.chars().count() == MAX_NAME));
    }

    #[test]
    fn replies_quote_the_message_they_answer() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        alice.s.send_text(ab, "dinner at 8?").unwrap();
        connect(&mut alice, &mut bob);
        // Bob answers Alice's message (seq 0 in his chat).
        bob.s.send_message(ba, "yes!", Some(0)).unwrap();
        // And Alice answers her own.
        alice.s.send_message(ab, "or 9", Some(0)).unwrap();
        connect(&mut bob, &mut alice);

        let original = alice.s.store.message(ab, 0).unwrap().unwrap();
        let on_bob = bob.s.store.message(ba, 0).unwrap().unwrap();
        assert!(original.id.is_some() && original.id == on_bob.id, "same id on both phones");
        assert_eq!(on_bob.sender, Some(alice.pk()));

        let msgs = alice.s.store.messages(ab, None).unwrap();
        let yes = msgs.iter().find(|m| m.body == "yes!").unwrap();
        let quote = yes.reply.as_ref().unwrap();
        assert_eq!((quote.id, quote.author, quote.snippet.as_str()), (original.id.unwrap(), alice.pk(), "dinner at 8?"));
        let msgs = bob.s.store.messages(ba, None).unwrap();
        let or9 = msgs.iter().find(|m| m.body == "or 9").unwrap();
        assert_eq!(or9.reply.as_ref().unwrap().author, alice.pk());
    }

    /// The group chat on `phone` with this title.
    pub(crate) fn group_of(phone: &Phone, title: &str) -> Option<i64> {
        phone
            .s
            .store
            .chats()
            .unwrap()
            .into_iter()
            .find(|(_, m)| m.kind == ChatKind::Group && m.title == title)
            .map(|(id, _)| id)
    }

    fn names(phone: &Phone, chat: i64) -> Vec<String> {
        let mut names: Vec<String> = phone.s.group_view(chat).unwrap().members.into_iter().map(|m| m.name).collect();
        names.sort();
        names
    }

    #[test]
    fn groups_work_without_a_server() {
        let (mut alice, mut bob, mut carol) = (Phone::new(), Phone::new(), Phone::new());
        alice.s.set_my_name("Alice");
        bob.s.set_my_name("Bob");
        carol.s.set_my_name("Carol");
        // Alice is friends with both; Bob and Carol don't know each other.
        let (ab, _) = befriend(&mut alice, &mut bob);
        let ac = alice.s.add_friend(&carol.s.my_code("Carol"), true).unwrap().chat_id;
        carol.s.add_friend(&alice.s.my_code("Alice"), true).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);

        let (group, invitees) = alice.s.create_group("Trip", &[ab, ac]).unwrap();
        assert_eq!(invitees.len(), 2);
        assert!(alice.s.group_view(group).unwrap().members.iter().filter(|m| m.pending).count() == 2);
        assert!(alice.s.pending_peers().unwrap().contains(&bob.pk()), "keeps trying to reach invitees");

        // Each joins when Alice reaches them.
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        connect(&mut alice, &mut bob); // Bob learns Carol joined
        let bg = group_of(&bob, "Trip").expect("Bob joined");
        let cg = group_of(&carol, "Trip").expect("Carol joined");
        assert_eq!(names(&bob, bg), ["Alice", "Carol", "You"]);
        assert_eq!(names(&carol, cg), ["Alice", "Bob", "You"]);
        assert!(alice.s.group_view(group).unwrap().members.iter().all(|m| !m.pending));
        assert!(!bob.s.group_view(bg).unwrap().admin_is_me);

        // Bob writes; Carol gets it straight from Bob, though they aren't friends.
        let reach = bob.s.send_to_chat(bg, "who's driving?", None).unwrap();
        assert_eq!(reach.len(), 2);
        assert!(carol.s.knows(&bob.pk()).unwrap() && !carol.s.is_friend(&bob.pk()).unwrap());
        connect(&mut bob, &mut carol);
        assert!(!bob.texts(bg)[0].2, "delivered only once everyone has it");
        connect(&mut bob, &mut alice);
        assert!(bob.texts(bg)[0].2);
        for (phone, chat) in [(&carol, cg), (&alice, group)] {
            let m = phone.s.store.messages(chat, None).unwrap().pop().unwrap();
            assert_eq!((m.body.as_str(), m.sender), ("who's driving?", Some(bob.pk())));
        }
        // Replies work in groups too.
        carol.s.send_to_chat(cg, "me", Some(0)).unwrap();
        connect(&mut carol, &mut alice);
        let reply = alice.s.store.messages(group, None).unwrap().pop().unwrap();
        assert_eq!(reply.reply.unwrap().author, bob.pk());

        // Only the admin manages the group.
        assert!(matches!(bob.s.group_rename(bg, "Mine"), Err(SessionError::NotAdmin)));
        alice.s.group_rename(group, "Road trip").unwrap();
        connect(&mut alice, &mut bob);
        assert!(group_of(&bob, "Road trip").is_some());

        // Carol leaves; Alice removes her for everyone.
        carol.s.group_leave(cg).unwrap();
        assert!(group_of(&carol, "Trip").is_none());
        connect(&mut carol, &mut alice);
        connect(&mut alice, &mut bob);
        assert_eq!(names(&bob, bg), ["Alice", "You"]);
        assert_eq!(names(&alice, group), ["Bob", "You"]);
        // Messages now reach Bob only.
        assert_eq!(alice.s.send_to_chat(group, "just us", None).unwrap(), vec![bob.pk()]);

        // Removed by the admin: Bob's copy goes quiet.
        alice.s.group_remove(group, &bob.pk()).unwrap();
        connect(&mut alice, &mut bob);
        let view = bob.s.group_view(bg).unwrap();
        assert!(!view.active);
        assert!(bob.s.send_to_chat(bg, "hello?", None).is_err());
    }

    /// Alice leads "Crew" with Bob and Carol in it; Dave is her friend (and
    /// Bob's) but not in the group.
    fn crew() -> (Phone, Phone, Phone, Phone, i64, [i64; 4]) {
        let (mut alice, mut bob, mut carol, mut dave) = (Phone::new(), Phone::new(), Phone::new(), Phone::new());
        for (phone, name) in [(&mut alice, "Alice"), (&mut bob, "Bob"), (&mut carol, "Carol"), (&mut dave, "Dave")] {
            phone.s.set_my_name(name);
        }
        let friends = |a: &mut Phone, an: &str, b: &mut Phone, bn: &str| {
            let ab = a.s.add_friend(&b.s.my_code(bn), true).unwrap().chat_id;
            b.s.add_friend(&a.s.my_code(an), true).unwrap();
            ab
        };
        let ab = friends(&mut alice, "Alice", &mut bob, "Bob");
        let ac = friends(&mut alice, "Alice", &mut carol, "Carol");
        let ad = friends(&mut alice, "Alice", &mut dave, "Dave");
        let bd = friends(&mut bob, "Bob", &mut dave, "Dave");
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        connect(&mut alice, &mut dave);
        let (group, _) = alice.s.create_group("Crew", &[ab, ac]).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        connect(&mut alice, &mut bob);
        (alice, bob, carol, dave, group, [ab, ac, ad, bd])
    }

    fn role_of(phone: &Phone, chat: i64, who: [u8; 32]) -> (bool, bool) {
        let m = phone.s.group_view(chat).unwrap().members.into_iter().find(|m| m.identity == who).unwrap();
        (m.admin, m.leader)
    }

    #[test]
    fn the_leader_makes_admins_who_ask_the_leader() {
        let (mut alice, mut bob, mut carol, mut dave, group, [_, _, _, bd]) = crew();
        let (bg, cg) = (group_of(&bob, "Crew").unwrap(), group_of(&carol, "Crew").unwrap());
        assert_eq!(role_of(&bob, bg, alice.pk()), (true, true), "the creator leads");
        assert!(alice.s.group_view(group).unwrap().i_am_admin);

        // Only joined friends can be admins, and a plain member can't add anyone.
        assert!(matches!(alice.s.group_make_admin(group, &dave.pk()), Err(SessionError::GroupRule(_))));
        assert!(matches!(bob.s.group_make_admin(bg, &carol.pk()), Err(SessionError::NotAdmin)));
        assert!(matches!(bob.s.group_add(bg, &[bd], false), Err(SessionError::NotAdmin)));

        alice.s.group_make_admin(group, &bob.pk()).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        assert!(bob.s.group_view(bg).unwrap().i_am_admin && !bob.s.group_view(bg).unwrap().admin_is_me);
        assert_eq!(role_of(&carol, cg, bob.pk()), (true, false), "everyone sees who the admins are");

        // Bob asks for Dave to be added; Alice's phone does it.
        bob.s.group_add(bg, &[bd], false).unwrap();
        connect(&mut bob, &mut alice);
        connect(&mut alice, &mut dave);
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        assert!(group_of(&dave, "Crew").is_some(), "Dave joined");
        assert_eq!(names(&carol, cg), ["Alice", "Bob", "Dave", "You"]);

        // Bob renames the group and removes Carol the same way.
        bob.s.group_rename(bg, "Crew 2").unwrap();
        bob.s.group_remove(bg, &carol.pk()).unwrap();
        connect(&mut bob, &mut alice);
        connect(&mut alice, &mut carol);
        connect(&mut alice, &mut bob);
        assert!(group_of(&alice, "Crew 2").is_some() && group_of(&bob, "Crew 2").is_some());
        assert!(!carol.s.group_view(cg).unwrap().active);
        assert_eq!(names(&alice, group), ["Bob", "Dave", "You"]);

        // Nobody can take the leader's place, and an admin can't remove one.
        assert!(matches!(bob.s.group_remove_admin(bg, &alice.pk()), Err(SessionError::GroupRule(_))));
        assert!(matches!(bob.s.group_remove(bg, &alice.pk()), Err(SessionError::GroupRule(_))));
        assert!(matches!(alice.s.group_remove_admin(group, &alice.pk()), Err(SessionError::GroupRule(_))));
        assert!(matches!(alice.s.group_remove(group, &alice.pk()), Err(SessionError::GroupRule(_))));

        // The leader takes Bob's rights back.
        alice.s.group_remove_admin(group, &bob.pk()).unwrap();
        connect(&mut alice, &mut bob);
        assert!(!bob.s.group_view(bg).unwrap().i_am_admin);
        assert!(matches!(bob.s.group_rename(bg, "Mine"), Err(SessionError::NotAdmin)));
    }

    #[test]
    fn a_stale_or_forged_roles_message_changes_nothing() {
        let (mut alice, mut bob, mut carol, _dave, group, _) = crew();
        let (bg, cg) = (group_of(&bob, "Crew").unwrap(), group_of(&carol, "Crew").unwrap());
        alice.s.group_make_admin(group, &bob.pk()).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        // Carol (not the leader) claims to make herself leader: ignored by Bob.
        let (_, mut meta) = bob.s.group_chat(&alice.s.chat(group).unwrap().group_id).unwrap().unwrap();
        let mut deliver = |from: [u8; 32], content| bob.s.deliver_group(bg, &mut meta, &from, content).unwrap();
        deliver(carol.pk(), Content::GroupRoles { leader: carol.pk(), admins: vec![], at: u64::MAX });
        // An older list from the real leader than the one already held: ignored.
        deliver(alice.pk(), Content::GroupRoles { leader: alice.pk(), admins: vec![], at: 1 });
        assert_eq!(role_of(&bob, bg, alice.pk()), (true, true));
        assert_eq!(role_of(&bob, bg, bob.pk()), (true, false));
        assert_eq!(role_of(&bob, bg, carol.pk()), (false, false));
        let _ = cg;
    }

    #[test]
    fn leadership_is_handed_over_not_dropped() {
        let (mut alice, mut bob, mut carol, _dave, group, [_, ac, _, _]) = crew();
        let (bg, cg) = (group_of(&bob, "Crew").unwrap(), group_of(&carol, "Crew").unwrap());
        let _ = ac;
        // A leader with admins can't just walk out, and only admins can take over.
        assert!(matches!(alice.s.group_hand_over(group, &bob.pk()), Err(SessionError::GroupRule(_))));
        alice.s.group_make_admin(group, &bob.pk()).unwrap();
        assert!(matches!(alice.s.group_leave(group), Err(SessionError::GroupRule(_))));
        alice.s.group_hand_over(group, &bob.pk()).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        connect(&mut bob, &mut carol);
        assert_eq!(role_of(&carol, cg, bob.pk()), (true, true));
        assert_eq!(role_of(&carol, cg, alice.pk()), (true, false), "the old leader stays an admin");
        assert!(bob.s.group_view(bg).unwrap().admin_is_me);

        // Bob now changes the group directly; Alice has to ask.
        bob.s.group_rename(bg, "Bob's crew").unwrap();
        connect(&mut bob, &mut alice);
        connect(&mut bob, &mut carol);
        assert!(group_of(&alice, "Bob's crew").is_some() && group_of(&carol, "Bob's crew").is_some());
        alice.s.group_rename(group, "Alice again").unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut bob, &mut carol);
        assert!(group_of(&carol, "Alice again").is_some());

        // Alice can leave now, and Bob (the leader) removes her for everyone.
        alice.s.group_leave(group).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut bob, &mut carol);
        assert_eq!(names(&bob, bg), ["Carol", "You"]);
        assert_eq!(names(&carol, cg), ["Bob", "You"]);
    }

    #[test]
    fn history_goes_to_new_members_only_if_the_leader_chooses() {
        let (mut alice, mut bob, mut carol, mut dave, group, [_, ac, ad, bd]) = crew();
        let (bg, cg) = (group_of(&bob, "Crew").unwrap(), group_of(&carol, "Crew").unwrap());
        let _ = (ac, bd);
        alice.s.send_to_chat(group, "meet at 6", None).unwrap();
        bob.s.send_to_chat(bg, "I'll bring snacks", None).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        connect(&mut bob, &mut carol);

        // Dave is added with the history.
        alice.s.group_add(group, &[ad], true).unwrap();
        connect(&mut alice, &mut dave);
        connect(&mut alice, &mut dave);
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        let dg = group_of(&dave, "Crew").expect("Dave joined");
        let msgs = dave.s.store.messages(dg, None).unwrap();
        let said: Vec<_> = msgs.iter().filter(|m| !m.notice).map(|m| (m.body.as_str(), m.sender, m.shared_by)).collect();
        assert_eq!(
            said,
            [
                ("meet at 6", Some(alice.pk()), Some(alice.pk())),
                ("I'll bring snacks", Some(bob.pk()), Some(alice.pk()))
            ]
        );
        assert!(msgs.iter().all(|m| !m.from_me));
        // Everyone is told, Dave included; no copy goes to the others.
        for (phone, chat, text) in [
            (&bob, bg, "Alice shared the last week of messages with Dave."),
            (&carol, cg, "Alice shared the last week of messages with Dave."),
            (&dave, dg, "Alice shared the last week of messages with you."),
            (&alice, group, "You shared the last week of messages with Dave."),
        ] {
            let notices: Vec<_> = phone.s.store.messages(chat, None).unwrap().into_iter().filter(|m| m.notice).collect();
            assert_eq!(notices.len(), 1, "one notice");
            assert_eq!(notices[0].body, text);
        }
        assert_eq!(bob.s.store.messages(bg, None).unwrap().iter().filter(|m| m.body == "meet at 6").count(), 1);
    }

    #[test]
    fn without_the_history_choice_a_new_member_sees_nothing_old() {
        let (mut alice, mut bob, mut carol, mut dave, group, [_, _, ad, _]) = crew();
        let _ = (&mut bob, &mut carol);
        alice.s.send_to_chat(group, "secret plans", None).unwrap();
        alice.s.group_add(group, &[ad], false).unwrap();
        connect(&mut alice, &mut dave);
        connect(&mut alice, &mut dave);
        let dg = group_of(&dave, "Crew").expect("Dave joined");
        assert!(dave.s.store.messages(dg, None).unwrap().is_empty());
    }

    #[test]
    fn history_that_arrives_before_the_welcome_waits_for_it() {
        let (mut alice, mut bob, _carol, mut dave, group, [_, _, ad, _]) = crew();
        let _ = &mut bob;
        let gid = alice.s.chat(group).unwrap().group_id;
        let item = |n: u8, who: [u8; 32]| crate::protocol::HistoryItem { id: [n; 16], sender: who, sent_at: now(), body: format!("old {n}") };
        // The leader's, before Dave has joined: kept until he does.
        dave.s.on_history(&alice.pk(), gid.clone(), vec![item(1, alice.pk()), item(2, bob.pk())]).unwrap();
        assert!(group_of(&dave, "Crew").is_none());
        alice.s.group_add(group, &[ad], false).unwrap();
        connect(&mut alice, &mut dave);
        connect(&mut alice, &mut dave);
        let dg = group_of(&dave, "Crew").unwrap();
        let bodies: Vec<_> = dave.s.store.messages(dg, None).unwrap().into_iter().map(|m| m.body).collect();
        assert_eq!(bodies, ["old 1", "old 2"]);
        // Someone who isn't the leader can't add to what Dave sees.
        dave.s.on_history(&bob.pk(), gid, vec![item(3, bob.pk())]).unwrap();
        assert_eq!(dave.s.store.messages(dg, None).unwrap().len(), 2);
    }

    #[test]
    fn polls_and_plans_count_everyones_answers() {
        use crate::protocol::{Plan, Poll};
        let (mut alice, mut bob, mut carol) = (Phone::new(), Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        let ac = alice.s.add_friend(&carol.s.my_code("Carol"), true).unwrap().chat_id;
        carol.s.add_friend(&alice.s.my_code("Alice"), true).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);

        // A friends' chat: Bob answers, then changes his mind.
        let poll = Poll { question: "Dinner?".into(), options: vec!["Pizza".into(), "Sushi".into()], ..Default::default() };
        assert_eq!(alice.s.send_poll(ab, poll).unwrap(), vec![bob.pk()]);
        connect(&mut alice, &mut bob);
        let seq = bob.s.store.messages(ba, None).unwrap().len() as i64 - 1;
        assert_eq!(bob.s.store.message(ba, seq).unwrap().unwrap().poll.unwrap().question, "Dinner?");
        bob.s.vote(ba, seq, vec![0]).unwrap();
        connect(&mut bob, &mut alice);
        bob.s.vote(ba, seq, vec![1]).unwrap();
        connect(&mut bob, &mut alice);
        let mine = alice.s.store.messages(ab, None).unwrap().pop().unwrap();
        assert!(mine.delivered);
        assert_eq!(mine.votes.len(), 1);
        assert_eq!((mine.votes[0].voter, mine.votes[0].choices.clone()), (bob.pk(), vec![1]));

        // A group plan: everyone sees the same answers.
        let (group, _) = alice.s.create_group("Trip", &[ab, ac]).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        connect(&mut alice, &mut bob);
        let (bg, cg) = (group_of(&bob, "Trip").unwrap(), group_of(&carol, "Trip").unwrap());
        let plan = Poll {
            question: "Beach day".into(),
            plan: Some(Plan { when: 1_900_000_000, place: "Goa".into() }),
            ..Default::default()
        };
        bob.s.send_poll(bg, plan).unwrap();
        // Carol hears of it from Alice's answer before Bob's poll reaches her.
        connect(&mut bob, &mut alice);
        let aseq = alice.s.store.messages(group, None).unwrap().len() as i64 - 1;
        alice.s.vote(group, aseq, vec![crate::poll::PLAN_GOING]).unwrap();
        connect(&mut alice, &mut carol);
        connect(&mut bob, &mut carol);
        let cseq = carol.s.store.messages(cg, None).unwrap().len() as i64 - 1;
        carol.s.vote(cg, cseq, vec![crate::poll::PLAN_MAYBE]).unwrap();
        connect(&mut carol, &mut bob);
        connect(&mut carol, &mut alice);
        connect(&mut alice, &mut bob);
        for (phone, chat) in [(&alice, group), (&bob, bg), (&carol, cg)] {
            let m = phone.s.store.messages(chat, None).unwrap().pop().unwrap();
            let p = m.poll.unwrap();
            assert_eq!((p.options.len(), p.plan.unwrap().place.as_str()), (3, "Goa"));
            let mut votes: Vec<_> = m.votes.iter().map(|v| (v.voter, v.choices.clone())).collect();
            votes.sort();
            let mut want = vec![(alice.pk(), vec![0]), (carol.pk(), vec![1])];
            want.sort();
            assert_eq!(votes, want);
        }
    }

    #[test]
    fn strangers_still_cant_connect() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        alice.s.create_group("Us", &[ab]).unwrap();
        let eve = Phone::new();
        assert!(!alice.s.knows(&eve.pk()).unwrap());
        // A friend who isn't in any group of ours is still just a friend.
        assert!(alice.s.knows(&bob.pk()).unwrap());
    }

    #[test]
    fn deleting_messages_for_me_and_for_everyone() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        alice.s.send_text(ab, "oops").unwrap();
        alice.s.send_text(ab, "keep").unwrap();
        bob.s.send_text(ba, "mine").unwrap();
        connect(&mut alice, &mut bob);
        // Alice takes back "oops" everywhere.
        assert_eq!(alice.s.delete_message(ab, 0, true).unwrap(), vec![bob.pk()]);
        connect(&mut alice, &mut bob);
        assert!(bob.texts(ba).iter().all(|m| m.1 != "oops"));
        assert!(alice.texts(ab).iter().all(|m| m.1 != "oops"));
        // Bob can't delete Alice's message on her phone, only on his.
        let keep = bob.s.store.messages_indexed(ba, None).unwrap().into_iter().find(|(_, m)| m.body == "keep").unwrap().0;
        assert!(bob.s.delete_message(ba, keep, true).unwrap().is_empty());
        connect(&mut bob, &mut alice);
        assert!(alice.texts(ab).iter().any(|m| m.1 == "keep"));
        assert!(bob.texts(ba).iter().all(|m| m.1 != "keep"));
        // New messages never reuse a deleted one's number.
        let before: Vec<i64> = alice.s.store.messages_indexed(ab, None).unwrap().iter().map(|(s, _)| *s).collect();
        alice.s.send_text(ab, "new").unwrap();
        let newest = alice.s.store.messages_indexed(ab, None).unwrap().last().unwrap().0;
        assert!(before.iter().all(|s| *s < newest));
    }

    #[test]
    fn clearing_a_chat_here_or_on_both_phones() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        for i in 0..3 {
            alice.s.send_text(ab, &format!("m{i}")).unwrap();
        }
        connect(&mut alice, &mut bob);
        bob.s.clear_chat(ba, false).unwrap();
        assert!(bob.texts(ba).is_empty() && alice.texts(ab).len() == 3, "only on Bob's phone");
        bob.s.send_text(ba, "hi again").unwrap();
        connect(&mut bob, &mut alice);
        alice.s.clear_chat(ab, true).unwrap();
        connect(&mut alice, &mut bob);
        assert!(alice.texts(ab).is_empty() && bob.texts(ba).is_empty());
        // Still friends.
        alice.s.send_text(ab, "after").unwrap();
        connect(&mut alice, &mut bob);
        assert_eq!(bob.texts(ba), [(false, "after".into(), true)]);
    }

    #[test]
    fn removing_a_friend_from_one_or_both_phones() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        // Only from Alice's phone: Bob still has her.
        alice.s.remove_friend(ab, false).unwrap();
        assert!(alice.s.friends().unwrap().is_empty());
        assert_eq!(bob.s.friends().unwrap().len(), 1);

        // Pair again, then remove on both phones.
        let (mut carol, mut dave) = (Phone::new(), Phone::new());
        let (cd, _) = befriend(&mut carol, &mut dave);
        connect(&mut carol, &mut dave);
        assert_eq!(carol.s.remove_friend(cd, true).unwrap(), vec![dave.pk()]);
        assert!(carol.s.friends().unwrap().is_empty());
        // The goodbye still reaches Dave, who deletes his chat with Carol.
        connect(&mut carol, &mut dave);
        assert!(dave.s.friends().unwrap().is_empty());
        assert!(carol.s.pending_peers().unwrap().is_empty(), "goodbye acknowledged");
        let _ = ba;
    }

    #[test]
    fn deleting_for_everyone_in_a_group() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        let (group, _) = alice.s.create_group("G", &[ab]).unwrap();
        connect(&mut alice, &mut bob);
        let bg = group_of(&bob, "G").unwrap();
        bob.s.send_to_chat(bg, "typo", None).unwrap();
        connect(&mut bob, &mut alice);
        assert_eq!(alice.texts(group).len(), 1);
        bob.s.delete_message(bg, 0, true).unwrap();
        connect(&mut bob, &mut alice);
        assert!(alice.texts(group).is_empty() && bob.texts(bg).is_empty());
    }

    /// Each person's reaction to the message at `seq`, sorted.
    fn reactions(phone: &Phone, chat: i64, seq: i64) -> Vec<([u8; 32], String)> {
        let m = phone.s.store.message(chat, seq).unwrap().unwrap();
        let mut out: Vec<_> = m.reactions.into_iter().filter(|r| !r.emoji.is_empty()).map(|r| (r.who, r.emoji)).collect();
        out.sort();
        out
    }

    #[test]
    fn reacting_replacing_and_taking_back_in_a_friends_chat() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        alice.s.send_text(ab, "pizza tonight?").unwrap();
        connect(&mut alice, &mut bob);
        bob.s.take_events();
        alice.s.take_events();

        // Bob reacts to Alice's message.
        assert_eq!(bob.s.react(ba, 0, " 👍 ").unwrap(), vec![alice.pk()]);
        assert_eq!(reactions(&bob, ba, 0), [(bob.pk(), "👍".into())]);
        connect(&mut bob, &mut alice);
        assert_eq!(reactions(&alice, ab, 0), [(bob.pk(), "👍".into())]);
        let events = alice.s.take_events();
        assert!(events.contains(&Event::Chat(ab)));
        assert!(!events.iter().any(|e| matches!(e, Event::Message(_))), "a reaction never notifies");

        // He changes his mind; Alice reacts to her own message too.
        bob.s.react(ba, 0, "❤️").unwrap();
        alice.s.react(ab, 0, "😋").unwrap();
        connect(&mut alice, &mut bob);
        let mut both = vec![(alice.pk(), "😋".to_string()), (bob.pk(), "❤️".to_string())];
        both.sort();
        assert_eq!(reactions(&alice, ab, 0), both);
        assert_eq!(reactions(&bob, ba, 0), both);
        assert_eq!(alice.s.store.message(ab, 0).unwrap().unwrap().reactions.len(), 2, "one each");

        // Taken back, on both phones.
        bob.s.react(ba, 0, "").unwrap();
        connect(&mut bob, &mut alice);
        assert_eq!(reactions(&alice, ab, 0), [(alice.pk(), "😋".into())]);
        assert_eq!(reactions(&bob, ba, 0), [(alice.pk(), "😋".into())]);
        // Not a message: nothing new in the chat.
        assert_eq!(alice.texts(ab).len(), 1);
        assert_eq!(bob.texts(ba).len(), 1);

        // Too long to be a reaction.
        assert!(bob.s.react(ba, 0, &"🔥".repeat(20)).is_err());
        assert!(bob.s.react(ba, 9, "👍").is_err(), "no such message");
    }

    #[test]
    fn reactions_to_unknown_messages_or_not_emoji_are_ignored() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        alice.s.send_text(ab, "hi").unwrap();
        connect(&mut alice, &mut bob);
        alice.s.take_events();
        // To a message Alice doesn't have: kept aside, never shown.
        alice.s.on_react(ab, &bob.pk(), [7; 16], "👍".into(), 1).unwrap();
        assert!(reactions(&alice, ab, 0).is_empty());
        assert!(alice.s.take_events().iter().all(|e| !matches!(e, Event::Chat(_))));
        // Not a reaction at all.
        let id = alice.s.store.message(ab, 0).unwrap().unwrap().id.unwrap();
        alice.s.on_react(ab, &bob.pk(), id, "a\nlong\nletter".into(), 2).unwrap();
        assert!(reactions(&alice, ab, 0).is_empty());
    }

    #[test]
    fn everyone_in_a_group_sees_reactions_even_ones_that_come_first() {
        let (mut alice, mut bob, mut carol) = (Phone::new(), Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        let ac = alice.s.add_friend(&carol.s.my_code("Carol"), true).unwrap().chat_id;
        carol.s.add_friend(&alice.s.my_code("Alice"), true).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        let (group, _) = alice.s.create_group("Trip", &[ab, ac]).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        connect(&mut alice, &mut bob);
        let (bg, cg) = (group_of(&bob, "Trip").unwrap(), group_of(&carol, "Trip").unwrap());

        // Bob writes; Alice gets it first and reacts.
        bob.s.send_to_chat(bg, "train at 6", None).unwrap();
        connect(&mut bob, &mut alice);
        let seq = alice.s.store.messages_indexed(group, None).unwrap().last().unwrap().0;
        assert_eq!(alice.s.react(group, seq, "🚆").unwrap().len(), 2);
        // Carol hears Alice's reaction before Bob's message reaches her (on
        // an older build, Alice doesn't pass Bob's message on).
        alice.features = 0;
        connect(&mut alice, &mut carol);
        alice.features = crate::protocol::FEATURES;
        assert!(carol.texts(cg).is_empty());
        connect(&mut bob, &mut carol);
        let cseq = carol.s.store.messages_indexed(cg, None).unwrap().last().unwrap().0;
        assert_eq!(reactions(&carol, cg, cseq), [(alice.pk(), "🚆".into())]);
        // Carol reacts too; everyone sees both.
        carol.s.react(cg, cseq, "👍").unwrap();
        connect(&mut carol, &mut alice);
        connect(&mut carol, &mut bob);
        connect(&mut alice, &mut bob);
        let mut want = vec![(alice.pk(), "🚆".to_string()), (carol.pk(), "👍".to_string())];
        want.sort();
        let bseq = bob.s.store.messages_indexed(bg, None).unwrap().last().unwrap().0;
        assert_eq!(reactions(&alice, group, seq), want);
        assert_eq!(reactions(&bob, bg, bseq), want);
        assert_eq!(reactions(&carol, cg, cseq), want);
    }

    #[test]
    fn typing_reaches_a_connected_friend_and_is_never_kept() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        bob.s.take_events();

        let (frame, to) = alice.s.typing(ab).unwrap().expect("a signal");
        assert_eq!(to, vec![bob.pk()]);
        assert_eq!(frame, Frame::Typing { group: Vec::new() });
        // Nothing waits to be sent, and nothing was written.
        assert!(alice.s.pending_peers().unwrap().is_empty(), "never queued");
        assert!(alice.s.store.outbox_for(&bob.pk()).unwrap().is_empty());
        // Too soon for another.
        assert!(alice.s.typing(ab).unwrap().is_none());

        // Over a live link Bob hears it: an event, no message, no answer.
        assert!(bob.s.on_frame(&alice.pk(), frame.clone()).unwrap().is_empty());
        let events = bob.s.take_events();
        assert_eq!(events, [Event::Typing { chat: ba, who: alice.pk(), name: "Alice".into() }]);
        assert!(bob.texts(ba).is_empty());
        assert!(bob.s.pending_peers().unwrap().is_empty());
        // A burst of the same signal is passed on once.
        bob.s.on_frame(&alice.pk(), frame.clone()).unwrap();
        assert!(bob.s.take_events().is_empty());

        // Notes have no one to tell.
        let notes = alice.s.store.create_chat(&ChatMeta {
            kind: ChatKind::Notes,
            title: "Notes".into(),
            sigil_key: vec![1],
            pinned: false,
            created_at: 0,
            group_id: Vec::new(),
            peers: Vec::new(),
            hidden: false,
            admin: None,
            admins: Vec::new(),
        });
        assert!(alice.s.typing(notes.unwrap()).unwrap().is_none());
    }

    #[test]
    fn typing_only_counts_from_someone_in_that_chat() {
        let (mut alice, mut bob, mut carol) = (Phone::new(), Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        alice.s.add_friend(&carol.s.my_code("Carol"), true).unwrap();
        carol.s.add_friend(&alice.s.my_code("Alice"), true).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        let (group, _) = alice.s.create_group("Trip", &[ab]).unwrap();
        connect(&mut alice, &mut bob);
        let bg = group_of(&bob, "Trip").unwrap();
        alice.s.take_events();

        // Bob types in the group: Alice sees it in the group, by his name.
        let (frame, to) = bob.s.typing(bg).unwrap().unwrap();
        assert_eq!(to, vec![alice.pk()]);
        alice.s.on_frame(&bob.pk(), frame.clone()).unwrap();
        assert_eq!(alice.s.take_events(), [Event::Typing { chat: group, who: bob.pk(), name: "Bob".into() }]);
        // Carol isn't in the group: her claim is ignored.
        alice.s.on_frame(&carol.pk(), frame).unwrap();
        assert!(alice.s.take_events().is_empty());
        // Nor is a group nobody has.
        alice.s.on_frame(&bob.pk(), Frame::Typing { group: vec![9; 16] }).unwrap();
        assert!(alice.s.take_events().is_empty());
    }

    #[test]
    fn friends_recognize_each_others_bluetooth_id_and_nobody_else_does() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        let eve = Phone::new();
        // Keys are swapped automatically once the phones have talked.
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        let minute = 29_000_000;
        let alice_id = alice.s.my_nearby_id(minute);
        assert_eq!(bob.s.nearby_friend(&alice_id, minute).unwrap(), Some(alice.pk()));
        assert_eq!(bob.s.nearby_friend(&alice_id, minute + 1).unwrap(), Some(alice.pk()), "a minute of clock skew");
        assert_eq!(bob.s.nearby_friend(&alice_id, minute + 2).unwrap(), None, "it rotates");
        assert_eq!(alice.s.nearby_friend(&bob.s.my_nearby_id(minute), minute).unwrap(), Some(bob.pk()));
        assert_eq!(bob.s.nearby_friend(&eve.s.my_nearby_id(minute), minute).unwrap(), None);
        assert_ne!(alice_id, alice.s.my_nearby_id(minute + 1));
        // Once removed, no longer recognized.
        alice.s.remove_friend(ab, false).unwrap();
        assert_eq!(alice.s.nearby_friend(&bob.s.my_nearby_id(minute), minute).unwrap(), None);
    }

    pub(crate) fn photo(phone: &Phone, len: usize) -> (std::path::PathBuf, Vec<u8>) {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
        let path = phone.dir.path().join("photo.jpg");
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }

    pub(crate) fn outgoing(path: &std::path::Path) -> crate::media::Outgoing<'_> {
        crate::media::Outgoing {
            path,
            kind: crate::protocol::MediaKind::Image,
            mime: "image/jpeg",
            name: "photo.jpg",
            caption: "sunset",
            thumb: vec![1, 2, 3],
            width: 4000,
            height: 3000,
            duration_ms: 0,
            album: None,
        }
    }

    #[test]
    fn daily_moments_reach_friends_and_go_away() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        let (path, bytes) = photo(&alice, 90_000);
        assert_eq!(alice.s.share_moment(outgoing(&path)).unwrap(), vec![bob.pk()]);
        connect(&mut alice, &mut bob);
        let seen = bob.s.moments().unwrap();
        assert_eq!(seen.len(), 1);
        let first = seen[0].clone();
        assert_eq!((first.from, first.caption.as_str(), first.seen), (Some(alice.pk()), "sunset", false));
        assert_eq!(bob.s.moment_bytes(&first.id).unwrap(), Some(bytes));
        assert!(bob.s.take_events().contains(&Event::Moments));
        bob.s.moment_seen(&first.id).unwrap();
        assert!(bob.s.moments().unwrap()[0].seen);

        // A second one replaces the first, on both phones.
        let (path, _) = photo(&alice, 500);
        alice.s.share_moment(outgoing(&path)).unwrap();
        connect(&mut alice, &mut bob);
        let now_seen = bob.s.moments().unwrap();
        assert_eq!(now_seen.len(), 1);
        assert_ne!(now_seen[0].id, first.id);
        assert_eq!(bob.s.moment_bytes(&first.id).unwrap(), None);
        assert_eq!(alice.s.moments().unwrap().len(), 1);

        // Taken down.
        alice.s.delete_my_moment().unwrap();
        connect(&mut alice, &mut bob);
        assert!(bob.s.moments().unwrap().is_empty() && alice.s.moments().unwrap().is_empty());

        // One from yesterday, arriving late, is ignored.
        let media = crate::protocol::Media { kind: crate::protocol::MediaKind::Image, ..Default::default() };
        bob.s.on_moment(&alice.pk(), [1; 16], now() - crate::moment::LIFETIME - 5, String::new(), media).unwrap();
        assert!(bob.s.moments().unwrap().is_empty());
    }

    #[test]
    fn search_and_catch_up_find_whats_new() {
        use crate::protocol::{Plan, Poll};
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        alice.s.set_my_name("Alice");
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        alice.s.send_text(ab, "Old news about the BEACH").unwrap();
        connect(&mut alice, &mut bob);
        let since = now() - 1;
        bob.s.send_text(ba, "Alice, the beach was lovely").unwrap();
        let poll = Poll { question: "Pizza or sushi?".into(), options: vec!["Pizza".into(), "Sushi".into()], ..Default::default() };
        bob.s.send_poll(ba, poll).unwrap();
        let plan = Poll {
            question: "Hike".into(),
            plan: Some(Plan { when: now() + 86_400, place: "Hills".into() }),
            ..Default::default()
        };
        bob.s.send_poll(ba, plan).unwrap();
        connect(&mut bob, &mut alice);

        let hits = alice.s.search("beach", false).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(!hits[0].from_me, "newest first");
        assert_eq!(alice.s.search("sushi pizza", false).unwrap().len(), 1, "poll choices are searched");
        assert!(alice.s.search("beach volleyball", false).unwrap().is_empty(), "every word must match");

        let up = alice.s.catch_up(since, false).unwrap();
        assert_eq!(up.chats.len(), 1);
        let chat = &up.chats[0];
        assert_eq!((chat.chat_id, chat.count, chat.mentions), (ab, 3, 1));
        assert_eq!(chat.senders, vec![bob.pk()]);
        assert_eq!(chat.last, "Plan: Hike");
        assert_eq!(up.plans.len(), 1);
        assert_eq!((up.plans[0].place.as_str(), up.plans[0].mine), ("Hills", None));
        assert_eq!(up.polls.len(), 1);
        // Answered: no longer waiting.
        alice.s.vote(ab, up.polls[0].seq, vec![1]).unwrap();
        assert!(alice.s.catch_up(since, false).unwrap().polls.is_empty());
        // Hidden chats stay out unless revealed.
        alice.s.set_hidden(ab, true).unwrap();
        assert!(alice.s.search("beach", false).unwrap().is_empty());
        assert!(alice.s.catch_up(since, false).unwrap().chats.is_empty());
        assert_eq!(alice.s.search("beach", true).unwrap().len(), 2);
    }

    #[test]
    fn shared_albums_collect_everyones_photos() {
        use crate::protocol::Album;
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        let album = Album { id: [7; 16], title: "  Goa trip ".into() };
        let (path, bytes) = photo(&alice, 70_000);
        let out = crate::media::Outgoing { album: Some(album.clone()), ..outgoing(&path) };
        alice.s.send_media(ab, out, None).unwrap();
        connect(&mut alice, &mut bob);
        // Bob adds one of his own.
        let (path, _) = photo(&bob, 1000);
        bob.s.send_media(ba, crate::media::Outgoing { album: Some(album), ..outgoing(&path) }, None).unwrap();
        connect(&mut bob, &mut alice);
        for (phone, chat) in [(&alice, ab), (&bob, ba)] {
            let in_album: Vec<_> = phone
                .s
                .store
                .messages(chat, None)
                .unwrap()
                .into_iter()
                .filter_map(|m| m.album.map(|a| (a.id, a.title, m.from_me)))
                .collect();
            assert_eq!(in_album.len(), 2);
            assert!(in_album.iter().all(|(id, title, _)| *id == [7; 16] && title == "Goa trip"));
        }
        assert_eq!(bob.s.media_bytes(ba, 0).unwrap(), Some(bytes));
    }

    #[test]
    fn photos_travel_encrypted_and_arrive_whole() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        // Several chunks' worth.
        let (path, bytes) = photo(&alice, 3 * crate::media::CHUNK + 1234);
        assert_eq!(alice.s.send_media(ab, outgoing(&path), None).unwrap(), vec![bob.pk()]);
        // Stored encrypted: the plain bytes aren't in the media folder.
        let stored = std::fs::read_dir(alice.dir.path().join("media")).unwrap().next().unwrap().unwrap().path();
        assert!(!std::fs::read(&stored).unwrap().windows(64).any(|w| w == &bytes[1000..1064]));
        assert_eq!(alice.s.media_bytes(ab, 0).unwrap().unwrap(), bytes);

        connect(&mut alice, &mut bob);
        let m = bob.s.store.message(ba, 0).unwrap().unwrap();
        assert_eq!(m.body, "sunset");
        let media = m.media.unwrap();
        assert_eq!((media.size, media.width, media.thumb.as_slice()), (bytes.len() as u64, 4000, &[1u8, 2, 3][..]));
        assert_eq!(bob.s.media_bytes(ba, 0).unwrap().unwrap(), bytes);
        assert!(alice.texts(ab)[0].2, "delivered once the file arrived");
        assert!(bob.s.take_events().contains(&Event::Message(ba)));

        // Delete for everyone takes the file too.
        alice.s.delete_message(ab, 0, true).unwrap();
        connect(&mut alice, &mut bob);
        assert!(bob.s.store.message(ba, 0).unwrap().is_none());
        assert_eq!(std::fs::read_dir(bob.dir.path().join("media")).unwrap().count(), 0);
    }

    #[test]
    fn each_kind_has_its_size_limit() {
        use crate::media::max_size;
        use crate::protocol::MediaKind;
        let mb = 1024 * 1024;
        assert_eq!(
            [MediaKind::Image, MediaKind::Video, MediaKind::Audio, MediaKind::File].map(max_size),
            [10 * mb, 20 * mb, 10 * mb, 50 * mb]
        );
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        let (path, _) = photo(&alice, (10 * mb + 1) as usize);
        assert!(matches!(alice.s.send_media(ab, outgoing(&path), None), Err(SessionError::TooLarge(10))));
        // As a file, the same bytes may go.
        let mut file = outgoing(&path);
        file.kind = MediaKind::File;
        alice.s.send_media(ab, file, None).unwrap();
        connect(&mut alice, &mut bob);
        assert_eq!(bob.s.media_bytes(ba, 0).unwrap().unwrap().len() as u64, 10 * mb + 1);
    }

    #[test]
    fn a_resent_or_forged_file_is_handled() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        let (path, bytes) = photo(&alice, 100);
        alice.s.send_media(ab, outgoing(&path), None).unwrap();
        connect(&mut alice, &mut bob);
        // A new link resends anything unacknowledged; nothing is, so the
        // file isn't sent twice and Bob's copy is intact.
        alice.s.link_started(&bob.pk());
        connect(&mut alice, &mut bob);
        assert_eq!(bob.s.media_bytes(ba, 0).unwrap().unwrap(), bytes);
        // Chunks for a file no message announced are ignored.
        let stray = Frame::BlobChunk { id: [9; 16], blob: [8; 16], index: 0, total: 1, bytes: vec![0; 40] };
        assert!(bob.s.on_frame(&alice.pk(), stray).unwrap().is_empty());
    }

    #[test]
    fn photos_in_groups_reach_every_member() {
        let (mut alice, mut bob, mut carol) = (Phone::new(), Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        let ac = alice.s.add_friend(&carol.s.my_code("Carol"), true).unwrap().chat_id;
        carol.s.add_friend(&alice.s.my_code("Alice"), true).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        let (group, _) = alice.s.create_group("Trip", &[ab, ac]).unwrap();
        connect(&mut alice, &mut bob);
        connect(&mut alice, &mut carol);
        connect(&mut alice, &mut bob);
        let (path, bytes) = photo(&alice, 70_000);
        assert_eq!(alice.s.send_media(group, outgoing(&path), None).unwrap().len(), 2);
        connect(&mut alice, &mut bob);
        assert!(!alice.texts(group)[0].2, "not everyone has it yet");
        connect(&mut alice, &mut carol);
        assert!(alice.texts(group)[0].2);
        for p in [&bob, &carol] {
            let g = group_of(p, "Trip").unwrap();
            assert_eq!(p.s.media_bytes(g, 0).unwrap().unwrap(), bytes);
        }
    }

    #[test]
    fn renaming_a_friend() {
        let (mut alice, bob) = (Phone::new(), Phone::new());
        let ab = alice.s.add_friend(&bob.s.my_code(""), true).unwrap().chat_id;
        assert_eq!(alice.s.chat(ab).unwrap().title, "Friend");
        alice.s.rename_chat(ab, "  Bob ").unwrap();
        alice.s.rename_chat(ab, " ").unwrap();
        let meta = alice.s.chat(ab).unwrap();
        assert_eq!((meta.title.as_str(), meta.peers[0].name.as_str()), ("Bob", "Bob"));
    }

    #[test]
    fn deleting_a_chat_drops_what_was_waiting_in_it() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, _) = befriend(&mut alice, &mut bob);
        alice.s.send_text(ab, "never mind").unwrap();
        alice.s.delete_chat(ab).unwrap();
        assert!(alice.s.pending_peers().unwrap().is_empty());
        assert!(alice.s.store.chats().unwrap().iter().all(|(id, _)| *id != ab));
    }

    #[test]
    fn strangers_and_forged_hellos_are_refused() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        befriend(&mut alice, &mut bob);
        let eve = Phone::new();
        let (nonce, challenge) = bob.s.challenge();
        // Eve is not bob's friend: she may only ask to be one.
        let hello = eve.s.hello(&bob.pk(), &challenge).unwrap();
        let who = bob.s.accept_hello(&nonce, &hello).unwrap();
        assert!(!bob.s.is_friend(&who).unwrap());
        assert!(matches!(bob.s.on_link_up(&who), Err(SessionError::NotAFriend)));
        assert!(matches!(
            bob.s.on_stranger_frame(&who, Frame::KeyPackageRequest),
            Err(SessionError::NotAFriend)
        ));
        assert!(bob.s.requests().is_empty());
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
                hidden: false,
                admin: None,
                admins: Vec::new(),
            })
            .unwrap();
        assert_eq!(alice.s.send_text(notes, "x").unwrap(), None);
        assert!(alice.s.pending_peers().unwrap().is_empty());
    }

    #[test]
    fn friend_codes_and_safety_numbers() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        // Alice scans Bob's QR in person; Bob types Alice's code.
        let on_alice = alice.s.add_friend(&bob.s.my_code("Bob"), true).unwrap();
        let on_bob = bob.s.add_friend(&alice.s.my_code("Alice"), false).unwrap();
        assert_eq!(
            (on_alice.friend.as_str(), on_bob.friend.as_str()),
            ("Bob", "Alice")
        );
        assert_eq!(on_alice.safety_number, on_bob.safety_number);
        assert_eq!(
            on_alice.sigil_key, on_bob.sigil_key,
            "same shared sigil on both phones"
        );
        assert_eq!(on_alice.friend_key, bob.pk());
        assert!(on_alice.verified && !on_bob.verified);
        assert_eq!(
            bob.s.safety_number(on_bob.chat_id).unwrap(),
            on_bob.safety_number
        );
        bob.s.set_verified(on_bob.chat_id, true).unwrap();
        assert!(bob.s.chat(on_bob.chat_id).unwrap().peers[0].verified);
        assert!(matches!(
            alice.s.add_friend(&bob.s.my_code("Bob"), true),
            Err(SessionError::AlreadyFriends)
        ));
        assert!(matches!(
            alice.s.add_friend(&alice.s.my_code("me"), true),
            Err(SessionError::OwnCode)
        ));
    }
}
