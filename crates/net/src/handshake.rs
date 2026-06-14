//! The handshake **exchange** — a sync state machine, transport-agnostic.
//!
//! Two peers each send their signed [`BzzAddress`] and verify the other's; on
//! success each holds the peer's proven overlay + blockchain address, and the
//! connection is trusted. Like the `wire` pull-sync pollers, this is a pure
//! `poll(bytes) -> {Send, Need, Done, Failed}` driver with no I/O — so the
//! deterministic in-memory transport drives it synchronously (verified here)
//! and the libp2p transport drives the same driver asynchronously. All async
//! lives in the transport shell, never in this logic.
//!
//! This is the mutual identity exchange; bee's full handshake adds a Syn/Ack
//! observed-underlay negotiation in protobuf (the byte-exact-interop step,
//! deferred with the live transport). The binding it establishes is the same.

use crate::BzzAddress;

/// A length-delimited frame: `len(4 BE) ‖ body`. (The transport carries one
/// message; the prefix lets a byte-stream transport find message boundaries.)
fn frame(body: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(4 + body.len());
    f.extend_from_slice(&(body.len() as u32).to_be_bytes());
    f.extend_from_slice(body);
    f
}

fn deframe(buf: &[u8]) -> Option<(Vec<u8>, usize)> {
    let len = u32::from_be_bytes(buf.get(..4)?.try_into().ok()?) as usize;
    let end = 4 + len;
    (buf.len() >= end).then(|| (buf[4..end].to_vec(), end))
}

/// What the driver wants the shell to do next.
#[derive(Debug, PartialEq, Eq)]
pub enum HsOut {
    /// Write these bytes to the peer.
    Send(Vec<u8>),
    /// Awaiting more bytes from the peer.
    Need,
    /// Handshake complete: the peer's verified ethereum (blockchain) address.
    Done([u8; 20]),
    /// The peer's address failed verification — drop the connection.
    Failed,
}

/// One side of the handshake. Symmetric: both peers run the same driver.
pub struct Handshake {
    mine: Vec<u8>,
    network_id: u64,
    sent: bool,
    done: bool,
    buf: Vec<u8>,
}

impl Handshake {
    pub fn new(addr: &BzzAddress, network_id: u64) -> Self {
        Handshake {
            mine: frame(&addr.encode()),
            network_id,
            sent: false,
            done: false,
            buf: Vec::new(),
        }
    }

    /// Feed received bytes (empty to start / when none arrived), get the next
    /// action. Send our address first, then verify the peer's.
    pub fn poll(&mut self, input: &[u8]) -> HsOut {
        self.buf.extend_from_slice(input);
        if !self.sent {
            self.sent = true;
            return HsOut::Send(self.mine.clone());
        }
        if self.done {
            return HsOut::Need;
        }
        let Some((msg, n)) = deframe(&self.buf) else {
            return HsOut::Need;
        };
        self.buf.drain(..n);
        self.done = true;
        match BzzAddress::decode(&msg).and_then(|a| a.verify(self.network_id)) {
            Some(eth) => HsOut::Done(eth),
            None => HsOut::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melissi_crypto::public_eth_address;

    const NET: u64 = 1;

    fn addr(secret: &[u8; 32], nonce: u8) -> BzzAddress {
        BzzAddress::new(
            secret,
            b"/ip4/127.0.0.1/tcp/1634",
            NET,
            [nonce; 32],
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap()
    }

    /// Drive two handshake state machines against each other synchronously —
    /// the deterministic in-memory transport. Each ends with the other's
    /// proven ethereum address.
    #[test]
    fn mutual_handshake_over_in_memory_pump() {
        let (sa, sb) = ([7u8; 32], [9u8; 32]);
        let mut a = Handshake::new(&addr(&sa, 1), NET);
        let mut b = Handshake::new(&addr(&sb, 2), NET);

        let HsOut::Send(msg_a) = a.poll(&[]) else {
            panic!("a sends first")
        };
        let HsOut::Send(msg_b) = b.poll(&[]) else {
            panic!("b sends first")
        };

        assert_eq!(
            a.poll(&msg_b),
            HsOut::Done(public_eth_address(&sb).unwrap())
        );
        assert_eq!(
            b.poll(&msg_a),
            HsOut::Done(public_eth_address(&sa).unwrap())
        );
    }

    /// A peer presenting a tampered address is rejected (the connection is
    /// untrusted) — the in-memory transport surfaces the same Failed verdict
    /// any transport would.
    #[test]
    fn tampered_peer_address_fails_handshake() {
        let mut a = Handshake::new(&addr(&[7u8; 32], 1), NET);
        let _ = a.poll(&[]); // a sends its addr

        let mut forged = addr(&[9u8; 32], 2);
        forged.overlay[0] ^= 0xff; // claim a different overlay than the key derives
        let mut framed = (forged.encode().len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&forged.encode());

        assert_eq!(a.poll(&framed), HsOut::Failed);
    }

    /// Bytes arriving in fragments (as a real stream delivers them) — the
    /// driver buffers until a full frame is present, then completes.
    #[test]
    fn handshake_reassembles_fragmented_frames() {
        let (sa, sb) = ([7u8; 32], [9u8; 32]);
        let mut a = Handshake::new(&addr(&sa, 1), NET);
        let _ = a.poll(&[]);
        let HsOut::Send(msg_b) = Handshake::new(&addr(&sb, 2), NET).poll(&[]) else {
            panic!()
        };
        // deliver byte-by-byte: Need until the last byte completes the frame
        for chunk in msg_b[..msg_b.len() - 1].chunks(1) {
            assert_eq!(a.poll(chunk), HsOut::Need);
        }
        assert_eq!(
            a.poll(&msg_b[msg_b.len() - 1..]),
            HsOut::Done(public_eth_address(&sb).unwrap())
        );
    }
}
