//! The open session (identity + encrypted store + MLS), shared by the API.

use sanctum_core::session::{Paired, Session};
use std::sync::{LazyLock, Mutex, MutexGuard};

static SESSION: LazyLock<Mutex<Option<Session>>> = LazyLock::new(Default::default);

/// Opens the encrypted store at `dir` and restores MLS state. Call once at
/// startup, before anything else that touches chats.
pub fn session_open(
    identity_secret: Vec<u8>,
    dir: String,
    store_key: Vec<u8>,
) -> anyhow::Result<()> {
    let secret: [u8; 32] = identity_secret
        .try_into()
        .map_err(|_| anyhow::anyhow!("identity secret must be 32 bytes"))?;
    let key: [u8; 32] = store_key
        .try_into()
        .map_err(|_| anyhow::anyhow!("store key must be 32 bytes"))?;
    let session = Session::open(secret, std::path::Path::new(&dir).join("sanctum.db"), &key)?;
    *lock() = Some(session);
    Ok(())
}

pub struct PairedView {
    pub chat_id: i64,
    pub friend: String,
    pub sigil_key: Vec<u8>,
    /// Read aloud on a call to confirm a remote pairing.
    pub safety_number: String,
}

impl From<Paired> for PairedView {
    fn from(p: Paired) -> Self {
        Self {
            chat_id: p.chat_id,
            friend: p.friend,
            safety_number: sanctum_core::identity::safety_number(&p.sigil_key),
            sigil_key: p.sigil_key,
        }
    }
}

/// Step 1: the code to show a friend.
pub fn invite_create(my_name: String) -> anyhow::Result<String> {
    with(|s| Ok(s.create_offer(&my_name)?))
}

pub struct AcceptView {
    pub paired: PairedView,
    /// The reply code to show the friend.
    pub reply: String,
}

/// Step 2: scanned a friend's code; returns the chat and the reply to show.
pub fn invite_accept(code: String, my_name: String) -> anyhow::Result<AcceptView> {
    with(|s| {
        let (paired, reply) = s.accept_offer(&code, &my_name)?;
        Ok(AcceptView {
            paired: paired.into(),
            reply,
        })
    })
}

/// Step 3: scanned the friend's reply; joins the chat.
pub fn invite_complete(code: String) -> anyhow::Result<PairedView> {
    with(|s| Ok(s.complete(&code)?.into()))
}

pub(crate) fn lock() -> MutexGuard<'static, Option<Session>> {
    SESSION.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn with<T>(f: impl FnOnce(&mut Session) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let mut guard = lock();
    let session = guard
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("session is not open"))?;
    f(session)
}
