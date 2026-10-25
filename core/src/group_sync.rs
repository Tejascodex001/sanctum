//! Members help deliver: group messages and files pass from member to
//! member, so they reach people who weren't online at the same time as
//! the sender, and a file isn't uploaded by its sender once per member.
//!
//! A group message is encrypted once and every member gets the identical
//! sealed bytes; a file's pieces are sealed with its own key and are
//! identical too. Members can already read the group, so passing those
//! bytes on costs no privacy, and nobody outside the group gets them.
//!
//! - **Keeping:** each member keeps the sealed bytes of the group's
//!   messages it sends and receives for a week ([`KEEP_FOR`]), at most
//!   [`KEEP_COUNT`] and [`KEEP_BYTES`] per group, sealed in the store like
//!   everything else, and remembers which files it holds in full. A message
//!   is known by a hash of its sealed bytes ([`sync_id`]), the same on
//!   every phone.
//! - **Catching up:** when two members' phones link, each tells the other
//!   what it has of every group they share ([`Frame::GroupHave`]); the
//!   other asks for what it's missing ([`Frame::GroupWant`]) and gets it as
//!   ordinary [`Frame::GroupMls`], processed like any group message (MLS
//!   refuses anything forged or replayed; duplicates are recognised by
//!   their hash and dropped before MLS sees them). Live links swap what's
//!   new every so often.
//! - **Offers first:** what the sender still owes a member who was away is
//!   offered (an entry with `owed`) instead of sent. A member who already
//!   got it from someone else confirms it ([`Frame::Ack`]), which counts as
//!   delivered, so nothing is uploaded twice and ✓✓ still means everyone
//!   has it. A brand-new message to someone online goes straight away.
//! - **Files:** the sender pushes a file to the first [`SEEDS`] members it
//!   reaches; the others fetch it ([`Frame::BlobWant`]) from any member
//!   holding it. A file's id is a keyed hash of its contents
//!   ([`blob_id`]), so a file assembled from pieces another member passed
//!   on is checked as a whole against the message that announced it before
//!   it's accepted.
//! - **Only members:** lists are swapped only with current members of the
//!   group (by MLS), and only with phones that understand them (learned in
//!   the handshake, see [`crate::protocol::FEATURES`]). Older phones get
//!   everything straight from the sender, as before.

use crate::media::{kv_name, read_chunks, Expected};
use crate::mls::{message_epoch, KEPT_EPOCHS};
use crate::protocol::{BlobId, Frame, HaveBlob, HaveEntry, MessageId, SyncId, FEATURE_GROUP_SYNC};
use crate::session::{now, Result, Session};
use crate::store::{ChatKind, ChatMeta, Kept, KeptItem, OutboxState};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Group messages and files are kept to pass on for a week...
pub const KEEP_FOR: u64 = 7 * 24 * 3600;
/// ...at most this many per group...
pub const KEEP_COUNT: usize = 300;
/// ...taking at most this much room per group.
pub const KEEP_BYTES: usize = 4 * 1024 * 1024;
/// At most this many messages and files in one list.
const MAX_HAVE: usize = 500;
/// The sender pushes a file to this many members; the rest fetch it from
/// whoever has it.
pub const SEEDS: usize = 3;
/// A member waits this long for someone other than the sender to have a
/// file before asking the sender for it.
const ASK_SENDER_AFTER: Duration = Duration::from_secs(45);
/// A file with no new piece for this long is asked of someone else.
const FETCH_STALL: Duration = Duration::from_secs(60);
/// An offer nobody answered is made again after this long.
const OFFER_AGAIN: Duration = Duration::from_secs(60);
/// On a live link, the whole list goes again this often (what's new goes
/// with every swap).
const FULL_EVERY: Duration = Duration::from_secs(600);
/// A message passed on that we couldn't read is asked for at most this
/// many times (the change that makes it readable may still be coming).
const TRIES: u8 = 3;
/// Kept messages are checked for age this often.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);

/// A group message's id: a hash of its sealed bytes, the same on every
/// member's phone.
pub fn sync_id(bytes: &[u8]) -> SyncId {
    let hash = blake3::Hasher::new_derive_key("sanctum group message v1").update(bytes).finalize();
    hash.as_bytes()[..16].try_into().unwrap()
}

/// A file's id: a hash of its contents keyed with its own key, so a file
/// pieced together from other members can be checked as a whole.
pub(crate) fn blob_id(key: &[u8; 32], plain: &[u8]) -> BlobId {
    let key = blake3::derive_key("sanctum blob id v1", key);
    blake3::keyed_hash(&key, plain).as_bytes()[..16].try_into().unwrap()
}

/// What members swap lists with, kept in memory only.
#[derive(Default)]
pub(crate) struct GroupSync {
    /// Linked members whose phones understand lists, by key.
    peers: HashMap<[u8; 32], PeerSync>,
    /// What's kept per group (ids only), oldest first; loaded when needed.
    index: HashMap<Vec<u8>, Vec<KeptRef>>,
    /// Members our files were pushed to (see [`SEEDS`]).
    seeds: HashMap<BlobId, HashSet<[u8; 32]>>,
    /// Members told not to ask us for our file (the seeds were taken):
    /// they're called back if a seed turns out not to have got it.
    declined: HashMap<BlobId, HashSet<[u8; 32]>>,
    /// Files being fetched now.
    pub(crate) fetching: HashMap<BlobId, Fetch>,
    /// When a file was announced to us.
    pub(crate) expected_at: HashMap<BlobId, Instant>,
    /// Members whose copy of a file didn't check out.
    bad: HashSet<(BlobId, [u8; 32])>,
    /// Passed-on messages we couldn't read, and how often.
    tried: HashMap<SyncId, u8>,
    pruned: Option<Instant>,
}

#[derive(Clone, Copy)]
struct KeptRef {
    id: [u8; 16],
    epoch: u64,
    blob: bool,
}

/// One linked member.
struct PeerSync {
    /// Outbox items that were waiting when the link came up: offered first,
    /// since the member may have got them from someone else meanwhile.
    backlog: HashSet<MessageId>,
    /// Outbox items offered, and when.
    offered: HashMap<MessageId, Instant>,
    /// Groups with an offer not answered yet: newer messages for them are
    /// offered too, so they arrive in order.
    awaiting: HashSet<Vec<u8>>,
    /// What the member has, or was told we have, per group.
    known: HashMap<Vec<u8>, HashSet<[u8; 16]>>,
    /// When we last sent the whole list, per group.
    full_at: HashMap<Vec<u8>, Instant>,
    /// Files the member holds, and whether they said to fetch now.
    holds: HashMap<BlobId, bool>,
}

