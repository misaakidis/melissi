//! Wire-level self-play: the sans-io node, driven entirely through the bee
//! wire adapter — real protobuf frames, positional bitvectors, re-offer on
//! fetch — against serving reserves. The M3-a exit: the node converges to the
//! SAME floors as the in-process sim, but every byte between puller and server
//! crosses the legacy `pkg/pullsync` coupling. If the adapter mistranslated
//! the core's semantics, the floors would break here.
//!
//! The harness is the "shell": it owns the streams (byte buffers), runs the
//! client pollers for the node's effects and the server pollers for the
//! reserve, and shuttles frames. Determinism is total — effects processed in
//! emission order; no clock, no threads.

use melissi_machine::Config;
use melissi_node::{Bin, Effect, Event, Node, Outcome};
use melissi_settlement::BinId;
use melissi_wire::adapter::*;
use std::collections::{BTreeMap, VecDeque};

type Triple = u32;
type PeerId = u8;

const NBINS: u8 = 2;
const RADIUS: Bin = 1;

fn bin_of(c: Triple) -> Bin {
    RADIUS + (c % NBINS as u32) as u8
}

/// A serving reserve: per-bin append log in BinID order. Implements the
/// adapter's `ServeReserve` (offer completeness, cursors, paging).
#[derive(Default)]
struct Reserve {
    bins: BTreeMap<Bin, BTreeMap<BinId, Triple>>,
    index: std::collections::BTreeSet<Triple>,
}

impl Reserve {
    fn store(&mut self, c: Triple) {
        if !self.index.insert(c) {
            return;
        }
        let bin = bin_of(c);
        let head = self.bins.get(&bin).and_then(|m| m.keys().last().copied()).unwrap_or(0);
        self.bins.entry(bin).or_default().insert(head + 1, c);
    }
}

impl ServeReserve for Reserve {
    fn collect(&self, bin: Bin, start: BinId, limit: usize) -> (Vec<(BinId, Triple)>, BinId) {
        let head = self.bins.get(&bin).and_then(|m| m.keys().last().copied()).unwrap_or(0);
        let entries: Vec<(BinId, Triple)> = self
            .bins
            .get(&bin)
            .map(|m| m.range(start..).map(|(&b, &c)| (b, c)).take(limit).collect())
            .unwrap_or_default();
        let topmost = entries.last().map(|&(b, _)| b).unwrap_or(head).max(start.saturating_sub(1));
        (entries, topmost.max(head.min(start.saturating_sub(1))))
    }
    fn has(&self, c: Triple) -> bool {
        self.index.contains(&c)
    }
    fn cursors(&self) -> Vec<u64> {
        (0..RADIUS + NBINS)
            .map(|b| self.bins.get(&b).and_then(|m| m.keys().last().copied()).unwrap_or(0))
            .collect()
    }
    fn epoch(&self) -> u64 {
        1
    }
}

/// One pending wire interaction the harness must drive to completion.
enum Job {
    Cursors { from: usize, server: usize, client: CursorsClient },
    Offer { from: usize, server: usize, client: OfferClient },
    Fetch { from: usize, server: usize, client: FetchClient },
}

struct WirePlay {
    nodes: Vec<Node>,
    reserves: Vec<Reserve>,
    byzantine: Vec<bool>,
    codec: SyntheticCodec,
    effects: VecDeque<(usize, Effect)>,
    jobs: VecDeque<Job>,
    served: Vec<u64>,
}

fn pid(i: usize) -> PeerId {
    i as PeerId
}

impl WirePlay {
    fn new(k: usize, byzantine: &[usize]) -> Self {
        WirePlay {
            nodes: (0..k).map(|_| Node::new(Config::PRODUCTION, RADIUS)).collect(),
            reserves: (0..k).map(|_| Reserve::default()).collect(),
            byzantine: (0..k).map(|i| byzantine.contains(&i)).collect(),
            codec: SyntheticCodec,
            effects: VecDeque::new(),
            jobs: VecDeque::new(),
            served: vec![0; k],
        }
    }

    fn seed(&mut self, i: usize, c: Triple) {
        self.reserves[i].store(c);
        self.nodes[i].preload(c);
    }

