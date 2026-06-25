//! The bee handshake **exchange** (`pkg/p2p/libp2p/internal/handshake`,
//! protocol `/swarm/handshake/15.0.0/handshake`) — a sync state machine,
//! transport-agnostic. Like the `wire` pull-sync pollers, it is a pure
//! `poll(bytes) -> {Send, Need, Done, Failed}` driver with no I/O, so the
//! deterministic in-memory transport drives it synchronously (verified here)
//! and the libp2p transport drives the same driver asynchronously.
//!
//! The exchange is **asymmetric** (bee's `Handshake` initiator vs `Handle`
//! responder), three [`crate::pb`] messages over gogo delimited framing:
//!
//! ```text
//!   initiator ──Syn(observed=peer underlay)──▶ responder
//!   initiator ◀────────SynAck(Syn, Ack)────── responder   (responder's identity)
//!   initiator ──────────────Ack────────────▶ responder    (initiator's identity)
//! ```
//!
//! Each side ends holding the peer's *verified* ethereum address (the overlay↔
//! key binding checked by [`crate::BzzAddress::verify`]). The network id must
//! match, or the peer is rejected.
//!
//! **Minimal vs bee.** bee uses the responder's echoed `ObservedUnderlay` to
//! learn its own public address and re-sign (NAT traversal); melissi advertises
//! its configured underlay and ignores the feedback. The observed underlay we
//! send is carried but not validated (bee validates it against the peer id —
//! the deferred live-interop refinement). The binding established is the same.

use crate::pb::{Ack, Syn, SynAck};
use crate::BzzAddress;
use melissi_protobuf::{deframe, frame};

/// Which side of the asymmetric exchange this driver plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Dials, sends `Syn` first, sends its `Ack` last.
    Initiator,
    /// Accepts, replies `SynAck`, reads the peer's `Ack` last.
    Responder,
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
    /// The peer's address failed verification, or the network id mismatched.
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Start,
    AwaitSynAck,
    AwaitSyn,
    AwaitAck,
    Finish([u8; 20]),
    Failed,
}

/// One side of the handshake driver.
pub struct Handshake {
    role: Role,
    mine: BzzAddress,
    network_id: u64,
    full_node: bool,
    /// The underlay we observed the peer at (goes in our `Syn`/`SynAck`).
    observed: Vec<u8>,
    step: Step,
    buf: Vec<u8>,
    /// The peer's overlay address, captured once its `Ack` verifies. Lets the
    /// shell learn *where in the chunk space* the peer sits (to grind our own
    /// overlay into its neighbourhood), not just its blockchain identity.
    peer_overlay: Option<[u8; 32]>,
}

impl Handshake {
    pub fn new(
        role: Role,
        mine: BzzAddress,
        network_id: u64,
        full_node: bool,
        observed_peer_underlay: Vec<u8>,
    ) -> Self {
        Handshake {
            role,
            mine,
            network_id,
            full_node,
            observed: observed_peer_underlay,
            step: Step::Start,
            buf: Vec::new(),
            peer_overlay: None,
        }
    }

    /// The peer's overlay address, available once the handshake reaches
    /// [`HsOut::Done`] (the `Ack` verified). `None` before then / on failure.
    pub fn peer_overlay(&self) -> Option<[u8; 32]> {
        self.peer_overlay
    }

    fn my_ack(&self) -> Ack {
        Ack {
            address: self.mine.clone(),
            network_id: self.network_id,
            full_node: self.full_node,
            welcome_message: String::new(),
        }
    }

    /// Check the peer's `Ack`: network id matches and the signed overlay↔key
    /// binding verifies. Returns the peer's ethereum address, or `None`.
    fn check(&mut self, ack: &Ack) -> Option<[u8; 20]> {
        (ack.network_id == self.network_id).then_some(())?;
        let eth = ack.address.verify(self.network_id)?;
        self.peer_overlay = Some(ack.address.overlay);
        Some(eth)
    }

    /// Feed received bytes (empty to start / when none arrived), get the next
    /// action.
    pub fn poll(&mut self, input: &[u8]) -> HsOut {
        self.buf.extend_from_slice(input);
        match (self.role, self.step) {
            // initiator: send Syn first.
            (Role::Initiator, Step::Start) => {
                self.step = Step::AwaitSynAck;
                let syn = Syn {
                    observed_underlay: self.observed.clone(),
                };
                HsOut::Send(frame(&syn.encode()))
            }
            (Role::Initiator, Step::AwaitSynAck) => {
                let Some((msg, n)) = deframe(&self.buf) else {
                    return HsOut::Need;
                };
                self.buf.drain(..n);
                match SynAck::decode(&msg).and_then(|sa| self.check(&sa.ack)) {
                    Some(eth) => {
                        // verified the responder; send our Ack, then finish.
                        self.step = Step::Finish(eth);
                        HsOut::Send(frame(&self.my_ack().encode()))
                    }
                    None => {
                        self.step = Step::Failed;
                        HsOut::Failed
                    }
                }
            }

            // responder: wait for the peer's Syn, reply SynAck.
            (Role::Responder, Step::Start) => {
                self.step = Step::AwaitSyn;
                self.drive_responder()
            }
            (Role::Responder, Step::AwaitSyn) => self.drive_responder(),
            (Role::Responder, Step::AwaitAck) => {
                let Some((msg, n)) = deframe(&self.buf) else {
                    return HsOut::Need;
                };
                self.buf.drain(..n);
                match Ack::decode(&msg).and_then(|ack| self.check(&ack)) {
                    Some(eth) => {
                        self.step = Step::Finish(eth);
                        HsOut::Done(eth)
                    }
                    None => {
                        self.step = Step::Failed;
                        HsOut::Failed
                    }
                }
            }

            (_, Step::Finish(eth)) => HsOut::Done(eth),
            (_, Step::Failed) => HsOut::Failed,
            // initiator never waits for a bare Syn/Ack; responder never a SynAck.
            (Role::Initiator, Step::AwaitSyn | Step::AwaitAck)
            | (Role::Responder, Step::AwaitSynAck) => HsOut::Failed,
        }
    }

