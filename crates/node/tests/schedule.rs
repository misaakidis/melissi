//! The node core driven through deterministic event schedules — single puller
//! against scripted peers, the M1 exit criterion: `MC_storm`'s scenario as a
//! schedule, plus the cross-peer settlement and entry-fault stories.
//!
//! The harness is the shell the sans-io core expects: it owns delivery and
//! "time" (the processing order), answers `Offer` effects from scripted
//! holdings, holds offers on empty ranges (the blocking live subscription),
//! and injects the adversarial events — omission, one spurious miss, churn,
//! a LIVE arrival — at chosen points. Same seedless determinism throughout:
//! same schedule in, same trace out.

use melissi_machine::Config;
use melissi_node::{Bin, Effect, Event, Node, Outcome};
use melissi_settlement::BinId;
use melissi_types::Triple;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type PeerId = u8;

/// Readable chunk number → real triple. The node-level scheduling scenarios
/// are content-agnostic; bins are assigned explicitly, so chunk numbers are
/// pure identities.
fn t(n: u32) -> Triple {
    Triple::mock(n)
}

/// A scripted serving peer: per-bin append logs in BinID order.
struct SimPeer {
    byzantine: bool,
    /// bin -> (binID -> triple): what the peer currently offers.
    bins: BTreeMap<Bin, BTreeMap<BinId, Triple>>,
}

impl SimPeer {
    fn head(&self, bin: Bin) -> BinId {
        self.bins
            .get(&bin)
            .and_then(|m| m.keys().last().copied())
            .unwrap_or(0)
    }
    fn holds(&self, c: Triple) -> bool {
        self.bins.values().any(|m| m.values().any(|&x| x == c))
    }
}

struct Harness {
    node: Node,
    peers: BTreeMap<PeerId, SimPeer>,
    /// Effects not yet answered, in emission order (the schedule).
    pending: VecDeque<Effect>,
    /// Offers held on an empty range: the live subscription, answered when
    /// the range gains entries (or never — they outlive the test harmlessly).
    held: Vec<(PeerId, Bin, BinId)>,
    /// (peer, triple) pairs whose NEXT fetch is turned into a spurious miss.
    spurious: BTreeSet<(PeerId, Triple)>,
    /// Entry-faults: triples every holder serves only as `Rejected`.
    bad: BTreeSet<Triple>,
    settled_log: Vec<(PeerId, Bin, BinId)>,
}

impl Harness {
    fn new(cfg: Config, radius: Bin) -> Self {
        Harness {
            node: Node::new(cfg, radius),
            peers: BTreeMap::new(),
            pending: VecDeque::new(),
            held: Vec::new(),
            spurious: BTreeSet::new(),
            bad: BTreeSet::new(),
            settled_log: Vec::new(),
        }
    }

    fn add_peer(&mut self, id: PeerId, byzantine: bool, holdings: &[(Bin, &[u32])]) {
        let mut bins = BTreeMap::new();
        for &(bin, cs) in holdings {
            let mut log = BTreeMap::new();
            for (i, &c) in cs.iter().enumerate() {
                log.insert((i + 1) as BinId, t(c));
            }
            bins.insert(bin, log);
        }
        self.peers.insert(id, SimPeer { byzantine, bins });
        self.feed(Event::PeerSeen(id));
    }

    fn feed(&mut self, ev: Event) {
        let fx = self.node.handle(ev);
        self.pending.extend(fx);
    }

