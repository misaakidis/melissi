//! bee's hive peer-discovery protocol (`/swarm/hive/2.0.0/peers`): a one-way
//! **push** of signed peer addresses. A node opens a `peers` stream and writes
//! one or more `Peers` messages (≤30 addresses each, bee's `maxBatchSize`),
//! then closes; the receiver reads until EOF, verifies each address, and learns
//! who else is on the network and where to dial them.
//!
//! The wire `BzzAddress` is byte-identical to the handshake's ([`crate::pb`]
//! reuses it via [`crate::BzzAddress`]) — same overlay↔key↔underlay binding,
//! same verification. So discovery inherits the handshake's forgery resistance:
//! a pushed peer whose overlay is not a commitment to its key is dropped.
//!
//! This module is pure (no libp2p) — the [`HiveReceiver`] is a sans-io
//! accumulator, like the `wire` pollers; the opt-in libp2p runner at the bottom
//! moves its bytes over a real stream.

use crate::BzzAddress;
use melissi_protobuf::{deframe, fields, frame, put_bytes_field};

/// bee's hive peers stream id (`pkg/p2p.NewSwarmStreamName(hive, 2.0.0, peers)`).
pub const PEERS_PROTOCOL_ID: &str = "/swarm/hive/2.0.0/peers";
/// bee's `maxBatchSize`: at most this many addresses per `Peers` message.
pub const MAX_BATCH: usize = 30;

/// Encode one `Peers { repeated BzzAddress peers = 1 }` message (no framing).
pub fn encode_peers(addrs: &[BzzAddress]) -> Vec<u8> {
    let mut b = Vec::new();
    for a in addrs {
        put_bytes_field(&mut b, 1, &a.encode());
    }
    b
}

/// Decode the `BzzAddress`es of a `Peers` message (unverified).
pub fn decode_peers(b: &[u8]) -> Vec<BzzAddress> {
    let mut out = Vec::new();
    let _ = fields(b, |f, _, p| {
        if f == 1 {
            if let Some(a) = BzzAddress::decode(p) {
                out.push(a);
            }
        }
    });
    out
}

/// A peer list ready to write to a `peers` stream: framed `Peers` messages,
/// batched at [`MAX_BATCH`] like bee's `BroadcastPeers`.
pub fn broadcast_frames(addrs: &[BzzAddress]) -> Vec<u8> {
    let mut out = Vec::new();
    for batch in addrs.chunks(MAX_BATCH) {
        out.extend_from_slice(&frame(&encode_peers(batch)));
    }
    out
}

/// A discovered peer: its proven overlay (its place in the chunk space — used
/// to judge proximity to our own), where to dial it, and its blockchain
/// address. Only peers whose signed binding verifies reach here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredPeer {
    pub overlay: [u8; 32],
    pub underlay: Vec<u8>,
    pub eth: [u8; 20],
}

/// Receives a hive push: feed stream bytes, it deframes `Peers` messages,
/// verifies each address, and accumulates the survivors. EOF is the terminal
/// (the sender closes after pushing) — the shell owns it and calls
/// [`HiveReceiver::into_peers`].
pub struct HiveReceiver {
    network_id: u64,
    buf: Vec<u8>,
    peers: Vec<DiscoveredPeer>,
}

impl HiveReceiver {
    pub fn new(network_id: u64) -> Self {
        HiveReceiver {
            network_id,
            buf: Vec::new(),
            peers: Vec::new(),
        }
    }

    /// Feed received bytes; decode and verify every complete `Peers` message
    /// now buffered. Idempotent across partial frames (buffers the remainder).
    pub fn feed(&mut self, input: &[u8]) {
        self.buf.extend_from_slice(input);
        while let Some((msg, n)) = deframe(&self.buf) {
            self.buf.drain(..n);
            for addr in decode_peers(&msg) {
                if let Some(eth) = addr.verify(self.network_id) {
                    self.peers.push(DiscoveredPeer {
                        overlay: addr.overlay,
                        underlay: addr.underlay,
                        eth,
                    });
                }
                // a forged/garbled address simply does not appear — dropped,
                // exactly as bee drops an address that fails ParseAddress.
            }
        }
    }

