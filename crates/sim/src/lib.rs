//! Deterministic self-play: `k` symmetric nodes, each both puller and server,
//! over a seeded in-process network. This is the layer TLC never covered —
//! the *composition*: the decomposition theorem (design §4) says the
//! neighbourhood is the direct sum of per-node instances, and here `k`
//! verified machines pull from each other until Θ-REP holds, with the
//! quantitative floors *measured* rather than argued:
//!
//!   - Θ-REP: every node converges to the full reserve;
//!   - the network delivery floor: Σ deliveries = Σ initial deficits, exactly
//!     — through omission, spurious misses, and adversarial message order;
//!   - serve-load fairness: free-choice routing balances to within one chunk;
//!   - freshness: a LIVE arrival at one node spreads to all.
//!
//! Determinism is total: the scheduler draws the next message from a seeded
//! splitmix64, so every run replays from `(scenario, seed)`. Adversarial
//! orderings are seeds, not threads.
//!
//! The serving side implemented here carries the obligations the specs assign
//! to it: offers are COMPLETE over their range (`IntervalSettlement`'s named
//! assumption), empty ranges BLOCK rather than answer (the live subscription
//! `OfferPacing` models as the honest `Answer` guard), and every delivery a
//! node stores immediately extends its own bin log — the epidemic channel
//! (`Gain` in `PullSyncerE`) by which holder sets grow.

use melissi_node::{Bin, Effect, Event, Node, Outcome, Policy};
use melissi_settlement::BinId;
// re-exported: `Triple` is in the sim's public surface (bin_of, Sim methods)
pub use melissi_types::{PeerId, Triple};
use std::collections::{BTreeMap, BTreeSet};

pub const NBINS: u8 = 2;
pub const RADIUS: Bin = 1;
/// Offers are PAGED, as bee's `pkg/pullsync` pages them: one Offer covers a
/// bounded window `[start, Topmost]`, not the whole bin. A peer reveals its
/// holdings a page at a time as the puller advances — so the choice set
/// assembles per page (all peers offer the same page concurrently), and the
/// grab a late scheduler can make is bounded to ONE page, not the backlog.
pub const PAGE: usize = 4;

/// Which bin a triple lives in — a stand-in for proximity order, derived
/// deterministically from the address (a low byte stands for the leading-bit
/// distance the real PO measures). `Triple::mock(n)` carries `n` in the low
/// address bytes, so this spreads the mock universe across bins as `n % NBINS`
/// did before identities were real.
pub fn bin_of(c: Triple) -> Bin {
    RADIUS + (c.address[31] % NBINS)
}

struct SimNode {
    puller: Node,
    byzantine: bool,
    /// The serving reserve: per-bin append log in BinID order.
    reserve: BTreeMap<Bin, BTreeMap<BinId, Triple>>,
    index: BTreeSet<Triple>,
    /// Offers held on an empty range: (requester, bin, start) — the blocking
    /// live subscription, answered when the bin grows.
    parked: Vec<(usize, Bin, BinId)>,
}

impl SimNode {
    fn head(&self, bin: Bin) -> BinId {
        self.reserve
            .get(&bin)
            .and_then(|m| m.keys().last().copied())
            .unwrap_or(0)
    }
}

enum Msg {
    /// An effect emitted by `from`, travelling to the peer it names.
    Req { from: usize, eff: Effect },
    /// An event travelling back to `to`.
    Resp { to: usize, ev: Event },
}

pub struct Sim {
    nodes: Vec<SimNode>,
    queue: Vec<Msg>,
    rng: u64,
    /// Deliveries served per node — the fairness measurement.
    pub served: Vec<u64>,
    /// Spurious-miss budget: an honest fetch outcome turned `Missed`,
    /// rng-placed (the misattributed-timeout reality, `TimeoutBudget`).
    pub spurious_budget: u32,
    /// All-together (concurrent) discovery: the honest cursor/offer handshake is
    /// prompt — fired to every holder at once, answered within a round-trip — so
    /// the choice set assembles before deliveries dominate and least-loaded
    /// routing balances (design §5.3, §5.6). The adversary still reorders
    /// DELIVERIES freely (the floor must survive that); it does not get to defer
    /// the honest discovery handshake across the whole sync. [`Sim::staggered`]
    /// drops this — the worst case a discovery barrier, or a tight window,
    /// otherwise has to cover.
    concurrent_discovery: bool,
    steps: u64,
}

fn peer_id(i: usize) -> PeerId {
    i as PeerId
}

/// The cursor/offer handshake messages — discovery, as opposed to delivery
/// (`Fetch`/`FetchResult`). Under concurrent discovery these are answered first.
fn is_discovery(m: &Msg) -> bool {
    match m {
        Msg::Req { eff, .. } => matches!(eff, Effect::GetCursors(_) | Effect::Offer { .. }),
        Msg::Resp { ev, .. } => {
            matches!(ev, Event::CursorsResult { .. } | Event::OfferResult { .. })
        }
    }
}