pub(crate) struct Fetch {
    pub(crate) from: [u8; 32],
    /// The last piece came then.
    pub(crate) at: Instant,
    /// Pieces came from someone other than the file's sender: the whole
    /// file is checked against its id.
    pub(crate) relayed: bool,
}

/// What a flush offers instead of sending, per group.
#[derive(Default)]
pub(crate) struct Offers {
    groups: HashMap<Vec<u8>, (Vec<HaveEntry>, Vec<HaveBlob>)>,
    /// Whether the peer is a member, per group (looked up once a flush).
    member: HashMap<Vec<u8>, bool>,
}

impl Session {
    /// A link to `peer` came up and they told us what their phone can do.
    /// Call after [`Session::link_started`], before
    /// [`Session::on_link_up`].
    pub fn link_features(&mut self, peer: &[u8; 32], features: u32) -> Result<()> {
        // Whatever they were sending us went with the old link.
        self.sync.fetching.retain(|_, f| f.from != *peer);
        if features & FEATURE_GROUP_SYNC == 0 {
            self.sync.peers.remove(peer);
            return Ok(());
        }
        let backlog = self
            .store
            .outbox_for(peer)?
            .into_iter()
            .filter(|(_, item)| matches!(item.state, OutboxState::Group { .. } | OutboxState::Blob { .. }))
            .map(|(id, _)| id)
            .collect();
        self.sync.peers.insert(
            *peer,
            PeerSync {
                backlog,
                offered: HashMap::new(),
                awaiting: HashSet::new(),
                known: HashMap::new(),
                full_at: HashMap::new(),
                holds: HashMap::new(),
            },
        );
        Ok(())
    }

    /// The link to `peer` is gone (and no other has taken its place). A file
    /// we pushed to them that they never confirmed didn't get there, so it
    /// no longer uses up one of the few seeds; members who were told to
    /// get it from someone else are told they may ask us after all (a
    /// phone that just dropped off can look connected for a moment, and
    /// would otherwise keep the file from the member who is online).
    pub fn link_ended(&mut self, peer: &[u8; 32]) -> Result<()> {
        self.sync.peers.remove(peer);
        for set in self.sync.declined.values_mut() {
            set.remove(peer);
        }
        let waiting: Vec<BlobId> = self
            .store
            .outbox_for(peer)?
            .into_iter()
            .filter_map(|(_, item)| match item.state {
                OutboxState::Blob { blob, .. } => Some(blob),
                _ => None,
            })
            .collect();
        for blob in waiting {
            let Some(seeds) = self.sync.seeds.get_mut(&blob) else { continue };
            if !seeds.remove(peer) || seeds.len() >= SEEDS {
                continue;
            }
            for q in self.sync.declined.remove(&blob).unwrap_or_default() {
                if !self.sync.peers.contains_key(&q) {
                    continue;
                }
                // Offer it to them again, this time with a go-ahead.
                for (id, item) in self.store.outbox_for(&q)? {
                    if matches!(item.state, OutboxState::Blob { blob: b, .. } if b == blob) {
                        if let Some(ps) = self.sync.peers.get_mut(&q) {
                            ps.offered.remove(&id);
                        }
                    }
                }
                if let Some(ps) = self.sync.peers.get_mut(&q) {
                    ps.known.values_mut().for_each(|k| {
                        k.remove(&blob);
                    });
                }
                self.pokes.push(q);
            }
        }
        Ok(())
    }

    /// Whether `peer`'s phone swaps lists with us.
    pub(crate) fn syncs(&self, peer: &[u8; 32]) -> bool {
        self.sync.peers.contains_key(peer)
    }

    /// Of these linked peers, those with something new to swap: the network
    /// asks their links to flush. Call now and then (every 30 seconds).
    pub fn sync_due(&mut self, linked: &[[u8; 32]]) -> Result<Vec<[u8; 32]>> {
        let mut due = Vec::new();
        let linked: Vec<[u8; 32]> = linked.iter().filter(|p| self.syncs(p)).copied().collect();
        for peer in &linked {
            let mut news = false;
            for (_, meta) in self.shared_groups(peer)? {
                let group = meta.group_id;
                let refs = self.kept_index(&group)?.to_vec();
                let ps = &self.sync.peers[peer];
                let known = ps.known.get(&group);
                news |= ps.full_at.get(&group).is_none_or(|t| t.elapsed() >= FULL_EVERY)
                    || refs.iter().any(|r| !known.is_some_and(|k| k.contains(&r.id)));
            }
            // A file we're waiting for that they have.
            news |= self.sync.peers[peer].holds.keys().any(|b| {
                !self.blob_done(b) && self.store.kv_get::<Expected>(&kv_name(b)).is_ok_and(|e| e.is_some())
            });
            if news {
                due.push(*peer);
            }
        }
        Ok(due)
    }

    // ---- Keeping -----------------------------------------------------

    /// Keeps a group message (sent or received) to pass on, and remembers
    /// it as seen. Returns its id.
    pub(crate) fn keep_message(&mut self, group: &[u8], bytes: &[u8]) -> Result<SyncId> {
        let id = sync_id(bytes);
        self.store.mark_seen(&id)?;
        let epoch = match message_epoch(bytes) {
            Some(epoch) => epoch,
            None => self.mls.epoch(group)?,
        };
        let kept = Kept { id, at: now(), epoch, item: KeptItem::Message(bytes.to_vec()) };
        self.keep(group, kept)?;
        Ok(id)
    }

    /// Keeps note that we hold this group file in full.
    pub(crate) fn keep_blob(&mut self, group: &[u8], blob: BlobId, total: u32) -> Result<()> {
        let epoch = self.mls.epoch(group)?;
        self.keep(group, Kept { id: blob, at: now(), epoch, item: KeptItem::Blob { total } })
    }

    fn keep(&mut self, group: &[u8], kept: Kept) -> Result<()> {
        let blob = matches!(kept.item, KeptItem::Blob { .. });
        let (added, dropped) = self.store.kept_put(group, &kept, KEEP_COUNT, KEEP_BYTES)?;
        if let Some(index) = self.sync.index.get_mut(group) {
            index.retain(|r| !dropped.contains(&r.id));
            if added {
                index.push(KeptRef { id: kept.id, epoch: kept.epoch, blob });
            }
        }
        Ok(())
    }

