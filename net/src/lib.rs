//! Sanctum's network engine: connections between friends' phones.
//!
//! [`engine::Node`] runs the protocol over any [`engine::Transport`]; in
//! the app that is [`tor::Tor`], where every phone is an onion service
//! whose key is its identity key.

pub mod direct;
pub mod engine;
pub mod feedback;
pub mod https;
pub mod nearby;
pub mod tor;

pub use engine::{NetEvent, Node, Transport};
