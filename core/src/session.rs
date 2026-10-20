//! A signed-in device: identity, encrypted store and MLS client together.
//! Everything the app does with friends goes through here, so pairing and
//! messaging can be tested end to end without the UI.

use crate::identity::{pair_fingerprint, safety_number, Identity};
use crate::invite::{FriendCode, InviteError};
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
    #[error("no such chat")]
    NoSuchChat,
}

pub type Result<T> = std::result::Result<T, SessionError>;

/// A friend just added from their code.
#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub chat_id: i64,
    pub friend: String,
    /// Both phones derive the same sigil from this.
    pub sigil_key: Vec<u8>,
    /// Both phones show this number, to compare on a call.
    pub safety_number: String,
}

pub struct Session {
    identity: Identity,
    pub store: Store,
    mls: MlsClient,
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
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.identity.public_key()
    }

    /// Your friend code, to show as a QR or share.
    pub fn my_code(&self, name: &str) -> String {
        FriendCode::new(self.public_key(), name).encode()
    }

    /// Adds the friend whose code was scanned, pasted or typed. Each friend
    /// adds the other's code; the encrypted session is set up over Tor on
    /// first contact.
    pub fn add_friend(&mut self, code: &str) -> Result<Paired> {
        let friend = FriendCode::decode(code)?;
        if friend.identity == self.public_key() {
            return Err(SessionError::OwnCode);
        }
        let already = self.store.chats()?.iter().any(|(_, m)| {
            m.kind == ChatKind::Direct && m.peers.iter().any(|p| p.identity == friend.identity)
        });
        if already {
            return Err(SessionError::AlreadyFriends);
        }

        let sigil_key = pair_fingerprint(&self.public_key(), &friend.identity).to_vec();
        let name = if friend.name.is_empty() {
            "Friend".to_string()
        } else {
            friend.name
        };
        let chat_id = self.store.create_chat(&ChatMeta {
            kind: ChatKind::Direct,
            title: name.clone(),
            sigil_key: sigil_key.clone(),
            pinned: false,
            created_at: now(),
            group_id: Vec::new(),
            peers: vec![Peer {
                identity: friend.identity,
                name: name.clone(),
                onion: None,
            }],
        })?;
        Ok(Paired {
            chat_id,
            friend: name,
            safety_number: safety_number(&sigil_key),
            sigil_key,
        })
    }

    /// Stores our message and, once the chat's encrypted session exists,
    /// returns the ciphertext to deliver. Until then the message waits here.
    pub fn send(&mut self, chat_id: i64, body: &str) -> Result<Option<Vec<u8>>> {
        let meta = self.chat(chat_id)?;
        let ciphertext = if meta.group_id.is_empty() {
            None
        } else {
            let ct = self.mls.encrypt(&meta.group_id, body.as_bytes())?;
            self.mls.persist(&self.store)?;
            Some(ct)
        };
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

    #[test]
    fn friends_add_each_other_and_see_the_same_sigil_and_number() {
        let (mut alice, mut bob) = (Phone::new(), Phone::new());
        let alice_code = alice.session.my_code("Alice");
        let bob_code = bob.session.my_code("Bob");
        println!("friend code: {} chars: {alice_code}", alice_code.len());

        let on_alice = alice.session.add_friend(&bob_code).unwrap();
        let on_bob = bob.session.add_friend(&alice_code).unwrap();
        assert_eq!(on_alice.friend, "Bob");
        assert_eq!(on_bob.friend, "Alice");
        assert_eq!(on_alice.sigil_key, on_bob.sigil_key);
        assert_eq!(on_alice.safety_number, on_bob.safety_number);

        // A swapped code (Mallory's instead of Bob's) gives a different number.
        let mallory = Phone::new();
        let mut carol = Phone::new();
        let swapped = carol
            .session
            .add_friend(&mallory.session.my_code("Bob"))
            .unwrap();
        assert_ne!(swapped.safety_number, on_alice.safety_number);
    }

    #[test]
    fn friends_survive_restarts_and_messages_wait_for_tor() {
        let (mut alice, bob) = (Phone::new(), Phone::new());
        let paired = alice
            .session
            .add_friend(&bob.session.my_code("Bob"))
            .unwrap();
        // No encrypted session yet: the message is kept, nothing to deliver.
        assert_eq!(alice.session.send(paired.chat_id, "see you").unwrap(), None);
        alice.restart();
        let meta = alice.session.chat(paired.chat_id).unwrap();
        assert_eq!(meta.kind, ChatKind::Direct);
        assert_eq!(meta.peers[0].identity, bob.session.public_key());
        assert_eq!(
            alice.session.store.messages(paired.chat_id, None).unwrap()[0].body,
            "see you"
        );
    }

    #[test]
    fn own_code_twice_and_typos_are_refused() {
        let (mut alice, bob) = (Phone::new(), Phone::new());
        let own = alice.session.my_code("Alice");
        assert!(matches!(
            alice.session.add_friend(&own),
            Err(SessionError::OwnCode)
        ));
        let code = bob.session.my_code("Bob");
        alice.session.add_friend(&code).unwrap();
        assert!(matches!(
            alice.session.add_friend(&code),
            Err(SessionError::AlreadyFriends)
        ));
        let mut typo: Vec<char> = code.chars().collect();
        let i = typo.len() - 4;
        typo[i] = if typo[i] == 'A' { 'B' } else { 'A' };
        let typo: String = typo.into_iter().collect();
        assert!(matches!(
            Phone::new().session.add_friend(&typo),
            Err(SessionError::Invite(
                InviteError::Typo | InviteError::Malformed
            ))
        ));
    }

    #[test]
    fn notes_and_friends_coexist() {
        let (mut alice, bob) = (Phone::new(), Phone::new());
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
        let friend = alice
            .session
            .add_friend(&bob.session.my_code("Bob"))
            .unwrap();
        let chats = alice.session.store.chats().unwrap();
        assert_eq!(chats.len(), 2);
        assert!(chats.iter().any(|(id, _)| *id == notes));
        assert!(chats.iter().any(|(id, _)| *id == friend.chat_id));
    }
}
