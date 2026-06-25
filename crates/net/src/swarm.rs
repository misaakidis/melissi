//! The one place a transport is chosen.
//!
//! Everything above the connection — the handshake driver, the pull-sync
//! session, hive, discovery — runs over [`libp2p_stream::Behaviour`]: byte
//! streams on an established connection, never a socket. So the transport is a
//! single construction-time concern, isolated here. Swap or extend it (WSS,
//! QUIC, WebRTC, memory) without touching a line of the protocols or the
//! verified core — they never name a transport.
//!
//! [`build_swarm`] is the shipped node: TCP + noise + yamux, with DNS so `/dns4`
//! and `/dnsaddr` bootnodes resolve at dial time. [`build_swarm_wss`] (feature
//! `wss`) adds a WebSocket-Secure leg *alongside* TCP — the same node, also
//! reachable over `…/tls/ws`; the dial multiaddr picks the leg. Dialing a live
//! WSS peer and browser-serving (AutoTLS certs) are interop concerns verified
//! against the live network, not here — deferred like the rest of the live path.

use libp2p::Swarm;
use libp2p_stream::Behaviour;

/// Build the node's Swarm — TCP + noise + yamux + DNS. A fresh identity per call.
pub fn build_swarm() -> Swarm<Behaviour> {
    build_swarm_with_key(libp2p::identity::Keypair::generate_ed25519())
}

/// As [`build_swarm`], but with a *given* libp2p identity. A reachable node needs
/// this: bee dials back the underlay we advertise, and that underlay must carry
/// our `/p2p/` id — so we must know our peer id up front (a fixed key), not a
/// fresh random one. The key is the libp2p transport identity, distinct from the
/// ethereum key that signs the overlay binding.
pub fn build_swarm_with_key(key: libp2p::identity::Keypair) -> Swarm<Behaviour> {
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
        .with_behaviour(|_| Behaviour::new())
        .expect("stream behaviour")
        .build()
}

/// As [`build_swarm`], plus a WebSocket-Secure leg (`…/tls/ws`) on the same
/// node. Async because the WS-over-TLS provider is set up at build time. TCP and
/// WSS coexist in one `OrTransport`; the dial multiaddr selects the leg, and
/// everything above the connection is unchanged.
#[cfg(feature = "wss")]
pub async fn build_swarm_wss() -> Swarm<Behaviour> {
    libp2p::SwarmBuilder::with_new_identity()
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
        .with_behaviour(|_| Behaviour::new())
        .expect("stream behaviour")
        .build()
}

/// The composed behaviour for a reachable node: the stream protocols plus a UPnP
/// client that maps our listen port on the router and reports the external
/// address. Only the `upnp` member is new — `stream` is the same byte-stream
/// behaviour every other build uses; the `Control` comes from `behaviour().stream`.
#[cfg(feature = "upnp")]
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct NodeBehaviour {
    pub stream: Behaviour,
    pub upnp: libp2p::upnp::tokio::Behaviour,
}

/// As [`build_swarm`], plus a UPnP-IGD client. When the node listens, UPnP asks
/// the gateway to port-map and emits `upnp::Event::NewExternalAddr` with our
/// routable address — the underlay we then advertise for bee's dial-back. Falls
/// back silently (`GatewayNotFound`/`NonRoutableGateway`) where UPnP is absent.
#[cfg(feature = "upnp")]
pub fn build_swarm_upnp() -> Swarm<NodeBehaviour> {
    libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .expect("tcp transport")
        .with_dns()
        .expect("dns resolver")
        .with_behaviour(|_| NodeBehaviour {
            stream: Behaviour::new(),
            upnp: libp2p::upnp::tokio::Behaviour::default(),
        })
        .expect("composed behaviour")
        .build()
}

#[cfg(all(test, feature = "upnp"))]
mod upnp_tests {
    use super::*;
    use libp2p::futures::StreamExt;
    use libp2p::swarm::SwarmEvent;
    use std::time::Duration;

    /// Live reachability probe: will this network's gateway grant a UPnP port
    /// map and report our external address? Listens, runs UPnP, prints the
    /// verdict. This is the cheap precondition check for the whole port-forward
    /// path — if UPnP yields an external addr, the node can advertise it as the
    /// dial-back underlay with no manual router config. `#[ignore]`d (depends on
    /// the local router). Run:
    ///
    /// ```text
    /// cargo test -p melissi-net --features upnp upnp_probe_external_addr -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn upnp_probe_external_addr() {
        let mut sw = build_swarm_upnp();
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
