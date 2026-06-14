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

mod discovery;
use discovery::Discovery;

use melissi_machine::{Config, PeerId, PullState};
use melissi_settlement::{BinId, PeerBinLog};
use melissi_types::Triple;
use std::collections::{BTreeMap, BTreeSet};

// `Bin` from the single-sourced identity seam; re-exported so downstream
// `use melissi_node::Bin` keeps resolving.
pub use melissi_types::Bin;

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
    CursorsResult {
        peer: PeerId,
        cursors: Vec<(Bin, BinId)>,
    },
    /// An offer (advertisement) for `[start, topmost]`.
    OfferResult {
        peer: PeerId,
        bin: Bin,
        start: BinId,
        refs: Vec<(BinId, Triple)>,
        topmost: BinId,
    },
    /// Per-triple outcomes of a `Fetch` (delivery step).
    FetchResult {
        peer: PeerId,
        bin: Bin,
        outcomes: Vec<(Triple, Outcome)>,
    },
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
    Offer {
        peer: PeerId,
        bin: Bin,
        start: BinId,
    },
    /// Fetch exactly these triples from this peer (want-by-reference).
    Fetch {
        peer: PeerId,
        bin: Bin,
        want: Vec<Triple>,
    },
    /// The `(peer, bin)` high-water advanced: the ONLY durable transition.
    Settled {
        peer: PeerId,
        bin: Bin,
        upto: BinId,
    },
}

/// The floor-achieving policy knobs (design §5.3, §5.6). Provably correctness-
/// neutral — they only choose among already-enabled `Want`s — so unlike the
/// machine's `Config` they break no safety/liveness property. They exist so
/// the sim can *ablate* them and measure the O5 fairness floor breaking, the
/// way the gate-critical mechanisms are ablated in TLA. Ship [`Policy::SHIPPED`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Route on cumulative realised assignments (true) vs outstanding load
    /// only (false). Outstanding-only is history-blind: balanced within a
    /// scheduling wave, skewed across waves — the `[6,6,12]` ablation.
    pub cumulative_routing: bool,
    /// Wait for the choice set to assemble before routing a bin (true), vs
    /// schedule eagerly off whoever answered first (false) — which hands the
    /// whole backlog to the first peer through discovery.
    pub discovery_barrier: bool,
}

impl Policy {
    pub const SHIPPED: Policy = Policy {
        cumulative_routing: true,
        discovery_barrier: true,
    };
}

impl Default for Policy {
    fn default() -> Self {
        Self::SHIPPED
    }
}

/// The node core. Pure; one owner; all mutation through the machine's actions.
pub struct Node {
    m: PullState<Triple>,
    policy: Policy,
    radius: Bin,
    peers: BTreeSet<PeerId>,
    /// HIST/LIVE boundary per (peer, bin), from `GetCursors`.
    cursors: BTreeMap<(PeerId, Bin), BinId>,
    /// Resume state per (peer, bin): interval high-water + offered window.
    logs: BTreeMap<(PeerId, Bin), PeerBinLog>,
    /// Which bin each known triple lives in (its PO depth = its priority).
    bin_of: BTreeMap<Triple, Bin>,
    /// Cumulative fetch assignments per peer — the routing key. Policy state
    /// only (provably-neutral): the §5.3 fairness floor is about REALISED
    /// serve totals, and outstanding load alone is history-blind — it
    /// balances within a wave but skews across waves (deeper bins schedule
    /// first). Outstanding load (`m.load`) stays as the tiebreak.
    assigned: BTreeMap<PeerId, u64>,
    /// The discovery barrier (design §5.6): the choice set assembles before
    /// routing chooses, else the first peer answered hands the whole backlog.
    disc: Discovery,
}

impl Node {
    pub fn new(cfg: Config, radius: Bin) -> Self {
        Self::with_policy(cfg, radius, Policy::SHIPPED)
    }

