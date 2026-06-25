//! The node runtime — the operational embodiment of `Composition.tla`: assemble
//! the neighbourhood supply, then pull. Discover (hive) → select by proximity
//! (overlay) → connect (handshake) → pull (the `wire` `Session`). All async; the
//! verified drivers stay sync.
//!
//! The client side has two halves, meeting at the `Composition.tla` seam:
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
//!
//! And a server side — [`serve`] — that makes the node *reachable*: it answers
//! bee's dial-back handshake (the `Role::Responder` driver), without which bee
//! marks us unreachable and never gossips to or serves us. [`run`] spawns both,
//! so a single call is a complete, dial-back-ready node — the last code mile to
//! a live pull (the remaining requirement, a routable address, is deployment).

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

/// Grind a handshake nonce until our overlay lands within `bits` proximity of
/// `target` — i.e. inside that node's neighbourhood. Our ethereum address is
/// fixed by our key; only the nonce is free (`overlay = keccak(ethAddr ‖
/// networkID ‖ nonce)`), so placing our overlay is a small proof-of-work.
///
/// Why: bee gossips its neighbourhood peers (`kademlia.go`'s depth-gated
/// broadcast) only to peers it considers neighbours — at/below its storage
/// depth. A random overlay is in nobody's neighbourhood, so it is never a
/// gossip target. Grinding our overlay close to a real node is the precondition
/// for being one. Returns the best nonce found and the proximity it achieves
/// (which may be `< bits` if `max_tries` is exhausted — the caller decides if it
/// is deep enough).
pub fn grind_overlay_nonce(
    eth: &[u8; 20],
    network_id: u64,
    target: &[u8; 32],
    bits: u8,
    max_tries: u64,
) -> ([u8; 32], u8) {
    let mut best = ([0u8; 32], 0u8);
    for i in 0..max_tries {
        let mut nonce = [0u8; 32];
        nonce[..8].copy_from_slice(&i.to_le_bytes());
        let overlay = melissi_overlay::overlay_address(eth, network_id, &nonce);
        let po = melissi_overlay::proximity(&overlay, target);
        if po > best.1 {
            best = (nonce, po);
            if po >= bits {
                break;
            }
        }
    }
    best
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

/// The remote address each connected peer was observed at — populated from
/// `ConnectionEstablished`. The responder echoes a dialer's address back in its
/// `SynAck` (so the dialer learns its own observed address, which bee validates
/// against its own peer id), so the server side needs this.
pub type PeerAddrs =
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<PeerId, libp2p::Multiaddr>>>;

/// Drive a libp2p swarm: poll its events (so connections + incoming streams make
/// progress), dial addresses sent on `dials`, and record each peer's remote
/// address into `addrs`. Run on its own task; the `Control`s taken from the
/// swarm beforehand open/accept streams against it.
async fn drive_swarm(
    mut sw: libp2p::Swarm<libp2p_stream::Behaviour>,
    mut dials: tokio::sync::mpsc::UnboundedReceiver<libp2p::Multiaddr>,
    addrs: PeerAddrs,
) {
    use libp2p::futures::StreamExt;
    use libp2p::swarm::SwarmEvent;
    loop {
        tokio::select! {
            ev = sw.select_next_some() => match ev {
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                    if logging() {
                        let dir = if endpoint.is_listener() { "← inbound (dial-back?)" } else { "→ outbound" };
                        eprintln!("  {dir} connection: {peer_id} @ {}", endpoint.get_remote_address());
                    }
                    addrs.lock().unwrap().insert(peer_id, endpoint.get_remote_address().clone());
                }
                SwarmEvent::ConnectionClosed { peer_id, cause, .. } if logging() => {
                    eprintln!("  ✗ connection closed: {peer_id} ({cause:?})");
                }
                _ => {}
            },
            addr = dials.recv() => match addr {
                Some(a) => { let _ = sw.dial(a); }
                None => break,
            }
        }
    }
}

/// Verbose connection/dial-back logging, opt-in via `MELISSI_LOG` (so the
/// library stays silent under test, and a live run can watch reachability).
fn logging() -> bool {
    std::env::var("MELISSI_LOG").is_ok()
}

