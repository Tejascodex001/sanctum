//! A call's pictures: splitting encoded frames into packets and putting
//! them back together, asking for a fresh start (a keyframe) after a loss,
//! choosing the quality the path can carry, and making sure pictures never
//! crowd out sound.
//!
//! **Frames** come from the phone's hardware encoder (H.264 or VP8). Each
//! is split into fragments of at most [`FRAGMENT`] bytes, so a packet fits
//! a UDP datagram without being split by the network and a Tor link can
//! slip sound in between. Every fragment carries a small header: which
//! fragment of how many, whether the frame is a keyframe, which way up the
//! picture is, and its size. Frames are numbered; a frame's number goes in
//! the sealed packet's `index` and its time on the sender's media clock in
//! `at_ms` (the same clock as the sound, for lip-sync).
//!
//! **Losses:** frames depend on the ones before them, so after a missing
//! frame nothing can be shown until the next keyframe. The receiver drops
//! frames until one arrives and asks the sender for it. A sender that has
//! to drop a frame (the link is full) also sends nothing more until its
//! next keyframe.
//!
//! **Sound first:** on a link, sound and pictures queue separately and
//! sound always goes first. Pictures only queue while the sound is keeping
//! up, and only up to about half a second's worth; past that whole frames
//! are dropped and the quality steps down.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Video formats, by the names phones offer them under. Every Android
/// phone (7 and up) can play both; H.264 is the one their hardware
/// encoders all record.
pub const H264: &str = "h264";
pub const VP8: &str = "vp8";

/// Encoded bytes per fragment.
pub const FRAGMENT: usize = 1100;
/// A frame's header in each fragment.
const HEADER: usize = 8;
/// Frames bigger than this are refused (fragment counts fit a byte).
pub const MAX_FRAME: usize = 255 * FRAGMENT;
/// A frame missing pieces while later ones are complete is given up after
/// this long (UDP can reorder a little; Tor doesn't).
const REORDER_WAIT: Duration = Duration::from_millis(80);
/// A frame still missing pieces after this long is lost.
const PIECES_WAIT: Duration = Duration::from_millis(600);
/// Keyframe requests at most this often.
pub const KEY_REQUEST_EVERY: Duration = Duration::from_millis(600);
/// Frames being put together at once, at most.
const PARTIALS_MAX: usize = 48;
/// Complete frames waiting to be shown, at most.
const READY_MAX: usize = 30;

/// What a frame is, beyond its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameInfo {
    /// A keyframe: can be decoded on its own.
    pub key: bool,
    /// How far to turn the picture clockwise to show it upright: 0, 90,
    /// 180 or 270.
    pub rotation: u16,
    pub width: u16,
    pub height: u16,
}

/// A frame, put back together.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoFrame {
    pub number: u32,
    /// The sender's media clock (the same as its sound's).
    pub at_ms: u32,
    pub info: FrameInfo,
    pub data: Vec<u8>,
}

fn header(index: u8, count: u8, info: FrameInfo) -> [u8; HEADER] {
    let w = info.width.to_be_bytes();
    let h = info.height.to_be_bytes();
    [index, count, u8::from(info.key), (info.rotation / 90 % 4) as u8, w[0], w[1], h[0], h[1]]
}

fn parse(fragment: &[u8]) -> Option<(u8, u8, FrameInfo, &[u8])> {
    if fragment.len() < HEADER {
        return None;
    }
    let (index, count) = (fragment[0], fragment[1]);
    if count == 0 || index >= count || fragment[3] > 3 {
        return None;
    }
    let info = FrameInfo {
        key: fragment[2] & 1 == 1,
        rotation: fragment[3] as u16 * 90,
        width: u16::from_be_bytes([fragment[4], fragment[5]]),
        height: u16::from_be_bytes([fragment[6], fragment[7]]),
    };
    Some((index, count, info, &fragment[HEADER..]))
}