    pub fn peers(&self) -> &[DiscoveredPeer] {
        &self.peers
    }

    pub fn into_peers(self) -> Vec<DiscoveredPeer> {
        self.peers
    }
}

#[cfg(feature = "libp2p")]
mod runner {
    use super::*;
    use futures::{AsyncReadExt, AsyncWriteExt};
    use libp2p::StreamProtocol;

    pub const PEERS_PROTOCOL: StreamProtocol = StreamProtocol::new(PEERS_PROTOCOL_ID);

    /// Read a peer push to completion over an async stream: feed the receiver
    /// until the sender closes, then return the verified peers.
    pub async fn receive_peers<S>(stream: &mut S, network_id: u64) -> Vec<DiscoveredPeer>
    where
        S: AsyncReadExt + Unpin,
    {
        let mut rx = HiveReceiver::new(network_id);
        let mut buf = [0u8; 8192];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => rx.feed(&buf[..n]),
            }
        }
        rx.into_peers()
    }

    /// Push a peer list over an async stream (one or more batched `Peers`
    /// messages), then leave the stream for the caller to close.
    pub async fn broadcast<S>(stream: &mut S, addrs: &[BzzAddress]) -> std::io::Result<()>
    where
        S: AsyncWriteExt + Unpin,
    {
        stream.write_all(&broadcast_frames(addrs)).await?;
        stream.flush().await
    }
}

#[cfg(feature = "libp2p")]
pub use runner::{broadcast, receive_peers, PEERS_PROTOCOL};

#[cfg(test)]
mod tests {
    use super::*;

    const NET: u64 = 1;

    fn addr(secret: &[u8; 32], nonce: u8) -> BzzAddress {
        BzzAddress::new(
            secret,
            b"/ip4/1.2.3.4/tcp/1634",
            NET,
            [nonce; 32],
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap()
    }

    /// A push of several peers round-trips: every signed address is recovered
    /// and verified, in order.
    #[test]
    fn peers_push_roundtrips_and_verifies() {
        let addrs: Vec<BzzAddress> = (1u8..=5).map(|i| addr(&[i; 32], i)).collect();
        let frames = broadcast_frames(&addrs);
        let mut rx = HiveReceiver::new(NET);
        // deliver in two arbitrary fragments — the receiver reassembles
        let split = frames.len() / 3;
        rx.feed(&frames[..split]);
        rx.feed(&frames[split..]);
        let got = rx.into_peers();
        assert_eq!(got.len(), addrs.len());
        for (a, d) in addrs.iter().zip(&got) {
            assert_eq!(d.overlay, a.overlay);
            assert_eq!(d.underlay, a.underlay);
            assert_eq!(d.eth, a.verify(NET).unwrap());
        }
    }

    /// Batching matches bee's `maxBatchSize`: 31 peers → 2 framed messages.
    #[test]
    fn large_lists_are_batched_by_thirty() {
        let addrs: Vec<BzzAddress> = (0u8..31).map(|i| addr(&[i + 1; 32], i)).collect();
        let frames = broadcast_frames(&addrs);
        // count frames by deframing
        let (mut b, mut msgs) = (frames.as_slice(), 0);
        while let Some((_, n)) = deframe(b) {
            msgs += 1;
            b = &b[n..];
        }
        assert_eq!(msgs, 2, "31 peers batch into 2 messages of ≤30");
        let mut rx = HiveReceiver::new(NET);
        rx.feed(&frames);
        assert_eq!(rx.peers().len(), 31, "all 31 survive across both batches");
    }

    /// A forged overlay in the push is dropped, not trusted.
    #[test]
    fn forged_peer_is_dropped() {
        let good = addr(&[7; 32], 1);
        let mut forged = addr(&[9; 32], 2);
        forged.overlay[0] ^= 0xff;
        let frames = broadcast_frames(&[good.clone(), forged]);
        let mut rx = HiveReceiver::new(NET);
        rx.feed(&frames);
        let got = rx.into_peers();
        assert_eq!(got.len(), 1, "only the well-bound peer survives");
        assert_eq!(got[0].overlay, good.overlay);
    }
}