fn peer_of(addr: &libp2p::Multiaddr) -> Option<PeerId> {
    addr.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

/// The address we observed `peer` at, with its `/p2p/` — what the responder
/// echoes in its `SynAck.Syn` so the dialer learns its own observed address.
/// Falls back to a non-empty placeholder if unseen (an empty observed omits the
/// `Syn` field and the exchange fails; a melissi initiator ignores the content,
/// bee validates it — hence the real address when we have it).
fn observed_of(addrs: &PeerAddrs, peer: &PeerId) -> Vec<u8> {
    if let Some(a) = addrs.lock().unwrap().get(peer).cloned() {
        let mut a = a;
        if !a
            .iter()
            .any(|p| matches!(p, libp2p::multiaddr::Protocol::P2p(_)))
        {
            a.push(libp2p::multiaddr::Protocol::P2p(*peer));
        }
        return a.to_vec();
    }
    "/ip4/127.0.0.1/tcp/1634"
        .parse::<libp2p::Multiaddr>()
        .unwrap()
        .to_vec()
}

/// The **responder/server** side — what makes this node *reachable*. A new peer
/// (bee) dials us back to classify our reachability; if it cannot complete a
/// handshake against us we are marked unreachable and pruned, never gossiped to
/// or served. This long-running accept-loop answers inbound handshakes with our
/// verified [`Role::Responder`] driver — the dial-back proof. Spawn it; `addrs`
/// is the map [`drive_swarm`] fills (for the observed-underlay echo). It accepts
/// the handshake protocol only; the hive `peers` push is the *client* side's
/// (`run`) acceptor, so the two coexist on one swarm without contending.
///
/// Reachability beyond the dial-back also needs a routable advertised underlay
/// (a public address bee can reach), which is deployment, not code.
pub async fn serve(
    mut server: Control,
    mine: BzzAddress,
    network_id: u64,
    full_node: bool,
    addrs: PeerAddrs,
) {
    use libp2p::futures::StreamExt;
    let Ok(mut hs_in) = server.accept(HANDSHAKE_PROTOCOL) else {
        return; // another acceptor already holds the handshake protocol
    };
    while let Some((peer, mut s)) = hs_in.next().await {
        let mine = mine.clone();
        let observed = observed_of(&addrs, &peer);
        if logging() {
            eprintln!("  ← dial-back handshake opened by {peer}");
        }
        tokio::spawn(async move {
            // answer the dial-back handshake (the reachability proof).
            let ok = run_handshake(
                &mut s,
                Role::Responder,
                &mine,
                network_id,
                full_node,
                observed,
            )
            .await;
            if logging() {
                let mark = if ok.is_some() { "✓" } else { "✗" };
                eprintln!("  {mark} dial-back handshake from {peer} — we are reachable");
            }
        });
    }
}

/// Handshake `bootnode` once over a throwaway connection and return its overlay
/// address — so a node can grind its *own* overlay into the bootnode's
/// neighbourhood ([`grind_overlay_nonce`]) before presenting itself, the
/// precondition for receiving that node's neighbourhood gossip. Builds and
/// drives its own swarm; `None` if the handshake fails.
pub async fn learn_peer_overlay(
    bootnode: libp2p::Multiaddr,
    mine: &BzzAddress,
    network_id: u64,
    full_node: bool,
) -> Option<[u8; 32]> {
    use libp2p::futures::StreamExt;
    let mut sw = crate::swarm::build_swarm();
    let mut ctrl = sw.behaviour().new_control();
    let boot = peer_of(&bootnode)?;
    sw.dial(bootnode.clone()).ok()?;
    tokio::spawn(async move {
        loop {
            sw.select_next_some().await;
        }
    });
    let mut hs = open_stream(&mut ctrl, boot, HANDSHAKE_PROTOCOL).await?;
    let (_eth, overlay) = crate::transport::run_handshake_learn(
        &mut hs,
        Role::Initiator,
        mine,
        network_id,
        full_node,
        bootnode.to_vec(),
    )
    .await?;
    Some(overlay)
}

/// Our network identity + the tile that defines our reserve — what a [`run`]
/// presents to peers and uses to select neighbours.
pub struct Identity<'a> {
    pub bzz: &'a BzzAddress,
    pub network_id: u64,
    pub full_node: bool,
    pub neighbourhood: &'a Neighbourhood,
}

