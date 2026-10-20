//! Recognizing friends' phones over Bluetooth without telling strangers
//! anything.
//!
//! A Bluetooth advert holds only a few bytes, so a phone can't carry a tag
//! per friend (as the Wi-Fi beacon does). Instead each phone advertises
//! one short ID that changes every minute, computed from a secret "nearby
//! key". The key is shared with friends only, inside MLS
//! (`Content::NearbyKey`); a friend computes our ID for this minute and
//! recognizes it. To anyone else the ID is random, and it can't be linked
//! from one minute to the next.

use crate::protocol::Content;
use crate::session::{Result, Session};
use crate::store::{OutboxItem, OutboxState};

/// Bytes of the rotating ID in an advert.
pub const ID_LEN: usize = 8;

/// This phone's nearby key, derived from its identity secret: stable, and
/// never sent to anyone but friends.
pub fn nearby_key(identity_secret: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key("sanctum nearby key v1", identity_secret)
}

/// The ID a phone with `key` advertises during `minute` (Unix minutes).
pub fn nearby_id(key: &[u8; 32], minute: u64) -> [u8; ID_LEN] {
    let mut input = b"sanctum nearby id v1".to_vec();
    input.extend_from_slice(&minute.to_le_bytes());
    blake3::keyed_hash(key, &input).as_bytes()[..ID_LEN].try_into().unwrap()
}

fn kv_name(peer: &[u8; 32]) -> Vec<u8> {
    [b"nearby-key:".as_slice(), peer].concat()
}

impl Session {
    /// What this phone advertises right now.
    pub fn my_nearby_id(&self, minute: u64) -> [u8; ID_LEN] {
        nearby_id(&nearby_key(&self.identity.secret()), minute)
    }

    /// The friend whose phone advertises `id` (this minute or the last:
    /// clocks differ a little).
    pub fn nearby_friend(&self, id: &[u8], minute: u64) -> Result<Option<[u8; 32]>> {
        for friend in self.friend_keys()? {
            let Some(key) = self.store.kv_get::<[u8; 32]>(&kv_name(&friend))? else {
                continue;
            };
            if [minute, minute.saturating_sub(1)].iter().any(|m| nearby_id(&key, *m) == id) {
                return Ok(Some(friend));
            }
        }
        Ok(None)
    }

    /// Before flushing to a friend: if we don't know their nearby key yet,
    /// send ours (they answer with theirs). Once per friend per run.
    pub(crate) fn offer_nearby_key(&mut self, peer: &[u8; 32]) -> Result<()> {
        if self.store.kv_get::<[u8; 32]>(&kv_name(peer))?.is_some() || !self.nearby_offered.insert(*peer) {
            return Ok(());
        }
        self.queue_nearby_key(peer)
    }

    /// A friend's nearby key arrived.
    pub(crate) fn on_nearby_key(&mut self, peer: &[u8; 32], key: [u8; 32]) -> Result<()> {
        let known = self.store.kv_get::<[u8; 32]>(&kv_name(peer))?;
        self.store.kv_put(&kv_name(peer), &key)?;
        // New to us: they may not have ours either.
        if known.is_none() && self.nearby_offered.insert(*peer) {
            self.queue_nearby_key(peer)?;
        }
        Ok(())
    }

    pub(crate) fn forget_nearby_key(&self, peer: &[u8; 32]) -> Result<()> {
        self.store.kv_delete(&kv_name(peer))?;
        Ok(())
    }

    fn queue_nearby_key(&mut self, peer: &[u8; 32]) -> Result<()> {
        let content = Content::NearbyKey(nearby_key(&self.identity.secret()));
        self.store.outbox_put(
            &rand::random(),
            &OutboxItem {
                peer: *peer,
                state: OutboxState::Plain(content.encode()),
                message: None,
            },
        )?;
        Ok(())
    }
}
