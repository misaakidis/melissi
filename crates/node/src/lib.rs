//! The sans-io pull-sync node core.
//!
//! One pure function of events to effects — no I/O, no clock, no threads, no
//! locks. The shell (deterministic sim now; tokio + libp2p later; the bee-wire
//! adapter when interop matters) owns time and delivery; the core owns every
//! scheduling decision, made exclusively through the verified machine's action
//! set. Determinism is total: same events in, same effects out.
//!
//! The advertisement/delivery split is the design's Table 8 realised: `Offer`
//! IS the advertisement step, `Fetch` IS the delivery step, and `Fetch` wants
//! BY REFERENCE (explicit triples) — the clean semantics. The bee wire's
//! positional bitvector and its re-offer are an adapter detail for the future
//! `wire` crate; this core never inherits the legacy coupling.
//!
//! Failure handling is ONE verb: every per-triple non-delivery is `Missed` →
//! `stall`. Entry-faults (`Rejected`: invalid stamp, replay — identical at
//! every holder) settle the triple globally. Peer departure and churn are
//! `lose_holder`: candidacy is governed by holdings, bars by behaviour.
//!
//! Interval settlement is the only durable state transition: `Effect::Settled`
//! is emitted exactly when a `(peer, bin)` high-water advances — the shell
//! persists it; everything else is soft state, rebuilt from offers.

use melissi_machine::{Config, PeerId, PullState, Triple};
use melissi_settlement::{BinId, PeerBinLog};
use std::collections::{BTreeMap, BTreeSet};

pub type Bin = u8;

/// What the shell reports per wanted triple. The three-way split is
/// load-bearing (never collapse): a peer-fault retries elsewhere, an
/// entry-fault settles everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Verified (hash + stamp) and stored.
    Delivered,
    /// Entry-fault: invalid stamp / replay — never storable, settles globally.
    Rejected,
    /// Peer-fault or gone: timeout, error, hash mismatch, absent from offer.
    Missed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A neighbourhood peer appeared (topology).
    PeerSeen(PeerId),
    /// It departed: claims release, holdings vanish — no bars.
    PeerGone(PeerId),
    /// `GetCursors` returned: per-bin head BinIDs — the HIST/LIVE boundary.
    CursorsResult { peer: PeerId, cursors: Vec<(Bin, BinId)> },
    /// An offer (advertisement) for `[start, topmost]`.
    OfferResult {
        peer: PeerId,
        bin: Bin,
        start: BinId,
        refs: Vec<(BinId, Triple)>,
        topmost: BinId,
    },
    /// Per-triple outcomes of a `Fetch` (delivery step).
    FetchResult { peer: PeerId, bin: Bin, outcomes: Vec<(Triple, Outcome)> },
    /// The shell's refresh signal: re-offer every covered-but-unsettled range
    /// (churn detection — the offer-diff). Rounds only ever extend coverage;
    /// re-advertising the same range is paced by the shell, which owns time —
    /// a sans-io core that re-offered eagerly would advertise in a busy loop.
    Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    GetCursors(PeerId),
    /// Keep one offer open per `(peer, bin)` — the live subscription: the
    /// serving side blocks on an empty range until something arrives.
    Offer { peer: PeerId, bin: Bin, start: BinId },
    /// Fetch exactly these triples from this peer (want-by-reference).
    Fetch { peer: PeerId, bin: Bin, want: Vec<Triple> },
    /// The `(peer, bin)` high-water advanced: the ONLY durable transition.
    Settled { peer: PeerId, bin: Bin, upto: BinId },
}

/// The node core. Pure; one owner; all mutation through the machine's actions.
pub struct Node {
    m: PullState,
    radius: Bin,
    peers: BTreeSet<PeerId>,
    /// HIST/LIVE boundary per (peer, bin), from `GetCursors`.
    cursors: BTreeMap<(PeerId, Bin), BinId>,
    /// Resume state per (peer, bin): interval high-water + offered window.
    logs: BTreeMap<(PeerId, Bin), PeerBinLog>,
    /// Which bin each known triple lives in (its PO depth = its priority).
    bin_of: BTreeMap<Triple, Bin>,
    /// One open offer per (peer, bin) — the live subscription discipline.
    offer_open: BTreeSet<(PeerId, Bin)>,
}