    fn kept_index(&mut self, group: &[u8]) -> Result<&[KeptRef]> {
        if !self.sync.index.contains_key(group) {
            let refs = self
                .store
                .kept_list(group)?
                .into_iter()
                .map(|k| KeptRef { id: k.id, epoch: k.epoch, blob: matches!(k.item, KeptItem::Blob { .. }) })
                .collect();
            self.sync.index.insert(group.to_vec(), refs);
        }
        Ok(&self.sync.index[group])
    }

    fn kept_has(&mut self, group: &[u8], id: &[u8; 16]) -> Result<bool> {
        Ok(self.kept_index(group)?.iter().any(|r| r.id == *id))
    }

    /// We left the group or were removed: nothing of it to pass on.
    pub(crate) fn sync_forget(&mut self, group: &[u8]) -> Result<()> {
        self.store.kept_forget(group)?;
        self.sync.index.remove(group);
        Ok(())
    }

    /// Drops what's been kept for longer than [`KEEP_FOR`] (at most hourly,
    /// unless `force`).
    pub(crate) fn prune_kept(&mut self, now: u64, force: bool) -> Result<()> {
        if !force && self.sync.pruned.is_some_and(|t| t.elapsed() < PRUNE_EVERY) {
            return Ok(());
        }
        self.sync.pruned = Some(Instant::now());
        if self.store.kept_prune(now.saturating_sub(KEEP_FOR))? > 0 {
            self.sync.index.clear();
        }
        Ok(())
    }

    // ---- Membership --------------------------------------------------

    /// Active groups `peer` is a member of (by MLS), with us.
    fn shared_groups(&self, peer: &[u8; 32]) -> Result<Vec<(i64, ChatMeta)>> {
        let mut out = Vec::new();
        for (id, meta) in self.store.chats()? {
            if meta.kind == ChatKind::Group
                && !meta.group_id.is_empty()
                && meta.peers.iter().any(|p| &p.identity == peer)
                && self.mls.members(&meta.group_id)?.contains(peer)
            {
                out.push((id, meta));
            }
        }
        Ok(out)
    }

    /// Whether `peer` is a current member of this group of ours.
    pub(crate) fn is_member(&self, group: &[u8], peer: &[u8; 32]) -> Result<bool> {
        Ok(self.group_chat(group)?.is_some() && self.mls.members(group)?.contains(peer))
    }

    /// The group of a group chat.
    pub(crate) fn group_of_chat(&self, chat_id: i64) -> Option<Vec<u8>> {
        let meta = self.chat(chat_id).ok()?;
        (meta.kind == ChatKind::Group && !meta.group_id.is_empty()).then_some(meta.group_id)
    }

    /// `peer` has this message (they sent it, or told us).
    pub(crate) fn sync_heard(&mut self, peer: &[u8; 32], group: &[u8], id: &[u8; 16]) {
        if let Some(ps) = self.sync.peers.get_mut(peer) {
            ps.known.entry(group.to_vec()).or_default().insert(*id);
        }
    }

    // ---- Flushing ----------------------------------------------------

    /// A group message in our outbox for `peer`: true to send it now, false
    /// if it's offered instead (or was, a moment ago).
    pub(crate) fn sync_send_message(
        &mut self,
        peer: &[u8; 32],
        id: &MessageId,
        group: &[u8],
        cipher: &[u8],
        offers: &mut Offers,
    ) -> Result<bool> {
        if !self.syncs(peer) || !self.offers_member(offers, group, peer)? {
            return Ok(true);
        }
        let ps = self.sync.peers.get_mut(peer).unwrap();
        if ps.offered.get(id).is_some_and(|at| at.elapsed() < OFFER_AGAIN) {
            return Ok(false);
        }
        let sid = sync_id(cipher);
        if ps.backlog.contains(id) || ps.awaiting.contains(group) {
            let epoch = message_epoch(cipher).unwrap_or_default();
            offers.groups.entry(group.to_vec()).or_default().0.push(HaveEntry { id: sid, epoch, owed: Some(*id) });
            return Ok(false);
        }
        ps.known.entry(group.to_vec()).or_default().insert(sid);
        Ok(true)
    }

    /// A group file in our outbox for `peer`: true to push its pieces now
    /// (the first few members, see [`SEEDS`]), false if it's offered.
    pub(crate) fn sync_send_blob(
        &mut self,
        peer: &[u8; 32],
        id: &MessageId,
        blob: &BlobId,
        message: Option<(i64, i64)>,
        offers: &mut Offers,
    ) -> Result<bool> {
        if !self.syncs(peer) {
            return Ok(true);
        }
        let Some(group) = message.and_then(|(chat, _)| self.group_of_chat(chat)) else {
            return Ok(true);
        };
        if !self.offers_member(offers, &group, peer)? {
            return Ok(true);
        }
        let ps = self.sync.peers.get_mut(peer).unwrap();
        if ps.offered.get(id).is_some_and(|at| at.elapsed() < OFFER_AGAIN) {
            return Ok(false);
        }
        let seeds = self.sync.seeds.entry(*blob).or_default();
        let ask = seeds.contains(peer) || seeds.len() < SEEDS;
        if ask && !ps.backlog.contains(id) {
            seeds.insert(*peer);
            return Ok(true);
        }
        if !ask {
            self.sync.declined.entry(*blob).or_default().insert(*peer);
        }
        offers.groups.entry(group).or_default().1.push(HaveBlob { blob: *blob, owed: Some(*id), ask });
        Ok(false)
    }

    fn offers_member(&self, offers: &mut Offers, group: &[u8], peer: &[u8; 32]) -> Result<bool> {
        if let Some(member) = offers.member.get(group) {
            return Ok(*member);
        }
        let member = self.is_member(group, peer)?;
        offers.member.insert(group.to_vec(), member);
        Ok(member)
    }

