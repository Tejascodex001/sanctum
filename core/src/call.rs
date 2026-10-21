//! Voice calls between two friends: ringing, answering and hanging up, and
//! the sealed sound in between.
//!
//! **Signaling** travels inside the pair's MLS group like a message
//! ([`Content::CallInvite`], [`Content::CallAnswer`], [`Content::CallEnd`]),
//! through the outbox, so it is end-to-end encrypted and arrives in order.
//! The invite carries a fresh random key for the call. An invite that
//! arrives more than [`RING_WINDOW_MS`] after it was made doesn't ring: it
//! becomes a missed call.
//!
//! **Sound** travels as [`Frame::CallMedia`] over the link that is up right
//! now (Tor, or a nearby link), never stored, queued or resent: a late
//! packet is useless. Each packet is sealed with ChaCha20-Poly1305 under a
//! key for its direction, derived from the call key; the nonce is the
//! direction and the packet's number, and numbers already seen are
//! rejected. Nobody in between (Tor relays, the local network) can listen
//! or replay, even on nearby links, which the link itself doesn't encrypt.
//!
//! What arrives is played through a [`JitterBuffer`], which smooths out the
//! network's uneven timing (Tor's especially) by waiting a little longer
//! when packets come unevenly.
//!
//! The media side ([`MediaHub`]) is shared with the network, which hands it
//! incoming packets without taking the session's lock, and with the phone's
//! audio, which seals and pulls packets 25 times a second.

use crate::protocol::{CallAnswer, CallId, CallOffer, Content, Frame, MessageId};
use crate::session::{clip, Event, Result, Session, SessionError};
use crate::store::{CallLog, ChatKind, OutboxItem, OutboxState, StoredMessage};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// An invite arriving later than this after it was made doesn't ring.
pub const RING_WINDOW_MS: u64 = 45_000;
/// The caller gives up after this long without an answer...
pub const RING_FOR_MS: u64 = 45_000;
/// ...or after this long if the friend's phone hasn't even got the call.
pub const UNREACHABLE_AFTER_MS: u64 = 25_000;
/// The phone called stops ringing a little after the caller gives up, in
/// case the caller's "never mind" doesn't arrive.
const RING_HERE_MS: u64 = 50_000;
/// Talking, but nothing heard from the friend for this long: the call
/// dropped.
pub const DROPPED_AFTER: Duration = Duration::from_secs(30);
/// Each audio packet holds this much sound (two 20 ms codec frames).
pub const AUDIO_PACKET_MS: u32 = 40;
/// Sound formats, by the names phones offer them under.
pub const OPUS: &str = "opus";
pub const AMR_WB: &str = "amr-wb";
const MAX_CODECS: usize = 8;
const MAX_CODEC_NAME: usize = 24;
/// Media packets are small; anything bigger is refused.
const MAX_MEDIA: usize = 16 * 1024;
/// Calls over lately, so a late copy of their invite never rings.
const DONE_KEPT: usize = 64;

/// Packet kinds inside a sealed media packet.
pub const AUDIO: u8 = 0;
/// For later builds; this one ignores it.
pub const VIDEO: u8 = 1;

/// Which way a packet goes: part of its nonce and of its key.
const FROM_CALLER: u8 = 0;
const FROM_CALLEE: u8 = 1;

/// Why a call ended, on the wire ([`Content::CallEnd`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Hangup = 0,
    Declined = 1,
    Busy = 2,
    /// The caller gave up before an answer.
    Cancelled = 3,
    /// The phones couldn't talk (no sound format in common, or the
    /// connection was lost).
    Failed = 4,
}

impl EndReason {
    pub fn from_wire(b: u8) -> Self {
        match b {
            1 => Self::Declined,
            2 => Self::Busy,
            3 => Self::Cancelled,
            4 => Self::Failed,
            _ => Self::Hangup,
        }
    }
}

/// How a call went, as this phone saw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// We talked, and someone hung up.
    Talked = 0,
    /// They called; we didn't answer (or were in another call).
    Missed = 1,
    /// Declined: by us (they called) or by them (we called).
    Declined = 2,
    /// We called; nobody answered.
    NoAnswer = 3,
    /// We called; their phone couldn't be reached.
    Unreachable = 4,
    /// We called; they were in another call.
    Busy = 5,
    /// The phones couldn't agree how to talk.
    Failed = 6,
    /// We talked until the connection was lost.
    Dropped = 7,
    /// We called and hung up before they answered.
    Cancelled = 8,
}

impl Outcome {
    pub fn from_wire(b: u8) -> Self {
        match b {
            1 => Self::Missed,
            2 => Self::Declined,
            3 => Self::NoAnswer,
            4 => Self::Unreachable,
            5 => Self::Busy,
            6 => Self::Failed,
            7 => Self::Dropped,
            8 => Self::Cancelled,
            _ => Self::Talked,
        }
    }

    /// What the chat shows for a call that went like this.
    pub fn describe(self, seconds: u32) -> String {
        match self {
            Self::Talked | Self::Dropped => format!("Voice call · {}", talk_time(seconds)),
            Self::Missed => "Missed call".into(),
            Self::Declined => "Declined call".into(),
            Self::NoAnswer => "No answer".into(),
            Self::Unreachable => "Call didn't go through".into(),
            Self::Busy => "Busy".into(),
            Self::Failed => "Call didn't connect".into(),
            Self::Cancelled => "Cancelled call".into(),
        }
    }
}

/// "4:12", or "1:02:03" past an hour.
pub fn talk_time(seconds: u32) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Where a call stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// We're calling; their phone hasn't got the call yet.
    Connecting,
    /// Ringing: on their phone (we called) or on ours.
    Ringing,
    /// Talking.
    Active,
    /// Over, and how it went.
    Ended(Outcome),
}

/// A call, for the app.
#[derive(Debug, Clone, PartialEq)]
pub struct CallView {
    pub id: CallId,
    /// The friend's chat.
    pub chat: i64,
    pub peer: [u8; 32],
    /// The friend, as this phone calls them.
    pub name: String,
    /// We called.
    pub outgoing: bool,
    pub video: bool,
    pub phase: Phase,
    /// When we started talking (Unix milliseconds).
    pub answered_at: Option<u64>,
    /// The sound format we record into, and the one we play (set once
    /// talking).
    pub send_audio: String,
    pub recv_audio: String,
}

/// The sound formats this phone can play and record, best first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioCaps {
    pub play: Vec<String>,
    pub record: Vec<String>,
}

pub(crate) struct Call {
    view: CallView,
    /// Ours (we called) or theirs: the key and what each side can do.
    offer: CallOffer,
    /// When it started on this phone (Unix milliseconds).
    since: u64,
    /// We called: the invite's outbox id, until their phone has it.
    invite: Option<MessageId>,
}

/// The call going on now, if any. Held in memory only.
#[derive(Default)]
pub(crate) struct Calls {
    now: Option<Call>,
    done: VecDeque<CallId>,
}