/// Splits a frame into fragments, each with its header. Empty if it's
/// empty or too big.
pub fn fragment(data: &[u8], info: FrameInfo) -> Vec<Vec<u8>> {
    if data.is_empty() || data.len() > MAX_FRAME {
        return Vec::new();
    }
    let count = data.len().div_ceil(FRAGMENT);
    data.chunks(FRAGMENT)
        .enumerate()
        .map(|(i, chunk)| {
            let mut out = Vec::with_capacity(HEADER + chunk.len());
            out.extend_from_slice(&header(i as u8, count as u8, info));
            out.extend_from_slice(chunk);
            out
        })
        .collect()
}

struct Partial {
    at_ms: u32,
    info: FrameInfo,
    pieces: Vec<Option<Vec<u8>>>,
    have: usize,
    first_seen: Instant,
    /// When its last piece arrived.
    complete_at: Option<Instant>,
}

/// Puts frames back together from fragments that may come late, twice, out
/// of order or not at all, and hands them out in order, starting from a
/// keyframe and skipping to the next keyframe after any loss.
pub struct Reassembler {
    partial: BTreeMap<u32, Partial>,
    /// The next frame to hand out (once one has been).
    next: Option<u32>,
    /// Lost something (or just started): only a keyframe will do.
    need_key: bool,
    last_request: Option<Instant>,
    ready: VecDeque<(VideoFrame, Instant)>,
    lost: u32,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self {
            partial: BTreeMap::new(),
            next: None,
            need_key: true,
            last_request: None,
            ready: VecDeque::new(),
            lost: 0,
        }
    }

    /// Frames lost so far (for tests and the curious).
    pub fn lost(&self) -> u32 {
        self.lost
    }

    /// A fragment of frame `number` arrived.
    pub fn push(&mut self, number: u32, at_ms: u32, fragment: &[u8], now: Instant) {
        let Some((index, count, info, bytes)) = parse(fragment) else {
            return;
        };
        if self.next.is_some_and(|next| number < next) {
            return; // Its turn is over.
        }
        if !self.partial.contains_key(&number) && self.partial.len() >= PARTIALS_MAX {
            // Far behind: start over from a keyframe.
            self.partial.clear();
            self.need_key = true;
        }
        let p = self.partial.entry(number).or_insert_with(|| Partial {
            at_ms,
            info,
            pieces: vec![None; count as usize],
            have: 0,
            first_seen: now,
            complete_at: None,
        });
        if p.pieces.len() != count as usize || p.pieces[index as usize].is_some() {
            return; // A duplicate, or doesn't fit the frame.
        }
        if index == 0 {
            p.info = info;
        }
        p.pieces[index as usize] = Some(bytes.to_vec());
        p.have += 1;
        if p.have == p.pieces.len() {
            p.complete_at = Some(now);
        }
        self.advance(now);
    }

    /// Moves complete frames to the ready queue, in order, and gives up on
    /// frames that won't complete.
    pub fn advance(&mut self, now: Instant) {
        loop {
            // Waiting for a keyframe: skip straight to the first complete one.
            if self.need_key {
                let key = self.partial.iter().find(|(_, p)| p.complete_at.is_some() && p.info.key).map(|(n, _)| *n);
                if let Some(k) = key {
                    let skipped = self.partial.range(..k).count() as u32;
                    self.lost += skipped;
                    self.partial = self.partial.split_off(&k);
                    self.next = Some(k);
                    self.need_key = false;
                }
            }
            let Some(next) = self.next.or_else(|| self.partial.keys().next().copied()) else {
                return;
            };
            if self.need_key {
                // Nothing usable until a keyframe; drop what's stale.
                let stale: Vec<u32> = self
                    .partial
                    .iter()
                    .filter(|(_, p)| now.saturating_duration_since(p.first_seen) > PIECES_WAIT)
                    .map(|(n, _)| *n)
                    .collect();
                for n in stale {
                    self.partial.remove(&n);
                    self.lost += 1;
                }
                return;
            }
            match self.partial.get(&next) {
                Some(p) if p.complete_at.is_some() => {
                    let p = self.partial.remove(&next).expect("there");
                    let data: Vec<u8> = p.pieces.into_iter().flatten().flatten().collect();
                    self.ready.push_back((VideoFrame { number: next, at_ms: p.at_ms, info: p.info, data }, now));
                    while self.ready.len() > READY_MAX {
                        // Not being shown: start again from a keyframe.
                        self.ready.clear();
                        self.need_key = true;
                    }
                    self.next = Some(next.wrapping_add(1));
                }
                current => {
                    let later_done = self
                        .partial
                        .range(next.wrapping_add(1)..)
                        .filter_map(|(_, p)| p.complete_at)
                        .min()
                        .is_some_and(|at| now.saturating_duration_since(at) >= REORDER_WAIT);
                    let too_slow =
                        current.is_some_and(|p| now.saturating_duration_since(p.first_seen) >= PIECES_WAIT);
                    if !(later_done || too_slow) {
                        return;
                    }
                    // Lost: nothing after it decodes until a keyframe.
                    self.partial.remove(&next);
                    self.lost += 1;
                    self.next = Some(next.wrapping_add(1));
                    self.need_key = true;
                }
            }
        }
    }

    /// Whether to ask the sender for a keyframe now (at most every
    /// [`KEY_REQUEST_EVERY`] while one is needed).
    pub fn wants_key(&mut self, now: Instant) -> bool {
        // Before the very first frame, the sender starts with a keyframe
        // anyway; ask only once something has arrived.
        if !self.need_key || (self.next.is_none() && self.partial.is_empty()) {
            return false;
        }
        if self.last_request.is_some_and(|t| now.saturating_duration_since(t) < KEY_REQUEST_EVERY) {
            return false;
        }
        self.last_request = Some(now);
        true
    }

    /// The oldest complete frame and when it became complete.
    pub fn peek(&self) -> Option<&(VideoFrame, Instant)> {
        self.ready.front()
    }

    pub fn pop(&mut self) -> Option<VideoFrame> {
        self.ready.pop_front().map(|(f, _)| f)
    }

    pub fn ready_len(&self) -> usize {
        self.ready.len()
    }

    /// Forgets everything (the friend's camera went off): the next frame
    /// shown will be a keyframe.
    pub fn reset(&mut self) {
        *self = Self { last_request: self.last_request, lost: self.lost, ..Self::new() };
    }
}