    /// The lists for `peer`, after a flush: what we owe them (offered) and
    /// what's new in the groups we share. Then any files to fetch from them.
    pub(crate) fn sync_frames(&mut self, peer: &[u8; 32], mut offers: Offers) -> Result<Vec<Frame>> {
        if !self.syncs(peer) {
            return Ok(Vec::new());
        }
        let shared: Vec<Vec<u8>> = self.shared_groups(peer)?.into_iter().map(|(_, m)| m.group_id).collect();
        let mut groups = shared.clone();
        groups.extend(offers.groups.keys().filter(|g| !shared.contains(g)).cloned());
        let mut out = Vec::new();
        for group in groups {
            let (owed, mut blobs) = offers.groups.remove(&group).unwrap_or_default();
            let refs = if shared.contains(&group) { self.kept_index(&group)?.to_vec() } else { Vec::new() };
            let ready: Vec<bool> = refs.iter().map(|r| r.blob && self.blob_done(&r.id)).collect();
            let ps = self.sync.peers.get_mut(peer).unwrap();
            if !refs.is_empty() && ps.full_at.get(&group).is_none_or(|t| t.elapsed() >= FULL_EVERY) {
                ps.known.remove(&group);
                ps.full_at.insert(group.clone(), Instant::now());
            }
            let known = ps.known.entry(group.clone()).or_default();
            let mut owed: Vec<HaveEntry> = owed;
            let mut ids = Vec::new();
            // In the order we got them, so they arrive in order; what we owe
            // but no longer keep goes first (it's older).
            for (r, ready) in refs.iter().zip(ready) {
                if let Some(i) = owed.iter().position(|e| e.id == r.id) {
                    ids.push(owed.remove(i));
                } else if !known.contains(&r.id) {
                    if r.blob {
                        if !ready || blobs.iter().any(|b| b.blob == r.id) {
                            continue;
                        }
                        // Our own file: only the first few fetch it from us.
                        let ask = self.sync.seeds.get(&r.id).is_none_or(|s| s.len() < SEEDS || s.contains(peer));
                        if !ask {
                            self.sync.declined.entry(r.id).or_default().insert(*peer);
                        }
                        blobs.push(HaveBlob { blob: r.id, owed: None, ask });
                    } else {
                        ids.push(HaveEntry { id: r.id, epoch: r.epoch, owed: None });
                    }
                }
                known.insert(r.id);
            }
            owed.append(&mut ids);
            let mut ids = owed;
            // Too long: the oldest of what we merely have waits for the next
            // full list; what we owe always goes (the rest of it next time).
            while ids.len() > MAX_HAVE {
                match ids.iter().position(|e| e.owed.is_none()) {
                    Some(i) => ids.remove(i),
                    None => ids.pop().unwrap(),
                };
            }
            blobs.truncate(MAX_HAVE);
            let at = Instant::now();
            for owed in ids.iter().filter_map(|e| e.owed).chain(blobs.iter().filter_map(|b| b.owed)) {
                ps.offered.insert(owed, at);
            }
            if ids.iter().any(|e| e.owed.is_some()) {
                ps.awaiting.insert(group.clone());
            }
            if !ids.is_empty() || !blobs.is_empty() {
                out.push(Frame::GroupHave { group, ids, blobs });
            }
        }
        out.extend(self.blob_fetches(peer)?);
        Ok(out)
    }

    // ---- Answering ---------------------------------------------------

    pub(crate) fn on_sync_frame(&mut self, peer: &[u8; 32], frame: Frame) -> Result<Vec<Frame>> {
        match frame {
            Frame::GroupHave { group, ids, blobs } => self.on_group_have(peer, &group, ids, blobs),
            Frame::GroupWant { group, ids } => self.on_group_want(peer, &group, ids),
            Frame::BlobWant { group, blob, from } => self.on_blob_want(peer, &group, blob, from),
            _ => Ok(Vec::new()),
        }
    }

    /// A member's list: confirm what they offered and we have, ask for what
    /// we're missing.
    fn on_group_have(
        &mut self,
        peer: &[u8; 32],
        group: &[u8],
        ids: Vec<HaveEntry>,
        blobs: Vec<HaveBlob>,
    ) -> Result<Vec<Frame>> {
        let offered = ids.iter().any(|e| e.owed.is_some());
        if self.group_chat(group)?.is_none() {
            // Not ours (yet, or any more): what's owed to us comes as before,
            // and is handled like any message for a group we don't have.
            let want: Vec<SyncId> = ids.iter().filter(|e| e.owed.is_some()).map(|e| e.id).collect();
            return Ok(if offered { vec![Frame::GroupWant { group: group.to_vec(), ids: want }] } else { Vec::new() });
        }
        // Only members pass messages on; anyone may offer what they owe us.
        let member = self.mls.members(group)?.contains(peer);
        let current = self.mls.epoch(group)?;
        let joined = self.joined_epoch(group)?;
        let mut out = Vec::new();
        let mut want = Vec::new();
        for e in ids.into_iter().take(MAX_HAVE) {
            if e.owed.is_none() && !member {
                continue;
            }
            self.sync_heard(peer, group, &e.id);
            let have = self.kept_has(group, &e.id)? || self.store.is_seen(&e.id)?;
            let readable = e.epoch >= joined
                && e.epoch + KEPT_EPOCHS >= current
                && self.sync.tried.get(&e.id).is_none_or(|t| *t < TRIES);
            if have || !readable {
                out.extend(e.owed.map(|id| Frame::Ack { id }));
            } else {
                want.push(e.id);
            }
        }
        for b in blobs.into_iter().take(MAX_HAVE) {
            if b.owed.is_none() && !member {
                continue;
            }
            self.sync_heard(peer, group, &b.blob);
            if self.blob_done(&b.blob) {
                out.extend(b.owed.map(|id| Frame::Ack { id }));
            } else if member {
                if let Some(ps) = self.sync.peers.get_mut(peer) {
                    ps.holds.insert(b.blob, b.ask);
                }
            }
        }
        if offered || !want.is_empty() {
            out.push(Frame::GroupWant { group: group.to_vec(), ids: want });
        }
        out.extend(self.blob_fetches(peer)?);
        Ok(out)
    }

