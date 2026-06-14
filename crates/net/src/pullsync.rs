//! The pull-sync **shell** over libp2p: drives the `wire` [`Session`] by
//! opening one short stream per [`Op`] (bee's `pkg/pullsync`, downstream
//! initiates), pumping the matching sync poller over it, and feeding the
//! result back. The protocol logic is the verified `wire` pollers; this module
//! only moves their bytes over real streams — the same split as `run_handshake`
//! for the handshake. All async lives here; the pollers stay sync.
//!
//! bee registers two stream handlers under `pullsync/1.4.0`: `cursors`
//! (`Syn → Ack`) and `pullsync` (`Get → Offer → Want → Delivery*`). The dialer
//! (downstream) opens both; the upstream only responds.

use futures::{AsyncReadExt, AsyncWriteExt};
use libp2p::{PeerId, StreamProtocol};
use libp2p_stream::Control;
use melissi_node::{Bin, Event};
use melissi_wire::adapter::{ClientOut, CursorsClient, FetchClient, OfferClient, TripleCodec};
use melissi_wire::session::{Op, Session};

/// bee's `cursors` stream (`pkg/p2p.NewSwarmStreamName(pullsync, 1.4.0, cursors)`).
pub const CURSORS_PROTOCOL: StreamProtocol = StreamProtocol::new("/swarm/pullsync/1.4.0/cursors");
/// bee's `pullsync` stream.
pub const PULLSYNC_PROTOCOL: StreamProtocol = StreamProtocol::new("/swarm/pullsync/1.4.0/pullsync");

const READ_CHUNK: usize = 8 * 1024;

/// Pump a sync client poller over an async stream: write its `Send`s, read for
/// its `Need`s, return the terminal `ClientOut`. `None` if the peer closed the
/// stream before the poller reached a terminal state (the shell-owned signal
/// the pollers turn into `Missed`).
async fn pump<S, F>(stream: &mut S, mut poll: F) -> Option<ClientOut>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
    F: FnMut(&[u8]) -> ClientOut,
{
    let mut buf = [0u8; READ_CHUNK];
    let mut input: Vec<u8> = Vec::new();
    loop {
        match poll(&input) {
            ClientOut::Send(bytes) => {
                input.clear();
                stream.write_all(&bytes).await.ok()?;
                let _ = stream.flush().await;
            }
            ClientOut::Need => {
                input.clear();
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => return None, // peer closed / errored
                    Ok(n) => input.extend_from_slice(&buf[..n]),
                }
            }
            terminal => return Some(terminal),
        }
    }
}

/// Open a fresh stream to `peer` under `proto`, retrying while the dial is
/// still pending (libp2p auto-dials on the first `open_stream`).
async fn open(ctrl: &mut Control, peer: PeerId, proto: StreamProtocol) -> Option<libp2p::Stream> {
    for _ in 0..100 {
        match ctrl.open_stream(peer, proto.clone()).await {
            Ok(s) => return Some(s),
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    None
}

/// Run one [`Op`] against `peer` over libp2p, returning the event to feed back
/// (or `None` if the stream could not be opened/driven).
async fn run_op<C: TripleCodec>(
    ctrl: &mut Control,
    peer: PeerId,
    op: Op,
    codec: &C,
) -> Option<Event> {
    match op {
        Op::Cursors { peer: pid } => {
            let mut s = open(ctrl, peer, CURSORS_PROTOCOL).await?;
            let mut client = CursorsClient::new();
            let term = pump(&mut s, |inp| client.poll(inp)).await?;
            match term {
                ClientOut::Cursors { cursors, .. } => {
                    Some(Event::CursorsResult { peer: pid, cursors })
                }
                _ => None,
            }
        }
        Op::Offer {
            peer: pid,
            bin,
            start,
        } => {
            let mut s = open(ctrl, peer, PULLSYNC_PROTOCOL).await?;
            let mut client = OfferClient::new(bin, start);
            let term = pump(&mut s, |inp| client.poll(codec, inp)).await?;
            match term {
                ClientOut::OfferDone { refs, topmost } => {
                    // close the advertisement politely: an all-zero Want tells
                    // the upstream to deliver nothing and FullClose.
                    let _ = s.write_all(&OfferClient::close_frame(refs.len())).await;
                    let _ = s.flush().await;
                    Some(Event::OfferResult {
                        peer: pid,
                        bin,
                        start,
                        refs,
                        topmost,
                    })
                }
                _ => None,
            }
        }
        Op::Fetch {
            peer: pid,
            bin,
            start,
            want,
        } => {
            let mut s = open(ctrl, peer, PULLSYNC_PROTOCOL).await?;
            let mut client = FetchClient::new(bin, start, want);
            let outcomes = match pump(&mut s, |inp| client.poll(codec, inp)).await {
                Some(ClientOut::FetchDone { outcomes }) => outcomes,
                // stream ended before every delivery: the shell closes it, and
                // unmet wants finalize as Missed (the adapter's shell signal).
                _ => match client.close() {
                    ClientOut::FetchDone { outcomes } => outcomes,
                    _ => Vec::new(),
                },
            };
            Some(Event::FetchResult {
                peer: pid,
                bin,
                outcomes,
            })
        }
    }
}

/// Drive a [`Session`] to quiescence against a single upstream `peer` over
/// libp2p: each network effect becomes a stream op, its result feeds back, and
/// the puller fills its reserve (the HIST drain). Returns once no effect
/// remains. The settled high-waters are left in the session for the caller to
/// persist ([`Session::take_settled`]).
pub async fn pull_from<C: TripleCodec>(
    ctrl: &mut Control,
    peer: PeerId,
    session: &mut Session,
    codec: &C,
) {
    while let Some(op) = session.next_op() {
        match run_op(ctrl, peer, op, codec).await {
            Some(ev) => session.feed(ev),
            None => break, // the connection dropped; stop this round
        }
    }
}

/// Fetch just the upstream's per-bin cursors (the `cursors` stream alone) —
/// the cheapest probe that a peer is reachable and serving pull-sync. Returns
/// the per-bin head BinIDs, or `None` on failure.
pub async fn get_cursors(
    ctrl: &mut Control,
    peer: PeerId,
) -> Option<Vec<(Bin, melissi_settlement::BinId)>> {
    let mut s = open(ctrl, peer, CURSORS_PROTOCOL).await?;
    let mut client = CursorsClient::new();
    match pump(&mut s, |inp| client.poll(inp)).await? {
        ClientOut::Cursors { cursors, .. } => Some(cursors),
        _ => None,
    }
}
