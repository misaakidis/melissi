//! The [`Session`] seam, end to end: a single puller, wrapped in `Session`,
//! syncs from a serving reserve over the REAL bee wire (cursors stream +
//! pullsync streams, protobuf frames, positional bitvector) until its reserve
//! fills. This is the composition the libp2p shell will run unchanged — here
//! the "shell" is an in-memory pump that opens a fresh stream per [`Op`] and
//! drives the matching `adapter` poller against the server pollers.
//!
//! `wireplay.rs` proves the multi-node floors with a bespoke loop; this proves
//! the reusable `Session` API drives the same wire to convergence.

use melissi_machine::Config;
use melissi_node::{Bin, Event, Node};
use melissi_settlement::BinId;
use melissi_types::{PeerId, Triple};
use melissi_wire::adapter::*;
use melissi_wire::codec::MintedCodec;
use melissi_wire::session::{Op, Session};
use std::collections::{BTreeMap, BTreeSet};

const NBINS: u8 = 2;
const RADIUS: Bin = 1;

fn bin_of(c: Triple) -> Bin {
    RADIUS + (c.address[31] % NBINS)
}

/// A serving reserve (per-bin append log), as in `wireplay`.
#[derive(Default)]
struct Reserve {
    bins: BTreeMap<Bin, BTreeMap<BinId, Triple>>,
    index: BTreeSet<Triple>,
}

impl Reserve {
    fn store(&mut self, c: Triple) {
        if !self.index.insert(c) {
            return;
        }
        let bin = bin_of(c);
        let head = self
            .bins
            .get(&bin)
            .and_then(|m| m.keys().last().copied())
            .unwrap_or(0);
        self.bins.entry(bin).or_default().insert(head + 1, c);
    }
}

impl ServeReserve for Reserve {
    fn collect(&self, bin: Bin, start: BinId, limit: usize) -> (Vec<(BinId, Triple)>, BinId) {
        let head = self
            .bins
            .get(&bin)
            .and_then(|m| m.keys().last().copied())
            .unwrap_or(0);
        let entries: Vec<(BinId, Triple)> = self
            .bins
            .get(&bin)
            .map(|m| {
                m.range(start..)
                    .map(|(&b, &c)| (b, c))
                    .take(limit)
                    .collect()
            })
            .unwrap_or_default();
        let topmost = entries
            .last()
            .map(|&(b, _)| b)
            .unwrap_or(head)
            .max(start.saturating_sub(1));
        (entries, topmost.max(head.min(start.saturating_sub(1))))
    }
    fn has(&self, c: Triple) -> bool {
        self.index.contains(&c)
    }
    fn cursors(&self) -> Vec<u64> {
        (0..RADIUS + NBINS)
            .map(|b| {
                self.bins
                    .get(&b)
                    .and_then(|m| m.keys().last().copied())
                    .unwrap_or(0)
            })
            .collect()
    }
    fn epoch(&self) -> u64 {
        1
    }
}