    /// Answer one pending effect. Returns false when the schedule is quiescent
    /// (only held live-subscriptions remain).
    fn step(&mut self) -> bool {
        let Some(effect) = self.pending.pop_front() else {
            return false;
        };
        match effect {
            Effect::GetCursors(p) => {
                let peer = &self.peers[&p];
                let cursors: Vec<(Bin, BinId)> =
                    peer.bins.keys().map(|&b| (b, peer.head(b))).collect();
                self.feed(Event::CursorsResult { peer: p, cursors });
            }
            Effect::Offer { peer, bin, start } => {
                if !self.answer_offer(peer, bin, start) {
                    self.held.push((peer, bin, start)); // empty range: block
                }
            }
            Effect::Fetch { peer, bin, want } => {
                let mut outcomes = Vec::new();
                for c in want {
                    let outcome = if self.bad.contains(&c) {
                        Outcome::Rejected // entry-fault: identical at every holder
                    } else if self.peers[&peer].byzantine {
                        Outcome::Missed // omission: advertises, never delivers
                    } else if self.spurious.remove(&(peer, c)) {
                        Outcome::Missed // the misfire: honest, but timed out once
                    } else if !self.peers[&peer].holds(c) {
                        Outcome::Missed // churned out under the claim
                    } else {
                        Outcome::Delivered
                    };
                    outcomes.push((c, outcome));
                }
                self.feed(Event::FetchResult {
                    peer,
                    bin,
                    outcomes,
                });
            }
            Effect::Settled { peer, bin, upto } => {
                self.settled_log.push((peer, bin, upto));
            }
        }
        true
    }

    fn run_to_quiescence(&mut self) {
        let mut steps = 0;
        while self.step() {
            steps += 1;
            assert!(steps < 10_000, "schedule did not quiesce");
        }
    }

    fn answer_offer(&mut self, p: PeerId, bin: Bin, start: BinId) -> bool {
        let peer = &self.peers[&p];
        let head = peer.head(bin);
        let refs: Vec<(BinId, Triple)> = peer
            .bins
            .get(&bin)
            .map(|m| m.range(start..).map(|(&b, &c)| (b, c)).collect())
            .unwrap_or_default();
        if refs.is_empty() && head < start {
            return false; // nothing at or past start: hold (live subscription)
        }
        self.feed(Event::OfferResult {
            peer: p,
            bin,
            start,
            refs,
            topmost: head.max(start),
        });
        true
    }

    /// Churn-out: the peer evicts a triple; visible at its next offer.
    fn lose(&mut self, p: PeerId, c: Triple) {
        let peer = self.peers.get_mut(&p).unwrap();
        for log in peer.bins.values_mut() {
            log.retain(|_, &mut x| x != c);
        }
    }

    /// A new entry lands at a peer (gain / LIVE arrival): appended with a
    /// fresh BinID; any held offer for that range is answered — the blocking
    /// subscription returning.
    fn arrive(&mut self, p: PeerId, bin: Bin, c: Triple) {
        let peer = self.peers.get_mut(&p).unwrap();
        let head = peer.head(bin);
        peer.bins.entry(bin).or_default().insert(head + 1, c);
        let ready: Vec<(PeerId, Bin, BinId)> = self
            .held
            .iter()
            .copied()
            .filter(|&(q, b, _)| q == p && b == bin)
            .collect();
        self.held.retain(|&(q, b, _)| !(q == p && b == bin));
        for (q, b, start) in ready {
            let answered = self.answer_offer(q, b, start);
            assert!(answered, "arrival must answer the held offer");
        }
    }

    fn assert_invariants(&self) {
        assert!(!self.node.conflict(), "ConflictFree tripped");
        self.node.check_invariants().expect("machine invariants");
        // Settled effects are monotone per (peer, bin).
        let mut last: BTreeMap<(PeerId, Bin), BinId> = BTreeMap::new();
        for &(p, b, upto) in &self.settled_log {
            let prev = last.insert((p, b), upto).unwrap_or(0);
            assert!(
                upto > prev,
                "Settled regressed for ({p},{b}): {prev} -> {upto}"
            );
        }
    }
}

// -----------------------------------------------------------------------------

/// Happy path, k = 3, full replication: every chunk fetched EXACTLY once —
/// the headline k× saving — and every peer's interval drains.
#[test]
fn full_replication_fetches_each_chunk_once() {
    let mut h = Harness::new(Config::PRODUCTION, 1);
    for p in 1..=3 {
        h.add_peer(p, false, &[(1, &[10, 20, 30])]);
    }
    h.run_to_quiescence();

    assert_eq!(h.node.deficit(), 0);
    assert_eq!(h.node.deliveries(), 3, "3 chunks, 3 deliveries — not k x 3");
    for p in 1..=3 {
        assert_eq!(
            h.node.interval(p, 1),
            Some(3),
            "peer {p}'s interval must drain"
        );
    }
    h.assert_invariants();
}