impl Calls {
    fn remember(&mut self, id: CallId) {
        if !self.done.contains(&id) {
            self.done.push_back(id);
            if self.done.len() > DONE_KEPT {
                self.done.pop_front();
            }
        }
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The first of `wanted` (in its order) that is also in `available`.
fn pick(wanted: &[String], available: &[String]) -> Option<String> {
    wanted.iter().find(|c| available.contains(c)).cloned()
}

/// Codec lists from the other phone: a few short names at most.
fn tidy(list: &mut Vec<String>) {
    list.truncate(MAX_CODECS);
    for name in list.iter_mut() {
        *name = clip(name, MAX_CODEC_NAME);
    }
}

impl Session {
    /// The call ringing, connecting or talking now, if any.
    pub fn call_now(&self) -> Option<CallView> {
        self.calls.now.as_ref().map(|c| c.view.clone())
    }

    /// The media side of calls, shared with the network and the phone's
    /// audio.
    pub fn media(&self) -> MediaHub {
        self.media.clone()
    }

    /// Calls the friend in this chat. Returns the friend to reach now.
    pub fn call_start(&mut self, chat_id: i64, caps: &AudioCaps) -> Result<([u8; 32], CallView)> {
        self.call_start_at(chat_id, caps, now_ms())
    }

    pub(crate) fn call_start_at(&mut self, chat_id: i64, caps: &AudioCaps, at: u64) -> Result<([u8; 32], CallView)> {
        let meta = self.chat(chat_id)?;
        // Friends' own chats only: not notes, groups or temp chats; and only
        // once they've added us back, so the call can be encrypted.
        let peer = match (meta.kind, meta.peers.first()) {
            (ChatKind::Direct, Some(p)) if !meta.group_id.is_empty() => p.identity,
            _ => return Err(SessionError::NotAFriend),
        };
        if self.calls.now.is_some() {
            return Err(SessionError::InCall);
        }
        let offer = CallOffer {
            call: rand::random(),
            key: rand::random(),
            sent_at: at,
            video: false,
            audio_in: caps.play.clone(),
            audio_out: caps.record.clone(),
            ..Default::default()
        };
        let invite = self.queue_call(peer, Content::CallInvite(offer.clone()))?;
        // Ready to hear them from their very first packet.
        self.media.start(MediaStream::new(offer.call, peer, &offer.key, true));
        let view = CallView {
            id: offer.call,
            chat: chat_id,
            peer,
            name: meta.title,
            outgoing: true,
            video: false,
            phase: Phase::Connecting,
            answered_at: None,
            send_audio: String::new(),
            recv_audio: String::new(),
        };
        self.calls.now = Some(Call { view: view.clone(), offer, since: at, invite: Some(invite) });
        self.events.push(Event::Call(view.clone()));
        Ok((peer, view))
    }

    /// Picks up the call ringing now. Returns the friend to reach.
    pub fn call_answer(&mut self, caps: &AudioCaps) -> Result<Option<[u8; 32]>> {
        let Some(call) = self.calls.now.as_mut() else {
            return Ok(None);
        };
        if call.view.outgoing || call.view.phase != Phase::Ringing {
            return Ok(None);
        }
        // Each side records into something the other can play: our
        // favourite for what we hear, theirs for what they hear.
        let caller_audio = pick(&caps.play, &call.offer.audio_out);
        let callee_audio = pick(&call.offer.audio_in, &caps.record);
        let (Some(caller_audio), Some(callee_audio)) = (caller_audio, callee_audio) else {
            return self.finish_call(Outcome::Failed, Some(EndReason::Failed));
        };
        let (peer, id, key) = (call.view.peer, call.view.id, call.offer.key);
        call.view.phase = Phase::Active;
        call.view.answered_at = Some(now_ms());
        call.view.send_audio = callee_audio.clone();
        call.view.recv_audio = caller_audio.clone();
        let view = call.view.clone();
        self.queue_call(
            peer,
            Content::CallAnswer(CallAnswer { call: id, caller_audio, callee_audio, ..Default::default() }),
        )?;
        self.media.start(MediaStream::new(id, peer, &key, false));
        self.media.set_live();
        self.events.push(Event::Call(view));
        Ok(Some(peer))
    }

    /// Hangs up: ends the call, declines it (ringing here) or gives up
    /// calling. Returns the friend to tell.
    pub fn call_end(&mut self) -> Result<Option<[u8; 32]>> {
        let Some(call) = &self.calls.now else {
            return Ok(None);
        };
        let (outcome, reason) = match (call.view.phase, call.view.outgoing) {
            (Phase::Active, _) => (Outcome::Talked, EndReason::Hangup),
            (_, true) => (Outcome::Cancelled, EndReason::Cancelled),
            (_, false) => (Outcome::Declined, EndReason::Declined),
        };
        self.finish_call(outcome, Some(reason))
    }

    /// Our invite reached their phone: it's ringing there.
    pub(crate) fn call_delivered(&mut self, id: &MessageId) {
        let Some(call) = self.calls.now.as_mut() else { return };
        if call.invite.as_ref() == Some(id) {
            call.invite = None;
            if call.view.phase == Phase::Connecting {
                call.view.phase = Phase::Ringing;
                self.events.push(Event::Call(call.view.clone()));
            }
        }
    }

    /// Timeouts: nobody answering, the friend unreachable, or the sound
    /// gone quiet for good. Runs with [`Session::purge`].
    pub(crate) fn call_tick(&mut self, now: u64, clock: Instant) -> Result<()> {
        let Some(call) = &self.calls.now else {
            return Ok(());
        };
        let age = now.saturating_sub(call.since);
        let (outcome, tell) = match (call.view.phase, call.view.outgoing) {
            (Phase::Connecting, true) if age > UNREACHABLE_AFTER_MS => {
                (Outcome::Unreachable, Some(EndReason::Cancelled))
            }
            (Phase::Ringing, true) if age > RING_FOR_MS => (Outcome::NoAnswer, Some(EndReason::Cancelled)),
            (Phase::Ringing, false) if age > RING_HERE_MS => (Outcome::Missed, None),
            (Phase::Active, _) if self.media.quiet_for(clock) > DROPPED_AFTER => {
                (Outcome::Dropped, Some(EndReason::Failed))
            }
            _ => return Ok(()),
        };
        if let Some(peer) = self.finish_call(outcome, tell)? {
            self.pokes.push(peer);
        }
        Ok(())
    }

    pub(crate) fn on_call_invite(&mut self, chat_id: i64, peer: &[u8; 32], mut offer: CallOffer) -> Result<()> {
        if self.calls.done.contains(&offer.call) {
            return Ok(());
        }
        for list in [&mut offer.audio_in, &mut offer.audio_out, &mut offer.video_in, &mut offer.video_out] {
            tidy(list);
        }
        offer.routes.clear();
        let now = now_ms();
        let at = offer.sent_at.min(now);
        // Too late to ring (they were offline, or we were): a missed call.
        if now.saturating_sub(offer.sent_at) > RING_WINDOW_MS {
            self.calls.remember(offer.call);
            return self.log_call(chat_id, false, Outcome::Missed, at, 0);
        }
        if let Some(current) = &self.calls.now {
            if current.view.id == offer.call {
                return Ok(());
            }
            // Both called each other at once: the call made by the phone
            // with the smaller key wins, on both phones.
            let both_calling = current.view.peer == *peer
                && current.view.outgoing
                && matches!(current.view.phase, Phase::Connecting | Phase::Ringing);
            if both_calling && *peer < self.public_key() {
                let ours = self.calls.now.take().expect("checked");
                self.media.stop();
                self.calls.remember(ours.view.id);
                if let Some(invite) = ours.invite {
                    self.drop_unsent(peer, &invite)?;
                }
            } else if current.view.peer == *peer {
                // The losing side of that, or a leftover: never rings.
                self.calls.remember(offer.call);
                return Ok(());
            } else {
                // In another call: busy.
                self.calls.remember(offer.call);
                self.queue_call(*peer, Content::CallEnd { call: offer.call, reason: EndReason::Busy as u8 })?;
                self.pokes.push(*peer);
                return self.log_call(chat_id, false, Outcome::Missed, at, 0);
            }
        }
        let view = CallView {
            id: offer.call,
            chat: chat_id,
            peer: *peer,
            name: self.friend_name(peer),
            outgoing: false,
            // This build answers with sound only.
            video: false,
            phase: Phase::Ringing,
            answered_at: None,
            send_audio: String::new(),
            recv_audio: String::new(),
        };
        self.calls.now = Some(Call { view: view.clone(), offer, since: now, invite: None });
        self.events.push(Event::Call(view));
        Ok(())
    }

    pub(crate) fn on_call_answer(&mut self, peer: &[u8; 32], answer: CallAnswer) -> Result<()> {
        let Some(call) = self.calls.now.as_mut() else {
            return Ok(());
        };
        if call.view.id != answer.call
            || call.view.peer != *peer
            || !call.view.outgoing
            || !matches!(call.view.phase, Phase::Connecting | Phase::Ringing)
        {
            return Ok(());
        }
        // They must pick from what we offered.
        if !call.offer.audio_out.contains(&answer.caller_audio) || !call.offer.audio_in.contains(&answer.callee_audio) {
            if let Some(peer) = self.finish_call(Outcome::Failed, Some(EndReason::Failed))? {
                self.pokes.push(peer);
            }
            return Ok(());
        }
        call.invite = None;
        call.view.phase = Phase::Active;
        call.view.answered_at = Some(now_ms());
        call.view.send_audio = answer.caller_audio;
        call.view.recv_audio = answer.callee_audio;
        let view = call.view.clone();
        self.media.set_live();
        self.events.push(Event::Call(view));
        Ok(())
    }

    pub(crate) fn on_call_end(&mut self, peer: &[u8; 32], id: CallId, reason: u8) -> Result<()> {
        let Some(call) = &self.calls.now else {
            self.calls.remember(id);
            return Ok(());
        };
        if call.view.id != id || call.view.peer != *peer {
            self.calls.remember(id);
            return Ok(());
        }
        let outcome = match (call.view.phase, call.view.outgoing, EndReason::from_wire(reason)) {
            (Phase::Active, _, EndReason::Failed) => Outcome::Dropped,
            (Phase::Active, _, _) => Outcome::Talked,
            (_, true, EndReason::Busy) => Outcome::Busy,
            (_, true, EndReason::Failed) => Outcome::Failed,
            (_, true, _) => Outcome::Declined,
            // They gave up before we answered.
            (_, false, _) => Outcome::Missed,
        };
        self.finish_call(outcome, None)?;
        Ok(())
    }

    /// Ends the call now: records it in the chat, stops the media and, with
    /// `tell`, lets the friend know why. Returns the friend to reach.
    fn finish_call(&mut self, outcome: Outcome, tell: Option<EndReason>) -> Result<Option<[u8; 32]>> {
        let Some(call) = self.calls.now.take() else {
            return Ok(None);
        };
        self.media.stop();
        self.calls.remember(call.view.id);
        let seconds = call.view.answered_at.map_or(0, |at| (now_ms().saturating_sub(at) / 1000) as u32);
        // Removed as a friend meanwhile: nowhere to record it.
        if self.chat(call.view.chat).is_ok() {
            let at = call.offer.sent_at.min(call.since);
            self.log_call(call.view.chat, call.view.outgoing, outcome, at, seconds)?;
        }
        let peer = call.view.peer;
        let mut view = call.view;
        view.phase = Phase::Ended(outcome);
        self.events.push(Event::Call(view));
        let Some(reason) = tell else {
            return Ok(None);
        };
        self.queue_call(peer, Content::CallEnd { call: call.offer.call, reason: reason as u8 })?;
        Ok(Some(peer))
    }

    /// The call's record in the chat, kept on this phone only.
    fn log_call(&mut self, chat_id: i64, outgoing: bool, outcome: Outcome, at_ms: u64, seconds: u32) -> Result<()> {
        self.store.add_message(
            chat_id,
            &StoredMessage {
                from_me: outgoing,
                body: outcome.describe(seconds),
                sent_at: at_ms / 1000,
                delivered: true,
                call: Some(CallLog { video: false, outcome: outcome as u8, seconds }),
                ..Default::default()
            },
        )?;
        self.events.push(Event::Chat(chat_id));
        if outcome == Outcome::Missed {
            self.events.push(Event::MissedCall(chat_id));
        }
        Ok(())
    }

    /// Signaling waits in the outbox like any message, so it's encrypted
    /// in order with everything else.
    fn queue_call(&mut self, peer: [u8; 32], content: Content) -> Result<MessageId> {
        let id = rand::random();
        self.store.outbox_put(
            &id,
            &OutboxItem { peer, state: OutboxState::Plain(content.encode()), message: None },
        )?;
        Ok(id)
    }

    /// Takes back something we queued, if it hasn't been encrypted yet
    /// (encrypted, it must go: the friend's MLS counter expects it).
    fn drop_unsent(&mut self, peer: &[u8; 32], id: &MessageId) -> Result<()> {
        let unsent = self
            .store
            .outbox_for(peer)?
            .into_iter()
            .any(|(i, item)| &i == id && matches!(item.state, OutboxState::Plain(_)));
        if unsent {
            self.store.outbox_remove(id)?;
        }
        Ok(())
    }
}

// ---- Media -----------------------------------------------------------

/// What a sealed media packet holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaPacket {
    /// [`AUDIO`], or (later builds) [`VIDEO`]. Others are ignored.
    pub kind: u8,
    /// This stream's own count: audio packet `i` plays at `i` × 40 ms.
    pub index: u32,
    /// The sender's media clock in milliseconds, for keeping sound and
    /// pictures together later.
    pub at_ms: u32,
    /// Encoded sound: [`AUDIO_PACKET_MS`] of it, in the agreed format, as
    /// the phone's audio code packs it.
    pub data: Vec<u8>,
}

/// One direction's key, derived from the call key.
fn direction_cipher(key: &[u8; 32], dir: u8) -> ChaCha20Poly1305 {
    let context = if dir == FROM_CALLER {
        "sanctum call media v1: from the caller"
    } else {
        "sanctum call media v1: from the one called"
    };
    ChaCha20Poly1305::new(&blake3::derive_key(context, key).into())
}

fn nonce(dir: u8, seq: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0] = dir;
    n[4..].copy_from_slice(&seq.to_be_bytes());
    n
}