fn splitmix(s: &mut u64) -> u64 {
    *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Sim {
    pub fn new(k: usize, byzantine: &[usize], seed: u64) -> Self {
        Self::with_policy(k, byzantine, seed, Policy::SHIPPED)
    }

    /// As [`Sim::new`], with the floor-achieving policy chosen explicitly — the
    /// fairness ablations construct nodes with a knob off and assert the O5
    /// floor breaks (the negatives matching the gate-critical TLA ablations).
    pub fn with_policy(k: usize, byzantine: &[usize], seed: u64, policy: Policy) -> Self {
        let nodes = (0..k)
            .map(|i| SimNode {
                puller: Node::with_policy(melissi_machine::Config::PRODUCTION, RADIUS, policy),
                byzantine: byzantine.contains(&i),
                reserve: BTreeMap::new(),
                index: BTreeSet::new(),
                parked: Vec::new(),
            })
            .collect();
        Sim {
            nodes,
            queue: Vec::new(),
            rng: seed ^ 0xD1B5_4A32_D192_ED03,
            served: vec![0; k],
            spurious_budget: 0,
            concurrent_discovery: true,
            steps: 0,
        }
    }

    /// Drop concurrent discovery: let the adversary defer the cursor/offer
    /// handshake too, so the choice set assembles staggered. The
    /// fairness ablations use this to show what all-together offers (or, on the
    /// old design, the discovery barrier) otherwise cover — and that a tight
    /// [`Policy::window`] bounds the resulting skew.
    pub fn staggered(mut self) -> Self {
        self.concurrent_discovery = false;
        self
    }

    /// Seed an initial holding: in the reserve (served) AND preloaded in the
    /// puller (the ReserveHas view) — never owed, never fetched.
    pub fn seed(&mut self, i: usize, c: Triple) {
        self.store(i, c);
        self.nodes[i].puller.preload(c);
    }

    /// A post-start arrival at node `i` (an upload landing): enters the
    /// reserve with a fresh BinID — past every cursor snapshot, so LIVE to
    /// all peers — and answers their standing tail offers.
    pub fn arrive(&mut self, i: usize, c: Triple) {
        self.nodes[i].puller.preload(c);
        self.store(i, c);
    }

    /// Everyone sees everyone: the neighbourhood (k <= 8, fully connected).
    pub fn start(&mut self) {
        for i in 0..self.nodes.len() {
            for j in 0..self.nodes.len() {
                if i != j {
                    self.feed(i, Event::PeerSeen(peer_id(j)));
                }
            }
        }
    }

    /// Ring topology: each node sees `fanout` peers on each side (2×fanout total).
    pub fn start_ring(&mut self, fanout: usize) {
        let k = self.nodes.len();
        if k == 0 || fanout == 0 {
            return;
        }
        for i in 0..k {
            for d in 1..=fanout {
                let left = (i + k - d) % k;
                let right = (i + d) % k;
                self.feed(i, Event::PeerSeen(peer_id(left)));
                self.feed(i, Event::PeerSeen(peer_id(right)));
            }
        }
    }

    fn feed(&mut self, i: usize, ev: Event) {
        for eff in self.nodes[i].puller.handle(ev) {
            self.queue.push(Msg::Req { from: i, eff });
        }
    }

    fn store(&mut self, i: usize, c: Triple) {
        if !self.nodes[i].index.insert(c) {
            return;
        }
        let bin = bin_of(c);
        let head = self.nodes[i].head(bin);
        self.nodes[i]
            .reserve
            .entry(bin)
            .or_default()
            .insert(head + 1, c);
        // the bin grew: answer the parked offers (the live subscription)
        let ready: Vec<(usize, Bin, BinId)> = self.nodes[i]
            .parked
            .iter()
            .copied()
            .filter(|&(_, b, start)| b == bin && self.nodes[i].head(bin) >= start)
            .collect();
        self.nodes[i]
            .parked
            .retain(|&(_, b, start)| !(b == bin && head + 1 >= start));
        for (requester, b, start) in ready {
            let resp = self.offer_response(i, b, start);
            self.queue.push(Msg::Resp {
                to: requester,
                ev: resp,
            });
        }
    }

    /// Offer completeness — the spec's named serving-side obligation: every
    /// entry held in `[start, Topmost]` is in the response. PAGED, as bee pages:
    /// at most [`PAGE`] entries, `Topmost` the last BinID in the page (the puller
    /// advances to the next page itself). The choice set therefore assembles a
    /// page at a time, all peers on the same page ~concurrently.
    fn offer_response(&self, j: usize, bin: Bin, start: BinId) -> Event {
        let refs: Vec<(BinId, Triple)> = self.nodes[j]
            .reserve
            .get(&bin)
            .map(|m| m.range(start..).take(PAGE).map(|(&b, &c)| (b, c)).collect())
            .unwrap_or_default();
        // Topmost covers exactly this page — a higher head means the puller will
        // re-offer the next tail (bee's sliding window).
        let topmost = refs
            .last()
            .map(|&(b, _)| b)
            .unwrap_or(start.saturating_sub(1))
            .max(start);
        Event::OfferResult {
            peer: peer_id(j),
            bin,
            start,
            refs,
            topmost,
        }
    }

    /// Deliver one randomly-chosen in-flight message. False when quiescent.
    pub fn step(&mut self) -> bool {
        if self.queue.is_empty() {
            return false;
        }
        self.steps += 1;
        assert!(self.steps < 2_000_000, "simulation did not quiesce");
        // Concurrent discovery: answer the honest handshake promptly (prefer
        // discovery messages when any are in flight), so offers arrive
        // all-together and the choice set assembles before deliveries dominate.
        // Deliveries stay adversarially ordered among themselves.
        let pool: Vec<usize> = if self.concurrent_discovery {
            let disc: Vec<usize> = (0..self.queue.len())
                .filter(|&i| is_discovery(&self.queue[i]))
                .collect();
            if disc.is_empty() {
                (0..self.queue.len()).collect()
            } else {
                disc
            }
        } else {
            (0..self.queue.len()).collect()
        };
        let idx = pool[(splitmix(&mut self.rng) % pool.len() as u64) as usize];
        let msg = self.queue.swap_remove(idx);
        match msg {
            Msg::Req { from, eff } => match eff {
                Effect::GetCursors(p) => {
                    let j = p as usize;
                    // cursors span ALL bins of the universe — an empty bin has
                    // head 0, and the standing offer on it is the channel a
                    // later arrival propagates through
                    let cursors: Vec<(Bin, BinId)> = (RADIUS..RADIUS + NBINS)
                        .map(|b| (b, self.nodes[j].head(b)))
                        .collect();
                    self.queue.push(Msg::Resp {
                        to: from,
                        ev: Event::CursorsResult { peer: p, cursors },
                    });
                }
                Effect::Offer { peer, bin, start } => {
                    let j = peer as usize;
                    if self.nodes[j].head(bin) >= start {
                        let resp = self.offer_response(j, bin, start);
                        self.queue.push(Msg::Resp { to: from, ev: resp });
                    } else {
                        self.nodes[j].parked.push((from, bin, start)); // block
                    }
                }
                Effect::Fetch { peer, bin, want } => {
                    let j = peer as usize;
                    let mut outcomes = Vec::new();
                    for c in want {
                        let outcome = if self.nodes[j].byzantine {
                            Outcome::Missed // omission: advertises, never serves
                        } else if self.spurious_budget > 0
                            && splitmix(&mut self.rng).is_multiple_of(4)
                        {
                            self.spurious_budget -= 1;
                            Outcome::Missed // the misattributed timeout
                        } else if !self.nodes[j].index.contains(&c) {
                            Outcome::Missed // churned out under the claim
                        } else {
                            self.served[j] += 1;
                            Outcome::Delivered
                        };
                        outcomes.push((c, outcome));
                    }
                    self.queue.push(Msg::Resp {
                        to: from,
                        ev: Event::FetchResult {
                            peer,
                            bin,
                            outcomes,
                        },
                    });
                }
                Effect::Settled { .. } => {} // persistence: a no-op in the sim
            },
            Msg::Resp { to, ev } => {
                let delivered: Vec<Triple> = match &ev {
                    Event::FetchResult { outcomes, .. } => outcomes
                        .iter()
                        .filter(|(_, o)| *o == Outcome::Delivered)
                        .map(|&(c, _)| c)
                        .collect(),
                    _ => Vec::new(),
                };
                self.feed(to, ev);
                // the store write: what the puller delivered, the node now
                // serves — holder sets grow (the epidemic / Gain channel)
                for c in delivered {
                    self.store(to, c);
                }
            }
        }
        true
    }

    pub fn run(&mut self) {
        while self.step() {}
    }

    // --- measurements -----------------------------------------------------------

    pub fn k(&self) -> usize {
        self.nodes.len()
    }

    pub fn node_has(&self, i: usize, c: Triple) -> bool {
        self.nodes[i].index.contains(&c)
    }

    pub fn deliveries(&self, i: usize) -> u64 {
        self.nodes[i].puller.deliveries() as u64
    }

    pub fn deficit(&self, i: usize) -> usize {
        self.nodes[i].puller.deficit()
    }

    /// The node's scheduling working-set size — bounded to the open window.
    pub fn working_set(&self, i: usize) -> usize {
        self.nodes[i].puller.working_set()
    }

    pub fn assert_invariants(&self) {
        for (i, n) in self.nodes.iter().enumerate() {
            assert!(!n.puller.conflict(), "node {i}: ConflictFree tripped");
            n.puller
                .check_invariants()
                .unwrap_or_else(|e| panic!("node {i}: {e}"));
        }
    }

    /// Θ-REP over the given universe: every node holds every chunk.
    pub fn assert_converged(&self, universe: &[Triple]) {
        for (i, n) in self.nodes.iter().enumerate() {
            assert_eq!(self.deficit(i), 0, "node {i} deficit");
            for &c in universe {
                assert!(n.index.contains(&c), "node {i} missing chunk {c:?}");
            }
        }
        self.assert_invariants();
    }
}
