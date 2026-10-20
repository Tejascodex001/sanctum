//! A signed-in device: identity, encrypted store and MLS client together.
//! Everything the app does with friends goes through here, so pairing and
//! messaging can be tested end to end without the UI.

use crate::identity::{pair_fingerprint, Identity};
use crate::invite::{Acceptance, Card, InviteError, Offer};
use crate::mls::{MlsClient, MlsError, Received};
use crate::store::{ChatKind, ChatMeta, Peer, Store, StoreError, StoredMessage};
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
    #[error("no code is being shown; show your code first")]
    NoPendingOffer,
    #[error("no such chat")]
    NoSuchChat,
    #[error("the code's key doesn't match its owner")]
    Mismatch,
}

pub type Result<T> = std::result::Result<T, SessionError>;

/// A chat both sides just created by pairing.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub chat_id: i64,
    pub friend: String,
    /// Both phones derive the same sigil from this.
    pub sigil_key: Vec<u8>,
}

pub struct Session {
    identity: Identity,
    pub store: Store,
    mls: MlsClient,
    /// The offer currently on screen, kept to check the reply against.
    pending_offer: Option<Offer>,
}

impl Session {
    pub fn open(
        identity_secret: [u8; 32],
        path: impl AsRef<Path>,
        store_key: &[u8; 32],
    ) -> Result<Self> {
        let identity = Identity::from_secret(identity_secret);
        let store = Store::open(path, store_key)?;
        let mls = MlsClient::load(&identity, &store)?;
        Ok(Self {
            identity,
            store,
            mls,
            pending_offer: None,
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.identity.public_key()
    }

    /// Step 1 (the person being added): a code for a friend to scan.
    pub fn create_offer(&mut self, name: &str) -> Result<String> {
        let offer = Offer {
            card: self.card(name),
            key_package: self.mls.key_package()?,
            created_at: now(),
        };
        self.mls.persist(&self.store)?;
        let code = offer.encode(&self.identity);
        self.pending_offer = Some(offer);
        Ok(code)
    }

    /// Step 2 (the person adding): scan their code, create the chat, and
    /// return the reply code to show them.
    pub fn accept_offer(&mut self, code: &str, name: &str) -> Result<(Paired, String)> {
        let offer = Offer::decode(code)?;
        let friend = offer.card.identity;
        if friend == self.public_key() {
            return Err(SessionError::OwnCode);
        }
        self.ensure_not_friends(&friend)?;

        let group = self.mls.create_group()?;
        let invite = self.mls.invite(&group, &offer.key_package)?;
        self.mls.persist(&self.store)?;
        // MLS already checked the key package's identity; make sure it is
        // the same person the code claims to be.
        if !self.mls.members(&group)?.contains(&friend) {
            return Err(SessionError::Mismatch);
        }

        let paired = self.save_chat(&offer.card, group)?;
        let reply = Acceptance {
            card: self.card(name),
            welcome: invite.welcome,
            offer_hash: offer.hash(),
        }
        .encode(&self.identity);
        Ok((paired, reply))
    }

    /// Step 3 (the person being added): scan the reply and join the chat.
    pub fn complete(&mut self, code: &str) -> Result<Paired> {
        let offer = self
            .pending_offer
            .as_ref()
            .ok_or(SessionError::NoPendingOffer)?;
        let acceptance = Acceptance::decode(code, offer)?;
        let friend = acceptance.card.identity;
        self.ensure_not_friends(&friend)?;

        let group = self.mls.join(&acceptance.welcome)?;
        self.mls.persist(&self.store)?;
        if !self.mls.members(&group)?.contains(&friend) {
            return Err(SessionError::Mismatch);
        }
        self.pending_offer = None;
        self.save_chat(&acceptance.card, group)
    }

    /// Encrypts a message for a chat and stores our copy. Returns the
    /// ciphertext to deliver.
    pub fn send(&mut self, chat_id: i64, body: &str) -> Result<Vec<u8>> {
        let meta = self.chat(chat_id)?;
        let ciphertext = self.mls.encrypt(&meta.group_id, body.as_bytes())?;
        self.mls.persist(&self.store)?;
        self.store.add_message(
            chat_id,
            &StoredMessage {
                from_me: true,
                body: body.into(),
                sent_at: now(),
            },
        )?;
        Ok(ciphertext)
    }

    /// Decrypts a delivered message for a chat and stores it.
    pub fn receive(&mut self, chat_id: i64, ciphertext: &[u8]) -> Result<Received> {
        let meta = self.chat(chat_id)?;
        let received = self.mls.receive(&meta.group_id, ciphertext)?;
        self.mls.persist(&self.store)?;
        if let Received::Message { plaintext, .. } = &received {
            self.store.add_message(
                chat_id,
                &StoredMessage {
                    from_me: false,
                    body: String::from_utf8_lossy(plaintext).into_owned(),
                    sent_at: now(),
                },
            )?;
        }
        Ok(received)
    }

    pub fn chat(&self, chat_id: i64) -> Result<ChatMeta> {
        self.store
            .chats()?
            .into_iter()
            .find(|(id, _)| *id == chat_id)
            .map(|(_, m)| m)
            .ok_or(SessionError::NoSuchChat)
    }

    fn card(&self, name: &str) -> Card {
        Card {
            identity: self.public_key(),
            name: name.trim().into(),
            onion: None,
        }
    }

    fn ensure_not_friends(&self, friend: &[u8; 32]) -> Result<()> {
        let already = self.store.chats()?.iter().any(|(_, m)| {
            m.kind == ChatKind::Direct && m.peers.iter().any(|p| &p.identity == friend)
        });
        if already {
            Err(SessionError::AlreadyFriends)
        } else {
            Ok(())
        }
    }

    fn save_chat(&self, friend: &Card, group_id: Vec<u8>) -> Result<Paired> {
        let sigil_key = pair_fingerprint(&self.public_key(), &friend.identity).to_vec();
        let name = if friend.name.is_empty() {
            "Friend".to_string()
        } else {
            friend.name.clone()
        };
        let chat_id = self.store.create_chat(&ChatMeta {
            kind: ChatKind::Direct,
            title: name.clone(),
            sigil_key: sigil_key.clone(),
            pinned: false,
            created_at: now(),
            group_id,
            peers: vec![Peer {
                identity: friend.identity,
                name: name.clone(),
                onion: friend.onion.clone(),
            }],
        })?;
        Ok(Paired {
            chat_id,
            friend: name,
            sigil_key,
        })
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Phone {
        session: Session,
        secret: [u8; 32],
        key: [u8; 32],
        dir: TempDir,
    }

    impl Phone {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let secret = Identity::generate().secret();
            let key = rand::random();
            let session = Session::open(secret, dir.path().join("s.db"), &key).unwrap();
            Self {
                session,
                secret,
                key,
                dir,
            }
        }

        fn restart(&mut self) {
            self.session =
                Session::open(self.secret, self.dir.path().join("s.db"), &self.key).unwrap();
        }
    }

    /// Bob shows his code, Alice scans it and shows a reply, Bob scans that.
    fn pair(alice: &mut Phone, bob: &mut Phone) -> (Paired, Paired) {
        let offer = bob.session.create_offer("Bob").unwrap();
        let (on_alice, reply) = alice.session.accept_offer(&offer, "Alice").unwrap();
        let on_bob = bob.session.complete(&reply).unwrap();
        (on_alice, on_bob)
    }

    #[test]
    fn pairing_in_person_then_chatting() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let offer = bob.session.create_offer("Bob").unwrap();
        let (on_alice, reply) = alice.session.accept_offer(&offer, "Alice").unwrap();
        println!(
            "offer code: {} chars, reply code: {} chars",
            offer.len(),
            reply.len()
        );
        // Must fit comfortably in a QR code scanned phone to phone.
        assert!(offer.len() < 1200, "offer is {} chars", offer.len());
        assert!(reply.len() < 2400, "reply is {} chars", reply.len());

        let on_bob = bob.session.complete(&reply).unwrap();
        assert_eq!(on_alice.friend, "Bob");
        assert_eq!(on_bob.friend, "Alice");
        assert_eq!(
            on_alice.sigil_key, on_bob.sigil_key,
            "both phones show the same sigil"
        );

        let ct = alice
            .session
            .send(on_alice.chat_id, "hey, it's me")
            .unwrap();
        assert!(matches!(
            bob.session.receive(on_bob.chat_id, &ct).unwrap(),
            Received::Message { sender, .. } if sender == alice.session.public_key()
        ));
        let ct = bob.session.send(on_bob.chat_id, "got it").unwrap();
        alice.session.receive(on_alice.chat_id, &ct).unwrap();

        let bodies = |p: &Phone, id| -> Vec<(bool, String)> {
            p.session
                .store
                .messages(id, None)
                .unwrap()
                .into_iter()
                .map(|m| (m.from_me, m.body))
                .collect()
        };
        assert_eq!(
            bodies(&alice, on_alice.chat_id),
            [(true, "hey, it's me".into()), (false, "got it".into())]
        );
        assert_eq!(
            bodies(&bob, on_bob.chat_id),
            [(false, "hey, it's me".into()), (true, "got it".into())]
        );
    }

    #[test]
    fn friendship_survives_restarts() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let (a, b) = pair(&mut alice, &mut bob);
        alice.restart();
        bob.restart();
        let ct = alice.session.send(a.chat_id, "still here").unwrap();
        bob.restart();
        bob.session.receive(b.chat_id, &ct).unwrap();
        assert_eq!(
            bob.session.store.messages(b.chat_id, None).unwrap()[0].body,
            "still here"
        );
        let meta = bob.session.chat(b.chat_id).unwrap();
        assert_eq!(meta.kind, ChatKind::Direct);
        assert_eq!(meta.peers[0].identity, alice.session.public_key());
    }

