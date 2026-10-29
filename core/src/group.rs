//! Group chats, peer to peer: no server orders or stores anything.
//!
//! Each group is an MLS group. A message is encrypted once and the same
//! bytes go to every member over their own link, each copy acknowledged on
//! its own; it shows as delivered once every member has it. Members also
//! pass messages and files on to each other (see `group_sync`), so a
//! message reaches someone even when its sender is offline.
//!
//! The leader (the creator) is the only one who changes membership. With a
//! single committer, members can never disagree about the group's state,
//! which is what a server would otherwise guarantee. The leader adds only
//! friends: it asks them for a key package over their link, commits the
//! addition, and sends them a welcome with the group's name and members.
//! Members who aren't friends with each other still connect directly for
//! the group's sake (see [`Session::knows`]).
//!
//! The leader can make a few members admins. An admin asks in the group
//! ([`GroupRequest`]) and the leader's phone does it, so there is still one
//! committer; the leader's own rights can't be taken away, only handed over
//! by the leader. When the leader adds someone they can pass on the last
//! week of text messages (see [`Session::group_add`]); everyone is told.

use crate::call::now_ms;
use crate::group_sync::sync_id;
use crate::mls::Received;
use crate::protocol::{Content, Frame, GroupInfo, GroupMember, GroupRequest, HistoryItem, MessageId, Quote};
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
    /// The leader or an admin.
    pub admin: bool,
    /// The one who created the group (or was handed it).
    pub leader: bool,
    pub me: bool,
    /// Invited by the admin, not joined yet (their phone hasn't been
    /// reached).
    pub pending: bool,
}

/// A group, as the app shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupView {
    pub title: String,
    /// We are the leader.
    pub admin_is_me: bool,
    /// We are the leader or an admin.
    pub i_am_admin: bool,
    /// False once we've left or been removed.
    pub active: bool,
    pub members: Vec<Member>,
}

/// Most people in a group, counting the admin. Members pass messages and
/// files on to each other (see `group_sync`), so a sender doesn't upload a
/// file once per member (see docs/DESIGN.md, "Groups at scale").
pub const MAX_GROUP: usize = 50;

/// Most admins besides the leader.
pub const MAX_ADMINS: usize = 5;

/// How far back, and how much, a new member is shown of what was said.
const HISTORY_DAYS: u64 = 7;
const HISTORY_MESSAGES: usize = 300;
/// Under the frame limit, with room for sealing.
const HISTORY_BYTES: usize = 150_000;

/// Refuses a group that would have more than [`MAX_GROUP`] people.
fn fits_group(people: usize) -> Result<()> {
    if people > MAX_GROUP {
        return Err(SessionError::GroupFull(MAX_GROUP));
    }
    Ok(())
}

impl Session {
    // ---- Admin -------------------------------------------------------

