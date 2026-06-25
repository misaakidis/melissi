//! The node runtime — the operational embodiment of `Composition.tla`: assemble
//! the neighbourhood supply, then pull. Discover (hive) → select by proximity
//! (overlay) → connect (handshake) → pull (the `wire` `Session`). All async; the
//! verified drivers stay sync.
//!
//! Two halves, meeting at the `Composition.tla` seam:
//!   - **discover + select** — [`accept_hive_push`] receives a hive `peers` push
//!     and [`select_neighbours`] keeps the peers whose proximity puts them in our
//!     reserve's tile (`overlay::Neighbourhood::is_neighbour`, the §4 locality
//!     cut). Connecting all of them is the supply the `neighbourhood` crate
//!     proves complete (`SupplyComplete`).
//!   - **connect + pull** — [`handshake_neighbour`] handshakes each, then
//!     [`assemble_and_pull`] drives the `wire` `Session` across the whole
//!     neighbourhood, routing each scheduled op to its peer's connection and
//!     failing over when one declines. The reserve fills from what the
//!     neighbourhood collectively holds — `Completeness`, operationally.

use crate::handshake::Role;
use crate::hive::{receive_peers, DiscoveredPeer, PEERS_PROTOCOL};
use crate::transport::{run_handshake, HANDSHAKE_PROTOCOL};
use crate::BzzAddress;
use libp2p::PeerId;
use libp2p_stream::Control;
use melissi_node::{Event, Outcome};
use melissi_overlay::Neighbourhood;
use melissi_wire::adapter::TripleCodec;
use melissi_wire::session::{Op, Session};
use std::time::Duration;

/// The node's own peer id for a neighbour (the `Session`/`Node` index a peer by
/// a small id; the runtime maps it to the libp2p [`PeerId`] to dial).
pub type NodePeerId = melissi_types::PeerId;

/// A discovered neighbour: the hive peer plus the libp2p peer id to dial it by
/// (parsed from its underlay multiaddr).
#[derive(Clone, Debug)]
pub struct Neighbour {
    pub peer: DiscoveredPeer,
    pub libp2p: PeerId,
    pub proximity: u8,
}

/// Of a set of discovered peers, the ones in our neighbourhood (proximity ≥
/// radius) — the supply tile. Connecting all of these is what the `neighbourhood`
/// crate proves complete; peers outside the tile hold none of our reserve (§4
/// locality lemma) and are dropped. Peers whose underlay carries no `/p2p/` id
/// are skipped (we cannot dial them).
pub fn select_neighbours(nbhd: &Neighbourhood, discovered: &[DiscoveredPeer]) -> Vec<Neighbour> {
    discovered
        .iter()
        .filter(|p| nbhd.is_neighbour(&p.overlay))
        .filter_map(|p| {
            libp2p_peer_of(&p.underlay).map(|libp2p| Neighbour {
                peer: p.clone(),
                libp2p,
                proximity: melissi_overlay::proximity(&nbhd.overlay, &p.overlay),
            })
        })
        .collect()
}

