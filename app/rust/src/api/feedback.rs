//! Feedback to the developer, delivered to their own Sanctum through Tor
//! (see `sanctum_core::feedback`), and the developer's inbox.

use super::session::{emit, real_session, runtime, profile_is_decoy, with_real, AppEventKind, TOR};
use sanctum_core::feedback::{Feedback, Received, Refused};
use sanctum_core::identity::{onion_address, parse_onion_address};
use sanctum_net::feedback::Gate;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use sanctum_net::tor::RunningOnionService;

/// Where everyone's feedback goes: the developer's feedback address, from
/// their inbox screen. Empty until it's set; the app then says feedback
/// isn't open yet.
const DEVELOPER: &str = "zh6d2lkkgnnjt2sp6poebrle647ejzk6mmgvk4wdfwhae4p4mvln3yqd.onion";
/// Undelivered feedback is retried soon at first (a feedback address that
/// just came online takes a few minutes to be found), then every 10
/// minutes: these many minutes after each failed try.
const RETRY_AFTER: [u64; 4] = [1, 2, 5, 10];
/// How often the retry loop looks for feedback that's due.
const RETRY_TICK: Duration = Duration::from_secs(60);

/// The feedback onion service, while receiving is on.
static SERVICE: LazyLock<Mutex<Option<Arc<RunningOnionService>>>> = LazyLock::new(Default::default);
static GATE: LazyLock<Arc<Gate>> = LazyLock::new(Default::default);
static SENDING: AtomicBool = AtomicBool::new(false);
/// Failed tries at a piece of feedback, and when to try next.
type Tries = HashMap<[u8; 16], (usize, Instant)>;
/// For each piece of feedback that couldn't be delivered yet. Memory only;
/// a restart tries everything again.
static TRIES: LazyLock<Mutex<Tries>> = LazyLock::new(Default::default);
static RETRYING: AtomicBool = AtomicBool::new(false);

/// What someone fills in.
pub struct FeedbackForm {
    /// Maker and model, or empty to leave it out.
    pub phone: String,
    /// Android's version, or empty to leave it out.
    pub android: String,
    /// Sanctum's build, or 0 to leave it out.
    pub build: i64,
    /// 1–5 stars, 0 for none.
    pub rating: u8,
    pub text: String,
}

pub enum FeedbackSent {
    /// On its way; delivered privately as soon as it can be.
    Queued,
    /// Nothing written and no stars.
    Empty,
    /// This phone has sent several today already.
    TooMany,
    /// This build doesn't know where to send it.
    NotOpen,
}

/// Feedback in the developer's inbox.
#[derive(Clone)]
pub struct FeedbackItem {
    /// When it arrived, in seconds since 1970.
    pub at: i64,
    pub phone: String,
    pub android: String,
    /// 0: not given.
    pub build: i64,
    /// 0: no stars.
    pub rating: u8,
    pub text: String,
}

fn developer() -> Option<[u8; 32]> {
    parse_onion_address(DEVELOPER)
}

/// Whether this build can send feedback (it knows where to).
#[flutter_rust_bridge::frb(sync)]
pub fn feedback_open() -> bool {
    developer().is_some()
}

/// Queues `form` for the developer and starts delivering it.
pub fn feedback_send(form: FeedbackForm) -> anyhow::Result<FeedbackSent> {
    let Some(to) = developer() else { return Ok(FeedbackSent::NotOpen) };
    let feedback = Feedback {
        phone: form.phone,
        android: form.android,
        build: form.build,
        rating: form.rating,
        text: form.text,
    };
    let queued = with_real(|s| Ok(s.feedback_queue(to, &feedback)?))?;
    Ok(match queued {
        Ok(()) => {
            deliver();
            FeedbackSent::Queued
        }
        Err(Refused::Empty) => FeedbackSent::Empty,
        Err(Refused::TooMany) => FeedbackSent::TooMany,
    })
}

/// How many pieces of feedback are still on their way.
pub fn feedback_waiting() -> anyhow::Result<u32> {
    with_real(|s| Ok(s.feedback_waiting()?.len() as u32))
}

