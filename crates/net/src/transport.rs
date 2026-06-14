//! The libp2p transport — the first *real* transport, behind the `libp2p`
//! feature. It does exactly one thing the in-memory transport doesn't: move
//! the bytes over a real network (TCP / noise / yamux). The protocol logic is
//! unchanged — it drives the same sync [`crate::handshake::Handshake`] state
//! machine that the deterministic in-memory pump drives in tests. All async
//! lives here; the verified drivers stay sync.
//!
//! This is the pluggable-transport seam realised: `run_handshake` runs the
//! driver over *any* `AsyncRead + AsyncWrite` stream, so libp2p, an in-memory
//! duplex, or a simulated network are interchangeable underneath it.
//!
//! Scope: the protocol is bee's — the real stream id [`HANDSHAKE_PROTOCOL`] and
//! the byte-exact protobuf Syn/SynAck/Ack exchange ([`crate::pb`]). Two libp2p
//! nodes complete it over real TCP (`two_nodes_handshake_over_tcp`), each
//! recovering the other's verified identity. What still needs a *live bee* peer
//! to exercise: the observed-underlay re-signing (NAT/address discovery, which
//! melissi skips — it advertises its configured underlay), peer discovery, and
//! running the `wire` pull-sync session over the established connection.

use crate::handshake::{Handshake, HsOut, Role};
use crate::BzzAddress;
use futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::StreamProtocol;

/// bee's handshake stream protocol id (`pkg/p2p.NewSwarmStreamName` =
/// `/swarm/{name}/{version}/{stream}`). Matching it byte-for-byte is what lets
/// melissi open the handshake stream on a live bee node.
pub const HANDSHAKE_PROTOCOL: StreamProtocol =
    StreamProtocol::new("/swarm/handshake/15.0.0/handshake");