// ---- Quality ----------------------------------------------------------

/// What the encoder should aim for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    /// Bits per second.
    pub bitrate: u32,
    pub fps: u8,
    /// The picture's shorter side, in pixels.
    pub height: u16,
}

/// What a path can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    pub start: u32,
    pub min: u32,
    pub max: u32,
    pub fps: u8,
    pub height: u16,
}

/// Through Tor: small and slow, so sound gets through.
pub const PRIVATE: Profile = Profile { start: 180_000, min: 80_000, max: 250_000, fps: 12, height: 240 };
/// Nearby or direct: much more room.
pub const FAST: Profile = Profile { start: 600_000, min: 200_000, max: 1_000_000, fps: 24, height: 480 };

/// Steps the bitrate down when the path backs up or loses frames, and
/// slowly back up while it keeps up.
#[derive(Debug, Clone)]
pub struct RateControl {
    profile: Profile,
    bitrate: u32,
    last_cut: Option<Instant>,
    /// Since when nothing has gone wrong.
    calm_since: Instant,
    last_raise: Instant,
}

/// Cuts at most this often (one problem often shows up several ways).
const CUT_EVERY: Duration = Duration::from_millis(500);
/// Calm for this long before raising...
const RAISE_AFTER: Duration = Duration::from_secs(2);
/// ...and then raising this often, by a twentieth of the most.
const RAISE_EVERY: Duration = Duration::from_secs(1);

impl RateControl {
    pub fn new(profile: Profile, now: Instant) -> Self {
        Self { profile, bitrate: profile.start, last_cut: None, calm_since: now, last_raise: now }
    }

    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The path changed (e.g. a direct path came up).
    pub fn set_profile(&mut self, profile: Profile, now: Instant) {
        if profile != self.profile {
            *self = Self::new(profile, now);
        }
    }