/// Extract the libp2p peer id from a serialised multiaddr underlay (the `/p2p/`
/// component), if present.
fn libp2p_peer_of(underlay: &[u8]) -> Option<PeerId> {
    let addr = libp2p::Multiaddr::try_from(underlay.to_vec()).ok()?;
    addr.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

/// Accept one hive `peers` push from a connected peer and return the neighbours
/// it reveals (verified + in our tile). bee broadcasts peers to a peer it
/// admits; `None` if no push arrives (we wait on `ctrl.accept`).
pub async fn accept_hive_push(
    ctrl: &mut Control,
    network_id: u64,
    nbhd: &Neighbourhood,
) -> Vec<Neighbour> {
    use libp2p::futures::StreamExt;
    let Ok(mut incoming) = ctrl.accept(PEERS_PROTOCOL) else {
        return Vec::new();
    };
    let Some((_peer, mut stream)) = incoming.next().await else {
        return Vec::new();
    };
    let discovered = receive_peers(&mut stream, network_id).await;
    select_neighbours(nbhd, &discovered)
}

/// Open the handshake stream to a (dialed) neighbour and run bee's handshake as
/// initiator. Returns the neighbour's verified blockchain address, or `None`.
pub async fn handshake_neighbour(
    ctrl: &mut Control,
    peer: PeerId,
    mine: &BzzAddress,
    network_id: u64,
    full_node: bool,
    observed: Vec<u8>,
) -> Option<[u8; 20]> {
    let mut s = open_stream(ctrl, peer, HANDSHAKE_PROTOCOL).await?;
    run_handshake(
        &mut s,
        Role::Initiator,
        mine,
        network_id,
        full_node,
        observed,
    )
    .await
}

async fn open_stream(
    ctrl: &mut Control,
    peer: PeerId,
    proto: libp2p::StreamProtocol,
) -> Option<libp2p::Stream> {
    for _ in 0..100 {
        match ctrl.open_stream(peer, proto.clone()).await {
            Ok(s) => return Some(s),
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    None
}

/// The empty/Missed result for an op that could not be run — the shell-owned
/// failure signal (a dropped/declining peer). The node treats it as no supply
/// from that peer and fails over to the others (an empty cursor set → no offers;
/// Missed wants → reschedule elsewhere).
fn failure_event(op: &Op) -> Event {
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

/// Drive the `Session` to quiescence against the assembled neighbourhood — the
/// connect-pull half. Each scheduled [`Op`] is routed to its peer's libp2p
/// connection (the `(NodePeerId, PeerId)` map); a failed op feeds the node a
/// Missed/empty result so it fails over across the supply. The neighbours must
/// already be dialed + handshaked. Returns once the puller quiesces (its reserve
/// is filled from what the neighbourhood holds).
pub async fn assemble_and_pull<C: TripleCodec>(
    ctrl: &mut Control,
    neighbours: &[(NodePeerId, PeerId)],
    session: &mut Session,
    codec: &C,
) {
    for (mid, _) in neighbours {
        session.add_peer(*mid);
    }
    let mut guard: u64 = 0;
    while let Some(op) = session.next_op() {
        guard += 1;
        if guard > 1_000_000 {
            break; // safety: a non-converging round (should not happen with real supply)
        }
        let target = neighbours
            .iter()
            .find(|(m, _)| *m == op.peer())
            .map(|(_, p)| *p);
        let result = match target {
            Some(p) => crate::pullsync::run_op(ctrl, p, op.clone(), codec).await,
            None => None,
        };
        match result {
            Some(ev) => session.feed(ev),
            // a failed op. An Offer that found an empty range BLOCKS (the standing
            // live subscription) — DROP it: feeding an empty offer would re-arm it
            // and spin (the session_play lesson). Cursors/Fetch failures feed
            // Missed/empty so the node treats that peer as no supply and fails over.
            None => {
                if !matches!(op, Op::Offer { .. }) {
                    session.feed(failure_event(&op));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::DiscoveredPeer;
    use melissi_overlay::overlay_address;

    // a discovered peer at a chosen overlay, with a dialable underlay carrying a
    // /p2p/ id (so select_neighbours keeps it).
    fn peer_at(overlay: [u8; 32]) -> DiscoveredPeer {
        // a syntactically valid multiaddr with a /p2p/ — the id is arbitrary here
        let underlay = "/ip4/1.2.3.4/tcp/1634/p2p/QmZsYCbkUXWpfR34PmUwMJvHwJtGfbcMMoAp1G2EydkpRA"
            .parse::<libp2p::Multiaddr>()
            .unwrap()
            .to_vec();
        DiscoveredPeer {
            overlay,
            underlay,
            eth: [0u8; 20],
        }
    }

    // --- connect-pull over libp2p: the operational Composition ----------------

    use libp2p::futures::StreamExt;
    use libp2p::swarm::SwarmEvent;
    use libp2p::{Multiaddr, Swarm};
    use melissi_machine::Config;
    use melissi_node::{Bin, Node};
    use melissi_settlement::BinId;
    use melissi_types::Triple;
    use melissi_wire::adapter::{CursorsServer, ServeReserve, ServerOut, ServerStream};
    use melissi_wire::codec::MintedCodec;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    const RADIUS: Bin = 1;
    const NBINS: u8 = 2;
    fn bin_of(c: Triple) -> Bin {
        RADIUS + (c.address[31] % NBINS)
    }

    fn node() -> Swarm<libp2p_stream::Behaviour> {
        libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .unwrap()
            .with_behaviour(|_| libp2p_stream::Behaviour::new())
            .unwrap()
            .build()
    }

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

    // serve one storage neighbour: accept cursors + pullsync, answer from `reserve`.
    // Awaits its own listen address, then drives the accept loop on a task.
    async fn spawn_server(reserve: Arc<Reserve>, codec: Arc<MintedCodec>) -> (PeerId, Multiaddr) {
        let mut sw = node();
        let peer = *sw.local_peer_id();
        let mut ctrl = sw.behaviour().new_control();
        let mut cur_in = ctrl.accept(crate::pullsync::CURSORS_PROTOCOL).unwrap();
        let mut pull_in = ctrl.accept(crate::pullsync::PULLSYNC_PROTOCOL).unwrap();
        sw.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let addr = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = sw.select_next_some().await {
                break address;
            }
        };
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sw.select_next_some() => {}
                    Some((_p, mut s)) = cur_in.next() => {
                        let r = reserve.clone();
                        tokio::spawn(async move { serve_cursors(&mut s, &r).await; });
                    }
                    Some((_p, mut s)) = pull_in.next() => {
                        let (r, c) = (reserve.clone(), codec.clone());
                        tokio::spawn(async move { serve_pullsync(&mut s, &r, &c).await; });
                    }
                }
            }
        });
        (peer, addr)
    }

    async fn serve_cursors<S: futures::AsyncReadExt + futures::AsyncWriteExt + Unpin>(
        s: &mut S,
        reserve: &Reserve,
    ) {
        let mut buf = [0u8; 4096];
        let mut input = Vec::new();
        loop {
            if let Some(ack) = CursorsServer::respond(reserve, &input) {
                let _ = s.write_all(&ack).await;
                let _ = s.flush().await;
                return;
            }
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => input.extend_from_slice(&buf[..n]),
            }
        }
    }

    async fn serve_pullsync<S: futures::AsyncReadExt + futures::AsyncWriteExt + Unpin>(
        s: &mut S,
        reserve: &Reserve,
        codec: &MintedCodec,
    ) {
        let mut stream = ServerStream::new();
        let mut buf = [0u8; 8192];
        let mut input: Vec<u8> = Vec::new();
        loop {
            match stream.poll(codec, reserve, &input) {
                ServerOut::Send(b) => {
                    input.clear();
                    if s.write_all(&b).await.is_err() {
                        return;
                    }
                    let _ = s.flush().await;
                }
                ServerOut::Need => {
                    input.clear();
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => input.extend_from_slice(&buf[..n]),
                    }
                }
                ServerOut::Done | ServerOut::Blocked { .. } => return,
            }
        }
    }

    /// The operational Composition: the reserve is SPREAD across two neighbours
    /// (one holds bin 1, the other bin 2 — no single neighbour has it all). The
    /// client assembles BOTH and pulls; routing each op to the right peer, it
    /// fills its whole reserve. Connecting only one would miss the other's bin —
    /// the single-source dependency, here over real libp2p streams.
    #[tokio::test]
    async fn assembles_two_neighbours_and_fills_the_reserve() {
        let mut mint = MintedCodec::new([1u8; 32], 0);
        let m: u32 = 8;
        let universe: Vec<Triple> = (0..m)
            .map(|n| mint.mint(&n.to_be_bytes(), n as u64, 0))
            .collect();
        // the minting codec holds the chunk payloads, so the SERVERS serve with
        // it; the client validates deliveries from the bytes with its own.
        let serve_codec = Arc::new(mint);
        let client_codec = MintedCodec::new([1u8; 32], 0);

        // spread by bin: neighbour A serves bin 1, neighbour B serves bin 2.
        let mut ra = Reserve::default();
        let mut rb = Reserve::default();
        for &c in &universe {
            if bin_of(c) == 1 {
                ra.store(c);
            } else {
                rb.store(c);
            }
        }
        assert!(
            !ra.index.is_empty() && !rb.index.is_empty(),
            "both bins populated"
        );
        let (a_peer, a_addr) = spawn_server(Arc::new(ra), serve_codec.clone()).await;
        let (b_peer, b_addr) = spawn_server(Arc::new(rb), serve_codec.clone()).await;

        // client: dial both neighbours, then assemble_and_pull.
        let mut cl = node();
        let mut ctrl = cl.behaviour().new_control();
        cl.dial(a_addr.with_p2p(a_peer).unwrap()).unwrap();
        cl.dial(b_addr.with_p2p(b_peer).unwrap()).unwrap();
        tokio::spawn(async move {
            loop {
                cl.select_next_some().await;
            }
        });

        let mut session = Session::new(Node::new(Config::PRODUCTION, RADIUS));
        let neighbours = [(1u8, a_peer), (2u8, b_peer)];
        tokio::time::timeout(
            Duration::from_secs(20),
            assemble_and_pull(&mut ctrl, &neighbours, &mut session, &client_codec),
        )
        .await
        .expect("pull timed out");

        for &c in &universe {
            assert!(
                session.node().has(c),
                "chunk {c:?} missing — supply not assembled"
            );
        }
        assert_eq!(
            session.node().deficit(),
            0,
            "reserve filled from the assembled neighbourhood"
        );
        assert_eq!(
            session.node().deliveries(),
            m,
            "exactly-once across both neighbours"
        );
        session.node().check_invariants().unwrap();
    }

    /// Selection keeps the peers in our tile (proximity ≥ radius) and drops the
    /// far ones — the §4 locality cut, on real overlays.
    #[test]
    fn selects_only_the_neighbourhood() {
        // our overlay; radius 1 means a neighbour must share the top bit.
        let ours = overlay_address(&[7u8; 20], 1, &[9u8; 32]);
        let nbhd = Neighbourhood::new(ours, 1);

        // a near peer: differs only in the last bit → very close (proximity ≥ radius)
        let mut near = ours;
        near[31] ^= 0x01;
        // a far peer: flip the top bit → proximity 0 < radius 1
        let mut far = ours;
        far[0] ^= 0x80;

        let got = select_neighbours(&nbhd, &[peer_at(near), peer_at(far)]);
        assert_eq!(got.len(), 1, "only the near peer is a neighbour");
        assert_eq!(got[0].peer.overlay, near);
        assert!(got[0].proximity >= 1);
    }
}
