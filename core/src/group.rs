//! Group chats, peer to peer: no server orders or stores anything.
//!
//! Each group is an MLS group. A message is encrypted once and the same
//! bytes go to every member over their own link, each copy acknowledged on
//! its own; it shows as delivered once every member has it.
//!
//! The admin (the creator) is the only one who changes membership. With a
//! single committer, members can never disagree about the group's state,
//! which is what a server would otherwise guarantee. The admin adds only
//! friends: it asks them for a key package over their link, commits the
//! addition, and sends them a welcome with the group's name and members.
//! Members who aren't friends with each other still connect directly for
//! the group's sake (see [`Session::knows`]).

use crate::mls::Received;
use crate::protocol::{Content, Frame, GroupInfo, GroupMember, MessageId, Quote};
use crate::session::{clip, now, Event, Result, Session, SessionError};
use crate::store::{ChatKind, ChatMeta, OutboxItem, OutboxState, Peer, StoredMessage};

/// Group messages we can't read are retried this many times (the change
/// that makes them readable may still be on its way), then dropped.
const UNREADABLE_TRIES: u8 = 5;

/// One member, as the app shows them.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub identity: [u8; 32],
    pub name: String,
    pub admin: bool,
    pub me: bool,
    /// Invited by the admin, not joined yet (their phone hasn't been
    /// reached).
    pub pending: bool,
}

/// A group, as the app shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupView {
    pub title: String,
    pub admin_is_me: bool,
    /// False once we've left or been removed.
    pub active: bool,
    pub members: Vec<Member>,
}

impl Session {
    // ---- Admin -------------------------------------------------------

    /// Starts a group with us as its admin, inviting these friends (their
    /// direct chats). Each joins when their phone is reached. Returns the
    /// chat and who to reach.
    pub fn create_group(&mut self, title: &str, friends: &[i64]) -> Result<(i64, Vec<[u8; 32]>)> {
        let group = self.mls.create_group()?;
        self.mls.persist(&self.store)?;
        let peers = self.friends_for(friends)?;
        let invitees = peers.iter().map(|p| p.identity).collect();
        let chat_id = self.store.create_chat(&ChatMeta {
            kind: ChatKind::Group,
            title: clip(title.trim(), 64),
            sigil_key: rand::random::<[u8; 32]>().to_vec(),
            pinned: false,
            created_at: now(),
            group_id: group,
            peers,
            hidden: false,
            admin: Some(self.public_key()),
        })?;
        Ok((chat_id, invitees))
    }

    /// Invites more friends (admin only).
    pub fn group_add(&mut self, chat_id: i64, friends: &[i64]) -> Result<Vec<[u8; 32]>> {
        let mut meta = self.admin_chat(chat_id)?;
        let mut invitees = Vec::new();
        for peer in self.friends_for(friends)? {
            if !meta.peers.iter().any(|p| p.identity == peer.identity) {
                invitees.push(peer.identity);
                meta.peers.push(peer);
            }
        }
        self.store.update_chat(chat_id, &meta)?;
        Ok(invitees)
    }

    /// Removes a member (admin only). They're told, and so is everyone else.
    pub fn group_remove(&mut self, chat_id: i64, member: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let mut meta = self.admin_chat(chat_id)?;
        meta.peers.retain(|p| &p.identity != member);
        self.store.update_chat(chat_id, &meta)?;
        if !self.mls.members(&meta.group_id)?.contains(member) {
            // Only invited: nothing to commit.
            return Ok(Vec::new());
        }
        let commit = self.mls.remove(&meta.group_id, member)?;
        self.mls.persist(&self.store)?;
        let mut reach = self.others(&meta)?;
        for peer in reach.iter().chain([member]) {
            self.enqueue(peer, &meta.group_id, commit.clone(), None)?;
        }
        reach.extend(self.broadcast(&meta, &Content::GroupInfo(self.info_of(&meta)?), None)?);
        reach.push(*member);
        Ok(dedup(reach))
    }