    /// The path backed up, or the friend lost a frame.
    pub fn congested(&mut self, now: Instant) {
        self.calm_since = now;
        if self.last_cut.is_some_and(|t| now.saturating_duration_since(t) < CUT_EVERY) {
            return;
        }
        self.last_cut = Some(now);
        self.bitrate = (self.bitrate as u64 * 7 / 10).max(self.profile.min as u64) as u32;
    }

    pub fn tick(&mut self, now: Instant) {
        if now.saturating_duration_since(self.calm_since) >= RAISE_AFTER
            && now.saturating_duration_since(self.last_raise) >= RAISE_EVERY
        {
            self.last_raise = now;
            self.bitrate = (self.bitrate + self.profile.max / 20).min(self.profile.max);
        }
    }

    pub fn quality(&self) -> Quality {
        Quality { bitrate: self.bitrate, fps: self.profile.fps, height: self.profile.height }
    }
}

// ---- Sound before pictures on a link ----------------------------------

/// Call packets waiting to be written on one link (Tor or nearby): sound
/// and pictures, counted separately, because sound always goes first.
#[derive(Debug, Default)]
pub struct Backlog {
    audio: AtomicUsize,
    video_bytes: AtomicUsize,
}

/// Sound packets waiting on a link, at most: about half a second. More
/// means the link can't keep up, and old sound is worse than none, so new
/// packets are dropped until it catches up.
pub const AUDIO_BACKLOG: usize = 12;
/// Pictures only queue while at most this much sound is waiting.
const AUDIO_CALM: usize = 2;
/// Picture bytes waiting on a link, at most. A keyframe always fits.
pub const VIDEO_BACKLOG: usize = 40 * 1024;