/// Drive the sync handshake state machine over an async byte stream. The dialer
/// plays [`Role::Initiator`], the accepting side [`Role::Responder`]. Returns
/// the peer's verified ethereum address, or `None` on failure / bad peer.
/// Transport-agnostic: works over any `AsyncRead + AsyncWrite`.
pub async fn run_handshake<S>(
    stream: &mut S,
    role: Role,
    mine: &BzzAddress,
    network_id: u64,
    full_node: bool,
    observed_peer_underlay: Vec<u8>,
) -> Option<[u8; 20]>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut hs = Handshake::new(
        role,
        mine.clone(),
        network_id,
        full_node,
        observed_peer_underlay,
    );
    let mut chunk = [0u8; 2048];
    let mut input: Vec<u8> = Vec::new();
    loop {
        match hs.poll(&input) {
            HsOut::Send(bytes) => {
                input.clear();
                stream.write_all(&bytes).await.ok()?;
                // Best-effort: the bytes are handed off. A flush error here is
                // the benign shutdown race — a peer that has read our final Ack
                // and closed its stream — not a handshake failure.
                let _ = stream.flush().await;
            }
            HsOut::Need => {
                input.clear();
                let n = stream.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None; // peer closed before completing
                }
                input.extend_from_slice(&chunk[..n]);
            }
            HsOut::Done(eth) => return Some(eth),
            HsOut::Failed => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::futures::StreamExt;
    use libp2p::swarm::SwarmEvent;
    use libp2p::{Multiaddr, Swarm};
    use melissi_crypto::public_eth_address;
    use std::time::Duration;

    const NET: u64 = 1;
    // A non-empty underlay (the peer's multiaddr) — `/ip4/127.0.0.1/tcp/1634`
    // in multiaddr binary. Real handshakes always carry one; an empty observed
    // underlay would omit the `Syn` field and be rejected, as bee rejects it.
    const OBSERVED: &[u8] = &[0x04, 0x7f, 0x00, 0x00, 0x01, 0x06, 0x06, 0x62];

    fn node() -> Swarm<libp2p_stream::Behaviour> {
        libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .unwrap()
            .with_dns()
            .unwrap()
            .with_behaviour(|_| libp2p_stream::Behaviour::new())
            .unwrap()
            .build()
    }

    fn bzz(secret: &[u8; 32], nonce: u8) -> BzzAddress {
        BzzAddress::new(
            secret,
            b"/ip4/127.0.0.1/tcp/0",
            NET,
            [nonce; 32],
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap()
    }

    /// Two real libp2p nodes connect over localhost TCP, open the handshake
    /// stream, and run the SAME sync handshake driver the in-memory pump runs
    /// — each recovering the other's verified ethereum address. The transport
    /// changed; the protocol logic did not.
    #[tokio::test]
    async fn two_nodes_handshake_over_tcp() {
        let (sa, sb) = ([7u8; 32], [9u8; 32]);
        let (eth_a, eth_b) = (
            public_eth_address(&sa).unwrap(),
            public_eth_address(&sb).unwrap(),
        );

        // node A: listen, accept the handshake stream, run the driver (server).
        let mut a = node();
        let a_peer = *a.local_peer_id();
        let mut a_ctrl = a.behaviour().new_control();
        let mut a_incoming = a_ctrl.accept(HANDSHAKE_PROTOCOL).unwrap();
        a.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let addr: Multiaddr = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = a.select_next_some().await {
                break address;
            }
        };
        tokio::spawn(async move {
            loop {
                a.select_next_some().await;
            }
        });
        let a_done = tokio::spawn(async move {
            let (_peer, mut s) = a_incoming.next().await.expect("incoming stream");
            // A accepts → responder.
            run_handshake(
                &mut s,
                Role::Responder,
                &bzz(&sa, 1),
                NET,
                true,
                OBSERVED.to_vec(),
            )
            .await
        });

        // node B: dial A, open the handshake stream, run the driver (client).
        let mut b = node();
        let mut b_ctrl = b.behaviour().new_control();
        b.dial(addr.with_p2p(a_peer).unwrap()).unwrap();
        tokio::spawn(async move {
            loop {
                b.select_next_some().await;
            }
        });
        let b_done = tokio::spawn(async move {
            let mut s = loop {
                match b_ctrl.open_stream(a_peer, HANDSHAKE_PROTOCOL).await {
                    Ok(s) => break s,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            };
            // B dials → initiator.
            run_handshake(
                &mut s,
                Role::Initiator,
                &bzz(&sb, 2),
                NET,
                true,
                OBSERVED.to_vec(),
            )
            .await
        });

        let timeout = Duration::from_secs(15);
        let a_peer_eth = tokio::time::timeout(timeout, a_done)
            .await
            .expect("A timeout")
            .unwrap();
        let b_peer_eth = tokio::time::timeout(timeout, b_done)
            .await
            .expect("B timeout")
            .unwrap();

        // A recovered B's address; B recovered A's. Mutual, over real TCP.
        assert_eq!(a_peer_eth, Some(eth_b));
        assert_eq!(b_peer_eth, Some(eth_a));
    }

    /// Live interop: dial a real Swarm **testnet** bee node, run bee's handshake
    /// as initiator, and recover its verified identity. Network + external peer,
    /// so `#[ignore]`d (never in CI). Run explicitly:
    ///
    /// ```text
    /// cargo test -p melissi-net --features libp2p live_testnet_handshake -- --ignored --nocapture
    /// ```
    ///
    /// The bootnode resolves from `/dnsaddr/testnet.ethswarm.org`; this pins one
    /// peer it currently expands to. Testnet networkID is 10
    /// (`go-storage-incentives-abi`). bee sends its SynAck (identity) before
    /// reading our Ack, so we verify it even if bee's picker later declines us.
    #[tokio::test]
    #[ignore]
    async fn live_testnet_handshake() {
        const TESTNET: u64 = 10;
        // bee-0.testnet.ethswarm.org (from _dnsaddr TXT); peer id in the addr.
        let peer_addr =
            "/ip4/49.12.172.37/tcp/32490/p2p/QmZsYCbkUXWpfR34PmUwMJvHwJtGfbcMMoAp1G2EydkpRA";
        let addr: Multiaddr = peer_addr.parse().unwrap();
        let peer: libp2p::PeerId = match addr.iter().find_map(|p| match p {
            libp2p::multiaddr::Protocol::P2p(id) => Some(id),
            _ => None,
        }) {
            Some(id) => id,
            None => panic!("no /p2p/ in addr"),
        };
        let observed = addr.to_vec(); // the multiaddr we dialed (valid for bee's Syn)

        // our identity: a fresh key, a syntactically valid underlay multiaddr
        // (bee only needs it to deserialize), testnet network id.
        let secret = [0x5au8; 32];
        let our_underlay = "/ip4/127.0.0.1/tcp/1634"
            .parse::<Multiaddr>()
            .unwrap()
            .to_vec();
        let mine = BzzAddress::new(
            &secret,
            &our_underlay,
            TESTNET,
            [0x11; 32],
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap();

        let mut sw = node();
        let mut ctrl = sw.behaviour().new_control();
        sw.dial(addr).unwrap();
        tokio::spawn(async move {
            loop {
                match sw.select_next_some().await {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                        eprintln!("connected to {peer_id}")
                    }
                    SwarmEvent::OutgoingConnectionError { error, .. } => {
                        eprintln!("dial error: {error:?}")
                    }
                    _ => {}
                }
            }
        });

        let result = tokio::time::timeout(Duration::from_secs(30), async move {
            let mut s = loop {
                match ctrl.open_stream(peer, HANDSHAKE_PROTOCOL).await {
                    Ok(s) => break s,
                    Err(e) => {
                        eprintln!("open_stream pending/err ({e:?}); retrying");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                }
            };
            run_handshake(&mut s, Role::Initiator, &mine, TESTNET, false, observed).await
        })
        .await;

        match result {
            Ok(Some(eth)) => {
                let hex: String = eth.iter().map(|b| format!("{b:02x}")).collect();
                eprintln!("✓ testnet bee verified — blockchain address 0x{hex}");
            }
            Ok(None) => {
                panic!("handshake ran but verification failed (version/protocol mismatch?)")
            }
            Err(_) => panic!("timed out connecting / opening the handshake stream"),
        }
    }
}
