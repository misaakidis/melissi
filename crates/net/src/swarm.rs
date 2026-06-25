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

#[cfg(all(test, feature = "wss"))]
mod tests {
    // The WSS leg constructs and multiplexes with TCP. No dial — live interop is
    // a network concern, deferred; this asserts the builder typechecks and runs.
    #[tokio::test]
    async fn wss_swarm_builds() {
        let _ = super::build_swarm_wss().await;
    }
}