    #[test]
    fn swapped_or_reused_codes_are_refused() {
        let (mut alice, mut bob, mut carol) = (Phone::new(), Phone::new(), Phone::new());

        // A reply made for Carol's code can't be used by Bob.
        bob.session.create_offer("Bob").unwrap();
        let carol_offer = carol.session.create_offer("Carol").unwrap();
        let (_, reply_for_carol) = alice.session.accept_offer(&carol_offer, "Alice").unwrap();
        assert!(matches!(
            bob.session.complete(&reply_for_carol),
            Err(SessionError::Invite(InviteError::NotForUs))
        ));

        // Scanning your own code, or a friend twice, is refused.
        let own = alice.session.create_offer("Alice").unwrap();
        assert!(matches!(
            alice.session.accept_offer(&own, "Alice"),
            Err(SessionError::OwnCode)
        ));
        let carol_again = carol.session.create_offer("Carol").unwrap();
        assert!(matches!(
            alice.session.accept_offer(&carol_again, "Alice"),
            Err(SessionError::AlreadyFriends)
        ));

        // A reply scanned without showing a code first.
        let fresh = Phone::new();
        let mut fresh = fresh;
        assert!(matches!(
            fresh.session.complete(&reply_for_carol),
            Err(SessionError::NoPendingOffer)
        ));
    }

    #[test]
    fn notes_and_friends_coexist() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let notes = alice
            .session
            .store
            .create_chat(&ChatMeta {
                kind: ChatKind::Notes,
                title: "Notes to self".into(),
                sigil_key: alice.session.public_key().to_vec(),
                pinned: false,
                created_at: 1,
                group_id: vec![],
                peers: vec![],
            })
            .unwrap();
        let (a, _) = pair(&mut alice, &mut bob);
        let chats = alice.session.store.chats().unwrap();
        assert_eq!(chats.len(), 2);
        assert!(chats.iter().any(|(id, _)| *id == notes));
        assert!(chats.iter().any(|(id, _)| *id == a.chat_id));
    }
}