/// The in-memory shell: open a fresh stream per op, pump the client poller
/// against the server pollers, return the typed result to the session.
fn run_op(op: Op, server: &Reserve, codec: &MintedCodec, session: &mut Session) {
    match op {
        Op::Cursors { peer } => {
            let mut client = CursorsClient::new();
            let syn = match client.poll(&[]) {
                ClientOut::Send(b) => b,
                _ => unreachable!("cursors sends Syn first"),
            };
            let ack = CursorsServer::respond(server, &syn).expect("server answers Syn");
            match client.poll(&ack) {
                ClientOut::Cursors { cursors, .. } => {
                    session.feed(Event::CursorsResult { peer, cursors })
                }
                _ => panic!("cursors did not complete"),
            }
        }
        Op::Offer { peer, bin, start } => {
            let mut client = OfferClient::new(bin, start);
            let mut stream = ServerStream::new();
            let get = match client.poll(codec, &[]) {
                ClientOut::Send(b) => b,
                _ => unreachable!(),
            };
            match stream.poll(codec, server, &get) {
                ServerOut::Send(offer) => match client.poll(codec, &offer) {
                    ClientOut::OfferDone { refs, topmost } => session.feed(Event::OfferResult {
                        peer,
                        bin,
                        start,
                        refs,
                        topmost,
                    }),
                    _ => panic!("offer truncated"),
                },
                // empty range: the server BLOCKS — the standing live
                // subscription. Drop it (feed nothing): the offer stays open,
                // no result re-triggers it, and the puller quiesces once HIST
                // is drained. Feeding a synthetic empty offer would re-arm the
                // subscription and spin.
                ServerOut::Blocked { .. } => {}
                _ => unreachable!(),
            }
        }
        Op::Fetch {
            peer,
            bin,
            start,
            want,
        } => {
            let mut client = FetchClient::new(bin, start, want);
            let mut stream = ServerStream::new();
            // Get -> Offer -> Want -> Delivery*, the wire's explicit alternation
            let get = match client.poll(codec, &[]) {
                ClientOut::Send(b) => b,
                ClientOut::FetchDone { outcomes } => {
                    return session.feed(Event::FetchResult {
                        peer,
                        bin,
                        outcomes,
                    })
                }
                _ => unreachable!(),
            };
            let offer = match stream.poll(codec, server, &get) {
                ServerOut::Send(b) => b,
                _ => Vec::new(),
            };
            let want = match client.poll(codec, &offer) {
                ClientOut::Send(b) => b,
                ClientOut::FetchDone { outcomes } => {
                    return session.feed(Event::FetchResult {
                        peer,
                        bin,
                        outcomes,
                    })
                }
                ClientOut::Need => {
                    return session.feed(Event::FetchResult {
                        peer,
                        bin,
                        outcomes: Vec::new(),
                    })
                }
                _ => unreachable!(),
            };
            let deliveries = match stream.poll(codec, server, &want) {
                ServerOut::Send(b) => b,
                _ => Vec::new(),
            };
            let outcomes = match client.poll(codec, &deliveries) {
                ClientOut::FetchDone { outcomes } => outcomes,
                _ => match client.close() {
                    ClientOut::FetchDone { outcomes } => outcomes,
                    _ => unreachable!("close always finishes"),
                },
            };
            session.feed(Event::FetchResult {
                peer,
                bin,
                outcomes,
            });
        }
    }
}

/// Cold start through `Session`: an empty puller fills its reserve from one
/// full server over the real wire, fetching each chunk exactly once.
#[test]
fn session_converges_over_the_wire() {
    let mut codec = MintedCodec::new([1u8; 32], 0);
    let m: u32 = 12;
    let universe: Vec<Triple> = (0..m)
        .map(|n| codec.mint(&n.to_be_bytes(), n as u64, 0))
        .collect();

    let mut server = Reserve::default();
    for &c in &universe {
        server.store(c);
    }

    const SERVER: PeerId = 1;
    let mut session = Session::new(Node::new(Config::PRODUCTION, RADIUS));
    session.add_peer(SERVER);

    let mut guard = 0;
    while let Some(op) = session.next_op() {
        guard += 1;
        assert!(guard < 1_000_000, "session did not converge");
        run_op(op, &server, &codec, &mut session);
    }

    for &c in &universe {
        assert!(
            session.node().has(c),
            "chunk {c:?} missing after the session"
        );
    }
    assert_eq!(session.node().deficit(), 0);
    assert_eq!(session.node().deliveries(), m, "exactly-once over the wire");
    assert!(!session.node().conflict());
    session.node().check_invariants().unwrap();
}

/// The settled high-waters surface as durable transitions a persistent shell
/// would write (settle-before-forget). After convergence each synced bin has
/// advanced its `(peer, bin)` high-water.
#[test]
fn session_surfaces_settled_high_waters() {
    let mut codec = MintedCodec::new([2u8; 32], 0);
    let universe: Vec<Triple> = (0..6u32)
        .map(|n| codec.mint(&n.to_be_bytes(), n as u64, 0))
        .collect();
    let mut server = Reserve::default();
    for &c in &universe {
        server.store(c);
    }

    const SERVER: PeerId = 1;
    let mut session = Session::new(Node::new(Config::PRODUCTION, RADIUS));
    session.add_peer(SERVER);

    let mut any_delivered = false;
    while let Some(op) = session.next_op() {
        if let Op::Fetch { .. } = &op {
            any_delivered = true;
        }
        run_op(op, &server, &codec, &mut session);
    }
    assert!(any_delivered, "the sync should have fetched something");
    let settled = session.take_settled();
    assert!(
        settled.iter().any(|&(p, _, upto)| p == SERVER && upto > 0),
        "a (peer,bin) high-water should have settled: {settled:?}"
    );
    // drained: a second take is empty (idempotent)
    assert!(session.take_settled().is_empty());
}