/// The §4.6 cross-peer story: the same chunks sit at different BinIDs at
/// different peers; each chunk is fetched from ONE of them, yet BOTH
/// intervals drain — settlement is global, advance is local.
#[test]
fn cross_peer_fetch_drains_both_intervals() {
    let mut h = Harness::new(Config::PRODUCTION, 1);
    h.add_peer(1, false, &[(1, &[10, 20])]); // BinIDs: 1->10, 2->20
    h.add_peer(2, false, &[(1, &[20, 10])]); // BinIDs: 1->20, 2->10
    h.run_to_quiescence();

    assert_eq!(h.node.deficit(), 0);
    assert_eq!(h.node.deliveries(), 2);
    assert_eq!(h.node.interval(1, 1), Some(2));
    assert_eq!(h.node.interval(2, 1), Some(2));
    h.assert_invariants();
}

/// An entry-fault (invalid stamp) settles by rejection: the interval advances
/// past it and the deficit ignores it — resume liveness, not delivery.
#[test]
fn rejected_entry_settles_and_never_pins_the_interval() {
    let mut h = Harness::new(Config::PRODUCTION, 1);
    h.bad.insert(t(20));
    h.add_peer(1, false, &[(1, &[10, 20, 30])]);
    h.add_peer(2, false, &[(1, &[10, 20, 30])]);
    h.run_to_quiescence();

    assert_eq!(
        h.node.deficit(),
        0,
        "the rejected entry is settled, not owed"
    );
    assert!(h.node.has(t(10)) && h.node.has(t(30)) && !h.node.has(t(20)));
    assert_eq!(h.node.deliveries(), 2);
    assert_eq!(
        h.node.interval(1, 1),
        Some(3),
        "the bad entry must not pin the interval"
    );
    h.assert_invariants();
}

/// MC_storm as an event schedule — every mechanism and relaxed assumption at
/// once, on a k=4 tile: a Byzantine omitter, a single-holder chunk, one
/// spurious miss on that chunk's only honest holder (exercising exhaustion +
/// reset), churn out of a claimed holding, and a LIVE arrival that is also
/// the deepest-priority chunk.
#[test]
fn storm_schedule() {
    let mut h = Harness::new(Config::PRODUCTION, 1);
    // chunk c lives in bin c (deeper bin = higher priority), per MC_storm:
    // Holds: 1:{1,2,4} 2:{2,3,4} 3:{3,4} 4:{1,2,3,4}; Byzantine {4}; LIVE {4}.
    // Chunk 4 is NOT in the initial logs — it arrives mid-run.
    h.add_peer(1, false, &[(1, &[1]), (2, &[2]), (4, &[])]);
    h.add_peer(2, false, &[(2, &[2]), (3, &[3]), (4, &[])]);
    h.add_peer(3, false, &[(3, &[3]), (4, &[])]);
    h.add_peer(4, true, &[(1, &[1]), (2, &[2]), (3, &[3]), (4, &[])]);

    // the misfire: chunk 1's only honest holder times out once -> peer 1 is
    // barred for chunk 1; with the omitter also barred, the bars cover every
    // holder -> reset-on-exhaustion -> the retry succeeds.
    h.spurious.insert((1, t(1)));

    // drain the HIST backlog under omission + the misfire
    h.run_to_quiescence();

    // churn: peer 3 evicts chunk 3 (its only honest copy at peer 3 — but
    // peer 2 still holds it; supply survives). Make peer 2 the one fetched
    // from by letting the schedule re-route after the lose shows up.
    h.lose(3, t(3));

    // the LIVE arrival: chunk 4 lands at its holders, deepest priority;
    // held offers for bin 4 answer (the blocking subscription returns).
    for p in [1, 2, 3, 4] {
        h.arrive(p, 4, t(4));
    }
    h.run_to_quiescence();

    assert_eq!(h.node.deficit(), 0, "storm must converge");
    for n in [1, 2, 3, 4] {
        assert!(h.node.has(t(n)), "chunk {n} missing");
    }
    // the floor: 4 chunks, 4 deliveries — through omission, a misfire,
    // churn, and a LIVE arrival.
    assert_eq!(h.node.deliveries(), 4);
    h.assert_invariants();
}