    fn feed(&mut self, i: usize, ev: Event) {
        for eff in self.nodes[i].handle(ev) {
            self.effects.push_back((i, eff));
        }
    }

    fn start(&mut self) {
        let k = self.nodes.len();
        for i in 0..k {
            for j in 0..k {
                if i != j {
                    self.feed(i, Event::PeerSeen(pid(j)));
                }
            }
        }
    }

    /// Turn one effect into a wire job (or settle, a no-op here).
    fn launch(&mut self, from: usize, eff: Effect) {
        match eff {
            Effect::GetCursors(p) => {
                self.jobs.push_back(Job::Cursors {
                    from,
                    server: p as usize,
                    client: CursorsClient::new(),
                });
            }
            Effect::Offer { peer, bin, start } => {
                self.jobs.push_back(Job::Offer {
                    from,
                    server: peer as usize,
                    client: OfferClient::new(bin, start),
                });
            }
            Effect::Fetch { peer, bin, want } => {
                self.jobs.push_back(Job::Fetch {
                    from,
                    server: peer as usize,
                    client: FetchClient::new(bin, start_of(&self.nodes[from], peer, bin), want),
                });
            }
            Effect::Settled { .. } => {} // persistence: no-op
        }
    }

    /// Run all jobs to completion, threading bytes through the pollers. A job
    /// that BLOCKS (empty range, no growth coming) is simply dropped — the
    /// live subscription with nothing to deliver; convergence does not need it.
    fn run(&mut self) {
        let mut guard = 0;
        loop {
            // drain freshly-emitted effects into jobs
            while let Some((from, eff)) = self.effects.pop_front() {
                self.launch(from, eff);
            }
            let Some(job) = self.jobs.pop_front() else { break };
            guard += 1;
            assert!(guard < 1_000_000, "wireplay did not converge");
            self.drive(job);
        }
    }

    /// Drive one job: a synchronous request/response loop over byte buffers.
    /// (Determinism: each job runs to its terminal ClientOut before the next;
    /// reordering jobs would be a separate adversarial schedule.)
    fn drive(&mut self, job: Job) {
        match job {
            Job::Cursors { from, server, mut client } => {
                let mut to_server = match client.poll(&[]) {
                    ClientOut::Send(b) => b,
                    _ => unreachable!("cursors client sends Syn first"),
                };
                let resp = CursorsServer::respond(&self.reserves[server], &to_server)
                    .expect("server answers Syn");
                to_server.clear();
                match client.poll(&resp) {
                    ClientOut::Cursors { cursors, .. } => {
                        self.feed(from, Event::CursorsResult { peer: pid(server), cursors });
                    }
                    _ => panic!("cursors did not complete"),
                }
            }
            Job::Offer { from, server, mut client, .. } => {
                let mut stream = ServerStream::new();
                // Get -> Offer
                let get = match client.poll(&self.codec, &[]) {
                    ClientOut::Send(b) => b,
                    _ => unreachable!(),
                };
                match stream.poll(&self.codec, &self.reserves[server], &get) {
                    ServerOut::Send(offer_bytes) => match client.poll(&self.codec, &offer_bytes) {
                        ClientOut::OfferDone { refs, topmost } => {
                            let (bin, start) = (client.bin, client.start);
                            self.feed(
                                from,
                                Event::OfferResult { peer: pid(server), bin, start, refs, topmost },
                            );
                        }
                        ClientOut::Need => panic!("offer truncated"),
                        _ => unreachable!(),
                    },
                    ServerOut::Blocked { .. } => { /* empty range: drop (live sub) */ }
                    _ => unreachable!(),
                }
            }
            Job::Fetch { from, server, mut client } => {
                let outcomes = self.drive_fetch(server, &mut client);
                let (bin, _) = (client.bin, client.start);
                self.feed(from, Event::FetchResult { peer: pid(server), bin, outcomes });
            }
        }
    }