    /// As [`Node::new`], with the floor-achieving policy chosen explicitly —
    /// the sim uses this to ablate fairness (see `Policy`).
    pub fn with_policy(cfg: Config, radius: Bin, policy: Policy) -> Self {
        Node {
            m: PullState::<Triple>::new(cfg),
            policy,
            radius,
            peers: BTreeSet::new(),
            cursors: BTreeMap::new(),
            logs: BTreeMap::new(),
            bin_of: BTreeMap::new(),
            assigned: BTreeMap::new(),
            disc: Discovery::new(),
        }
    }

    /// The ReserveHas view at sync start: `c` is already held.
    pub fn preload(&mut self, c: Triple) -> bool {
        self.m.preload(c)
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
                self.disc.forget_peer(p);
                // released claims may be re-routable right away
                self.round()
            }

            Event::CursorsResult { peer, cursors } => {
                self.disc.mark_cursored(peer);
                let mut fx = Vec::new();
                for (bin, head) in cursors {
                    if bin < self.radius {
                        continue; // outside the reserve: not synced
                    }
                    self.cursors.insert((peer, bin), head);
                    let log = self.logs.entry((peer, bin)).or_insert_with(PeerBinLog::new);
                    if head < log.next() {
                        // nothing in the HIST range: resolved without an
                        // answer (the standing offer is the LIVE tail)
                        self.disc.resolve(peer, bin);
                    }
                    if self.disc.try_open(peer, bin) {
                        fx.push(Effect::Offer {
                            peer,
                            bin,
                            start: log.next(),
                        });
                    }
                }
                fx
            }

            Event::OfferResult {
                peer,
                bin,
                start,
                refs,
                topmost,
            } => {
                self.disc.close(peer, bin);
                self.disc.resolve(peer, bin);
                let cursor = self.cursors.get(&(peer, bin)).copied().unwrap_or(0);
                let log = self.logs.entry((peer, bin)).or_insert_with(PeerBinLog::new);
                // An answer covers at least what was asked: a hostile
                // under-stated Topmost must not leave next() > covered, or
                // the round re-justifies itself and the advertisement loops
                // (OfferPacing: credits are granted by transitions, and an
                // answer must close the tail it was emitted for).
                log.cover(topmost.max(start));

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
                self.round()
            }

            Event::Tick => {
                let mut fx = Vec::new();
                let keys: Vec<(PeerId, Bin)> = self.logs.keys().copied().collect();
                for (peer, bin) in keys {
                    if self.peers.contains(&peer) && self.disc.try_open(peer, bin) {
                        let start = self.logs[&(peer, bin)].next();
                        fx.push(Effect::Offer { peer, bin, start });
                    }
                }
                fx
            }