fn aad(call: &CallId, dir: u8, seq: u64) -> Vec<u8> {
    [b"sanctum call v1".as_slice(), call, &[dir], &seq.to_be_bytes()].concat()
}

/// Seals a packet. Each (direction, `seq`) must be used once per call.
pub fn seal_packet(key: &[u8; 32], dir: u8, call: &CallId, seq: u64, packet: &MediaPacket) -> Vec<u8> {
    seal_with(&direction_cipher(key, dir), dir, call, seq, packet)
}

fn seal_with(cipher: &ChaCha20Poly1305, dir: u8, call: &CallId, seq: u64, packet: &MediaPacket) -> Vec<u8> {
    let plain = postcard::to_stdvec(packet).expect("serializable");
    cipher
        .encrypt(Nonce::from_slice(&nonce(dir, seq)), Payload { msg: &plain, aad: &aad(call, dir, seq) })
        .expect("sealing doesn't fail")
}

/// Opens a packet; None if it wasn't sealed with this key for this call,
/// direction and number, or was changed on the way.
pub fn open_packet(key: &[u8; 32], dir: u8, call: &CallId, seq: u64, sealed: &[u8]) -> Option<MediaPacket> {
    open_with(&direction_cipher(key, dir), dir, call, seq, sealed)
}

fn open_with(cipher: &ChaCha20Poly1305, dir: u8, call: &CallId, seq: u64, sealed: &[u8]) -> Option<MediaPacket> {
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce(dir, seq)), Payload { msg: sealed, aad: &aad(call, dir, seq) })
        .ok()?;
    postcard::from_bytes(&plain).ok()
}