    /// The fetch round-trip, following the wire's explicit alternation:
    /// client Get -> server Offer -> client Want -> server Delivery* ->
    /// client done. A byzantine server omits the delivery batch (it still
    /// advertises, the §6 omission model). Returns per-triple outcomes.
    fn drive_fetch(&mut self, server: usize, client: &mut FetchClient) -> Vec<(Triple, Outcome)> {
        let mut stream = ServerStream::new();
        // 1. client sends Get
        let get = match client.poll(&self.codec, &[]) {
            ClientOut::Send(b) => b,
            ClientOut::FetchDone { outcomes } => return outcomes,
            _ => unreachable!("fetch starts with Get"),
        };
        // 2. server answers the Offer (advertisement — even a byzantine peer
        //    advertises; omission is at delivery)
        let offer = match stream.poll(&self.codec, &self.reserves[server], &get) {
            ServerOut::Send(b) => b,
            ServerOut::Done | ServerOut::Blocked { .. } => Vec::new(),
            ServerOut::Need => unreachable!(),
        };
        // 3. client matches by triple, sends the Want bitvector (or finishes
        //    if the offer was empty)
        let want = match client.poll(&self.codec, &offer) {
            ClientOut::Send(b) => b,
            ClientOut::FetchDone { outcomes } => return outcomes,
            ClientOut::Need => return Vec::new(),
            _ => unreachable!(),
        };
        // 4. server delivers the wanted subset — byzantine omits it
        let deliveries = match stream.poll(&self.codec, &self.reserves[server], &want) {
            ServerOut::Send(b) if !self.byzantine[server] => {
                self.count_served(server, &b);
                b
            }
            _ => Vec::new(), // Done, omitted, or blocked: no deliveries
        };
        // 5. client finalizes; the shell closes the stream so unmet wants
        //    become Missed (omission, short delivery, or timeout)
        match client.poll(&self.codec, &deliveries) {
            ClientOut::FetchDone { outcomes } => outcomes,
            _ => match client.close() {
                ClientOut::FetchDone { outcomes } => outcomes,
                _ => unreachable!("close always finishes"),
            },
        }
    }

    fn count_served(&mut self, server: usize, frames: &[u8]) {
        // count non-zero-address deliveries in the frame batch
        let mut b = frames;
        while let Some((msg, n)) = melissi_wire::pb::deframe(b) {
            if let Some(d) = melissi_wire::pb::Delivery::decode(&msg) {
                if !d.data.is_empty() && !d.address.iter().all(|&x| x == 0) {
                    self.served[server] += 1;
                }
            }
            b = &b[n..];
        }
    }
}

fn start_of(_node: &Node, _peer: PeerId, _bin: Bin) -> BinId {
    // the node's Fetch effect was built from its current interval; the adapter
    // re-offers from `start`, and matches wanted triples by identity, so an
    // approximate start is sound (conservative re-offer). Use 1 (bin base).
    1
}

// -----------------------------------------------------------------------------

/// Cold start over the wire: one empty node, k-1 full servers. Converges to
/// the full reserve, fetching each chunk exactly once — across real frames.
#[test]
fn wire_cold_start_converges_at_the_floor() {
    let m: u32 = 12;
    let universe: Vec<Triple> = (0..m).collect();
    for k in [2usize, 3, 4] {
        let mut w = WirePlay::new(k, &[]);
        for i in 1..k {
            for &c in &universe {
                w.seed(i, c);
            }
        }
        w.start();
        w.run();
        for &c in &universe {
            assert!(w.nodes[0].has(c), "k={k}: chunk {c} missing over the wire");
        }
        assert_eq!(w.nodes[0].deficit(), 0, "k={k}");
        assert_eq!(w.nodes[0].deliveries(), m, "k={k}: exactly-once over the wire");
        assert!(!w.nodes[0].conflict());
        w.nodes[0].check_invariants().unwrap();
    }
}

/// A Byzantine server advertises everything, delivers nothing; the empty node
/// still converges via the honest server — failover across the real wire.
#[test]
fn wire_byzantine_omitter_is_failed_over() {
    let m: u32 = 8;
    let universe: Vec<Triple> = (0..m).collect();
    let mut w = WirePlay::new(3, &[2]); // node 2 omits
    for i in 1..3 {
        for &c in &universe {
            w.seed(i, c);
        }
    }
    w.start();
    w.run();
    for &c in &universe {
        assert!(w.nodes[0].has(c), "chunk {c} missing");
    }
    assert_eq!(w.nodes[0].deliveries(), m);
    assert_eq!(w.served[2], 0, "the omitter served nothing over the wire");
    w.nodes[0].check_invariants().unwrap();
}