    /// A member asks for messages: what we owe them comes under its outbox
    /// id (their ack clears it), the rest under its hash.
    fn on_group_want(&mut self, peer: &[u8; 32], group: &[u8], ids: Vec<SyncId>) -> Result<Vec<Frame>> {
        if let Some(ps) = self.sync.peers.get_mut(peer) {
            ps.awaiting.remove(group);
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let member = self.is_member(group, peer)?;
        let mut owed = HashMap::new();
        for (id, item) in self.store.outbox_for(peer)? {
            if let OutboxState::Group { group: g, cipher } = item.state {
                if g == group {
                    owed.insert(sync_id(&cipher), (id, cipher));
                }
            }
        }
        let mut out = Vec::new();
        for sid in ids.into_iter().take(MAX_HAVE) {
            if let Some((id, bytes)) = owed.remove(&sid) {
                if let Some(ps) = self.sync.peers.get_mut(peer) {
                    ps.offered.insert(id, Instant::now());
                }
                out.push(Frame::GroupMls { group: group.to_vec(), id, bytes });
            } else if member {
                if let Some(Kept { item: KeptItem::Message(bytes), .. }) = self.store.kept_get(group, &sid)? {
                    out.push(Frame::GroupMls { group: group.to_vec(), id: sid, bytes });
                }
            }
            self.sync_heard(peer, group, &sid);
        }
        Ok(out)
    }

    /// A member asks for a file's pieces.
    fn on_blob_want(&mut self, peer: &[u8; 32], group: &[u8], blob: BlobId, from: u32) -> Result<Vec<Frame>> {
        let owed = self.store.outbox_for(peer)?.into_iter().find_map(|(id, item)| match item.state {
            OutboxState::Blob { blob: b, .. } if b == blob => Some(id),
            _ => None,
        });
        if owed.is_none() && !(self.is_member(group, peer)? && self.kept_has(group, &blob)?) {
            return Ok(Vec::new());
        }
        let Ok(chunks) = read_chunks(&self.blob_path(&blob)) else {
            return Ok(Vec::new());
        };
        let id = match owed {
            Some(id) => {
                // The original sender: they're one of the few we push to.
                self.sync.seeds.entry(blob).or_default().insert(*peer);
                self.blobs_sent.insert((*peer, id), Instant::now());
                id
            }
            None => blob,
        };
        let total = chunks.len() as u32;
        Ok(chunks
            .into_iter()
            .enumerate()
            .skip(from as usize)
            .map(|(index, bytes)| Frame::BlobChunk { id, blob, index: index as u32, total, bytes })
            .collect())
    }

    /// Files we're waiting for that `peer` has: ask for them, unless someone
    /// else is sending them. The file's sender is asked only if it says so,
    /// or after a while (others who have it are asked first).
    pub(crate) fn blob_fetches(&mut self, peer: &[u8; 32]) -> Result<Vec<Frame>> {
        let Some(holds) = self.sync.peers.get(peer).map(|ps| ps.holds.clone()) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (blob, ask) in holds {
            if self.blob_done(&blob) {
                if let Some(ps) = self.sync.peers.get_mut(peer) {
                    ps.holds.remove(&blob);
                }
                continue;
            }
            let Some(expected) = self.store.kv_get::<Expected>(&kv_name(&blob))? else {
                continue;
            };
            let Some(group) = self.group_of_chat(expected.chat_id) else { continue };
            if self.sync.bad.contains(&(blob, *peer))
                || self.sync.fetching.get(&blob).is_some_and(|f| f.at.elapsed() < FETCH_STALL)
                || !self.mls.members(&group)?.contains(peer)
            {
                continue;
            }
            let waited = self.sync.expected_at.get(&blob).is_none_or(|t| t.elapsed() >= ASK_SENDER_AFTER);
            if !ask && !waited {
                continue;
            }
            let from = self.blob_progress(&blob);
            let relayed = expected.from != *peer || self.sync.fetching.get(&blob).is_some_and(|f| f.relayed);
            self.sync.fetching.insert(blob, Fetch { from: *peer, at: Instant::now(), relayed });
            out.push(Frame::BlobWant { group, blob, from });
        }
        Ok(out)
    }

    /// A file's pieces were checked as a whole and don't match: not from
    /// this member again.
    pub(crate) fn blob_rejected(&mut self, blob: &BlobId, peer: &[u8; 32]) {
        self.sync.bad.insert((*blob, *peer));
        self.sync.fetching.remove(blob);
    }

    /// The epoch we joined a group in (0 for groups we made, or joined
    /// before this was recorded).
    fn joined_epoch(&self, group: &[u8]) -> Result<u64> {
        Ok(self.store.kv_get::<u64>(&joined_name(group))?.unwrap_or(0))
    }

    pub(crate) fn set_joined_epoch(&self, group: &[u8]) -> Result<()> {
        self.store.kv_put(&joined_name(group), &self.mls.epoch(group)?)?;
        Ok(())
    }

    /// A passed-on message we couldn't read: maybe later, a few times.
    pub(crate) fn sync_unreadable(&mut self, group: &[u8], bytes: &[u8]) -> Result<()> {
        let id = sync_id(bytes);
        let tries = self.sync.tried.entry(id).or_insert(0);
        *tries += 1;
        let current = self.mls.epoch(group)?;
        // From an epoch we've left behind, it never will be.
        if *tries >= TRIES || message_epoch(bytes).is_none_or(|e| e < current) {
            self.store.mark_seen(&id)?;
        }
        Ok(())
    }
}

fn joined_name(group: &[u8]) -> Vec<u8> {
    [b"group-joined:".as_slice(), group].concat()
}

#[cfg(test)]
impl Session {
    /// Pretends every file was announced long ago (tests can't wait).
    pub(crate) fn sync_waited(&mut self) {
        self.sync.expected_at.clear();
    }