/// Packet numbers seen lately: each is accepted once. Older than the
/// window counts as seen (it would be far too late to play anyway).
#[derive(Debug, Default)]
pub struct ReplayWindow {
    top: Option<u64>,
    /// Bit `i`: `top - i` was seen.
    seen: u128,
}

impl ReplayWindow {
    pub fn fresh(&self, seq: u64) -> bool {
        match self.top {
            None => true,
            Some(top) if seq > top => true,
            Some(top) => {
                let back = top - seq;
                back < 128 && self.seen & (1u128 << back) == 0
            }
        }
    }

    pub fn mark(&mut self, seq: u64) {
        match self.top {
            Some(top) if seq <= top => {
                let back = top - seq;
                if back < 128 {
                    self.seen |= 1u128 << back;
                }
            }
            Some(top) => {
                let shift = seq - top;
                self.seen = if shift >= 128 { 0 } else { self.seen << shift };
                self.seen |= 1;
                self.top = Some(seq);
            }
            None => {
                self.top = Some(seq);
                self.seen = 1;
            }
        }
    }
}

/// The media of the call going on now: keys, packet numbers and what's
/// waiting to be played.
pub struct MediaStream {
    call: CallId,
    peer: [u8; 32],
    send: ChaCha20Poly1305,
    recv: ChaCha20Poly1305,
    send_dir: u8,
    next_seq: u64,
    replay: ReplayWindow,
    /// Since when we're talking; nothing is sent or played before.
    live_since: Option<Instant>,
    last_heard: Option<Instant>,
    audio_index: u32,
    jitter: JitterBuffer,
}

impl MediaStream {
    pub fn new(call: CallId, peer: [u8; 32], key: &[u8; 32], caller: bool) -> Self {
        let (send_dir, recv_dir) = if caller { (FROM_CALLER, FROM_CALLEE) } else { (FROM_CALLEE, FROM_CALLER) };
        Self {
            call,
            peer,
            send: direction_cipher(key, send_dir),
            recv: direction_cipher(key, recv_dir),
            send_dir,
            next_seq: 0,
            replay: ReplayWindow::default(),
            live_since: None,
            last_heard: None,
            audio_index: 0,
            jitter: JitterBuffer::new(AUDIO_PACKET_MS),
        }
    }
}

/// What the player should play next.
#[derive(Debug, Clone, PartialEq)]
pub enum Pulled {
    /// The next packet.
    Packet(Vec<u8>),
    /// It's missing: fill the gap with something that sounds like it.
    Lost,
    /// Nothing to play yet: silence.
    Silence,
}

/// The call's media, shared by the session (which starts and stops it),
/// the network (which hands it packets) and the phone's audio.
#[derive(Clone, Default)]
pub struct MediaHub(Arc<Mutex<Option<MediaStream>>>);

impl MediaHub {
    pub(crate) fn start(&self, stream: MediaStream) {
        *self.0.lock().unwrap() = Some(stream);
    }

    pub(crate) fn stop(&self) {
        self.0.lock().unwrap().take();
    }

    pub(crate) fn set_live(&self) {
        if let Some(s) = self.0.lock().unwrap().as_mut() {
            s.live_since = Some(Instant::now());
        }
    }

    /// The friend we're talking to, if talking.
    pub fn peer(&self) -> Option<[u8; 32]> {
        self.0.lock().unwrap().as_ref().filter(|s| s.live_since.is_some()).map(|s| s.peer)
    }

    /// Seals [`AUDIO_PACKET_MS`] of encoded sound for the friend: the frame
    /// and who to send it to. None unless talking.
    pub fn seal_audio(&self, data: &[u8]) -> Option<([u8; 32], Frame)> {
        let mut guard = self.0.lock().unwrap();
        let s = guard.as_mut()?;
        s.live_since?;
        if data.len() > MAX_MEDIA {
            return None;
        }
        let packet = MediaPacket {
            kind: AUDIO,
            index: s.audio_index,
            at_ms: s.audio_index.wrapping_mul(AUDIO_PACKET_MS),
            data: data.to_vec(),
        };
        s.audio_index = s.audio_index.wrapping_add(1);
        let seq = s.next_seq;
        s.next_seq += 1;
        let sealed = seal_with(&s.send, s.send_dir, &s.call, seq, &packet);
        Some((s.peer, Frame::CallMedia { call: s.call, seq, sealed }))
    }

    /// A [`Frame::CallMedia`] from `peer`. Taken only from the friend in
    /// the call, sealed with this call's key, and once. Returns whether it
    /// was taken.
    pub fn receive(&self, peer: &[u8; 32], call: &CallId, seq: u64, sealed: &[u8]) -> bool {
        self.receive_at(peer, call, seq, sealed, Instant::now())
    }

    pub fn receive_at(&self, peer: &[u8; 32], call: &CallId, seq: u64, sealed: &[u8], now: Instant) -> bool {
        let mut guard = self.0.lock().unwrap();
        let Some(s) = guard.as_mut() else {
            return false;
        };
        if &s.peer != peer || &s.call != call || sealed.len() > MAX_MEDIA + 64 || !s.replay.fresh(seq) {
            return false;
        }
        let Some(packet) = open_with(&s.recv, s.send_dir ^ 1, call, seq, sealed) else {
            return false;
        };
        s.replay.mark(seq);
        s.last_heard = Some(now);
        if packet.kind == AUDIO {
            s.jitter.push(packet.index, packet.data, now);
        }
        true
    }