    /// Starts a group with us as its admin, inviting these friends (their
    /// direct chats). Each joins when their phone is reached. Returns the
    /// chat and who to reach.
    pub fn create_group(&mut self, title: &str, friends: &[i64]) -> Result<(i64, Vec<[u8; 32]>)> {
        fits_group(friends.len() + 1)?;
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
            admins: Vec::new(),
        })?;
        Ok((chat_id, invitees))
    }

    /// Invites more friends. The leader does it; an admin asks the leader.
    /// With `share_history`, the new members are also given the last week of
    /// text messages (everyone in the group is told).
    pub fn group_add(&mut self, chat_id: i64, friends: &[i64], share_history: bool) -> Result<Vec<[u8; 32]>> {
        let (meta, leader) = self.manager_chat(chat_id)?;
        if !leader {
            let mut told = Vec::new();
            for peer in self.friends_for(friends)? {
                if meta.peers.iter().all(|p| p.identity != peer.identity) {
                    let request = GroupRequest::Add { who: peer.identity, history: share_history };
                    told = self.broadcast(&meta, &Content::GroupRequest(request), None)?;
                }
            }
            return Ok(told);
        }
        self.add_invitees(chat_id, meta, friends, share_history)
    }

    fn add_invitees(
        &mut self,
        chat_id: i64,
        mut meta: ChatMeta,
        friends: &[i64],
        share_history: bool,
    ) -> Result<Vec<[u8; 32]>> {
        let mut invitees = Vec::new();
        for peer in self.friends_for(friends)? {
            if !meta.peers.iter().any(|p| p.identity == peer.identity) {
                invitees.push(peer.identity);
                meta.peers.push(peer);
            }
        }
        // Everyone invited, plus us.
        fits_group(meta.peers.len() + 1)?;
        self.store.update_chat(chat_id, &meta)?;
        if share_history {
            for peer in &invitees {
                self.store.kv_put(&history_name(&meta.group_id, peer), &true)?;
            }
        }
        Ok(invitees)
    }

    /// Removes a member. The leader does it; an admin asks the leader (and
    /// can't remove another admin). They're told, and so is everyone else.
    pub fn group_remove(&mut self, chat_id: i64, member: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let (mut meta, leader) = self.manager_chat(chat_id)?;
        if !leader {
            if is_admin(&meta, member) {
                return Err(SessionError::GroupRule("Only the group leader can remove an admin."));
            }
            return self.broadcast(&meta, &Content::GroupRequest(GroupRequest::Remove { who: *member }), None);
        }
        if *member == self.public_key() {
            return Err(SessionError::GroupRule("Hand over leadership before you leave the group."));
        }
        let was_admin = meta.admins.contains(member);
        meta.peers.retain(|p| &p.identity != member);
        meta.admins.retain(|a| a != member);
        self.store.update_chat(chat_id, &meta)?;
        if !self.mls.members(&meta.group_id)?.contains(member) {
            // Only invited: nothing to commit.
            return Ok(Vec::new());
        }
        let commit = self.mls.remove(&meta.group_id, member)?;
        self.mls.persist(&self.store)?;
        self.keep_message(&meta.group_id, &commit)?;
        let mut reach = self.others(&meta)?;
        for peer in reach.iter().chain([member]) {
            self.enqueue(peer, &meta.group_id, commit.clone(), None)?;
        }
        reach.extend(self.broadcast(&meta, &Content::GroupInfo(self.info_of(&meta)?), None)?);
        if was_admin {
            reach.extend(self.send_roles(&meta)?);
        }
        reach.push(*member);
        Ok(dedup(reach))
    }

    /// Renames the group. The leader does it; an admin asks the leader.
    pub fn group_rename(&mut self, chat_id: i64, title: &str) -> Result<Vec<[u8; 32]>> {
        let (mut meta, leader) = self.manager_chat(chat_id)?;
        let title = clip(title.trim(), 64);
        if title.is_empty() {
            return Ok(Vec::new());
        }
        if !leader {
            return self.broadcast(&meta, &Content::GroupRequest(GroupRequest::Rename { title }), None);
        }
        meta.title = title;
        self.store.update_chat(chat_id, &meta)?;
        self.broadcast(&meta, &Content::GroupInfo(self.info_of(&meta)?), None)
    }

    /// Makes a member an admin (leader only). They must have joined, be the
    /// leader's friend, and there is room for only a few.
    pub fn group_make_admin(&mut self, chat_id: i64, member: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let mut meta = self.admin_chat(chat_id)?;
        if !self.mls.members(&meta.group_id)?.contains(member) || !self.is_friend(member)? {
            return Err(SessionError::GroupRule(
                "Admins must be your friends and already in the group.",
            ));
        }
        if meta.admins.contains(member) {
            return Ok(Vec::new());
        }
        if meta.admins.len() >= MAX_ADMINS {
            return Err(SessionError::GroupRule("A group can have up to 5 admins besides its leader."));
        }
        meta.admins.push(*member);
        self.store.update_chat(chat_id, &meta)?;
        self.send_roles(&meta)
    }

    /// Takes admin rights from an admin. The leader can; so can any admin,
    /// but nobody can take the leader's.
    pub fn group_remove_admin(&mut self, chat_id: i64, member: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let (mut meta, leader) = self.manager_chat(chat_id)?;
        if meta.admin == Some(*member) {
            return Err(SessionError::GroupRule(
                "The group leader stays an admin until they hand over leadership themselves.",
            ));
        }
        if !leader {
            return self.broadcast(&meta, &Content::GroupRequest(GroupRequest::Demote { who: *member }), None);
        }
        if !meta.admins.contains(member) {
            return Ok(Vec::new());
        }
        meta.admins.retain(|a| a != member);
        self.store.update_chat(chat_id, &meta)?;
        self.send_roles(&meta)
    }

    /// Hands leadership to an admin (leader only). The leader stays an admin.
    pub fn group_hand_over(&mut self, chat_id: i64, to: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let mut meta = self.admin_chat(chat_id)?;
        if !meta.admins.contains(to) {
            return Err(SessionError::GroupRule("Leadership can only go to an admin."));
        }
        let joined = self.mls.members(&meta.group_id)?;
        if meta.peers.iter().any(|p| !joined.contains(&p.identity)) {
            return Err(SessionError::GroupRule(
                "Wait until everyone you invited has joined, then hand over leadership.",
            ));
        }
        let me = self.public_key();
        meta.admins.retain(|a| a != to);
        meta.admins.push(me);
        meta.admin = Some(*to);
        self.store.update_chat(chat_id, &meta)?;
        // From now on the new leader's word counts.
        let reach = self.send_roles(&meta)?;
        self.events.push(Event::Chat(chat_id));
        Ok(reach)
    }

    /// Leaves the group: tells everyone, then deletes it from this phone.
    /// (A leader with admins hands leadership over first. A group left
    /// without a leader carries on, but nobody can add or remove members.)
    pub fn group_leave(&mut self, chat_id: i64) -> Result<Vec<[u8; 32]>> {
        let meta = self.chat(chat_id)?;
        if meta.kind != ChatKind::Group {
            return Err(SessionError::NoSuchChat);
        }
        if meta.admin == Some(self.public_key()) && !meta.admins.is_empty() && !meta.group_id.is_empty() {
            return Err(SessionError::GroupRule("Hand over leadership to an admin before you leave the group."));
        }
        let reach = if meta.group_id.is_empty() {
            Vec::new()
        } else {
            self.broadcast(&meta, &Content::GroupLeave, None)?
        };
        // Already encrypted, the goodbyes still go out from the outbox.
        self.store.delete_chat(chat_id)?;
        if !meta.group_id.is_empty() {
            self.sync_forget(&meta.group_id)?;
        }
        Ok(reach)
    }

    /// Tells the group who leads it and who its admins are.
    fn send_roles(&mut self, meta: &ChatMeta) -> Result<Vec<[u8; 32]>> {
        let leader = meta.admin.unwrap_or_else(|| self.public_key());
        let at = now_ms();
        self.store.kv_put(&roles_name(&meta.group_id), &at)?;
        self.broadcast(meta, &Content::GroupRoles { leader, admins: meta.admins.clone(), at }, None)
    }

    /// An admin or the leader, and whether it's the leader.
    fn manager_chat(&self, chat_id: i64) -> Result<(ChatMeta, bool)> {
        let meta = self.chat(chat_id)?;
        let me = self.public_key();
        if meta.kind != ChatKind::Group || !is_admin(&meta, &me) || meta.group_id.is_empty() {
            return Err(SessionError::NotAdmin);
        }
        let leader = meta.admin == Some(me);
        Ok((meta, leader))
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
            admin: is_admin(&meta, &me),
            leader: meta.admin == Some(me),
            me: true,
            pending: false,
        }];
        for p in &meta.peers {
            members.push(Member {
                identity: p.identity,
                name: p.name.clone(),
                admin: is_admin(&meta, &p.identity),
                leader: meta.admin == Some(p.identity),
                me: false,
                pending: active && !joined.contains(&p.identity),
            });
        }
        Ok(GroupView {
            admin_is_me: meta.admin == Some(me),
            i_am_admin: is_admin(&meta, &me),
            title: meta.title,
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
                media: None,
                ..Default::default()
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
            Frame::GroupHave { .. } | Frame::GroupWant { .. } | Frame::BlobWant { .. } => self.on_sync_frame(peer, frame),
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
        self.keep_message(group, &invite.commit)?;
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
        let mut reach = self.broadcast(&meta, &Content::GroupInfo(info), None)?;
        if !meta.admins.is_empty() {
            reach.extend(self.send_roles(&meta)?);
        }
        reach.extend(self.share_history(chat_id, &meta, peer)?);
        self.pokes.extend(reach);
        self.events.push(Event::Chat(chat_id));
        self.flush(peer)
    }

    /// If the leader chose so when adding them: gives a new member the last
    /// week of text messages, over their own chat (the rest of the group
    /// doesn't get a copy), and tells the group it happened.
    fn share_history(&mut self, chat_id: i64, meta: &ChatMeta, peer: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let name = history_name(&meta.group_id, peer);
        if self.store.kv_get::<bool>(&name)?.is_none() {
            return Ok(Vec::new());
        }
        self.store.kv_delete(&name)?;
        let items = self.history_items(chat_id)?;
        let Some((direct, _)) = self.direct_chat(peer)? else {
            return Ok(Vec::new());
        };
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut reach = self.tell_chat(direct, Content::HistoryBatch { group: meta.group_id.clone(), items })?;
        reach.extend(self.broadcast(meta, &Content::HistoryShared { with: *peer }, None)?);
        let who = meta.peers.iter().find(|p| &p.identity == peer).map_or("them", |p| p.name.as_str());
        self.add_notice(chat_id, format!("You shared the last week of messages with {who}."))?;
        Ok(reach)
    }

    /// The last week of text messages of a group, oldest first, within what
    /// fits in one batch.
    fn history_items(&self, chat_id: i64) -> Result<Vec<HistoryItem>> {
        let me = self.public_key();
        let since = now().saturating_sub(HISTORY_DAYS * 24 * 3600);
        let mut items: Vec<HistoryItem> = Vec::new();
        let mut bytes = 0;
        for m in self.store.messages(chat_id, None)?.into_iter().rev() {
            if m.sent_at < since || items.len() >= HISTORY_MESSAGES {
                break;
            }
            let (Some(id), false, true, None, None, None) =
                (m.id, m.notice, !m.body.is_empty(), &m.media, &m.poll, &m.call)
            else {
                continue;
            };
            let sender = if m.from_me { me } else { m.sender.unwrap_or(me) };
            let cost = m.body.len() + 80;
            if bytes + cost > HISTORY_BYTES {
                break;
            }
            bytes += cost;
            items.push(HistoryItem { id, sender, sent_at: m.sent_at, body: m.body });
        }
        items.reverse();
        Ok(items)
    }

    /// A line in the chat that no one wrote ("You shared…").
    fn add_notice(&mut self, chat_id: i64, body: String) -> Result<()> {
        self.store.add_message(chat_id, &StoredMessage { body, sent_at: now(), delivered: true, notice: true, ..Default::default() })?;
        self.events.push(Event::Chat(chat_id));
        Ok(())
    }

    /// Recent messages from the leader of a group we're in. They may come
    /// before the welcome does: then they wait until we've joined.
    pub(crate) fn on_history(&mut self, peer: &[u8; 32], group: Vec<u8>, items: Vec<HistoryItem>) -> Result<()> {
        match self.group_chat(&group)? {
            Some((chat_id, meta)) if meta.admin == Some(*peer) => self.apply_history(chat_id, &meta, items),
            Some(_) => Ok(()),
            // One waiting at a time: more than that is not from a leader
            // about to welcome us.
            None => Ok(self.store.kv_put(STASH, &(group, *peer, items))?),
        }
    }

    fn apply_history(&mut self, chat_id: i64, meta: &ChatMeta, mut items: Vec<HistoryItem>) -> Result<()> {
        let me = self.public_key();
        let limit = now();
        items.truncate(HISTORY_MESSAGES);
        items.sort_by_key(|i| i.sent_at);
        let mut added = false;
        for item in items {
            if item.body.is_empty() || item.sender == me || self.store.message_by_id(chat_id, &item.id)?.is_some() {
                continue;
            }
            self.store.add_message(
                chat_id,
                &StoredMessage {
                    body: clip(&item.body, 8000),
                    sent_at: item.sent_at.min(limit),
                    delivered: true,
                    id: Some(item.id),
                    sender: Some(item.sender),
                    shared_by: meta.admin,
                    ..Default::default()
                },
            )?;
            added = true;
        }
        if added {
            self.events.push(Event::Chat(chat_id));
        }
        Ok(())
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
        // Messages from before we joined can't be read: never asked for.
        self.set_joined_epoch(&group)?;
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
            admins: Vec::new(),
        };
        apply_info(&mut meta, &info, &me);
        let chat_id = match self.group_chat(&group)? {
            Some((chat_id, _)) => {
                self.store.update_chat(chat_id, &meta)?;
                chat_id
            }
            None => self.store.create_chat(&meta)?,
        };
        // The leader's earlier messages may have come first.
        if let Some((stashed, from, items)) = self.store.kv_get::<(Vec<u8>, [u8; 32], Vec<HistoryItem>)>(STASH)? {
            if stashed == group {
                self.store.kv_delete(STASH)?;
                if from == info.admin {
                    self.apply_history(chat_id, &meta, items)?;
                }
            }
        }
        self.events.push(Event::Chat(chat_id));
        Ok(())
    }

    fn on_group_message(&mut self, peer: &[u8; 32], group: &[u8], id: MessageId, bytes: &[u8]) -> Result<Vec<Frame>> {
        let ack = Ok(vec![Frame::Ack { id }]);
        // A group we left: nothing to do but stop the resends.
        let Some((chat_id, mut meta)) = self.group_chat(group)? else {
            return ack;
        };
        // The same message may come from its sender and from members passing
        // it on: known by the hash of its bytes, it's taken once.
        let sid = sync_id(bytes);
        self.sync_heard(peer, group, &sid);
        if self.store.is_seen(&id)? || self.store.is_seen(&sid)? {
            return ack;
        }
        // Passed on by a member (under its hash, not an outbox id): nobody
        // resends it, so an unreadable one isn't counted down as below.
        let relayed = id == sid;
        // Any current member may pass on anyone's message; MLS says whose it
        // is. Someone else may only send their own, as before.
        let member = |s: &Self| s.mls.members(group).is_ok_and(|m| m.contains(peer));
        let mut out = Vec::new();
        match self.mls.receive(group, bytes) {
            Ok(Received::Message { sender, plaintext }) if sender == *peer || member(self) => {
                self.mls.persist(&self.store)?;
                self.keep_message(group, bytes)?;
                if let Some(content) = Content::decode(&plaintext) {
                    self.deliver_group(chat_id, &mut meta, &sender, content)?;
                }
                // It may announce a file this member has.
                out.extend(self.blob_fetches(peer)?);
            }
            Ok(Received::Message { .. }) => self.mls.persist(&self.store)?,
            Ok(Received::MembersChanged) => {
                self.mls.persist(&self.store)?;
                self.keep_message(group, bytes)?;
                self.sync_members(chat_id, &mut meta)?;
            }
            Ok(Received::Removed) => {
                self.mls.persist(&self.store)?;
                self.sync_forget(group)?;
                meta.group_id = Vec::new();
                meta.peers = Vec::new();
                self.store.update_chat(chat_id, &meta)?;
                self.events.push(Event::Chat(chat_id));
            }
            Err(_) if relayed => {
                self.sync_unreadable(group, bytes)?;
                return Ok(Vec::new());
            }
            Err(_) => {
                let tries = self.unreadable.entry(id).or_insert(0);
                *tries += 1;
                if *tries < UNREADABLE_TRIES {
                    // Not acknowledged: it comes again with the next resend.
                    return Ok(Vec::new());
                }
                self.unreadable.remove(&id);
                self.store.mark_seen(&sid)?;
            }
        }
        self.store.mark_seen(&id)?;
        out.insert(0, Frame::Ack { id });
        Ok(out)
    }

    /// Applies a group message from `sender` (whose it is, by MLS: it may
    /// have been passed on by another member).
    pub(crate) fn deliver_group(&mut self, chat_id: i64, meta: &mut ChatMeta, peer: &[u8; 32], content: Content) -> Result<()> {
        let me = self.public_key();
        match content {
            Content::GroupText {
                id,
                body,
                sent_at,
                quote,
            } => {
                self.add_received(
                    chat_id,
                    StoredMessage {
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
                        media: None,
                        ..Default::default()
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
            Content::React { id, emoji, at } => self.on_react(chat_id, peer, id, emoji, at)?,
            Content::Edit { id, body, at } => self.on_edit(chat_id, peer, id, body, at)?,
            // Delete for everyone: only the sender's own message.
            Content::DeleteMessage { id } => {
                if let Some((seq, m)) = self.store.message_by_id(chat_id, &id)? {
                    if m.sender == Some(*peer) {
                        if let Some(media) = &m.media {
                            self.delete_blobs(&[media.blob]);
                        }
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
                    meta.admins.retain(|a| a != peer);
                    self.store.update_chat(chat_id, meta)?;
                }
                self.events.push(Event::Chat(chat_id));
            }
            // Only the leader says who leads; the newest list wins.
            Content::GroupRoles { leader, admins, at } if meta.admin == Some(*peer) => {
                let joined = self.mls.members(&meta.group_id)?;
                let name = roles_name(&meta.group_id);
                let newest = self.store.kv_get::<u64>(&name)?.is_none_or(|last| at > last);
                if joined.contains(&leader) && newest {
                    self.store.kv_put(&name, &at)?;
                    meta.admin = Some(leader);
                    meta.admins = admins
                        .into_iter()
                        .filter(|a| *a != leader && joined.contains(a))
                        .take(MAX_ADMINS + 1)
                        .collect();
                    meta.admins.dedup();
                    self.store.update_chat(chat_id, meta)?;
                    self.events.push(Event::Chat(chat_id));
                }
            }
            // An admin asks; only the leader's phone acts, and only for an admin.
            Content::GroupRequest(request) if meta.admin == Some(me) && meta.admins.contains(peer) => {
                let reach = self.on_request(chat_id, meta, request)?;
                self.pokes.extend(reach);
            }
            Content::HistoryShared { with } if meta.admin == Some(*peer) => {
                let leader = self.member_name(meta, peer);
                let text = if with == me {
                    format!("{leader} shared the last week of messages with you.")
                } else {
                    format!("{leader} shared the last week of messages with {}.", self.member_name(meta, &with))
                };
                self.add_notice(chat_id, text)?;
            }
            Content::Pin { id, at } => self.on_pin(chat_id, id, at)?,
            Content::Disappear { seconds, at } => {
                let who = self.member_name(meta, peer);
                self.on_disappear(chat_id, &who, seconds, at)?;
            }
            Content::Location { id, sent_at, lat, lon } => self.on_location(chat_id, peer, id, sent_at, lat, lon)?,
            _ => {}
        }
        Ok(())
    }

    /// The leader does what an admin asked, if the rules allow it. An
    /// admin's mistake or a stale request is dropped, not an error.
    fn on_request(&mut self, chat_id: i64, meta: &ChatMeta, request: GroupRequest) -> Result<Vec<[u8; 32]>> {
        let outcome = match request {
            GroupRequest::Add { who, history } => match self.direct_chat(&who)? {
                Some((direct, _)) if self.is_friend(&who)? => {
                    self.add_invitees(chat_id, meta.clone(), &[direct], history)
                }
                _ => return Ok(Vec::new()),
            },
            GroupRequest::Remove { who } if !is_admin(meta, &who) => self.group_remove(chat_id, &who),
            GroupRequest::Remove { .. } => return Ok(Vec::new()),
            GroupRequest::Rename { title } => self.group_rename(chat_id, &title),
            GroupRequest::Demote { who } => self.group_remove_admin(chat_id, &who),
        };
        match outcome {
            Err(SessionError::GroupRule(_) | SessionError::GroupFull(_)) => Ok(Vec::new()),
            other => other,
        }
    }

    /// What we call a member of a group.
    fn member_name(&self, meta: &ChatMeta, who: &[u8; 32]) -> String {
        if *who == self.public_key() {
            return "You".into();
        }
        meta.peers.iter().find(|p| &p.identity == who).map_or_else(|| "A member".into(), |p| p.name.clone())
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
        let joined: Vec<[u8; 32]> = meta.peers.iter().map(|p| p.identity).collect();
        meta.admins.retain(|a| joined.contains(a) || *a == me);
        self.store.update_chat(chat_id, meta)?;
        self.events.push(Event::Chat(chat_id));
        Ok(())
    }

    // ---- Helpers -----------------------------------------------------

    pub(crate) fn broadcast_content(&mut self, meta: &ChatMeta, content: &Content) -> Result<Vec<[u8; 32]>> {
        self.broadcast(meta, content, None)
    }

    /// A message (its delivery is tracked) to every other member.
    pub(crate) fn broadcast_message(
        &mut self,
        meta: &ChatMeta,
        content: &Content,
        message: (i64, i64),
    ) -> Result<Vec<[u8; 32]>> {
        self.broadcast(meta, content, Some(message))
    }

    /// Encrypts once and queues the same bytes for every other member.
    fn broadcast(&mut self, meta: &ChatMeta, content: &Content, message: Option<(i64, i64)>) -> Result<Vec<[u8; 32]>> {
        let cipher = self.mls.encrypt(&meta.group_id, &content.encode())?;
        self.mls.persist(&self.store)?;
        // Kept to pass on, like what we receive.
        self.keep_message(&meta.group_id, &cipher)?;
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
    pub(crate) fn others(&self, meta: &ChatMeta) -> Result<Vec<[u8; 32]>> {
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

    pub(crate) fn group_chat(&self, group: &[u8]) -> Result<Option<(i64, ChatMeta)>> {
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
                    // A friend we've verified stays verified in the group.
                    out.push(Peer { verified: p.verified, ..new_peer(p.identity, &meta.title) });
                }
            }
        }
        Ok(out)
    }
}

/// Names from the admin's member list; our own entry is left out. People
/// we already knew keep what we know about them (verified, address): every
/// update to the list (someone joins, a rename) must not undo that.
fn apply_info(meta: &mut ChatMeta, info: &GroupInfo, me: &[u8; 32]) {
    let known = std::mem::take(&mut meta.peers);
    meta.peers = info
        .members
        .iter()
        .filter(|m| &m.identity != me)
        .map(|m| {
            let name = clip(m.name.trim(), 64);
            let name = if name.is_empty() { "Member" } else { &name };
            match known.iter().find(|p| p.identity == m.identity) {
                Some(p) => Peer { name: name.into(), ..p.clone() },
                None => new_peer(m.identity, name),
            }
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

/// The leader or an admin.
fn is_admin(meta: &ChatMeta, who: &[u8; 32]) -> bool {
    meta.admin == Some(*who) || meta.admins.contains(who)
}

/// Where the leader remembers that a new member is to be given the history.
fn history_name(group: &[u8], peer: &[u8; 32]) -> Vec<u8> {
    [b"history-for:".as_slice(), group, peer].concat()
}

/// When the roles we hold were set (the leader's clock), per group.
fn roles_name(group: &[u8]) -> Vec<u8> {
    [b"roles-at:".as_slice(), group].concat()
}

/// A history that arrived before the welcome that makes sense of it.
const STASH: &[u8] = b"history-stash";

fn dedup(mut v: Vec<[u8; 32]>) -> Vec<[u8; 32]> {
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn groups_stop_at_the_limit() {
        assert!(fits_group(MAX_GROUP).is_ok());
        assert!(matches!(fits_group(MAX_GROUP + 1), Err(SessionError::GroupFull(n)) if n == MAX_GROUP));
    }
}

#[cfg(test)]
mod info_tests {
    use super::*;

    fn meta(peers: Vec<Peer>) -> ChatMeta {
        ChatMeta {
            kind: ChatKind::Group,
            title: "Crew".into(),
            sigil_key: vec![1],
            pinned: false,
            created_at: 0,
            group_id: vec![9],
            peers,
            hidden: false,
            admin: None,
            admins: Vec::new(),
        }
    }

    #[test]
    fn a_new_member_list_keeps_who_we_verified() {
        let (me, dana, asha, newcomer) = ([1; 32], [2; 32], [3; 32], [4; 32]);
        let mut known = new_peer(dana, "Dana");
        known.verified = true;
        known.onion = Some("dana.onion".into());
        let mut meta = meta(vec![known, new_peer(asha, "Asha")]);
        let member = |identity, name: &str| GroupMember { identity, name: name.into() };
        let info = GroupInfo {
            title: "Crew".into(),
            sigil_key: vec![1],
            admin: me,
            members: vec![member(me, "Me"), member(dana, "Dana R."), member(asha, "Asha"), member(newcomer, "Neel")],
        };
        apply_info(&mut meta, &info, &me);
        assert_eq!(meta.peers.len(), 3);
        let dana = meta.peers.iter().find(|p| p.identity == dana).unwrap();
        assert!(dana.verified, "Dana stays verified when someone joins");
        assert_eq!(dana.onion.as_deref(), Some("dana.onion"));
        assert_eq!(dana.name, "Dana R.", "names still come from the admin's list");
        let neel = meta.peers.iter().find(|p| p.identity == newcomer).unwrap();
        assert!(!neel.verified, "only the newcomer is unverified");
    }
}
