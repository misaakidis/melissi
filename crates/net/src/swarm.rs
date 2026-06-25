//! The one place a transport — and the node's libp2p behaviour — is chosen.
//!
//! Everything *above* the connection (handshake, pull-sync, hive, pricing) runs
//! over [`libp2p_stream`] byte streams, transport-blind. But a bee peer also
//! expects two libp2p baseline protocols *on* the connection, so we compose them
//! into one [`NodeBehaviour`]:
//!   - **identify** — bee learns our listen address from it and, with that,
//!     *persists our overlay*; without it bee Resets our pull-sync streams with
//!     "overlay address for peer not found".
//!   - **ping** — bee's reacher pings us to classify reachability.
//!   - **upnp** (feature `upnp`) — a UPnP-IGD client that maps our listen port and
//!     reports our external address, exactly bee's `NATManager`. One more field.
//!
//! The `Control` for the stream protocols comes from `behaviour().stream`.
//!
//! Transport is a separate, construction-time concern: [`build_swarm`] is TCP +
//! noise + yamux + DNS; [`build_swarm_wss`] (feature `wss`) adds a WebSocket-
//! Secure leg alongside TCP — the same behaviour, a second transport leg.

use libp2p::identity::Keypair;
use libp2p::Swarm;
use libp2p_stream::Behaviour as StreamBehaviour;

/// The node's composed behaviour: the byte-stream protocols plus the libp2p
/// baseline bee expects (identify + ping), and a UPnP client under the `upnp`
/// feature. Stream protocols open/accept via `behaviour().stream`.
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct NodeBehaviour {
    pub stream: StreamBehaviour,
    pub identify: libp2p::identify::Behaviour,
    pub ping: libp2p::ping::Behaviour,
    #[cfg(feature = "upnp")]
    pub upnp: libp2p::upnp::tokio::Behaviour,
}

/// Construct the composed behaviour for a node identity.
fn node_behaviour(key: &Keypair) -> NodeBehaviour {
    NodeBehaviour {
        stream: StreamBehaviour::new(),
        identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
            "/swarm/1.0.0".into(),
            key.public(),
        )),
        ping: libp2p::ping::Behaviour::new(libp2p::ping::Config::new()),
        #[cfg(feature = "upnp")]
        upnp: libp2p::upnp::tokio::Behaviour::default(),
    }
}

/// Build the node's Swarm — TCP + noise + yamux + DNS. A fresh identity per call.
pub fn build_swarm() -> Swarm<NodeBehaviour> {
    build_swarm_with_key(Keypair::generate_ed25519())
}

/// As [`build_swarm`], but with a *given* libp2p identity. A reachable node needs
/// this: bee dials back the underlay we advertise, and that underlay must carry
/// our `/p2p/` id — so we must know our peer id up front (a fixed key), not a
/// fresh random one. The key is the libp2p transport identity, distinct from the
/// ethereum key that signs the overlay binding.
pub fn build_swarm_with_key(key: Keypair) -> Swarm<NodeBehaviour> {
    libp2p::SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("tcp transport")
        .with_dns()
        .expect("dns resolver")
        .with_behaviour(node_behaviour)
        .expect("behaviour")
        .build()
}

/// As [`build_swarm`], plus a WebSocket-Secure leg (`…/tls/ws`) on the same node.
/// Async because the WS-over-TLS provider is set up at build time. TCP and WSS
/// coexist in one `OrTransport`; the dial multiaddr selects the leg, and the
/// behaviour above is unchanged.
#[cfg(feature = "wss")]
pub async fn build_swarm_wss() -> Swarm<NodeBehaviour> {
    build_swarm_wss_with_key(Keypair::generate_ed25519()).await
}

/// As [`build_swarm_wss`], with a given libp2p identity (see
/// [`build_swarm_with_key`]). Carries both TCP and WSS legs, so peers discovered
/// over WSS that advertise a TCP underlay are still dialable.
#[cfg(feature = "wss")]
pub async fn build_swarm_wss_with_key(key: Keypair) -> Swarm<NodeBehaviour> {
    libp2p::SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("tcp transport")
        .with_dns()
        .expect("dns resolver")
        .with_websocket(libp2p::noise::Config::new, libp2p::yamux::Config::default)
        .await
        .expect("websocket transport")
        .with_behaviour(node_behaviour)
        .expect("behaviour")
        .build()
}

#[cfg(all(test, feature = "upnp"))]
mod upnp_tests {
    use super::*;
    use libp2p::futures::StreamExt;
    use libp2p::swarm::SwarmEvent;
    use std::time::Duration;

    /// Live reachability probe: will this network's gateway grant a UPnP port map
    /// and report our external address? Listens, runs UPnP, prints the verdict —
    /// the cheap precondition check for the port-forward path. `#[ignore]`d.
    ///
    /// ```text
    /// cargo test -p melissi-net --features upnp upnp_probe_external_addr -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn upnp_probe_external_addr() {
        let mut sw = build_swarm();
        sw.listen_on("/ip4/0.0.0.0/tcp/0".parse().unwrap())
            .expect("listen");
        let verdict = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let SwarmEvent::Behaviour(NodeBehaviourEvent::Upnp(ev)) =
                    sw.select_next_some().await
                {
                    use libp2p::upnp::Event::*;
                    match ev {
                        NewExternalAddr(addr) => {
                            return format!(
                                "✓ UPnP external addr {addr} — reachable; advertise this as the dial-back underlay"
                            )
                        }
                        GatewayNotFound => {
                            return "· no UPnP gateway (router has UPnP-IGD off or absent)".to_string()
                        }
                        NonRoutableGateway => {
                            return "· gateway found but non-routable (CGNAT / double-NAT) — UPnP cannot help".to_string()
                        }
                        _ => {}
                    }
                }
            }
        })
        .await;
        match verdict {
            Ok(v) => eprintln!("{v}"),
            Err(_) => eprintln!(
                "· no UPnP verdict in 20s (gateway slow/unresponsive — treat as unavailable)"
            ),
        }
    }
}

#[cfg(all(test, feature = "wss"))]
mod tests {
    // The WSS leg constructs and multiplexes with TCP. No dial — live interop is
    // a network concern, deferred; this asserts the builder typechecks and runs.
    #[tokio::test]
    async fn wss_swarm_builds() {
        let _ = super::build_swarm_wss().await;
    }
}