    /// The next [`AUDIO_PACKET_MS`] of sound to play. Call it once per
    /// packet's worth of playing time.
    pub fn pull_audio(&self) -> Pulled {
        self.pull_audio_at(Instant::now())
    }

    pub fn pull_audio_at(&self, now: Instant) -> Pulled {
        match self.0.lock().unwrap().as_mut() {
            Some(s) if s.live_since.is_some() => s.jitter.pull(now),
            _ => Pulled::Silence,
        }
    }

    /// How long nothing has been heard while talking.
    pub(crate) fn quiet_for(&self, now: Instant) -> Duration {
        let guard = self.0.lock().unwrap();
        let Some(s) = guard.as_ref() else {
            return Duration::ZERO;
        };
        let since = match (s.live_since, s.last_heard) {
            (Some(live), Some(heard)) => live.max(heard),
            (Some(live), None) => live,
            _ => return Duration::ZERO,
        };
        now.saturating_duration_since(since)
    }
}

// ---- Jitter buffer -----------------------------------------------------

/// Waits at least this long before playing...
pub const JITTER_MIN_MS: u32 = 60;
/// ...and at most this long, however uneven the network.
pub const JITTER_MAX_MS: u32 = 400;
/// Never holds more than this (older packets are dropped).
const HELD_MAX_MS: u32 = 1500;
/// Arrival times remembered for judging the network: about 6 seconds.
const DELAY_WINDOW: usize = 150;
/// Missing this many packets in a row with nothing waiting: the network
/// stalled; wait to fill up again.
const STALL_AFTER: u32 = 3;

/// Holds arriving packets until it's their turn to play, in order.
///
/// It plays once enough is waiting to ride out the network's unevenness:
/// it tracks how late each packet arrives compared to the earliest one
/// (relative to when it was made), and waits as long as the latest 5% of
/// recent packets needed, between [`JITTER_MIN_MS`] and [`JITTER_MAX_MS`].
/// Tor delivers in order but in bursts, so it mostly grows the wait; when
/// the network calms down, it plays a packet's worth less now and then to
/// shrink it again. A missing packet with later ones waiting is lost; with
/// nothing waiting, the network has stalled: the player fills in, and the
/// packet plays when it comes (a little later than planned).
pub struct JitterBuffer {
    packet_ms: u32,
    packets: BTreeMap<u32, Vec<u8>>,
    /// The packet to play next.
    next: Option<u32>,
    playing: bool,
    misses: u32,
    /// The first arrival: (when, which packet).
    base: Option<(Instant, u32)>,
    /// Recent arrivals' lateness against `base`, in milliseconds.
    delays: VecDeque<i64>,
    target_ms: u32,
    last_trim: Option<Instant>,
}

impl JitterBuffer {
    pub fn new(packet_ms: u32) -> Self {
        Self {
            packet_ms,
            packets: BTreeMap::new(),
            next: None,
            playing: false,
            misses: 0,
            base: None,
            delays: VecDeque::new(),
            target_ms: JITTER_MIN_MS,
            last_trim: None,
        }
    }

    /// How long it waits before playing, now.
    pub fn target_ms(&self) -> u32 {
        self.target_ms
    }

    /// How much sound is waiting, from the next packet to the last.
    pub fn held_ms(&self) -> u32 {
        let (Some(&first), Some(&last)) = (self.packets.keys().next(), self.packets.keys().next_back()) else {
            return 0;
        };
        let start = self.next.unwrap_or(first).min(first);
        (last - start + 1).saturating_mul(self.packet_ms)
    }

    pub fn push(&mut self, index: u32, data: Vec<u8>, now: Instant) {
        if self.next.is_some_and(|next| index < next) {
            return; // Too late: its turn is over.
        }
        self.packets.insert(index, data);
        while self.held_ms() > HELD_MAX_MS {
            let Some((first, _)) = self.packets.pop_first() else { break };
            if self.next.is_none_or(|next| next <= first) {
                self.next = Some(first + 1);
            }
        }
        let (t0, i0) = *self.base.get_or_insert((now, index));
        let elapsed = now.saturating_duration_since(t0).as_millis() as i64;
        let delay = elapsed - (index as i64 - i0 as i64) * self.packet_ms as i64;
        self.delays.push_back(delay);
        if self.delays.len() > DELAY_WINDOW {
            self.delays.pop_front();
        }
        self.target_ms = self.estimate();
    }

    fn estimate(&self) -> u32 {
        let Some(&min) = self.delays.iter().min() else {
            return JITTER_MIN_MS;
        };
        let mut late: Vec<i64> = self.delays.iter().map(|d| d - min).collect();
        late.sort_unstable();
        let p95 = late[(late.len() - 1) * 95 / 100];
        ((p95 + self.packet_ms as i64 / 2).clamp(JITTER_MIN_MS as i64, JITTER_MAX_MS as i64)) as u32
    }

    pub fn pull(&mut self, now: Instant) -> Pulled {
        if !self.playing {
            if self.packets.is_empty() || self.held_ms() < self.target_ms {
                return Pulled::Silence;
            }
            let first = *self.packets.keys().next().expect("not empty");
            self.next = Some(self.next.map_or(first, |next| next.max(first)));
            self.playing = true;
            self.misses = 0;
        }
        let n = self.next.expect("set while playing");
        if let Some(data) = self.packets.remove(&n) {
            self.next = Some(n + 1);
            self.misses = 0;
            // Waiting longer than needed: skip a packet, at most once a
            // second, so the delay shrinks back.
            let trimmed_lately = self.last_trim.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(1));
            if self.held_ms() > self.target_ms + 3 * self.packet_ms && !trimmed_lately {
                if let Some((skipped, _)) = self.packets.pop_first() {
                    self.next = Some(skipped + 1);
                    self.last_trim = Some(now);
                }
            }
            return Pulled::Packet(data);
        }
        if self.packets.is_empty() {
            // Nothing after it either: the network stalled. Keep waiting for
            // this one, and after a few missed turns fill up again first.
            self.misses += 1;
            if self.misses >= STALL_AFTER {
                self.playing = false;
            }
        } else {
            // Later ones are here: this one is lost.
            self.next = Some(n + 1);
        }
        Pulled::Lost
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{befriend, connect, Phone};
    use crate::session::Event;

