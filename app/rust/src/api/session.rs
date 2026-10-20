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
            sigil_key: p.sigil_key,
            safety_number: p.safety_number,
        }
    }
}

/// Your friend code, to show as a QR or share.
pub fn my_code(my_name: String) -> anyhow::Result<String> {
    with(|s| Ok(s.my_code(&my_name)))
}

/// Adds the friend whose code was scanned, pasted or typed.
pub fn friend_add(code: String) -> anyhow::Result<PairedView> {
    with(|s| Ok(s.add_friend(&code)?.into()))
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