    /// Renames the group (admin only).
    pub fn group_rename(&mut self, chat_id: i64, title: &str) -> Result<Vec<[u8; 32]>> {
        let mut meta = self.admin_chat(chat_id)?;
        let title = clip(title.trim(), 64);
        if title.is_empty() {
            return Ok(Vec::new());
        }
        meta.title = title;
        self.store.update_chat(chat_id, &meta)?;
        self.broadcast(&meta, &Content::GroupInfo(self.info_of(&meta)?), None)
    }

    /// Leaves the group: tells everyone, then deletes it from this phone.
    /// (An admin who leaves leaves the group without one: it carries on,
    /// but nobody can add or remove members.)
    pub fn group_leave(&mut self, chat_id: i64) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        if meta.kind != ChatKind::Group {
            return Err(SessionError::NoSuchChat);
        }
        let reach = if meta.group_id.is_empty() {
            Vec::new()
        } else {
            self.broadcast(&meta, &Content::GroupLeave, None)?
        };
        // Already encrypted, the goodbyes still go out from the outbox.
        self.store.delete_chat(chat_id)?;
        Ok(reach)
    }

    /// What the app shows about a group.
    pub fn group_view(&self, chat_id: i64) -> Result<GroupView> {
        let meta = self.chat(chat_id)?;
        let me = self.public_key();
        let active = !meta.group_id.is_empty();
        let joined = if active { self.mls.members(&meta.group_id)? } else { Vec::new() };
        let mut members = vec![Member {
            identity: me,
            name: "You".into(),
            admin: meta.admin == Some(me),
            me: true,
            pending: false,
        }];
        for p in &meta.peers {
            members.push(Member {
                identity: p.identity,
                name: p.name.clone(),
                admin: meta.admin == Some(p.identity),
                me: false,
                pending: active && !joined.contains(&p.identity),
            });
        }
        Ok(GroupView {
            title: meta.title,
            admin_is_me: meta.admin == Some(me),
            active,
            members,
        })
    }

    // ---- Sending -----------------------------------------------------

    /// Sends to any chat: a group's members, or the friend of a direct
    /// chat. Returns who to reach.
    pub fn send_to_chat(&mut self, chat_id: i64, body: &str, reply_to: Option<i64>) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        if meta.kind != ChatKind::Group {
            return Ok(self.send_message(chat_id, body, reply_to)?.into_iter().collect());
        }
        if meta.group_id.is_empty() {
            return Err(SessionError::NotAFriend);
        }
        let id: MessageId = rand::random();
        let sent_at = now();
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
                delivered: false,
                id: Some(id),
                reply: quote.clone(),
                sender: None,
            },
        )?;
        let content = Content::GroupText {
            id,
            body: body.into(),
            sent_at,
            quote,
        };
        self.events.push(Event::Chat(chat_id));
        self.broadcast(&meta, &content, Some((chat_id, seq)))
    }

    // ---- The network -------------------------------------------------

    /// Someone in a group with us (not necessarily a friend).
    pub(crate) fn shares_group(&self, peer: &[u8; 32]) -> Result<bool> {
        Ok(self.store.chats()?.into_iter().any(|(_, m)| {
            m.kind == ChatKind::Group && !m.group_id.is_empty() && m.peers.iter().any(|p| &p.identity == peer)
        }))
    }

    /// Friends we've invited to a group of ours who haven't joined yet:
    /// the network keeps trying to reach them.
    pub(crate) fn pending_invitees(&self) -> Result<Vec<[u8; 32]>> {
        let mut out = Vec::new();
        for (_, meta) in self.admin_groups()? {
            let joined = self.mls.members(&meta.group_id)?;
            out.extend(meta.peers.iter().map(|p| p.identity).filter(|p| !joined.contains(p)));
        }
        Ok(dedup(out))
    }

    /// On reaching a friend we've invited: ask for their key package.
    pub(crate) fn group_invites_for(&self, peer: &[u8; 32]) -> Result<Vec<Frame>> {
        if !self.is_friend(peer)? {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (_, meta) in self.admin_groups()? {
            if meta.peers.iter().any(|p| &p.identity == peer) && !self.mls.members(&meta.group_id)?.contains(peer) {
                out.push(Frame::GroupKeyPackageRequest { group: meta.group_id });
            }
        }
        Ok(out)
    }

    pub(crate) fn on_group_frame(&mut self, peer: &[u8; 32], frame: Frame) -> Result<Vec<Frame>> {
        match frame {
            // A friend is adding us to their group.
            Frame::GroupKeyPackageRequest { group } => {
                if !self.is_friend(peer)? {
                    return Err(SessionError::NotAFriend);
                }
                let key_package = self.mls.key_package()?;
                self.mls.persist(&self.store)?;
                Ok(vec![Frame::GroupKeyPackage { group, key_package }])
            }
            Frame::GroupKeyPackage { group, key_package } => self.admit(peer, &group, &key_package),
            Frame::GroupWelcome { id, welcome, info } => {
                if self.store.is_seen(&id)? {
                    return Ok(vec![Frame::Ack { id }]);
                }
                // Only from the admin, who must be our friend.
                if info.admin != *peer || !self.is_friend(peer)? {
                    return Err(SessionError::Protocol);
                }
                self.join_group(&welcome, info)?;
                self.store.mark_seen(&id)?;
                Ok(vec![Frame::Ack { id }])
            }
            Frame::GroupMls { group, id, bytes } => self.on_group_message(peer, &group, id, &bytes),
            _ => Err(SessionError::Protocol),
        }
    }

    /// The admin got an invitee's key package: add them, welcome them, and
    /// tell everyone.
    fn admit(&mut self, peer: &[u8; 32], group: &[u8], key_package: &[u8]) -> Result<Vec<Frame>> {
        let me = self.public_key();
        let Some((chat_id, meta)) = self.group_chat(group)? else {
            return Ok(Vec::new());
        };
        let before = self.mls.members(group)?;
        let invited = meta.peers.iter().any(|p| &p.identity == peer);
        if meta.admin != Some(me) || !invited || before.contains(peer) {
            return Ok(Vec::new());
        }
        let invite = self.mls.invite_expecting(group, key_package, peer)?;
        self.mls.persist(&self.store)?;
        // Everyone already in applies the change...
        for member in before.iter().filter(|m| **m != me) {
            self.enqueue(member, group, invite.commit.clone(), None)?;
        }
        // ...the newcomer joins with the welcome...
        let info = self.info_of(&meta)?;
        let id: MessageId = rand::random();
        let welcome = Frame::GroupWelcome {
            id,
            welcome: invite.welcome,
            info: info.clone(),
        };
        self.store.outbox_put(
            &id,
            &OutboxItem {
                peer: *peer,
                state: OutboxState::Frame(welcome.to_bytes()),
                message: None,
            },
        )?;
        // ...and everyone learns their name.
        let reach = self.broadcast(&meta, &Content::GroupInfo(info), None)?;
        self.pokes.extend(reach);
        self.events.push(Event::Chat(chat_id));
        self.flush(peer)
    }

    fn join_group(&mut self, welcome: &[u8], info: GroupInfo) -> Result<()> {
        let me = self.public_key();
        let group = self.mls.join(welcome)?;
        self.mls.persist(&self.store)?;
        // The group really has the members the admin says.
        let mut members = self.mls.members(&group)?;
        let mut claimed: Vec<[u8; 32]> = info.members.iter().map(|m| m.identity).collect();
        members.sort();
        claimed.sort();
        if members != claimed || !members.contains(&me) {
            return Err(SessionError::Protocol);
        }
        let mut meta = ChatMeta {
            kind: ChatKind::Group,
            title: clip(&info.title, 64),
            sigil_key: info.sigil_key.clone(),
            pinned: false,
            created_at: now(),
            group_id: group.clone(),
            peers: Vec::new(),
            hidden: false,
            admin: Some(info.admin),
        };
        apply_info(&mut meta, &info, &me);
        let chat_id = match self.group_chat(&group)? {
            Some((chat_id, _)) => {
                self.store.update_chat(chat_id, &meta)?;
                chat_id
            }
            None => self.store.create_chat(&meta)?,
        };
        self.events.push(Event::Chat(chat_id));
        Ok(())
    }

    fn on_group_message(&mut self, peer: &[u8; 32], group: &[u8], id: MessageId, bytes: &[u8]) -> Result<Vec<Frame>> {
        let ack = Ok(vec![Frame::Ack { id }]);
        // A group we left: nothing to do but stop the resends.
        let Some((chat_id, mut meta)) = self.group_chat(group)? else {
            return ack;
        };
        if self.store.is_seen(&id)? {
            return ack;
        }
        match self.mls.receive(group, bytes) {
            Ok(Received::Message { sender, plaintext }) if sender == *peer => {
                self.mls.persist(&self.store)?;
                if let Some(content) = Content::decode(&plaintext) {
                    self.deliver_group(chat_id, &mut meta, peer, content)?;
                }
            }
            Ok(Received::Message { .. }) => self.mls.persist(&self.store)?,
            Ok(Received::MembersChanged) => {
                self.mls.persist(&self.store)?;
                self.sync_members(chat_id, &mut meta)?;
            }
            Ok(Received::Removed) => {
                self.mls.persist(&self.store)?;
                meta.group_id = Vec::new();
                meta.peers = Vec::new();
                self.store.update_chat(chat_id, &meta)?;
                self.events.push(Event::Chat(chat_id));
            }
            Err(_) => {
                let tries = self.unreadable.entry(id).or_insert(0);
                *tries += 1;
                if *tries < UNREADABLE_TRIES {
                    // Not acknowledged: it comes again with the next resend.
                    return Ok(Vec::new());
                }
                self.unreadable.remove(&id);
            }
        }
        self.store.mark_seen(&id)?;
        ack
    }

    fn deliver_group(&mut self, chat_id: i64, meta: &mut ChatMeta, peer: &[u8; 32], content: Content) -> Result<()> {
        let me = self.public_key();
        match content {
            Content::GroupText {
                id,
                body,
                sent_at,
                quote,
            } => {
                self.store.add_message(
                    chat_id,
                    &StoredMessage {
                        from_me: false,
                        body,
                        sent_at: sent_at.min(now()),
                        delivered: true,
                        id: Some(id),
                        reply: quote.map(|q| Quote {
                            snippet: clip(&q.snippet, 140),
                            ..q
                        }),
                        sender: Some(*peer),
                    },
                )?;
                self.events.push(Event::Chat(chat_id));
                self.events.push(Event::Message(chat_id));
            }
            // Only the admin speaks for the group.
            Content::GroupInfo(info) if meta.admin == Some(*peer) => {
                meta.title = clip(&info.title, 64);
                meta.sigil_key = info.sigil_key.clone();
                apply_info(meta, &info, &me);
                self.store.update_chat(chat_id, meta)?;
                self.events.push(Event::Chat(chat_id));
            }
            // Delete for everyone: only the sender's own message.
            Content::DeleteMessage { id } => {
                if let Some((seq, m)) = self.store.message_by_id(chat_id, &id)? {
                    if m.sender == Some(*peer) {
                        self.store.delete_message(chat_id, seq)?;
                        self.events.push(Event::Chat(chat_id));
                    }
                }
            }
            Content::GroupLeave => {
                if meta.admin == Some(me) {
                    let reach = self.group_remove(chat_id, peer)?;
                    self.pokes.extend(reach);
                } else {
                    meta.peers.retain(|p| &p.identity != peer);
                    self.store.update_chat(chat_id, meta)?;
                }
                self.events.push(Event::Chat(chat_id));
            }
            _ => {}
        }
        Ok(())
    }

    /// After a membership change: members are whoever MLS says, keeping
    /// the names we know.
    fn sync_members(&mut self, chat_id: i64, meta: &mut ChatMeta) -> Result<()> {
        let me = self.public_key();
        let members = self.mls.members(&meta.group_id)?;
        let mut peers: Vec<Peer> = Vec::new();
        for identity in members.into_iter().filter(|m| *m != me) {
            let known = meta.peers.iter().find(|p| p.identity == identity).cloned();
            peers.push(known.unwrap_or_else(|| new_peer(identity, "Member")));
        }
        meta.peers = peers;
        self.store.update_chat(chat_id, meta)?;
        self.events.push(Event::Chat(chat_id));
        Ok(())
    }

    // ---- Helpers -----------------------------------------------------

    pub(crate) fn broadcast_content(&mut self, meta: &ChatMeta, content: &Content) -> Result<Vec<[u8; 32]>> {
        self.broadcast(meta, content, None)
    }

    /// Encrypts once and queues the same bytes for every other member.
    fn broadcast(&mut self, meta: &ChatMeta, content: &Content, message: Option<(i64, i64)>) -> Result<Vec<[u8; 32]>> {
        let cipher = self.mls.encrypt(&meta.group_id, &content.encode())?;
        self.mls.persist(&self.store)?;
        let recipients = self.others(meta)?;
        for peer in &recipients {
            self.enqueue(peer, &meta.group_id, cipher.clone(), message)?;
        }
        Ok(recipients)
    }

    fn enqueue(&mut self, peer: &[u8; 32], group: &[u8], cipher: Vec<u8>, message: Option<(i64, i64)>) -> Result<()> {
        self.store.outbox_put(
            &rand::random(),
            &OutboxItem {
                peer: *peer,
                state: OutboxState::Group {
                    group: group.to_vec(),
                    cipher,
                },
                message,
            },
        )?;
        Ok(())
    }

    /// The group's members other than us, by MLS.
    fn others(&self, meta: &ChatMeta) -> Result<Vec<[u8; 32]>> {
        let me = self.public_key();
        Ok(self.mls.members(&meta.group_id)?.into_iter().filter(|m| *m != me).collect())
    }

    fn info_of(&self, meta: &ChatMeta) -> Result<GroupInfo> {
        let me = self.public_key();
        let members = self
            .mls
            .members(&meta.group_id)?
            .into_iter()
            .map(|identity| GroupMember {
                identity,
                name: if identity == me {
                    self.my_name.clone()
                } else {
                    meta.peers
                        .iter()
                        .find(|p| p.identity == identity)
                        .map_or_else(String::new, |p| p.name.clone())
                },
            })
            .collect();
        Ok(GroupInfo {
            title: meta.title.clone(),
            sigil_key: meta.sigil_key.clone(),
            admin: me,
            members,
        })
    }

    fn group_chat(&self, group: &[u8]) -> Result<Option<(i64, ChatMeta)>> {
        Ok(self
            .store
            .chats()?
            .into_iter()
            .find(|(_, m)| m.kind == ChatKind::Group && m.group_id == group))
    }

    fn admin_groups(&self) -> Result<Vec<(i64, ChatMeta)>> {
        let me = self.public_key();
        Ok(self
            .store
            .chats()?
            .into_iter()
            .filter(|(_, m)| m.kind == ChatKind::Group && m.admin == Some(me) && !m.group_id.is_empty())
            .collect())
    }

    fn admin_chat(&self, chat_id: i64) -> Result<ChatMeta> {
        let meta = self.chat(chat_id)?;
        if meta.kind != ChatKind::Group || meta.admin != Some(self.public_key()) || meta.group_id.is_empty() {
            return Err(SessionError::NotAdmin);
        }
        Ok(meta)
    }

    /// The friends of these direct chats, as group members.
    fn friends_for(&self, chats: &[i64]) -> Result<Vec<Peer>> {
        let mut out: Vec<Peer> = Vec::new();
        for &chat in chats {
            let meta = self.chat(chat)?;
            if meta.kind != ChatKind::Direct {
                continue;
            }
            for p in meta.peers {
                if !out.iter().any(|o| o.identity == p.identity) {
                    out.push(new_peer(p.identity, &meta.title));
                }
            }
        }
        Ok(out)
    }
}

/// Names from the admin's member list; our own entry is left out.
fn apply_info(meta: &mut ChatMeta, info: &GroupInfo, me: &[u8; 32]) {
    meta.peers = info
        .members
        .iter()
        .filter(|m| &m.identity != me)
        .map(|m| {
            let name = clip(m.name.trim(), 64);
            new_peer(m.identity, if name.is_empty() { "Member" } else { &name })
        })
        .collect();
}

fn new_peer(identity: [u8; 32], name: &str) -> Peer {
    Peer {
        identity,
        name: name.into(),
        onion: None,
        verified: false,
    }
}

fn dedup(mut v: Vec<[u8; 32]>) -> Vec<[u8; 32]> {
    v.sort();
    v.dedup();
    v
}