            Event::FetchResult {
                peer,
                bin: _, // outcomes carry their own ids; the round re-derives work
                outcomes,
            } => {
                for (c, outcome) in outcomes {
                    match outcome {
                        Outcome::Delivered => {
                            // the machine guard (p ∈ want[c]) must hold: the
                            // shell reports only what was asked of it
                            let ok = self.m.deliver(c, peer);
                            debug_assert!(ok, "delivery without a lease: {c:?} from {peer}");
                        }
                        Outcome::Rejected => {
                            self.m.reject(c);
                        }
                        Outcome::Missed => {
                            self.m.stall(c, peer);
                        }
                    }
                }
                self.round()
            }
        }
    }

    /// One settle-reset-schedule pass: the body every event arm ends in.
    fn round(&mut self) -> Vec<Effect> {
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
                fx.push(Effect::Settled {
                    peer: key.0,
                    bin: key.1,
                    upto,
                });
            }
            // Keep one offer open per (peer, bin) — but rounds only EXTEND
            // coverage (the uncovered tail: the live subscription). Re-offering
            // covered-but-unsettled ground is the shell's Tick: retries within
            // a covered range go through known holders, not fresh adverts.
            let covered = self.logs[&key].topmost();
            if next > covered && self.peers.contains(&key.0) && self.disc.try_open(key.0, key.1) {
                fx.push(Effect::Offer {
                    peer: key.0,
                    bin: key.1,
                    start: next,
                });
            }
        }

        // 2. Prune the working set to the open window. A settled id (got or
        //    rejected) that has left every offer window is done — GC it from
        //    the scheduling maps, so memory tracks the window, not all history
        //    ever seen. The reserve itself (got/rejected) is retained — that is
        //    the stored data (a store-backed view in a deployed node), and it
        //    is what stops a re-offer re-fetching. (Also shrinks the maps the
        //    reset and schedule passes below iterate.)
        let windowed: BTreeSet<Triple> = self
            .logs
            .values()
            .flat_map(|l| l.unsettled().map(|(_, c)| c))
            .collect();
        let done: Vec<Triple> = self
            .bin_of
            .keys()
            .copied()
            .filter(|&c| (self.m.has(c) || self.m.is_rejected(c)) && !windowed.contains(&c))
            .collect();
        for c in done {
            self.m.forget(c);
            self.bin_of.remove(&c);
        }

        // 3. Reset: bars covering every current holder clear (cooldown) —
        //    a misattributed stall costs a round, never the chunk.
        let candidates: Vec<Triple> = self.bin_of.keys().copied().collect();
        for c in candidates {
            self.m.reset_excluded(c);
        }

        // 4. Schedule: resolve the machine's nondeterminism by policy —
        //    deepest bin first (the prio guard), least-loaded holder, peer id
        //    as the deterministic tiebreak. Policy only picks among ENABLED
        //    wants, so it can break nothing (provably-neutral).
        let mut batches: BTreeMap<(PeerId, Bin), Vec<Triple>> = BTreeMap::new();
        loop {
            // the discovery barrier: route only within bins whose choice set
            // is assembled — every peer for the bin answered (or had nothing)
            let enabled: Vec<(Triple, PeerId)> = self
                .m
                .enabled_wants()
                .into_iter()
                .filter(|(c, _)| {
                    if !self.policy.discovery_barrier {
                        return true; // ablation: schedule before the choice set assembles
                    }
                    let bin = self.bin_of.get(c).copied().unwrap_or(0);
                    self.disc.bin_ready(bin, self.peers.iter().copied(), |p| {
                        self.cursors.contains_key(&(p, bin))
                    })
                })
                .collect();
            if enabled.is_empty() {
                break;
            }
            // deepest first, then lowest triple id for determinism
            let (&(c, _), _) = enabled
                .iter()
                .map(|e @ (c, _)| (e, (self.bin_of.get(c).copied().unwrap_or(0), *c)))
                .max_by_key(|&(_, (bin, c))| (bin, std::cmp::Reverse(c)))
                .unwrap();
            // least-assigned among c's enabled holders (realised totals —
            // the §5.3 floor), outstanding load then peer id as tiebreaks
            let p = enabled
                .iter()
                .filter(|&&(d, _)| d == c)
                .map(|&(_, p)| p)
                .min_by_key(|&p| {
                    // realised totals first (the §5.3 floor); the ablation
                    // drops them, leaving history-blind outstanding load
                    let realised = if self.policy.cumulative_routing {
                        self.assigned.get(&p).copied().unwrap_or(0)
                    } else {
                        0
                    };
                    (realised, self.m.load(p), p)
                })
                .unwrap();
            let ok = self.m.want(c, p);
            debug_assert!(ok, "policy chose a disabled want");
            *self.assigned.entry(p).or_default() += 1;
            batches
                .entry((p, self.bin_of.get(&c).copied().unwrap_or(0)))
                .or_default()
                .push(c);
        }
        for ((peer, bin), want) in batches {
            fx.push(Effect::Fetch { peer, bin, want });
        }
        fx
    }

    // --- observability ---------------------------------------------------------

    /// The size of the scheduling working set — ids currently tracked for
    /// fetching. The bounded-working-set property: this tracks the open offer
    /// window, not all history, so after convergence it returns to ~0 as
    /// settled ids are pruned (the reserve `got` is retained, but it is not
    /// scheduling state — store-backed in a deployed node).
    pub fn working_set(&self) -> usize {
        self.bin_of.len()
    }

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