/// The whole node, end to end — the operational `Composition`, threaded. It
/// spawns the [`serve`] responder (so we are reachable — bee can dial us back),
/// then as a client: dial the bootnode and handshake it, receive its hive push
/// and SELECT the neighbours (our tile), dial + handshake each, and
/// `assemble_and_pull` to fill the reserve. The caller owns the clock (bound
/// this in a timeout); it returns when the puller quiesces or no neighbour is
/// reachable. `bootnode` carries a `/p2p/`. A reachable advertised underlay (a
/// public address bee can dial) is the one remaining requirement, and it is
/// deployment, not code.
pub async fn run<C: TripleCodec>(
    swarm: libp2p::Swarm<libp2p_stream::Behaviour>,
    bootnode: libp2p::Multiaddr,
    id: &Identity<'_>,
    session: &mut Session,
    codec: &C,
) {
    let (mine, network_id, full_node, nbhd) =
        (id.bzz, id.network_id, id.full_node, id.neighbourhood);
    let mut ctrl = swarm.behaviour().new_control();
    let serve_ctrl = swarm.behaviour().new_control();
    let Some(boot_peer) = peer_of(&bootnode) else {
        return;
    };
    let (dial_tx, dial_rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = dial_tx.send(bootnode.clone());
    let addrs: PeerAddrs = Default::default();
    tokio::spawn(drive_swarm(swarm, dial_rx, addrs.clone()));
    // be reachable: answer bee's dial-back handshake (the responder side), so we
    // are classified Public and kept/gossiped/served. Coexists with the client
    // acceptors below (handshake protocol vs the hive/pullsync we open).
    tokio::spawn(serve(
        serve_ctrl,
        mine.clone(),
        network_id,
        full_node,
        addrs.clone(),
    ));

    // 1. handshake the bootnode (bee serves discovery only to handshaked peers).
    if handshake_neighbour(
        &mut ctrl,
        boot_peer,
        mine,
        network_id,
        full_node,
        bootnode.to_vec(),
    )
    .await
    .is_none()
    {
        return;
    }
    if logging() {
        eprintln!("  ✓ handshaked bootnode; awaiting hive gossip…");
    }
    // 2. receive its hive push and keep the neighbours in our tile.
    let neighbours = accept_hive_push(&mut ctrl, network_id, nbhd).await;
    if logging() {
        eprintln!("  · hive push: {} neighbours in our tile", neighbours.len());
    }
    // 3. dial + handshake each neighbour; the connected ones are the supply.
    let mut connected: Vec<(NodePeerId, PeerId)> = Vec::new();
    let mut next: NodePeerId = 1;
    for n in &neighbours {
        let Ok(addr) = libp2p::Multiaddr::try_from(n.peer.underlay.clone()) else {
            continue;
        };
        let _ = dial_tx.send(addr.clone());
        if handshake_neighbour(
            &mut ctrl,
            n.libp2p,
            mine,
            network_id,
            full_node,
            addr.to_vec(),
        )
        .await
        .is_some()
        {
            connected.push((next, n.libp2p));
            next += 1;
        }
    }
    // 4. pull the reserve from the assembled neighbourhood.
    assemble_and_pull(&mut ctrl, &connected, session, codec).await;
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
        crate::swarm::build_swarm()
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

    fn bzz(secret: &[u8; 32]) -> BzzAddress {
        let underlay = "/ip4/127.0.0.1/tcp/1634"
            .parse::<Multiaddr>()
            .unwrap()
            .to_vec();
        BzzAddress::new(secret, &underlay, 1, [1u8; 32], 1_700_000_000, [0u8; 20]).unwrap()
    }

    /// The responder side: a node running [`serve`] accepts an INBOUND handshake
    /// (the dial-back that proves reachability to bee) and the dialer recovers
    /// its verified identity — our `Role::Responder` driver, now persistent and
    /// behind a real listener. This is what makes melissi dial-back-ready.
    #[tokio::test]
    async fn serving_node_accepts_inbound_handshake() {
        const NET: u64 = 1;
        // server S: listen, then run the accept-loop on its swarm.
        let mut s = node();
        let s_peer = *s.local_peer_id();
        let s_ctrl = s.behaviour().new_control();
        s.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let s_addr = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = s.select_next_some().await {
                break address;
            }
        };
        let (s_dial_tx, s_dial_rx) = tokio::sync::mpsc::unbounded_channel();
        let _keep = s_dial_tx; // keep the channel open so the driver keeps running
        let addrs: PeerAddrs = Default::default();
        tokio::spawn(drive_swarm(s, s_dial_rx, addrs.clone()));
        tokio::spawn(serve(s_ctrl, bzz(&[3u8; 32]), NET, true, addrs));

        // client C: dial S and handshake it as initiator.
        let mut c = node();
        let mut c_ctrl = c.behaviour().new_control();
        c.dial(s_addr.with_p2p(s_peer).unwrap()).unwrap();
        tokio::spawn(async move {
            loop {
                c.select_next_some().await;
            }
        });
        let observed = "/ip4/127.0.0.1/tcp/1634"
            .parse::<Multiaddr>()
            .unwrap()
            .to_vec();
        let eth = tokio::time::timeout(Duration::from_secs(15), async move {
            let mut hs = loop {
                match c_ctrl.open_stream(s_peer, HANDSHAKE_PROTOCOL).await {
                    Ok(s) => break s,
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            };
            run_handshake(
                &mut hs,
                Role::Initiator,
                &bzz(&[4u8; 32]),
                NET,
                true,
                observed,
            )
            .await
        })
        .await
        .expect("handshake timed out");

        // C recovered S's verified identity — the inbound handshake was served.
        assert_eq!(
            eth,
            Some(melissi_crypto::public_eth_address(&[3u8; 32]).unwrap())
        );
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

    /// Live reality check: handshake the testnet bootnode, then wait for its hive
    /// `peers` push and report what a **random-overlay** light peer is given —
    /// how many peers, their proximity to us, and how many land in our tile.
    /// `#[ignore]`d (network + external peer). Run:
    ///
    /// ```text
    /// cargo test -p melissi-net --features libp2p live_testnet_discovery -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn live_testnet_discovery() {
        const TESTNET: u64 = 10;
        let addr = crate::dnsaddr::tcp_bootnodes(TESTNET)
            .await
            .expect("resolve testnet bootnode")
            .into_iter()
            .next()
            .unwrap();
        let boot = peer_of(&addr).unwrap();

        let secret = [0x5au8; 32];
        let our_underlay = "/ip4/127.0.0.1/tcp/1634"
            .parse::<Multiaddr>()
            .unwrap()
            .to_vec();
        let mine = BzzAddress::new(
            &secret,
            &our_underlay,
            TESTNET,
            [0x11; 32],
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap();
        let eth = melissi_crypto::public_eth_address(&secret).unwrap();
        let our_overlay = overlay_address(&eth, TESTNET, &[0x11; 32]);
        let radius: u8 = 8; // a representative reserve depth for the probe

        let mut sw = node();
        let mut ctrl = sw.behaviour().new_control();
        let mut hive_in = ctrl.accept(PEERS_PROTOCOL).unwrap();
        sw.dial(addr.clone()).unwrap();
        tokio::spawn(async move {
            loop {
                sw.select_next_some().await;
            }
        });

        let out = tokio::time::timeout(Duration::from_secs(30), async move {
            let mut hs = loop {
                match ctrl.open_stream(boot, HANDSHAKE_PROTOCOL).await {
                    Ok(s) => break s,
                    Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                }
            };
            let id = run_handshake(
                &mut hs,
                Role::Initiator,
                &mine,
                TESTNET,
                true, // present as a full node — bee may gossip to full nodes
                addr.to_vec(),
            )
            .await
            .expect("handshake verified");
            let hx: String = id.iter().map(|b| format!("{b:02x}")).collect();
            eprintln!("✓ handshake with testnet bee 0x{hx}; awaiting hive push…");
            // wait for bee to gossip its peers to us
            let (_p, mut stream) = hive_in.next().await?;
            Some(receive_peers(&mut stream, TESTNET).await)
        })
        .await;

        match out {
            Ok(Some(peers)) => {
                let proximities: Vec<u8> = peers
                    .iter()
                    .map(|p| melissi_overlay::proximity(&our_overlay, &p.overlay))
                    .collect();
                let in_tile = proximities.iter().filter(|&&po| po >= radius).count();
                eprintln!(
                    "✓ hive push: {} peers; proximities {:?}; {} in our tile (radius {})",
                    peers.len(),
                    proximities,
                    in_tile,
                    radius,
                );
                eprintln!(
                    "(a random overlay rarely shares a deep prefix with sparse testnet nodes — \
                     expect few/no neighbours; a real pull needs an overlay placed in a populated tile)"
                );
            }
            Ok(None) => eprintln!("· bee admitted the handshake but sent no hive push this run"),
            Err(_) => {
                eprintln!("· no hive push within 30s (bee declined to gossip to a light peer)")
            }
        }
    }

    /// The nonce grinder lands our overlay in a target's neighbourhood. Offline,
    /// deterministic: our eth is fixed by the key, the target is arbitrary, and
    /// grinding to a modest depth is a few thousand keccaks.
    #[test]
    fn grind_places_overlay_in_target_neighbourhood() {
        let eth = melissi_crypto::public_eth_address(&[0x5au8; 32]).unwrap();
        let target = overlay_address(
            &melissi_crypto::public_eth_address(&[0x99u8; 32]).unwrap(),
            10,
            &[0x77; 32],
        );
        let (nonce, po) = grind_overlay_nonce(&eth, 10, &target, 12, 5_000_000);
        assert!(po >= 12, "ground to proximity {po}, want ≥ 12");
        // the returned nonce actually reproduces the claimed proximity.
        let got = overlay_address(&eth, 10, &nonce);
        assert_eq!(melissi_overlay::proximity(&got, &target), po);
    }

    /// The decisive live experiment: present to the testnet bootnode as a **full
    /// node whose overlay is ground into the bootnode's own neighbourhood**, then
    /// watch which of three things bee does — (a) gossips its neighbourhood peers
    /// to us (`kademlia.go`'s depth-gated broadcast fires; discovery works, and
    /// we try a cursors pull from the closest), (b) **closes the connection**
    /// (the reachability prune — our `127.0.0.1` underlay is undialable, so bee
    /// drops us before gossiping; reachability is the wall, not overlay/patience),
    /// or (c) keeps us but stays silent past the ~15-minute timer.
    ///
    /// This isolates the variable the 30-second random-overlay probe could not:
    /// being a *neighbour* removes "not in anyone's neighbourhood" as the reason
    /// for silence. `#[ignore]`d (network + a ~16-minute wait). Run:
    ///
    /// ```text
    /// cargo test -p melissi-net --features libp2p live_testnet_neighbour_pull -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore]
    async fn live_testnet_neighbour_pull() {
        use crate::transport::run_handshake_learn;

        const TESTNET: u64 = 10;
        let addr = crate::dnsaddr::tcp_bootnodes(TESTNET)
            .await
            .expect("resolve testnet bootnode")
            .into_iter()
            .next()
            .unwrap();
        let boot = peer_of(&addr).unwrap();
        let boot_underlay = addr.to_vec();
        let secret = [0x5au8; 32];
        let eth = melissi_crypto::public_eth_address(&secret).unwrap();
        let our_underlay = "/ip4/127.0.0.1/tcp/1634"
            .parse::<Multiaddr>()
            .unwrap()
            .to_vec();

        // --- phase 1: handshake once to learn the bootnode's overlay -----------
        let bee_overlay = {
            let mut sw = node();
            let mut ctrl = sw.behaviour().new_control();
            sw.dial(addr.clone()).unwrap();
            tokio::spawn(async move {
                loop {
                    sw.select_next_some().await;
                }
            });
            let probe = BzzAddress::new(
                &secret,
                &our_underlay,
                TESTNET,
                [0x11; 32],
                1_700_000_000,
                [0u8; 20],
            )
            .unwrap();
            let observed = boot_underlay.clone();
            let learn = tokio::time::timeout(Duration::from_secs(30), async move {
                let mut hs = loop {
                    match ctrl.open_stream(boot, HANDSHAKE_PROTOCOL).await {
                        Ok(s) => break s,
                        Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                    }
                };
                run_handshake_learn(&mut hs, Role::Initiator, &probe, TESTNET, true, observed).await
            })
            .await
            .expect("phase-1 handshake timed out");
            let (id, ov) = learn.expect("phase-1 handshake verified");
            let hx: String = id.iter().map(|b| format!("{b:02x}")).collect();
            let ox: String = ov.iter().take(6).map(|b| format!("{b:02x}")).collect();
            eprintln!("✓ phase 1: bootnode 0x{hx}, overlay 0x{ox}…");
            ov
        };

        // --- phase 2: grind our overlay into the bootnode's neighbourhood ------
        let (nonce, po) = grind_overlay_nonce(&eth, TESTNET, &bee_overlay, 18, 50_000_000);
        eprintln!("✓ phase 2: ground our overlay to proximity {po} of the bootnode");
        let mine = BzzAddress::new(
            &secret,
            &our_underlay,
            TESTNET,
            nonce,
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap();

        // --- phase 3: reconnect as a deep neighbour, watch ~16m -----------------
        let mut sw = node();
        let mut ctrl = sw.behaviour().new_control();
        let mut hive_in = ctrl.accept(PEERS_PROTOCOL).unwrap();
        let (closed_tx, mut closed_rx) = tokio::sync::mpsc::channel::<String>(4);
        sw.dial(addr.clone()).unwrap();
        tokio::spawn(async move {
            loop {
                if let SwarmEvent::ConnectionClosed { peer_id, cause, .. } =
                    sw.select_next_some().await
                {
                    if peer_id == boot {
                        let _ = closed_tx.send(format!("{cause:?}")).await;
                    }
                }
            }
        });
        let mut hs = loop {
            match ctrl.open_stream(boot, HANDSHAKE_PROTOCOL).await {
                Ok(s) => break s,
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        };
        run_handshake(
            &mut hs,
            Role::Initiator,
            &mine,
            TESTNET,
            true,
            boot_underlay.clone(),
        )
        .await
        .expect("phase-3 handshake verified");
        eprintln!("✓ phase 3: handshook as a deep neighbour (proximity {po}); watching ≤16m for gossip / prune…");

        tokio::select! {
            push = hive_in.next() => {
                let Some((_p, mut stream)) = push else {
                    eprintln!("· hive stream opened then closed with no peers");
                    return;
                };
                let discovered = receive_peers(&mut stream, TESTNET).await;
                let nbhd = Neighbourhood::new(overlay_address(&eth, TESTNET, &nonce), po.min(8));
                let mut tile = select_neighbours(&nbhd, &discovered);
                tile.sort_by(|a, b| b.proximity.cmp(&a.proximity));
                eprintln!("✓ GOSSIP: bee pushed {} peers; {} in our tile", discovered.len(), tile.len());
                if let Some(best) = tile.first() {
                    eprintln!("  closest neighbour proximity {} — dialing for cursors…", best.proximity);
                    sw_dial_and_cursors(&mut ctrl, best, &mine, TESTNET, &our_underlay).await;
                } else {
                    eprintln!("  (gossip arrived but no peer landed in our radius this push)");
                }
            }
            closed = closed_rx.recv() => {
                eprintln!("· bee CLOSED the connection ({}) — the reachability prune: an undialable underlay is dropped before the gossip timer, even as a deep neighbour. Reachability is the wall.", closed.unwrap_or_default());
            }
            _ = tokio::time::sleep(Duration::from_secs(16 * 60)) => {
                eprintln!("· 16m: still connected, no gossip. A deep neighbour bee keeps but does not broadcast to — depth deeper than ground, or gossip otherwise gated.");
            }
        }
    }

    // Dial a discovered neighbour and pull its cursors — a live liveness check on
    // a real storage neighbour (the channel a full pull then drains).
    async fn sw_dial_and_cursors(
        ctrl: &mut Control,
        n: &Neighbour,
        mine: &BzzAddress,
        network_id: u64,
        observed: &[u8],
    ) {
        let mut hs = match ctrl.open_stream(n.libp2p, HANDSHAKE_PROTOCOL).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  · could not open handshake to neighbour: {e:?}");
                return;
            }
        };
        if run_handshake(
            &mut hs,
            Role::Initiator,
            mine,
            network_id,
            true,
            observed.to_vec(),
        )
        .await
        .is_none()
        {
            eprintln!("  · neighbour handshake failed");
            return;
        }
        match crate::pullsync::get_cursors(ctrl, n.libp2p).await {
            Some(cursors) => eprintln!(
                "  ✓ PULLED from a storage neighbour: {} cursors (live reserve channel open)",
                cursors.len()
            ),
            None => eprintln!("  · neighbour negotiated cursors but returned none"),
        }
    }
}