impl Backlog {
    /// Room for a sound packet? Takes it if so.
    pub fn take_audio(&self) -> bool {
        self.audio
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < AUDIO_BACKLOG).then_some(n + 1))
            .is_ok()
    }

    /// Room for a whole frame of `bytes`? Takes it if so. Never while sound
    /// is backing up.
    pub fn take_video(&self, bytes: usize) -> bool {
        if self.audio.load(Ordering::Acquire) > AUDIO_CALM {
            return false;
        }
        self.video_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n == 0 || n + bytes <= VIDEO_BACKLOG).then_some(n + bytes)
            })
            .is_ok()
    }

    /// A sound packet was written (or given up).
    pub fn audio_done(&self) {
        let _ = self.audio.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| Some(n.saturating_sub(1)));
    }

    /// `bytes` of pictures were written (or given up).
    pub fn video_done(&self, bytes: usize) {
        let _ = self.video_bytes.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| Some(n.saturating_sub(bytes)));
    }

    pub fn audio_waiting(&self) -> usize {
        self.audio.load(Ordering::Acquire)
    }

    pub fn video_waiting(&self) -> usize {
        self.video_bytes.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn info(key: bool) -> FrameInfo {
        FrameInfo { key, rotation: 270, width: 320, height: 240 }
    }

    /// A frame's bytes: recognisable, `len` long.
    fn bytes(n: u32, len: usize) -> Vec<u8> {
        (0..len).map(|i| (i as u32 * 7 + n) as u8).collect()
    }

    fn numbers(r: &mut Reassembler) -> Vec<u32> {
        std::iter::from_fn(|| r.pop()).map(|f| f.number).collect()
    }

    #[test]
    fn frames_split_and_come_back_whole() {
        let data = bytes(1, 3 * FRAGMENT + 17);
        let parts = fragment(&data, info(true));
        assert_eq!(parts.len(), 4);
        assert!(parts.iter().all(|p| p.len() <= FRAGMENT + HEADER));
        let mut r = Reassembler::new();
        let t = Instant::now();
        for p in parts.iter().rev() {
            r.push(5, 200, p, t);
        }
        let f = r.pop().unwrap();
        assert_eq!((f.number, f.at_ms, f.info, f.data), (5, 200, info(true), data));
        assert!(fragment(&[], info(true)).is_empty());
        assert!(fragment(&vec![0; MAX_FRAME + 1], info(true)).is_empty());
    }

    #[test]
    fn duplicates_and_reordering_are_harmless() {
        let t = Instant::now();
        let mut r = Reassembler::new();
        let frames: Vec<Vec<Vec<u8>>> =
            (0..4).map(|n| fragment(&bytes(n, 2 * FRAGMENT + 5), info(n == 0))).collect();
        // Every piece twice, frames interleaved and backwards within each.
        for round in 0..2 {
            for piece in (0..3).rev() {
                for (n, f) in frames.iter().enumerate() {
                    r.push(n as u32, n as u32 * 40, &f[piece], t + ms(round));
                }
            }
        }
        let got: Vec<VideoFrame> = std::iter::from_fn(|| r.pop()).collect();
        assert_eq!(got.iter().map(|f| f.number).collect::<Vec<_>>(), [0, 1, 2, 3]);
        assert!(got.iter().enumerate().all(|(n, f)| f.data == bytes(n as u32, 2 * FRAGMENT + 5)));
        assert_eq!(r.lost(), 0);
        assert!(!r.wants_key(t));
    }

    #[test]
    fn a_lost_piece_skips_to_the_next_keyframe_and_asks_for_one() {
        let t = Instant::now();
        let mut r = Reassembler::new();
        let push_all = |r: &mut Reassembler, n: u32, key: bool, at: Instant, skip: Option<usize>| {
            for (i, p) in fragment(&bytes(n, 2 * FRAGMENT), info(key)).iter().enumerate() {
                if Some(i) != skip {
                    r.push(n, n * 40, p, at);
                }
            }
        };
        push_all(&mut r, 0, true, t, None);
        push_all(&mut r, 1, false, t, None);
        push_all(&mut r, 2, false, t, Some(1)); // a piece goes missing
        push_all(&mut r, 3, false, t + ms(40), None);
        push_all(&mut r, 4, false, t + ms(80), None);
        assert_eq!(numbers(&mut r), [0, 1]);
        // A moment for the piece to turn up; then 2 is lost, and 3 and 4
        // can't be shown without it.
        r.advance(t + ms(200));
        assert!(r.pop().is_none());
        assert!(r.wants_key(t + ms(200)));
        assert!(!r.wants_key(t + ms(300)), "not too often");
        assert!(r.wants_key(t + ms(200) + KEY_REQUEST_EVERY));
        push_all(&mut r, 5, false, t + ms(300), None);
        push_all(&mut r, 6, true, t + ms(340), None);
        push_all(&mut r, 7, false, t + ms(380), None);
        assert_eq!(numbers(&mut r), [6, 7]);
        assert!(!r.wants_key(t + ms(2000)), "back on track");
        // A late piece of an old frame changes nothing.
        push_all(&mut r, 2, false, t + ms(400), None);
        assert!(r.pop().is_none());
    }

    #[test]
    fn it_starts_from_a_keyframe() {
        let t = Instant::now();
        let mut r = Reassembler::new();
        // Joined late (the camera came on before we were listening).
        for n in 10..13 {
            for p in fragment(&bytes(n, 10), info(false)) {
                r.push(n, 0, &p, t);
            }
        }
        assert!(r.pop().is_none());
        assert!(r.wants_key(t));
        for p in fragment(&bytes(13, 10), info(true)) {
            r.push(13, 0, &p, t);
        }
        assert_eq!(numbers(&mut r), [13]);
    }

    #[test]
    fn a_frame_missing_pieces_for_too_long_is_lost() {
        let t = Instant::now();
        let mut r = Reassembler::new();
        for p in fragment(&bytes(0, 10), info(true)) {
            r.push(0, 0, &p, t);
        }
        let half = fragment(&bytes(1, 2 * FRAGMENT), info(false));
        r.push(1, 40, &half[0], t);
        r.advance(t + ms(100));
        assert_eq!(numbers(&mut r), [0]);
        assert!(!r.wants_key(t + ms(100)), "may still come");
        r.advance(t + PIECES_WAIT + ms(1));
        assert!(r.wants_key(t + PIECES_WAIT + ms(1)));
    }

    #[test]
    fn rubbish_is_ignored() {
        let mut r = Reassembler::new();
        let t = Instant::now();
        r.push(0, 0, &[1, 1, 0, 0, 0, 0, 0, 0, 9], t); // index past count
        r.push(0, 0, &[0, 0, 0, 0], t);
        r.push(0, 0, &[0, 1, 1, 7, 0, 0, 0, 0, 9], t); // no such rotation
        assert!(r.pop().is_none());
    }

    #[test]
    fn quality_steps_down_when_congested_and_creeps_back() {
        let t = Instant::now();
        let mut rate = RateControl::new(PRIVATE, t);
        assert_eq!(rate.quality(), Quality { bitrate: 180_000, fps: 12, height: 240 });
        rate.congested(t);
        assert_eq!(rate.quality().bitrate, 126_000);
        rate.congested(t + ms(100));
        assert_eq!(rate.quality().bitrate, 126_000, "one problem, one cut");
        for i in 1..10 {
            rate.congested(t + ms(600 * i));
        }
        assert_eq!(rate.quality().bitrate, PRIVATE.min);
        let calm = t + ms(5400);
        rate.tick(calm + ms(1000));
        assert_eq!(rate.quality().bitrate, PRIVATE.min, "calm, but not for long");
        for s in 2..40 {
            rate.tick(calm + Duration::from_secs(s));
        }
        assert_eq!(rate.quality().bitrate, PRIVATE.max);
        rate.set_profile(FAST, calm);
        assert_eq!(rate.quality(), Quality { bitrate: 600_000, fps: 24, height: 480 });
    }

    #[test]
    fn sound_goes_first_when_a_link_backs_up() {
        // A slow link: it writes one sound packet and 1.5 KB of pictures
        // per 40 ms tick, while a 40 ms sound packet and a 3 KB frame (a
        // 16 KB keyframe every second) arrive each tick.
        let link = Backlog::default();
        let mut queue: VecDeque<(bool, usize)> = VecDeque::new();
        let (mut audio_sent, mut audio_dropped, mut video_sent, mut video_dropped) = (0, 0, 0, 0);
        for tick in 0..250 {
            if link.take_audio() {
                queue.push_front((true, 1)); // sound jumps the queue
                audio_sent += 1;
            } else {
                audio_dropped += 1;
            }
            let frame = if tick % 25 == 0 { 16 * 1024 } else { 3 * 1024 };
            if link.take_video(frame) {
                queue.push_back((false, frame));
                video_sent += 1;
            } else {
                video_dropped += 1;
            }
            // The link writes: sound first, then up to 1.5 KB of pictures.
            let mut budget = 1536usize;
            while let Some((audio, size)) = queue.front().copied() {
                if audio {
                    queue.pop_front();
                    link.audio_done();
                } else if budget > 0 {
                    let n = size.min(budget);
                    budget -= n;
                    if n == size {
                        queue.pop_front();
                    } else {
                        queue.front_mut().unwrap().1 -= n;
                    }
                    link.video_done(n);
                } else {
                    break;
                }
            }
        }
        assert_eq!(audio_dropped, 0, "sound always got through");
        assert_eq!(audio_sent, 250);
        assert!(video_dropped > 0 && video_sent > 0, "{video_sent} sent, {video_dropped} dropped");
        assert!(link.video_waiting() <= VIDEO_BACKLOG + 16 * 1024);
    }

    #[test]
    fn no_pictures_while_sound_is_stuck() {
        let link = Backlog::default();
        assert!(link.take_video(500));
        for _ in 0..AUDIO_BACKLOG {
            assert!(link.take_audio());
        }
        assert!(!link.take_audio(), "full");
        assert!(!link.take_video(10), "sound is backing up: pictures wait");
        for _ in 0..AUDIO_BACKLOG {
            link.audio_done();
        }
        link.video_done(500);
        assert!(link.take_video(VIDEO_BACKLOG + 1), "an empty queue takes even a big keyframe");
        assert!(!link.take_video(1));
    }
}