/// The offer-diff churn path: an UNSETTLED entry that vanishes from a peer's
/// re-offer is forgotten as a gap — candidacy drops with the holding, no bar
/// is placed, and the peer's interval still drains past it (the other holder
/// keeps the chunk visible; settlement is global).
#[test]
fn churned_out_entry_is_forgotten_without_a_bar() {
    let mut h = Harness::new(Config::PRODUCTION, 1);
    h.add_peer(1, false, &[(1, &[20])]); //        BinIDs: 1 -> 20
    h.add_peer(2, false, &[(1, &[20, 30])]); //    BinIDs: 1 -> 20, 2 -> 30
                                             // drive discovery until both fetches are scheduled
    while h
        .pending
        .front()
        .is_some_and(|fx| !matches!(fx, Effect::Fetch { .. }))
    {
        h.step();
    }
    // hold back peer 1's fetch (it carries chunk 20's claim), let peer 2's run
    let held_fetch = h.pending.pop_front().expect("peer 1's fetch");
    assert!(matches!(held_fetch, Effect::Fetch { peer: 1, .. }));
    h.lose(2, t(20)); // churn: peer 2 evicts 20 while it is claimed at peer 1
    h.run_to_quiescence(); // peer 2's fetch (chunk 30) completes
                           // the shell's refresh: re-offer the covered-but-unsettled range; peer 2's
                           // fresh offer no longer names 20 — the diff fires
    h.feed(Event::Tick);
    h.run_to_quiescence();
    // the diff fired: 20 left peer 2's window as a gap, and the interval
    // drained to topmost although 20 is still unsettled — it stays visible
    // through peer 1's window, where its claim is in flight.
    assert_eq!(
        h.node.interval(2, 1),
        Some(2),
        "gap must not pin peer 2's interval"
    );
    assert_eq!(h.node.deficit(), 1, "chunk 20 still owed (claim in flight)");
    // now peer 1 delivers it
    h.pending.push_back(held_fetch);
    h.run_to_quiescence();
    assert_eq!(h.node.deficit(), 0);
    assert_eq!(h.node.deliveries(), 2);
    assert_eq!(h.node.interval(1, 1), Some(1));
    h.assert_invariants();
}

/// Peer departure mid-claim: claims release with no bars; the chunks reroute
/// to the surviving holder.
#[test]
fn peer_gone_releases_claims_and_reroutes() {
    let mut h = Harness::new(Config::PRODUCTION, 1);
    h.add_peer(1, false, &[(1, &[10, 20])]);
    h.add_peer(2, false, &[(1, &[10, 20])]);
    // process discovery only as far as the first fetches being scheduled:
    // answer cursors + offers, then drop peer 1 before answering its fetch.
    while let Some(fx) = h.pending.front() {
        if matches!(fx, Effect::Fetch { .. }) {
            break;
        }
        h.step();
    }
    // whatever was claimed from peer 1 is now in flight to a dead peer
    h.feed(Event::PeerGone(1));
    // drop the stale fetch effects addressed to peer 1 (the shell's cancel)
    h.pending.retain(|fx| {
        !matches!(
            fx,
            Effect::Fetch { peer: 1, .. } | Effect::Offer { peer: 1, .. }
        )
    });
    h.run_to_quiescence();

    assert_eq!(h.node.deficit(), 0);
    assert!(h.node.has(t(10)) && h.node.has(t(20)));
    assert_eq!(h.node.deliveries(), 2);
    h.assert_invariants();
}