    /// Responder: consume the peer's `Syn`, emit `SynAck`. `Need` until the
    /// `Syn` frame is complete.
    fn drive_responder(&mut self) -> HsOut {
        let Some((msg, n)) = deframe(&self.buf) else {
            return HsOut::Need;
        };
        self.buf.drain(..n);
        let Some(_syn) = Syn::decode(&msg) else {
            self.step = Step::Failed;
            return HsOut::Failed;
        };
        self.step = Step::AwaitAck;
        let synack = SynAck {
            syn: Syn {
                observed_underlay: self.observed.clone(),
            },
            ack: self.my_ack(),
        };
        HsOut::Send(frame(&synack.encode()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use melissi_crypto::public_eth_address;

    const NET: u64 = 1;
    const UNDERLAY: &[u8] = &[0x04, 0x7f, 0x00, 0x00, 0x01, 0x06, 0x06, 0x62]; // /ip4/127.0.0.1/tcp/1634

    fn bzz(secret: &[u8; 32], nonce: u8) -> BzzAddress {
        BzzAddress::new(secret, UNDERLAY, NET, [nonce; 32], 1_700_000_000, [0u8; 20]).unwrap()
    }

    /// Drive an initiator and a responder against each other synchronously —
    /// the deterministic in-memory transport. Each ends with the other's
    /// proven ethereum address. This is bee's Syn → SynAck → Ack exchange.
    #[test]
    fn mutual_handshake_over_in_memory_pump() {
        let (sa, sb) = ([7u8; 32], [9u8; 32]);
        let mut i = Handshake::new(Role::Initiator, bzz(&sa, 1), NET, true, UNDERLAY.to_vec());
        let mut r = Handshake::new(Role::Responder, bzz(&sb, 2), NET, true, UNDERLAY.to_vec());

        // initiator → Syn
        let HsOut::Send(syn) = i.poll(&[]) else {
            panic!("initiator sends Syn first")
        };
        // responder consumes Syn → SynAck
        assert_eq!(r.poll(&[]), HsOut::Need); // nothing yet
        let HsOut::Send(synack) = r.poll(&syn) else {
            panic!("responder replies SynAck")
        };
        // initiator consumes SynAck → Ack (and has verified the responder)
        let HsOut::Send(ack) = i.poll(&synack) else {
            panic!("initiator sends Ack")
        };
        // initiator is now done with the responder's identity
        assert_eq!(i.poll(&[]), HsOut::Done(public_eth_address(&sb).unwrap()));
        // responder consumes Ack → done with the initiator's identity
        assert_eq!(r.poll(&ack), HsOut::Done(public_eth_address(&sa).unwrap()));
    }

    /// A peer presenting a tampered address is rejected. Here the responder
    /// forges its overlay; the initiator's check of the SynAck fails.
    #[test]
    fn tampered_peer_address_fails_handshake() {
        let mut i = Handshake::new(
            Role::Initiator,
            bzz(&[7u8; 32], 1),
            NET,
            true,
            UNDERLAY.to_vec(),
        );
        let mut forged = bzz(&[9u8; 32], 2);
        forged.overlay[0] ^= 0xff; // claim an overlay the key does not derive
        let synack = SynAck {
            syn: Syn {
                observed_underlay: UNDERLAY.to_vec(),
            },
            ack: Ack {
                address: forged,
                network_id: NET,
                full_node: true,
                welcome_message: String::new(),
            },
        };
        let _ = i.poll(&[]); // Syn
        assert_eq!(i.poll(&frame(&synack.encode())), HsOut::Failed);
    }

    /// A network-id mismatch is rejected even with a valid signature.
    #[test]
    fn network_id_mismatch_fails_handshake() {
        let mut i = Handshake::new(
            Role::Initiator,
            bzz(&[7u8; 32], 1),
            NET,
            true,
            UNDERLAY.to_vec(),
        );
        let synack = SynAck {
            syn: Syn {
                observed_underlay: UNDERLAY.to_vec(),
            },
            ack: Ack {
                address: bzz(&[9u8; 32], 2),
                network_id: NET + 1, // wrong network
                full_node: true,
                welcome_message: String::new(),
            },
        };
        let _ = i.poll(&[]);
        assert_eq!(i.poll(&frame(&synack.encode())), HsOut::Failed);
    }

    /// Bytes arriving in fragments (as a real stream delivers them) — the
    /// driver buffers until a full frame is present, then completes.
    #[test]
    fn handshake_reassembles_fragmented_frames() {
        let (sa, sb) = ([7u8; 32], [9u8; 32]);
        let mut i = Handshake::new(Role::Initiator, bzz(&sa, 1), NET, true, UNDERLAY.to_vec());
        let mut r = Handshake::new(Role::Responder, bzz(&sb, 2), NET, true, UNDERLAY.to_vec());
        let HsOut::Send(syn) = i.poll(&[]) else {
            panic!()
        };
        let HsOut::Send(synack) = r.poll(&syn) else {
            panic!()
        };
        // deliver the SynAck byte-by-byte: Need until the last byte completes it
        for chunk in synack[..synack.len() - 1].chunks(1) {
            assert_eq!(i.poll(chunk), HsOut::Need);
        }
        assert!(matches!(
            i.poll(&synack[synack.len() - 1..]),
            HsOut::Send(_)
        ));
        assert_eq!(i.poll(&[]), HsOut::Done(public_eth_address(&sb).unwrap()));
    }
}