    pub(crate) fn kept_count(&mut self, group: &[u8]) -> usize {
        self.kept_index(group).map_or(0, |r| r.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Media;
    use crate::session::tests::{befriend, connect, group_of, link, outgoing, photo, Phone, Traffic};

    /// A group of `n` people: the first is its admin, friends with everyone;
    /// the others needn't be friends. Returns the phones and each one's chat.
    pub(crate) fn group(n: usize) -> (Vec<Phone>, Vec<i64>) {
        let mut phones: Vec<Phone> = (0..n).map(|_| Phone::new()).collect();
        let (admin, rest) = phones.split_first_mut().unwrap();
        let mut friends = Vec::new();
        for p in rest.iter_mut() {
            friends.push(befriend(admin, p).0);
            connect(admin, p);
        }
        let (chat, _) = admin.s.create_group("Crew", &friends).unwrap();
        // Each joins as the admin reaches them, then hears of the others.
        for p in rest.iter_mut() {
            connect(admin, p);
        }
        for p in rest.iter_mut() {
            connect(admin, p);
        }
        let mut chats = vec![chat];
        chats.extend(rest.iter().map(|p| group_of(p, "Crew").expect("joined")));
        for (p, c) in phones.iter().zip(&chats) {
            assert_eq!(p.s.group_view(*c).unwrap().members.len(), n);
        }
        (phones, chats)
    }

    fn pair(phones: &mut [Phone], a: usize, b: usize) -> (&mut Phone, &mut Phone) {
        assert!(a != b);
        if a < b {
            let (x, y) = phones.split_at_mut(b);
            (&mut x[a], &mut y[0])
        } else {
            let (x, y) = phones.split_at_mut(a);
            (&mut y[0], &mut x[b])
        }
    }

    fn texts(p: &Phone, chat: i64) -> Vec<String> {
        p.s.store.messages(chat, None).unwrap().into_iter().map(|m| m.body).collect()
    }

    fn group_id(p: &Phone, chat: i64) -> Vec<u8> {
        p.s.chat(chat).unwrap().group_id
    }

    fn is_new(f: &Frame) -> bool {
        matches!(f, Frame::GroupHave { .. } | Frame::GroupWant { .. } | Frame::BlobWant { .. })
    }

    fn mls_frames(frames: &[Frame]) -> usize {
        frames.iter().filter(|f| matches!(f, Frame::GroupMls { .. })).count()
    }

    fn chunks(frames: &[Frame]) -> usize {
        frames.iter().filter(|f| matches!(f, Frame::BlobChunk { .. })).count()
    }

    #[test]
    fn a_message_reaches_a_member_while_its_sender_is_offline() {
        // Alice (the admin), Bob and Carol; Bob and Carol aren't friends.
        let (mut p, chats) = group(3);
        p[1].s.send_to_chat(chats[1], "running late", None).unwrap();
        // Only Alice is online with Bob; then Bob goes offline.
        let (bob, alice) = pair(&mut p, 1, 0);
        connect(bob, alice);
        assert!(!bob.texts(chats[1])[0].2, "Carol doesn't have it yet");
        // Carol comes online and reaches Alice: Alice passes it on.
        let (alice, carol) = pair(&mut p, 0, 2);
        let t = link(alice, carol);
        assert_eq!(texts(&p[2], chats[2]), ["running late"]);
        let m = p[2].s.store.messages(chats[2], None).unwrap().pop().unwrap();
        assert_eq!(m.sender, Some(p[1].pk()), "it's Bob's, though Alice passed it on");
        assert!(t.from_dialer.iter().any(|f| matches!(f, Frame::GroupHave { .. })));
        // Bob is back and reaches Carol: he offers it rather than sending
        // it, Carol says she has it, and it counts as delivered.
        let (bob, carol) = pair(&mut p, 1, 2);
        let t = link(bob, carol);
        assert_eq!(mls_frames(&t.from_dialer), 0, "not uploaded again");
        assert!(bob.texts(chats[1])[0].2, "✓✓: everyone has it");
        assert_eq!(texts(&p[2], chats[2]), ["running late"], "no duplicate");
        assert!(p[1].s.pending_peers().unwrap().is_empty());
    }

    #[test]
    fn duplicates_are_taken_once_without_errors() {
        let (mut p, chats) = group(3);
        let bob_key = p[1].pk();
        let carol_key = p[2].pk();
        p[1].s.send_to_chat(chats[1], "pizza?", None).unwrap();
        // Bob's own copy for Carol, as it would go out.
        let (id, bytes) = p[1]
            .s
            .store
            .outbox_for(&carol_key)
            .unwrap()
            .into_iter()
            .find_map(|(id, item)| match item.state {
                OutboxState::Group { cipher, .. } => Some((id, cipher)),
                _ => None,
            })
            .unwrap();
        let group = group_id(&p[1], chats[1]);
        // Carol gets it from Alice first...
        let (bob, alice) = pair(&mut p, 1, 0);
        connect(bob, alice);
        let (alice, carol) = pair(&mut p, 0, 2);
        connect(alice, carol);
        assert_eq!(texts(&p[2], chats[2]), ["pizza?"]);
        // ...then Bob's copy arrives, and again from Alice: each is simply
        // acknowledged.
        let direct = Frame::GroupMls { group: group.clone(), id, bytes: bytes.clone() };
        assert_eq!(p[2].s.on_frame(&bob_key, direct).unwrap(), vec![Frame::Ack { id }]);
        let sid = sync_id(&bytes);
        let passed_on = Frame::GroupMls { group, id: sid, bytes };
        let alice_key = p[0].pk();
        p[2].s.take_events();
        assert_eq!(p[2].s.on_frame(&alice_key, passed_on).unwrap(), vec![Frame::Ack { id: sid }]);
        assert_eq!(texts(&p[2], chats[2]), ["pizza?"]);
        assert!(p[2].s.take_events().is_empty(), "no second notification");
    }

    #[test]
    fn only_members_swap_lists() {
        let (mut p, chats) = group(3);
        // Dave is Alice's friend, but not in the group.
        let mut dave = Phone::new();
        befriend(&mut p[0], &mut dave);
        connect(&mut p[0], &mut dave);
        p[1].s.send_to_chat(chats[1], "members only", None).unwrap();
        let (bob, alice) = pair(&mut p, 1, 0);
        connect(bob, alice);
        let group = group_id(&p[0], chats[0]);
        let t = link(&mut p[0], &mut dave);
        assert!(!t.from_dialer.iter().any(is_new), "nothing about the group goes to Dave");
        // Dave asks anyway: nothing.
        let sid = p[0].s.kept_index(&group).unwrap().last().unwrap().id;
        let dpk = dave.pk();
        let want = Frame::GroupWant { group: group.clone(), ids: vec![sid] };
        assert!(p[0].s.on_frame(&dpk, want).unwrap().is_empty());
        let have = Frame::GroupHave {
            group: group.clone(),
            ids: vec![HaveEntry { id: [1; 16], epoch: 9, owed: None }],
            blobs: vec![HaveBlob { blob: [2; 16], owed: None, ask: true }],
        };
        assert!(p[0].s.on_frame(&dpk, have).unwrap().is_empty(), "nothing wanted from a non-member");
        let blob = Frame::BlobWant { group, blob: [2; 16], from: 0 };
        assert!(p[0].s.on_frame(&dpk, blob).unwrap().is_empty());
    }

    #[test]
    fn older_builds_get_everything_from_the_sender() {
        let (mut p, chats) = group(3);
        p[2].features = 0;
        let (path, bytes) = photo(&p[1], 2 * crate::media::CHUNK + 5);
        p[1].s.send_media(chats[1], outgoing(&path), None).unwrap();
        p[1].s.send_to_chat(chats[1], "see?", None).unwrap();
        let (bob, alice) = pair(&mut p, 1, 0);
        connect(bob, alice);
        // Alice never passes anything to Carol's older phone...
        let (alice, carol) = pair(&mut p, 0, 2);
        let t = link(alice, carol);
        assert!(!t.from_dialer.iter().any(is_new));
        assert!(texts(&p[2], chats[2]).is_empty());
        // ...Bob sends it all to her himself, as before.
        let (bob, carol) = pair(&mut p, 1, 2);
        let t = link(bob, carol);
        assert!(!t.from_dialer.iter().any(is_new));
        assert_eq!(chunks(&t.from_dialer), 3);
        assert_eq!(texts(&p[2], chats[2]), ["sunset", "see?"]);
        assert_eq!(p[2].s.media_bytes(chats[2], 0).unwrap().unwrap(), bytes);
        assert!(p[1].texts(chats[1]).iter().all(|m| m.2));
    }

    #[test]
    fn files_are_fetched_from_members_who_have_them() {
        // Alice (the sender) and four others.
        let (mut p, chats) = group(5);
        let (path, bytes) = photo(&p[0], 3 * crate::media::CHUNK + 7);
        p[0].s.send_media(chats[0], outgoing(&path), None).unwrap();
        // The first few she reaches get the file from her...
        for i in 1..=SEEDS {
            let (alice, other) = pair(&mut p, 0, i);
            let t = link(alice, other);
            assert_eq!(chunks(&t.from_dialer), 4);
        }
        // ...the next gets the message, and only an offer of the file.
        let (alice, eve) = pair(&mut p, 0, 4);
        let t = link(alice, eve);
        assert_eq!(chunks(&t.from_dialer), 0);
        assert_eq!(texts(&p[4], chats[4]), ["sunset"]);
        assert_eq!(p[4].s.media_bytes(chats[4], 0).unwrap(), None);
        // Eve reaches Bob, who has it: she fetches it from him, checked
        // against the message.
        let (eve, bob) = pair(&mut p, 4, 1);
        let t = link(eve, bob);
        assert_eq!(chunks(&t.from_reached), 4);
        assert_eq!(p[4].s.media_bytes(chats[4], 0).unwrap().unwrap(), bytes);
        assert!(!p[0].texts(chats[0])[0].2);
        // Alice reaches Eve again: Eve says she has it; nothing is uploaded.
        let (alice, eve) = pair(&mut p, 0, 4);
        let t = link(alice, eve);
        assert_eq!(chunks(&t.from_dialer) + mls_frames(&t.from_dialer), 0);
        assert!(p[0].texts(chats[0])[0].2, "✓✓ once everyone has the file");
    }

    #[test]
    fn the_sender_is_asked_when_nobody_else_has_the_file() {
        let (mut p, chats) = group(5);
        let (path, bytes) = photo(&p[0], 2 * crate::media::CHUNK);
        p[0].s.send_media(chats[0], outgoing(&path), None).unwrap();
        for i in 1..=SEEDS {
            let (alice, other) = pair(&mut p, 0, i);
            connect(alice, other);
        }
        let (alice, eve) = pair(&mut p, 0, 4);
        connect(alice, eve);
        assert_eq!(p[4].s.media_bytes(chats[4], 0).unwrap(), None);
        // Nobody else turns up for a while: Eve asks Alice.
        p[4].s.sync_waited();
        let (alice, eve) = pair(&mut p, 0, 4);
        let t = link(alice, eve);
        assert_eq!(chunks(&t.from_dialer), 2);
        assert_eq!(p[4].s.media_bytes(chats[4], 0).unwrap().unwrap(), bytes);
        assert!(p[0].texts(chats[0])[0].2);
    }

    #[test]
    fn a_seed_that_dropped_off_frees_its_place() {
        let (mut p, chats) = group(5);
        let (path, bytes) = photo(&p[0], 3 * crate::media::CHUNK + 7);
        // Eve is connected when Alice shares the file, and is gone a moment
        // later (her phone dropped off before Alice noticed).
        let (alice_pk, eve_pk) = (p[0].pk(), p[4].pk());
        let features = p[0].features & p[4].features;
        p[0].s.link_started(&eve_pk);
        p[0].s.link_features(&eve_pk, features).unwrap();
        p[0].s.send_media(chats[0], outgoing(&path), None).unwrap();
        assert_eq!(chunks(&p[0].s.on_link_up(&eve_pk).unwrap()), 4);
        // Two others get it from Alice; the next one is only told to get it
        // from someone else.
        for i in 1..=SEEDS - 1 {
            let (alice, other) = pair(&mut p, 0, i);
            assert_eq!(chunks(&link(alice, other).from_dialer), 4);
        }
        let (alice, dave) = pair(&mut p, 0, 3);
        assert_eq!(chunks(&link(alice, dave).from_dialer), 0);
        assert_eq!(p[3].s.media_bytes(chats[3], 0).unwrap(), None);
        // Alice finds Eve's link is dead: Dave, still connected, is sent
        // the file at once instead of waiting for a long time.
        p[0].s.take_pokes();
        p[0].s.link_ended(&eve_pk).unwrap();
        let dave_pk = p[3].pk();
        assert_eq!(p[0].s.take_pokes(), [dave_pk]);
        let mut to_dave = p[0].s.on_link_up(&dave_pk).unwrap();
        // (Dave was linked before Alice shared the file, so it's offered
        // with a go-ahead; he asks, and she sends it.)
        let mut sent = 0;
        while !to_dave.is_empty() {
            sent += chunks(&to_dave);
            let mut to_alice = Vec::new();
            for f in to_dave.drain(..) {
                to_alice.extend(p[3].s.on_frame(&alice_pk, f).unwrap());
            }
            for f in to_alice {
                to_dave.extend(p[0].s.on_frame(&dave_pk, f).unwrap());
            }
        }
        assert_eq!(sent, 4);
        assert_eq!(p[3].s.media_bytes(chats[3], 0).unwrap().unwrap(), bytes);
    }

    #[test]
    fn a_forged_copy_of_a_file_is_refused() {
        let (mut p, chats) = group(5);
        let (path, bytes) = photo(&p[0], 2 * crate::media::CHUNK + 1);
        p[0].s.send_media(chats[0], outgoing(&path), None).unwrap();
        for i in 1..=SEEDS {
            let (alice, other) = pair(&mut p, 0, i);
            connect(alice, other);
        }
        let (alice, eve) = pair(&mut p, 0, 4);
        connect(alice, eve);
        // Bob swaps his copy for another picture sealed with the same key:
        // every piece opens, but it isn't what Alice sent.
        let media: Media = p[1].s.store.message(chats[1], 0).unwrap().unwrap().media.unwrap();
        let forged = forge(&media, 2 * crate::media::CHUNK + 1);
        std::fs::write(p[1].s.blob_path(&media.blob), forged).unwrap();
        let (eve, bob) = pair(&mut p, 4, 1);
        connect(eve, bob);
        assert_eq!(p[4].s.media_bytes(chats[4], 0).unwrap(), None, "refused");
        // From Carol, the real one.
        let (eve, carol) = pair(&mut p, 4, 2);
        connect(eve, carol);
        assert_eq!(p[4].s.media_bytes(chats[4], 0).unwrap().unwrap(), bytes);
    }

    /// Other bytes, sealed as `media`'s pieces would be.
    fn forge(media: &Media, len: usize) -> Vec<u8> {
        use chacha20poly1305::aead::{Aead, KeyInit};
        let cipher = chacha20poly1305::XChaCha20Poly1305::new((&media.key).into());
        let fake = vec![0x42u8; len];
        let mut out = Vec::new();
        for (i, piece) in fake.chunks(crate::media::CHUNK).enumerate() {
            let sealed = cipher.encrypt(&crate::media::nonce(&media.blob, i as u32), piece).unwrap();
            out.extend((sealed.len() as u32).to_le_bytes());
            out.extend(sealed);
        }
        out
    }

    #[test]
    fn kept_messages_go_after_a_week_and_are_capped() {
        let (mut p, chats) = group(2);
        let group = group_id(&p[0], chats[0]);
        p[0].s.send_to_chat(chats[0], "hello", None).unwrap();
        assert!(p[0].s.kept_count(&group) > 0);
        p[0].s.prune_kept(now() + KEEP_FOR - 60, true).unwrap();
        assert!(p[0].s.kept_count(&group) > 0, "not a week yet");
        p[0].s.prune_kept(now() + KEEP_FOR + 60, true).unwrap();
        assert_eq!(p[0].s.kept_count(&group), 0);
        // Caps, by count and by size: the oldest go first.
        let store = &p[0].s.store;
        for i in 0..20u8 {
            let kept = Kept { id: [i; 16], at: now(), epoch: 0, item: KeptItem::Message(vec![i; 1000]) };
            store.kept_put(b"g", &kept, 8, 1 << 20).unwrap();
        }
        let ids: Vec<u8> = store.kept_list(b"g").unwrap().iter().map(|k| k.id[0]).collect();
        assert_eq!(ids, (12..20).collect::<Vec<u8>>());
        let big = Kept { id: [99; 16], at: now(), epoch: 0, item: KeptItem::Message(vec![0; 5000]) };
        let (added, dropped) = store.kept_put(b"g", &big, 8, 6000).unwrap();
        assert!(added && !dropped.is_empty());
        assert!(store.kept_list(b"g").unwrap().len() <= 2);
        // Leaving forgets it all.
        let (_, gone) = store.kept_put(b"h", &big, 8, 1 << 20).unwrap();
        assert!(gone.is_empty());
        store.kept_forget(b"h").unwrap();
        assert!(store.kept_list(b"h").unwrap().is_empty());
    }

    #[test]
    fn leaving_forgets_what_was_kept() {
        let (mut p, chats) = group(3);
        let group = group_id(&p[1], chats[1]);
        p[1].s.send_to_chat(chats[1], "bye", None).unwrap();
        assert!(p[1].s.kept_count(&group) > 0);
        p[1].s.group_leave(chats[1]).unwrap();
        assert!(p[1].s.store.kept_list(&group).unwrap().is_empty());
    }

    /// A simulated group of 16 phones (setting up a big group's encryption
    /// is slow in a debug build; see the ignored test below for 30 and 50).
    #[test]
    fn a_big_group_shares_the_load() {
        scale(16);
    }

    /// The numbers in docs/DESIGN.md: `cargo test -p sanctum-core --release
    /// big_groups -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn big_groups_numbers() {
        scale(30);
        scale(50);
    }

    /// What the sender uploads for a message and for a 1 MB photo in a group
    /// of `n`, and a message that reaches everyone while its sender is
    /// offline.
    fn scale(n: usize) {
        #[allow(non_snake_case)]
        let N = n;
        let started = std::time::Instant::now();
        let (mut p, chats) = group(N);
        let setup = started.elapsed();

        // Text: straight from the sender to everyone online.
        p[1].s.send_to_chat(chats[1], "dinner at 8", None).unwrap();
        let mut text_up = 0;
        for i in (0..N).filter(|i| *i != 1) {
            let (bob, other) = pair(&mut p, 1, i);
            text_up += link(bob, other).dialer_sent;
        }
        assert!(p[1].texts(chats[1]).last().unwrap().2);

        // A 1 MB photo: pushed to a few, fetched by the rest from them.
        let size = 1024 * 1024;
        let (path, bytes) = photo(&p[1], size);
        p[1].s.send_media(chats[1], outgoing(&path), None).unwrap();
        let photo_started = std::time::Instant::now();
        let mut sender_up = 0;
        for i in (0..N).filter(|i| *i != 1) {
            let (bob, other) = pair(&mut p, 1, i);
            sender_up += link(bob, other).dialer_sent;
        }
        // Members who meet pass it on (each reaches the one before it).
        let mut others_up = 0;
        let order: Vec<usize> = (0..N).filter(|i| *i != 1).collect();
        for w in order.windows(2) {
            let (a, b) = pair(&mut p, w[1], w[0]);
            let t: Traffic = link(a, b);
            others_up += t.dialer_sent + t.reached_sent;
        }
        // Bob comes back to everyone: they confirm, nothing is re-sent.
        for i in (0..N).filter(|i| *i != 1) {
            let (bob, other) = pair(&mut p, 1, i);
            sender_up += link(bob, other).dialer_sent;
        }
        let photo_time = photo_started.elapsed();
        for (i, c) in chats.iter().enumerate().filter(|(i, _)| *i != 1) {
            let seq = p[i].s.store.messages_indexed(*c, None).unwrap().last().unwrap().0;
            assert_eq!(p[i].s.media_bytes(*c, seq).unwrap().unwrap().len(), bytes.len());
        }
        assert!(p[1].texts(chats[1]).last().unwrap().2, "✓✓");
        // Without members' help Bob would upload it N - 1 times.
        assert!(sender_up < (SEEDS + 1) * size, "sender uploaded {sender_up}");

        // Bob writes and goes offline after reaching only Alice; the others
        // get it from Alice, or from whoever got it from her.
        p[1].s.send_to_chat(chats[1], "on my way", None).unwrap();
        let (bob, alice) = pair(&mut p, 1, 0);
        connect(bob, alice);
        for i in 2..N {
            let (alice, other) = pair(&mut p, if i % 2 == 0 { 0 } else { i - 1 }, i);
            connect(alice, other);
        }
        for (i, c) in chats.iter().enumerate().filter(|(i, _)| *i != 1) {
            assert_eq!(texts(&p[i], *c).last().unwrap(), "on my way", "member {i}");
        }
        println!(
            "group of {N}: setup {setup:?}; text: sender sent {text_up} bytes in all; \
             1 MB photo: sender sent {sender_up} bytes ({:.1}x the photo; {}x without help), \
             members passed on {others_up} bytes, {photo_time:?}",
            sender_up as f64 / size as f64,
            N - 1
        );
    }
}