impl Node {
    pub fn new(cfg: Config, radius: Bin) -> Self {
        Node {
            m: PullState::new(cfg),
            radius,
            peers: BTreeSet::new(),
            cursors: BTreeMap::new(),
            logs: BTreeMap::new(),
            bin_of: BTreeMap::new(),
            offer_open: BTreeSet::new(),
        }
    }

    /// The single entry point: ingest one event, return the effects.
    pub fn handle(&mut self, ev: Event) -> Vec<Effect> {
        match ev {
            Event::PeerSeen(p) => {
                if self.peers.insert(p) {
                    vec![Effect::GetCursors(p)]
                } else {
                    vec![]
                }
            }

            Event::PeerGone(p) => {
                self.peers.remove(&p);
                // Churn-out, not misbehaviour: release claims, drop holdings,
                // no bars (NoFalseExclusion is behaviour-only).
                for c in self.m.held_by(p) {
                    self.m.lose_holder(c, p);
                }
                for c in self.m.claims_of(p) {
                    self.m.lose_holder(c, p);
                }
                self.logs.retain(|&(q, _), _| q != p);
                self.cursors.retain(|&(q, _), _| q != p);
                self.offer_open.retain(|&(q, _)| q != p);
                // released claims may be re-routable right away
                self.round(None)
            }

            Event::CursorsResult { peer, cursors } => {
                let mut fx = Vec::new();
                for (bin, head) in cursors {
                    if bin < self.radius {
                        continue; // outside the reserve: not synced
                    }
                    self.cursors.insert((peer, bin), head);
                    let log = self.logs.entry((peer, bin)).or_insert_with(PeerBinLog::new);
                    if self.offer_open.insert((peer, bin)) {
                        fx.push(Effect::Offer { peer, bin, start: log.next() });
                    }
                }
                fx
            }

            Event::OfferResult { peer, bin, start, refs, topmost } => {
                self.offer_open.remove(&(peer, bin));
                let cursor = self.cursors.get(&(peer, bin)).copied().unwrap_or(0);
                let log = self.logs.entry((peer, bin)).or_insert_with(PeerBinLog::new);
                log.cover(topmost);

                // Churn-out detection: an entry of this window inside the
                // covered range that the fresh offer no longer names is no
                // longer held by this peer — a gap, not an obligation.
                let offered: BTreeSet<BinId> = refs.iter().map(|&(b, _)| b).collect();
                let lost: Vec<(BinId, Triple)> = log
                    .unsettled()
                    .filter(|&(b, _)| b >= start && b <= topmost && !offered.contains(&b))
                    .collect();
                for (b, c) in lost {
                    log.forget_entry(b);
                    self.m.lose_holder(c, peer);
                }

                // Ingest: window + holders + arrival (the cursor decides
                // HIST vs LIVE; arrival is observation — EnableLive gates
                // claiming, never arrival).
                for &(b, c) in &refs {
                    let log = self.logs.get_mut(&(peer, bin)).unwrap();
                    log.observe(b, c);
                    self.m.observe_holder(c, peer);
                    self.bin_of.insert(c, bin);
                    self.m.set_prio(c, bin); // deeper bin = higher priority
                    if b > cursor {
                        self.m.arrive_live(c);
                    } else {
                        self.m.arrive_hist(c);
                    }
                }
                self.round(Some((peer, bin)))
            }

            Event::Tick => {
                let mut fx = Vec::new();
                let keys: Vec<(PeerId, Bin)> = self.logs.keys().copied().collect();
                for (peer, bin) in keys {
                    if self.peers.contains(&peer) && self.offer_open.insert((peer, bin)) {
                        let start = self.logs[&(peer, bin)].next();
                        fx.push(Effect::Offer { peer, bin, start });
                    }
                }
                fx
            }

            Event::FetchResult { peer, bin, outcomes } => {
                for (c, outcome) in outcomes {
                    match outcome {
                        Outcome::Delivered => {
                            // the machine guard (p ∈ want[c]) must hold: the
                            // shell reports only what was asked of it
                            let ok = self.m.deliver(c, peer);
                            debug_assert!(ok, "delivery without a lease: {c} from {peer}");
                        }
                        Outcome::Rejected => {
                            self.m.reject(c);
                        }
                        Outcome::Missed => {
                            self.m.stall(c, peer);
                        }
                    }
                }
                self.round(Some((peer, bin)))
            }
        }
    }

