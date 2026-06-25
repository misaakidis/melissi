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

use melissi_node::{Bin, Effect, Event, Node, Outcome};
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

impl Op {
    /// The peer this op targets — the shell routes it to that peer's connection.
    pub fn peer(&self) -> PeerId {
        match self {
            Op::Cursors { peer } | Op::Offer { peer, .. } | Op::Fetch { peer, .. } => *peer,
        }
    }
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

/// **The carrier seam.** Run one scheduled [`Op`] against a real connection and
/// return the [`Event`] to feed back — or `None` if it could not be carried (a
/// dropped / declining peer). This is the *only* thing a transport must provide
/// to drive the verified pull-sync: everything above it ([`Session`], `Node`,
/// `machine`) is sans-io and carrier-blind. The in-memory tests, the libp2p
/// shell, and any future carrier (a full third-party client stack) each supply
/// one `OpRunner`; the loop ([`drive`]) is identical for all of them.
pub trait OpRunner {
    /// Carry one operation. `async` so a real runner can do stream I/O; the
    /// verified loop only awaits it and never names a transport.
    fn run(&mut self, op: Op) -> impl std::future::Future<Output = Option<Event>>;
}

/// The empty / `Missed` event for an op that could not be carried — the
/// shell-owned failure signal. An empty cursor set yields no offers; `Missed`
/// wants reschedule elsewhere.
pub fn failure_event(op: &Op) -> Event {
    match op {
        Op::Cursors { peer } => Event::CursorsResult {
            peer: *peer,
            cursors: Vec::new(),
        },
        Op::Offer { peer, bin, start } => Event::OfferResult {
            peer: *peer,
            bin: *bin,
            start: *start,
            refs: Vec::new(),
            topmost: *start,
        },
        Op::Fetch {
            peer, bin, want, ..
        } => Event::FetchResult {
            peer: *peer,
            bin: *bin,
            outcomes: want.iter().map(|&t| (t, Outcome::Missed)).collect(),
        },
    }
}

/// Drive a [`Session`] to quiescence over any [`OpRunner`] — the carrier-neutral
/// pull loop. A carried result feeds back; a failure feeds `Missed`/empty so the
/// node fails over — EXCEPT a failed `Offer`, which is DROPPED (feeding an empty
/// offer for a blocked range re-arms the standing live subscription and spins:
/// the `session_play` lesson). Returns when the puller quiesces.
pub async fn drive<R: OpRunner>(session: &mut Session, runner: &mut R) {
    let mut guard: u64 = 0;
    while let Some(op) = session.next_op() {
        guard += 1;
        if guard > 1_000_000 {
            break; // safety: a non-converging round (should not happen with real supply)
        }
        match runner.run(op.clone()).await {
            Some(ev) => session.feed(ev),
            None if !matches!(op, Op::Offer { .. }) => session.feed(failure_event(&op)),
            None => {}
        }
    }
}