    fn caps(play: &[&str], record: &[&str]) -> AudioCaps {
        AudioCaps {
            play: play.iter().map(|s| s.to_string()).collect(),
            record: record.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn both() -> AudioCaps {
        caps(&[OPUS, AMR_WB], &[OPUS, AMR_WB])
    }

    /// Alice and Bob, friends with encryption set up.
    fn friends() -> (Phone, Phone, i64, i64) {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (ab, ba) = befriend(&mut alice, &mut bob);
        connect(&mut alice, &mut bob);
        alice.s.take_events();
        bob.s.take_events();
        (alice, bob, ab, ba)
    }

    fn calls(events: &[Event]) -> Vec<Phase> {
        events.iter().filter_map(|e| if let Event::Call(v) = e { Some(v.phase) } else { None }).collect()
    }

    fn logs(p: &Phone, chat: i64) -> Vec<(bool, String)> {
        p.s.store
            .messages(chat, None)
            .unwrap()
            .into_iter()
            .filter(|m| m.call.is_some())
            .map(|m| (m.from_me, m.body))
            .collect()
    }

    /// Alice calls Bob and he answers.
    fn talking() -> (Phone, Phone, i64, i64) {
        let (mut alice, mut bob, ab, ba) = friends();
        alice.s.call_start(ab, &both()).unwrap();
        connect(&mut alice, &mut bob);
        bob.s.call_answer(&both()).unwrap().unwrap();
        connect(&mut bob, &mut alice);
        assert_eq!(alice.s.call_now().unwrap().phase, Phase::Active);
        alice.s.take_events();
        bob.s.take_events();
        (alice, bob, ab, ba)
    }

    #[test]
    fn a_call_rings_is_answered_and_ends_on_both_phones() {
        let (mut alice, mut bob, ab, ba) = friends();
        let (peer, view) = alice.s.call_start(ab, &caps(&[OPUS, AMR_WB], &[AMR_WB])).unwrap();
        assert_eq!((peer, view.phase, view.outgoing), (bob.pk(), Phase::Connecting, true));
        assert_eq!(alice.s.pending_peers().unwrap(), vec![bob.pk()]);

        connect(&mut alice, &mut bob);
        let rung = bob.s.call_now().unwrap();
        assert_eq!((rung.phase, rung.outgoing, rung.chat, rung.name.as_str()), (Phase::Ringing, false, ba, "Alice"));
        assert_eq!(calls(&bob.s.take_events()), [Phase::Ringing]);
        // Delivered: it rings over there.
        assert_eq!(calls(&alice.s.take_events()), [Phase::Connecting, Phase::Ringing]);

        // Bob records Opus only; Alice only AMR-WB. Each plays both.
        assert_eq!(bob.s.call_answer(&caps(&[OPUS, AMR_WB], &[OPUS])).unwrap(), Some(alice.pk()));
        let b = bob.s.call_now().unwrap();
        assert_eq!((b.phase, b.send_audio.as_str(), b.recv_audio.as_str()), (Phase::Active, OPUS, AMR_WB));
        connect(&mut bob, &mut alice);
        let a = alice.s.call_now().unwrap();
        assert_eq!((a.phase, a.send_audio.as_str(), a.recv_audio.as_str()), (Phase::Active, AMR_WB, OPUS));
        assert!(a.answered_at.is_some());

        assert_eq!(alice.s.call_end().unwrap(), Some(bob.pk()));
        assert!(alice.s.call_now().is_none());
        connect(&mut alice, &mut bob);
        assert!(bob.s.call_now().is_none());
        assert!(calls(&bob.s.take_events()).contains(&Phase::Ended(Outcome::Talked)));
        assert_eq!(logs(&alice, ab), [(true, "Voice call · 0:00".to_string())]);
        assert_eq!(logs(&bob, ba), [(false, "Voice call · 0:00".to_string())]);
        assert!(alice.s.pending_peers().unwrap().is_empty(), "all signaling delivered");
    }

    #[test]
    fn declining_and_giving_up() {
        let (mut alice, mut bob, ab, ba) = friends();
        alice.s.call_start(ab, &both()).unwrap();
        connect(&mut alice, &mut bob);
        bob.s.call_end().unwrap();
        connect(&mut bob, &mut alice);
        assert!(calls(&alice.s.take_events()).contains(&Phase::Ended(Outcome::Declined)));
        assert_eq!(logs(&alice, ab), [(true, "Declined call".to_string())]);
        assert_eq!(logs(&bob, ba), [(false, "Declined call".to_string())]);

        // Alice gives up before Bob answers: a missed call for him.
        alice.s.call_start(ab, &both()).unwrap();
        connect(&mut alice, &mut bob);
        alice.s.call_end().unwrap();
        connect(&mut alice, &mut bob);
        assert!(bob.s.call_now().is_none());
        let events = bob.s.take_events();
        assert!(calls(&events).contains(&Phase::Ended(Outcome::Missed)));
        assert!(events.contains(&Event::MissedCall(ba)));
        assert_eq!(logs(&alice, ab).last().unwrap().1, "Cancelled call");
        assert_eq!(logs(&bob, ba).last().unwrap().1, "Missed call");
    }

    #[test]
    fn a_late_invite_is_a_missed_call_and_never_rings() {
        let (mut alice, mut bob, ab, ba) = friends();
        alice.s.call_start_at(ab, &both(), now_ms() - RING_WINDOW_MS - 5_000).unwrap();
        connect(&mut alice, &mut bob);
        assert!(bob.s.call_now().is_none());
        let events = bob.s.take_events();
        assert!(calls(&events).is_empty(), "never rang");
        assert!(events.contains(&Event::MissedCall(ba)));
        assert_eq!(logs(&bob, ba), [(false, "Missed call".to_string())]);
    }

    #[test]
    fn calling_someone_unreachable_gives_up_and_they_see_a_missed_call() {
        let (mut alice, mut bob, ab, ba) = friends();
        alice.s.call_start(ab, &both()).unwrap();
        alice.s.call_tick(now_ms() + 10_000, Instant::now()).unwrap();
        assert_eq!(alice.s.call_now().unwrap().phase, Phase::Connecting, "still trying");
        alice.s.call_tick(now_ms() + UNREACHABLE_AFTER_MS + 1_000, Instant::now()).unwrap();
        assert!(alice.s.call_now().is_none());
        assert!(calls(&alice.s.take_events()).contains(&Phase::Ended(Outcome::Unreachable)));
        assert_eq!(alice.s.take_pokes(), [bob.pk()]);
        assert_eq!(logs(&alice, ab), [(true, "Call didn't go through".to_string())]);

        // Bob comes online: one missed call, no ringing left behind.
        connect(&mut bob, &mut alice);
        assert!(bob.s.call_now().is_none());
        assert_eq!(logs(&bob, ba), [(false, "Missed call".to_string())]);
    }

    #[test]
    fn unanswered_calls_time_out_on_both_phones() {
        let (mut alice, mut bob, ab, ba) = friends();
        alice.s.call_start(ab, &both()).unwrap();
        connect(&mut alice, &mut bob);
        alice.s.call_tick(now_ms() + RING_FOR_MS + 1_000, Instant::now()).unwrap();
        assert_eq!(logs(&alice, ab), [(true, "No answer".to_string())]);
        // Bob's phone stops by itself even if Alice's "never mind" is lost.
        bob.s.call_tick(now_ms() + RING_HERE_MS + 1_000, Instant::now()).unwrap();
        assert!(bob.s.call_now().is_none());
        assert_eq!(logs(&bob, ba), [(false, "Missed call".to_string())]);
        // When it does arrive, nothing more happens.
        connect(&mut alice, &mut bob);
        assert_eq!(logs(&bob, ba).len(), 1);
    }

    #[test]
    fn a_second_caller_hears_busy() {
        let (mut alice, mut bob, _, ba) = talking();
        let mut carol = Phone::new();
        let (cb, bc) = befriend(&mut carol, &mut bob);
        connect(&mut carol, &mut bob);
        carol.s.call_start(cb, &both()).unwrap();
        connect(&mut carol, &mut bob);
        assert_eq!(bob.s.call_now().unwrap().peer, alice.pk(), "still talking to Alice");
        connect(&mut bob, &mut carol);
        assert!(carol.s.call_now().is_none());
        assert!(calls(&carol.s.take_events()).contains(&Phase::Ended(Outcome::Busy)));
        assert_eq!(logs(&carol, cb), [(true, "Busy".to_string())]);
        assert_eq!(logs(&bob, bc), [(false, "Missed call".to_string())]);
        assert!(logs(&bob, ba).is_empty());
        let _ = &mut alice;
    }

    #[test]
    fn calling_each_other_at_once_settles_on_one_call() {
        let (mut alice, mut bob, ab, ba) = friends();
        alice.s.call_start(ab, &both()).unwrap();
        bob.s.call_start(ba, &both()).unwrap();
        connect(&mut alice, &mut bob);
        let (a, b) = (alice.s.call_now().unwrap(), bob.s.call_now().unwrap());
        assert_eq!(a.id, b.id, "one call");
        // The smaller key's call won: that phone calls, the other rings.
        let alice_won = alice.pk() < bob.pk();
        assert_eq!(a.outgoing, alice_won);
        assert_eq!(b.outgoing, !alice_won);
        assert!(logs(&alice, ab).is_empty() && logs(&bob, ba).is_empty(), "no stray records");
    }

    #[test]
    fn only_friends_chats_can_call() {
        let (mut alice, _bob, ab, _) = friends();
        let notes = alice.s.store.chats().unwrap().into_iter().find(|(_, m)| m.kind == ChatKind::Notes);
        if let Some((notes, _)) = notes {
            assert!(alice.s.call_start(notes, &both()).is_err());
        }
        // A friend who hasn't added us back yet.
        let mut dave = Phone::new();
        let ad = alice.s.add_friend(&dave.s.my_code("Dave"), true).unwrap().chat_id;
        assert!(alice.s.call_start(ad, &both()).is_err());
        alice.s.call_start(ab, &both()).unwrap();
        let _ = &mut dave;
        assert!(matches!(alice.s.call_start(ab, &both()), Err(SessionError::InCall)));
    }

    #[test]
    fn sound_flows_sealed_both_ways() {
        let (alice, bob, _, _) = talking();
        let t0 = Instant::now();
        for i in 0..4u8 {
            let (to, frame) = alice.s.media().seal_audio(&[i; 60]).unwrap();
            assert_eq!(to, bob.pk());
            let Frame::CallMedia { call, seq, sealed } = &frame else { panic!() };
            assert!(!sealed.windows(60).any(|w| w == [i; 60]), "sealed");
            assert!(bob.s.media().receive_at(&alice.pk(), call, *seq, sealed, t0 + Duration::from_millis(40 * i as u64)));
        }
        let hub = bob.s.media();
        let t = t0 + Duration::from_millis(200);
        assert_eq!(hub.pull_audio_at(t), Pulled::Packet(vec![0; 60]));
        assert_eq!(hub.pull_audio_at(t), Pulled::Packet(vec![1; 60]));
        // And back, through the session like any frame.
        let (_, frame) = bob.s.media().seal_audio(b"hello").unwrap();
        let mut alice = alice;
        assert!(alice.s.on_frame(&bob.pk(), frame.clone()).unwrap().is_empty());
        let Frame::CallMedia { call, seq, sealed } = frame else { panic!() };
        assert!(!alice.s.media().receive(&bob.pk(), &call, seq, &sealed), "already taken");
    }

    #[test]
    fn media_is_taken_only_once_only_from_the_peer_and_only_with_the_key() {
        let (alice, bob, _, _) = talking();
        let hub = bob.s.media();
        let (_, frame) = alice.s.media().seal_audio(b"one").unwrap();
        let Frame::CallMedia { call, seq, sealed } = frame else { panic!() };
        // From someone else (even with the right bytes): refused.
        let carol = Phone::new();
        assert!(!hub.receive(&carol.pk(), &call, seq, &sealed));
        assert!(hub.receive(&alice.pk(), &call, seq, &sealed));
        assert!(!hub.receive(&alice.pk(), &call, seq, &sealed), "replayed");
        // Changed on the way, or under another number: refused.
        let (_, frame) = alice.s.media().seal_audio(b"two").unwrap();
        let Frame::CallMedia { call, seq, mut sealed } = frame else { panic!() };
        assert!(!hub.receive(&alice.pk(), &call, seq + 5, &sealed));
        sealed[0] ^= 1;
        assert!(!hub.receive(&alice.pk(), &call, seq, &sealed));
        // Sealed with another key, or for another call: refused.
        let packet = MediaPacket { kind: AUDIO, index: 9, at_ms: 0, data: b"x".to_vec() };
        let forged = seal_packet(&[7; 32], FROM_CALLER, &call, 50, &packet);
        assert!(!hub.receive(&alice.pk(), &call, 50, &forged));
        assert!(!hub.receive(&alice.pk(), &[1; 16], 51, &sealed));
        // Our own packets reflected back: the wrong direction.
        let (_, own) = bob.s.media().seal_audio(b"mine").unwrap();
        let Frame::CallMedia { call, seq, sealed } = own else { panic!() };
        assert!(!hub.receive(&alice.pk(), &call, seq + 100, &sealed));
    }

    #[test]
    fn nothing_is_sent_or_kept_unless_talking() {
        let (mut alice, mut bob, ab, _) = friends();
        assert!(alice.s.media().seal_audio(b"hi").is_none());
        alice.s.call_start(ab, &both()).unwrap();
        assert!(alice.s.media().seal_audio(b"hi").is_none(), "not answered yet");
        connect(&mut alice, &mut bob);
        assert!(bob.s.media().seal_audio(b"hi").is_none(), "ringing");
        bob.s.call_answer(&both()).unwrap();
        let (_, frame) = bob.s.media().seal_audio(b"hi").unwrap();
        bob.s.call_end().unwrap();
        assert!(bob.s.media().seal_audio(b"hi").is_none(), "hung up");
        // Media never lands in the outbox.
        assert!(bob.s.store.outbox_for(&alice.pk()).unwrap().iter().all(|(_, i)| matches!(i.state, OutboxState::Plain(_))));
        let Frame::CallMedia { call, seq, sealed } = frame else { panic!() };
        assert!(alice.s.media().receive(&bob.pk(), &call, seq, &sealed), "taken: Alice still calling");
    }

    #[test]
    fn a_silent_call_drops() {
        let (mut alice, bob, ab, _) = talking();
        alice.s.call_tick(now_ms(), Instant::now() + Duration::from_secs(10)).unwrap();
        assert!(alice.s.call_now().is_some());
        alice.s.call_tick(now_ms(), Instant::now() + DROPPED_AFTER + Duration::from_secs(1)).unwrap();
        assert!(alice.s.call_now().is_none());
        assert_eq!(alice.s.take_pokes(), [bob.pk()]);
        assert!(logs(&alice, ab)[0].1.starts_with("Voice call"));
    }

    #[test]
    fn calls_survive_old_phones_ignoring_them() {
        // Content an older build can't read is acknowledged and dropped
        // there; here, an unknown end reason is a plain hang-up.
        assert_eq!(EndReason::from_wire(200), EndReason::Hangup);
        assert_eq!(Outcome::from_wire(200), Outcome::Talked);
        assert_eq!(talk_time(252), "4:12");
        assert_eq!(talk_time(3723), "1:02:03");
    }

    // ---- Sealing and replay ----

    #[test]
    fn packets_open_only_with_their_key_call_direction_and_number() {
        let packet = MediaPacket { kind: AUDIO, index: 3, at_ms: 120, data: vec![1, 2, 3] };
        let key = [5u8; 32];
        let sealed = seal_packet(&key, FROM_CALLER, &[1; 16], 7, &packet);
        assert_eq!(open_packet(&key, FROM_CALLER, &[1; 16], 7, &sealed), Some(packet.clone()));
        assert!(open_packet(&[6; 32], FROM_CALLER, &[1; 16], 7, &sealed).is_none());
        assert!(open_packet(&key, FROM_CALLEE, &[1; 16], 7, &sealed).is_none());
        assert!(open_packet(&key, FROM_CALLER, &[2; 16], 7, &sealed).is_none());
        assert!(open_packet(&key, FROM_CALLER, &[1; 16], 8, &sealed).is_none());
        // Same number, other direction: a different nonce and key.
        assert_ne!(seal_packet(&key, FROM_CALLEE, &[1; 16], 7, &packet), sealed);
    }

    #[test]
    fn the_replay_window_takes_each_number_once() {
        let mut w = ReplayWindow::default();
        for seq in [5, 3, 9, 4, 200] {
            assert!(w.fresh(seq));
            w.mark(seq);
            assert!(!w.fresh(seq));
        }
        assert!(w.fresh(199) && w.fresh(100));
        assert!(!w.fresh(9), "too old to matter");
        assert!(!w.fresh(5));
    }

    // ---- Jitter buffer ----

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Plays `arrivals` (packet index, arrival ms) through a buffer pulled
    /// every 40 ms for `until` ms; returns what was played.
    fn play(arrivals: &[(u32, u64)], until: u64) -> (Vec<Pulled>, JitterBuffer) {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(40);
        let mut out = Vec::new();
        let mut arrivals = arrivals.to_vec();
        arrivals.sort_by_key(|a| a.1);
        let mut next = 0;
        let mut t = 0;
        while t <= until {
            while next < arrivals.len() && arrivals[next].1 <= t {
                let (i, at) = arrivals[next];
                jb.push(i, vec![i as u8], t0 + ms(at));
                next += 1;
            }
            out.push(jb.pull(t0 + ms(t)));
            t += 40;
        }
        // Up to the last packet played: after it, the stream simply ended.
        let end = out.iter().rposition(|p| matches!(p, Pulled::Packet(_))).map_or(0, |i| i + 1);
        out.truncate(end);
        (out, jb)
    }

    fn played(out: &[Pulled]) -> Vec<u32> {
        out.iter().filter_map(|p| if let Pulled::Packet(d) = p { Some(d[0] as u32) } else { None }).collect()
    }

    #[test]
    fn a_steady_stream_plays_in_order_after_a_short_wait() {
        let arrivals: Vec<(u32, u64)> = (0..20).map(|i| (i, 100 + i as u64 * 40)).collect();
        let (out, jb) = play(&arrivals, 1200);
        assert_eq!(played(&out), (0..20).collect::<Vec<_>>());
        assert!(!out.contains(&Pulled::Lost));
        assert_eq!(jb.target_ms(), JITTER_MIN_MS);
        // It waited for a packet or two before starting.
        let first = out.iter().position(|p| matches!(p, Pulled::Packet(_))).unwrap();
        assert!((3..=5).contains(&first), "{first}");
    }

    #[test]
    fn missing_and_reordered_packets() {
        let mut arrivals: Vec<(u32, u64)> = (0..12).filter(|i| *i != 6).map(|i| (i, i as u64 * 40)).collect();
        // 4 overtakes 3.
        arrivals.iter_mut().find(|a| a.0 == 4).unwrap().1 = 140;
        arrivals.iter_mut().find(|a| a.0 == 3).unwrap().1 = 150;
        let (out, _) = play(&arrivals, 800);
        assert_eq!(played(&out), [0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11]);
        assert_eq!(out.iter().filter(|p| **p == Pulled::Lost).count(), 1, "6 was concealed");
    }

    #[test]
    fn late_packets_are_dropped() {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(40);
        for i in 0..4 {
            jb.push(i, vec![i as u8], t0);
        }
        assert_eq!(jb.pull(t0), Pulled::Packet(vec![0]));
        assert_eq!(jb.pull(t0), Pulled::Packet(vec![1]));
        jb.push(0, vec![0], t0);
        assert_eq!(jb.pull(t0), Pulled::Packet(vec![2]));
    }

    #[test]
    fn tor_like_bursts_make_it_wait_longer_and_play_smoothly() {
        // Packets made every 40 ms arrive in bursts every 200 ms.
        let arrivals: Vec<(u32, u64)> = (0..150).map(|i| (i, (i as u64 * 40 / 200 + 1) * 200 + 50)).collect();
        let (out, jb) = play(&arrivals, 6400);
        assert!((160..=JITTER_MAX_MS).contains(&jb.target_ms()), "{}", jb.target_ms());
        // After settling in, nothing is concealed.
        let settled = &out[50..];
        assert!(!settled.contains(&Pulled::Lost), "{settled:?}");
        let got = played(&out);
        assert!(got.windows(2).all(|w| w[0] < w[1]), "in order");
        assert!(got.len() >= 140, "{}", got.len());
    }

    #[test]
    fn a_stall_is_ridden_out_without_losing_sound() {
        // Steady, then nothing for 400 ms, then everything at once.
        let arrivals: Vec<(u32, u64)> =
            (0..40).map(|i| (i, if (10..20).contains(&i) { 800 } else { i as u64 * 40 })).collect();
        let (out, jb) = play(&arrivals, 2400);
        assert_eq!(played(&out), (0..40).collect::<Vec<_>>(), "everything played, in order");
        assert!(jb.target_ms() > JITTER_MIN_MS, "learned to wait longer");
    }

    #[test]
    fn a_backlog_shrinks_back() {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(40);
        for i in 0..2 {
            jb.push(i, vec![], t0 + ms(i as u64 * 40));
        }
        jb.pull(t0 + ms(80));
        // A pile arrives at once (on time, judged by the first packet).
        for i in 2..20 {
            jb.push(i, vec![], t0 + ms(i as u64 * 40));
        }
        let before = jb.held_ms();
        for k in 0..10 {
            assert!(matches!(jb.pull(t0 + ms(800 + k * 40)), Pulled::Packet(_)));
        }
        // Ten played, and one skipped to catch up (at most one a second).
        assert_eq!(jb.held_ms(), before - 11 * 40);
    }

    #[test]
    fn it_never_holds_too_much() {
        let t0 = Instant::now();
        let mut jb = JitterBuffer::new(40);
        for i in 0..100 {
            jb.push(i, vec![], t0);
        }
        assert!(jb.held_ms() <= HELD_MAX_MS);
    }
}