    /// One settle-reset-schedule pass: the body every event arm ends in.
    fn round(&mut self, _touched: Option<(PeerId, Bin)>) -> Vec<Effect> {
        let mut fx = Vec::new();

        // 1. Settle: settle before you forget — got or terminally rejected.
        //    Settlement is global (by triple); advance is local (per log).
        let keys: Vec<(PeerId, Bin)> = self.logs.keys().copied().collect();
        for key in keys {
            let (settled_upto, next) = {
                let m = &self.m;
                let log = self.logs.get_mut(&key).unwrap();
                let upto = log.advance(|c| m.has(c) || m.is_rejected(c));
                (upto, log.next())
            };
            if let Some(upto) = settled_upto {
                fx.push(Effect::Settled { peer: key.0, bin: key.1, upto });
            }
            // Keep one offer open per (peer, bin) — but rounds only EXTEND
            // coverage (the uncovered tail: the live subscription). Re-offering
            // covered-but-unsettled ground is the shell's Tick: retries within
            // a covered range go through known holders, not fresh adverts.
            let covered = self.logs[&key].topmost();
            if next > covered
                && self.peers.contains(&key.0)
                && self.offer_open.insert(key)
            {
                fx.push(Effect::Offer { peer: key.0, bin: key.1, start: next });
            }
        }

        // 2. Reset: bars covering every current holder clear (cooldown) —
        //    a misattributed stall costs a round, never the chunk.
        let candidates: Vec<Triple> = self.bin_of.keys().copied().collect();
        for c in candidates {
            self.m.reset_excluded(c);
        }

        // 3. Schedule: resolve the machine's nondeterminism by policy —
        //    deepest bin first (the prio guard), least-loaded holder, peer id
        //    as the deterministic tiebreak. Policy only picks among ENABLED
        //    wants, so it can break nothing (provably-neutral).
        let mut batches: BTreeMap<(PeerId, Bin), Vec<Triple>> = BTreeMap::new();
        loop {
            let enabled = self.m.enabled_wants();
            if enabled.is_empty() {
                break;
            }
            // deepest first, then lowest triple id for determinism
            let (&(c, _), _) = enabled
                .iter()
                .map(|e @ (c, _)| (e, (self.bin_of.get(c).copied().unwrap_or(0), *c)))
                .max_by_key(|&(_, (bin, c))| (bin, std::cmp::Reverse(c)))
                .unwrap();
            // least-loaded among c's enabled holders; peer id tiebreak
            let p = enabled
                .iter()
                .filter(|&&(d, _)| d == c)
                .map(|&(_, p)| p)
                .min_by_key(|&p| (self.m.load(p), p))
                .unwrap();
            let ok = self.m.want(c, p);
            debug_assert!(ok, "policy chose a disabled want");
            batches.entry((p, self.bin_of.get(&c).copied().unwrap_or(0))).or_default().push(c);
        }
        for ((peer, bin), want) in batches {
            fx.push(Effect::Fetch { peer, bin, want });
        }
        fx
    }

    // --- observability ---------------------------------------------------------

    /// `Phi`: chunks offered-and-unsettled, not yet stored.
    pub fn deficit(&self) -> usize {
        self.m.deficit()
    }

    pub fn has(&self, c: Triple) -> bool {
        self.m.has(c)
    }

    /// The ConflictFree tripwire — must never fire.
    pub fn conflict(&self) -> bool {
        self.m.conflict()
    }

    /// Total deliveries — the DeliveryFloor evidence (≈ distinct missing
    /// chunks, not k×).
    pub fn deliveries(&self) -> u32 {
        self.m.deliveries()
    }

    pub fn interval(&self, peer: PeerId, bin: Bin) -> Option<BinId> {
        self.logs.get(&(peer, bin)).map(|l| l.interval())
    }

    pub fn check_invariants(&self) -> Result<(), &'static str> {
        self.m.check_invariants()
    }
}
