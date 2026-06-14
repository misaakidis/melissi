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
//! Scope: melissi↔melissi over real TCP is what's verified here (the
//! `two_nodes_handshake_over_tcp` test). Interop with a live *bee* node also
//! needs bee's exact protocol id and the protobuf Syn/Ack exchange — the
//! deferred, live-peer step.

use crate::handshake::{Handshake, HsOut};
use crate::BzzAddress;
use futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::StreamProtocol;

/// The melissi handshake stream protocol. (bee's is `/swarm/handshake/13.0.0/
/// handshake`; matching it is part of the deferred bee-interop step.)
pub const HANDSHAKE_PROTOCOL: StreamProtocol = StreamProtocol::new("/melissi/handshake/1.0.0");

/// Drive the sync handshake state machine over an async byte stream. Returns
/// the peer's verified ethereum address, or `None` on failure / bad peer.
/// Transport-agnostic: works over any `AsyncRead + AsyncWrite`.
pub async fn run_handshake<S>(
    stream: &mut S,
    addr: &BzzAddress,
    network_id: u64,
) -> Option<[u8; 20]>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut hs = Handshake::new(addr, network_id);
    let mut chunk = [0u8; 2048];
    let mut input: Vec<u8> = Vec::new();
    loop {
        match hs.poll(&input) {
            HsOut::Send(bytes) => {
                input.clear();
                stream.write_all(&bytes).await.ok()?;
                stream.flush().await.ok()?;
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

    fn node() -> Swarm<libp2p_stream::Behaviour> {
        libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
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
            run_handshake(&mut s, &bzz(&sa, 1), NET).await
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
            run_handshake(&mut s, &bzz(&sb, 2), NET).await
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
}
