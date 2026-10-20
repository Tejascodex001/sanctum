//! Tor, via Arti. Every phone is an onion service whose key is its Sanctum
//! identity key, so a friend's code is their address. Nobody (friends,
//! network operators, Tor relays) learns anyone's IP address.

use crate::engine::{BoxConn, Node, Transport};
use arti_client::config::TorClientConfigBuilder;
use arti_client::{TorClient, TorClientConfig};
use async_trait::async_trait;
use futures::StreamExt;
use sanctum_core::identity::onion_address;
use std::path::Path;
use std::sync::Arc;
use tor_cell::relaycell::msg::Connected;
use tor_hscrypto::pk::HsIdKeypair;
use tor_hsservice::config::OnionServiceConfigBuilder;
use tor_hsservice::{handle_rend_requests, RunningOnionService};
use tor_llcrypto::pk::ed25519;

/// The virtual port Sanctum listens on inside its onion service.
pub const PORT: u16 = 1;

type Runtime = tor_rtcompat::PreferredRuntime;

pub struct Tor {
    client: Arc<TorClient<Runtime>>,
    _service: Arc<RunningOnionService>,
}

/// Dials friends' onion services.
struct Dialer {
    client: Arc<TorClient<Runtime>>,
}

#[async_trait]
impl Transport for Dialer {
    async fn dial(&self, peer: [u8; 32]) -> anyhow::Result<BoxConn> {
        let stream = self
            .client
            .connect((onion_address(&peer).as_str(), PORT))
            .await?;
        Ok(Box::new(stream))
    }
}

/// The client config: Arti keeps its state and cache in the app's private
/// directories.
fn config(state_dir: &Path, cache_dir: &Path) -> anyhow::Result<TorClientConfig> {
    let mut builder = TorClientConfigBuilder::from_directories(state_dir, cache_dir);
    // Android app directories don't have the Unix ownership Arti expects
    // to verify; they are private to the app already.
    builder.storage().permissions().dangerously_trust_everyone();
    Ok(builder.build()?)
}

impl Tor {
    /// Connects to Tor and starts this phone's onion service. Builds the
    /// [`Node`] (via `make_node`) once Tor can dial, and hands it every
    /// incoming connection.
    pub async fn start(
        state_dir: &Path,
        cache_dir: &Path,
        identity_secret: [u8; 32],
        make_node: impl FnOnce(Arc<dyn Transport>) -> Arc<Node>,
    ) -> anyhow::Result<(Arc<Self>, Arc<Node>)> {
        let client = TorClient::create_bootstrapped(config(state_dir, cache_dir)?).await?;
        let node = make_node(Arc::new(Dialer {
            client: client.isolated_client(),
        }));

        let service_config = OnionServiceConfigBuilder::default()
            .nickname("sanctum".parse()?)
            .build()?;
        let keypair: HsIdKeypair =
            ed25519::ExpandedKeypair::from(&ed25519::Keypair::from_bytes(&identity_secret)).into();
        // The key is stored by Arti on first launch; later launches load it.
        let launched = match client.launch_onion_service_with_hsid(service_config.clone(), keypair)
        {
            Ok(launched) => launched.map(|(service, requests)| (service, requests.boxed())),
            Err(_) => client
                .launch_onion_service(service_config)?
                .map(|(service, requests)| (service, requests.boxed())),
        };
        let (service, requests) =
            launched.ok_or_else(|| anyhow::anyhow!("onion service disabled"))?;

        let accepting = Arc::clone(&node);
        tokio::spawn(async move {
            let mut streams = handle_rend_requests(requests);
            while let Some(request) = streams.next().await {
                let node = Arc::clone(&accepting);
                tokio::spawn(async move {
                    if let Ok(stream) = request.accept(Connected::new_empty()).await {
                        node.accept(Box::new(stream));
                    }
                });
            }
        });
        node.set_online(true);
        Ok((
            Arc::new(Self {
                client,
                _service: service,
            }),
            node,
        ))
    }

    /// Our onion address, as Arti sees it (should equal
    /// `onion_address(our key)`).
    pub fn client(&self) -> &Arc<TorClient<Runtime>> {
        &self.client
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use safelog::DisplayRedacted;
    use sanctum_core::identity::Identity;
    use tor_hscrypto::pk::HsId;

    #[test]
    fn our_onion_address_matches_arti() {
        for _ in 0..20 {
            let id = Identity::generate();
            let hsid = HsId::from(id.public_key());
            assert_eq!(
                onion_address(&id.public_key()),
                hsid.display_unredacted().to_string()
            );
        }
    }

    #[test]
    fn identity_key_becomes_the_onion_key() {
        let id = Identity::generate();
        let keypair: HsIdKeypair =
            ed25519::ExpandedKeypair::from(&ed25519::Keypair::from_bytes(&id.secret())).into();
        let hsid = HsId::from(keypair.as_ref().public().to_bytes());
        assert_eq!(
            onion_address(&id.public_key()),
            hsid.display_unredacted().to_string()
        );
    }
}
