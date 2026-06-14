//! The pull-sync **session**: the loop that turns the node's effects into wire
//! operations and feeds the results back, until the puller quiesces. It is the
//! sans-io seam between the verified core ([`melissi_node::Node`]) and a real
//! connection — the same effect→event loop the `sim` runs, but each effect is
//! a real stream exchange (the `adapter` pollers) over a real transport.
//!
//! bee runs **one short stream per operation** (`pkg/pullsync`, downstream
//! initiates): a `cursors` stream (`Syn → Ack`) and a `pullsync` stream
//! (`Get → Offer → Want → Delivery*`), both under `pullsync/1.4.0`. So the
//! session yields one [`Op`] per network effect; the shell opens the matching
//! stream, drives the matching poller to completion, and reports the typed
//! result. `Settled` is durable, not a network op — the shell may persist the
//! high-water (the only durable transition) and the session moves on.
//!
//! The session owns the [`Node`]; the shell owns the codec, the transport, and
//! the clock (stream-end / timeout → `Outcome::Missed`, the `adapter`'s
//! shell-owned signal). This keeps the session transport-agnostic: the
//! in-memory pump verifies it (see the tests) and the libp2p shell runs the
//! same loop over real streams.

use melissi_node::{Bin, Effect, Event, Node};
use melissi_settlement::BinId;
use melissi_types::{PeerId, Triple};
use std::collections::VecDeque;

/// A network operation the shell must perform — one bee stream each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// The `cursors` stream: `Syn → Ack`, learn the peer's per-bin heads.
    Cursors { peer: PeerId },
    /// A `pullsync` stream, advertisement leg: `Get → Offer`, what the peer
    /// holds in `[start, ..]`.
    Offer {
        peer: PeerId,
        bin: Bin,
        start: BinId,
    },
    /// A `pullsync` stream, delivery leg: fetch exactly these triples. `start`
    /// is the **settled high-water** for this `(peer, bin)` — the resume point
    /// (`IntervalSettlement`). The adapter re-offers from it and matches wants
    /// by triple, so re-offering from the last-settled floor is the documented
    /// conservative resume (idempotent over already-settled ground), never a
    /// skip. The wire carries no per-entry positions, so this is as precise as
    /// the legacy coupling allows; exact resume is the reconciliation upgrade.
    Fetch {
        peer: PeerId,
        bin: Bin,
        start: BinId,
        want: Vec<Triple>,
    },
}

/// Drives a [`Node`] over a connection. Owns the node; sequences its effects
/// into [`Op`]s; feeds wire results back as events.
pub struct Session {
    node: Node,
    queue: VecDeque<Effect>,
    /// `(peer, bin, upto)` high-waters the node has settled — the durable
    /// transitions the shell may persist. Drained by [`Session::take_settled`].
    settled: Vec<(PeerId, Bin, BinId)>,
}

impl Session {
    pub fn new(node: Node) -> Self {
        Session {
            node,
            queue: VecDeque::new(),
            settled: Vec::new(),
        }
    }

    /// Announce a peer to sync from — the entry point (the node responds with a
    /// `GetCursors`, which [`Session::next_op`] then yields).
    pub fn add_peer(&mut self, peer: PeerId) {
        self.feed(Event::PeerSeen(peer));
    }

    /// Feed a wire result back into the core and enqueue the effects it emits.
    pub fn feed(&mut self, ev: Event) {
        self.queue.extend(self.node.handle(ev));
    }

    /// The next network operation, or `None` when the puller is quiescent
    /// (nothing left to ask any peer). Durable `Settled` transitions are
    /// recorded (see [`Session::take_settled`]) and skipped — they are not
    /// network work.
    pub fn next_op(&mut self) -> Option<Op> {
        while let Some(eff) = self.queue.pop_front() {
            match eff {
                Effect::GetCursors(peer) => return Some(Op::Cursors { peer }),
                Effect::Offer { peer, bin, start } => return Some(Op::Offer { peer, bin, start }),
                Effect::Fetch { peer, bin, want } => {
                    // re-offer from the settled high-water — the resume point
                    // the node already tracks (IntervalSettlement), not a
                    // session-side memory of offer windows.
                    let start = self.node.interval(peer, bin).unwrap_or(0);
                    return Some(Op::Fetch {
                        peer,
                        bin,
                        start,
                        want,
                    });
                }
                Effect::Settled { peer, bin, upto } => self.settled.push((peer, bin, upto)),
            }
        }
        None
    }

    /// Drain the settled high-waters accumulated since the last call — the
    /// durable transitions a persistent shell writes before forgetting.
    pub fn take_settled(&mut self) -> Vec<(PeerId, Bin, BinId)> {
        std::mem::take(&mut self.settled)
    }

    pub fn node(&self) -> &Node {
        &self.node
    }
}