/// Tries to deliver the waiting feedback that's due, in the background.
/// Each piece goes on circuits of its own and says nothing about who sent
/// it.
fn deliver() {
    let Some(tor) = TOR.lock().unwrap().clone() else { return };
    if SENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    runtime().spawn(async move {
        let waiting = with_real(|s| Ok(s.feedback_waiting()?)).unwrap_or_default();
        let now = Instant::now();
        let due = waiting.into_iter().filter(|q| TRIES.lock().unwrap().get(&q.id).is_none_or(|(_, at)| *at <= now));
        for queued in due.collect::<Vec<_>>() {
            let sent = async {
                let mut conn = tor.dial_alone(&queued.to).await?;
                sanctum_net::feedback::send(&mut conn, &queued.feedback).await
            };
            if tokio::time::timeout(Duration::from_secs(120), sent).await.is_ok_and(|r| r.is_ok()) {
                let _ = with_real(|s| Ok(s.feedback_delivered(&queued.id)?));
                TRIES.lock().unwrap().remove(&queued.id);
            } else {
                let mut tries = TRIES.lock().unwrap();
                let failed = tries.get(&queued.id).map_or(0, |(n, _)| *n) + 1;
                let wait = RETRY_AFTER[(failed - 1).min(RETRY_AFTER.len() - 1)];
                tries.insert(queued.id, (failed, Instant::now() + Duration::from_secs(wait * 60)));
            }
        }
        SENDING.store(false, Ordering::Release);
    });
}

/// Tor is up: deliver what's waiting (and keep retrying), and take
/// feedback in if this is the developer's phone.
pub(crate) fn tor_up() {
    deliver();
    if !RETRYING.swap(true, Ordering::AcqRel) {
        runtime().spawn(async {
            loop {
                tokio::time::sleep(RETRY_TICK).await;
                deliver();
            }
        });
    }
    if with_real(|s| Ok(s.feedback_receiving()?)).unwrap_or(false) {
        let _ = serve();
    }
}

// ---- The developer's side ----

/// Whether this phone takes feedback in. Always off while the decoy shows.
pub fn feedback_receiving() -> anyhow::Result<bool> {
    if profile_is_decoy() {
        return Ok(false);
    }
    with_real(|s| Ok(s.feedback_receiving()?))
}

/// Turns taking feedback in on or off. On, this phone serves its feedback
/// address (once Tor is up); off, it stops, and keeps the same address
/// for next time.
pub fn feedback_set_receiving(enabled: bool) -> anyhow::Result<()> {
    anyhow::ensure!(!profile_is_decoy(), "not available");
    with_real(|s| Ok(s.set_feedback_receiving(enabled)?))?;
    if enabled {
        serve()
    } else {
        SERVICE.lock().unwrap().take();
        Ok(())
    }
}

/// This phone's feedback address, to build into the app.
pub fn feedback_address() -> anyhow::Result<String> {
    anyhow::ensure!(!profile_is_decoy(), "not available");
    with_real(|s| Ok(onion_address(&s.feedback_identity()?.public_key())))
}

/// Starts the feedback onion service, if Tor is up and it isn't running.
fn serve() -> anyhow::Result<()> {
    let Some(tor) = TOR.lock().unwrap().clone() else { return Ok(()) };
    let mut slot = SERVICE.lock().unwrap();
    if slot.is_some() {
        return Ok(());
    }
    let secret = with_real(|s| Ok(s.feedback_identity()?.secret()))?;
    let _guard = runtime().enter();
    let service = tor.serve_feedback(&secret, |conn| {
        runtime().spawn(async move {
            let _ = sanctum_net::feedback::serve(conn, &GATE, |feedback| {
                let kept = real_session().is_ok_and(|s| s.lock().unwrap().feedback_arrived(&feedback).unwrap_or(false));
                if kept {
                    emit(AppEventKind::Feedback, 0);
                }
                kept
            })
            .await;
        });
    })?;
    *slot = Some(service);
    Ok(())
}

/// Stops taking feedback in for now (the panic wipe).
pub(crate) fn stop() {
    SERVICE.lock().unwrap().take();
}

/// Feedback received, newest first. Empty while the decoy shows.
pub fn feedback_inbox() -> anyhow::Result<Vec<FeedbackItem>> {
    if profile_is_decoy() {
        return Ok(Vec::new());
    }
    with_real(|s| Ok(s.feedback_inbox()?.into_iter().map(item).collect()))
}

pub fn feedback_delete(item: FeedbackItem) -> anyhow::Result<()> {
    anyhow::ensure!(!profile_is_decoy(), "not available");
    let received = Received {
        at: item.at as u64,
        feedback: Feedback {
            phone: item.phone,
            android: item.android,
            build: item.build,
            rating: item.rating,
            text: item.text,
        },
    };
    with_real(|s| Ok(s.feedback_delete(&received)?))
}

fn item(r: Received) -> FeedbackItem {
    FeedbackItem {
        at: r.at as i64,
        phone: r.feedback.phone,
        android: r.feedback.android,
        build: r.feedback.build,
        rating: r.feedback.rating,
        text: r.feedback.text,
    }
}
