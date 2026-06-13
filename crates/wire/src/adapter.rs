//! The bee-wire adapter: maps the core's clean semantics (want-by-reference,
//! per-triple outcomes, range-covering offers) onto the legacy coupling —
//! the `Want` bitvector indexes positionally into the offer ON THE SAME
//! stream, so `Fetch` re-offers (one extra `Get → Offer` round-trip, the
//! ~128× cheaper leg), wants are matched against the FRESH offer by triple,
//! and a zero-address delivery is the server's "I no longer have it".
//!
//! THE WIRE HAS NO PER-ENTRY BinIDs — an `Offer` carries triples plus
//! `Topmost`, positions unknown. The adapter assigns CONSERVATIVE synthetic
//! positions: entry `i` of an offer for `[start, …]` gets `start + i`, which
//! is ≤ its true BinID (BinIDs strictly increase and the entries are a
//! subset of the range). Consequence: a partial interval advance lands at or
//! below the true position, so the next `Get` may RE-OFFER already-settled
//! ground (idempotent, suppressed by `got`) but can never SKIP unsettled
//! ground — conservative, never unsound. Exact resume would need per-entry
//! BinIDs on the wire: a protocol note for the reconciliation upgrade (§7).

use crate::pb;
use melissi_node::Outcome;
use melissi_settlement::BinId;
use melissi_types::{Bin, Triple};
use std::collections::BTreeMap;

/// The boundary where real content lands later (BMT hashing, postage stamp
/// validation — M3-b). For wire self-play the codec is synthetic and the
/// validation hook decides peer-fault vs entry-fault, exactly the §4.3
/// three-way split.
pub trait TripleCodec {
    fn address(&self, c: Triple) -> Vec<u8>;
    fn batch_id(&self, c: Triple) -> Vec<u8>;
    fn stamp_hash(&self, c: Triple) -> Vec<u8>;
    /// bee's stamp is 113 bytes (batchID ‖ index ‖ timestamp ‖ sig).
    fn stamp(&self, c: Triple) -> Vec<u8>;
    fn data(&self, c: Triple) -> Vec<u8>;
    /// Reverse: identify the triple a wire `Chunk` names.
    fn triple_of(&self, address: &[u8], batch_id: &[u8], stamp_hash: &[u8]) -> Option<Triple>;
    /// Identify the triple a `Delivery` settles (address + parsed stamp).
    fn triple_of_delivery(&self, d: &pb::Delivery) -> Option<Triple>;
    /// Verify a delivery: `Delivered` (stored), `Rejected` (entry-fault:
    /// invalid stamp/replay — identical at every holder), `Missed`
    /// (peer-fault: garbage data — retry elsewhere).
    fn validate(&self, c: Triple, d: &pb::Delivery) -> Outcome;
}

// The real codec — minting + content/payment self-verification over the real
// triple — lives in `crate::codec::MintedCodec`. There is no synthetic codec:
// once `Triple` is `(address, batchID, stampHash)`, identity is not invented.

/// What a client state machine wants the shell to do next.
pub enum ClientOut {
    /// Write these frames to the stream.
    Send(Vec<u8>),
    /// Waiting on more stream bytes.
    Need,
    /// Cursors stream done.
    Cursors { cursors: Vec<(Bin, BinId)>, epoch: u64 },
    /// Advertisement done: synthetic-positioned refs + topmost.
    OfferDone { refs: Vec<(BinId, Triple)>, topmost: BinId },
    /// Delivery cycle done: per-triple outcomes, never collapsed.
    FetchDone { outcomes: Vec<(Triple, Outcome)> },
}

/// `Syn → Ack`: the cursors stream.
pub struct CursorsClient {
    buf: Vec<u8>,
    sent: bool,
}

impl CursorsClient {
    pub fn new() -> Self {
        CursorsClient { buf: Vec::new(), sent: false }
    }
    pub fn poll(&mut self, input: &[u8]) -> ClientOut {
        if !self.sent {
            self.sent = true;
            return ClientOut::Send(pb::frame(&pb::Syn {}.encode()));
        }
        self.buf.extend_from_slice(input);
        if let Some((msg, n)) = pb::deframe(&self.buf) {
            self.buf.drain(..n);
            let ack = pb::Ack::decode(&msg).expect("malformed Ack");
            let cursors =
                ack.cursors.iter().enumerate().map(|(b, &h)| (b as Bin, h)).collect();
            return ClientOut::Cursors { cursors, epoch: ack.epoch };
        }
        ClientOut::Need
    }
}

/// The advertisement leg of `Effect::Offer`: `Get → Offer`, close with an
/// all-zero `Want` (verified quiet path on bee master).
pub struct OfferClient {
    pub bin: Bin,
    pub start: BinId,
    buf: Vec<u8>,
    sent: bool,
}

impl OfferClient {
    pub fn new(bin: Bin, start: BinId) -> Self {
        OfferClient { bin, start, buf: Vec::new(), sent: false }
    }
    pub fn poll<C: TripleCodec>(&mut self, codec: &C, input: &[u8]) -> ClientOut {
        if !self.sent {
            self.sent = true;
            let get = pb::Get { bin: u32::from(self.bin), start: self.start };
            return ClientOut::Send(pb::frame(&get.encode()));
        }
        self.buf.extend_from_slice(input);
        if let Some((msg, n)) = pb::deframe(&self.buf) {
            self.buf.drain(..n);
            let offer = pb::Offer::decode(&msg).expect("malformed Offer");
            // conservative synthetic positions: entry i at start + i <= true BinID
            let refs: Vec<(BinId, Triple)> = offer
                .chunks
                .iter()
                .enumerate()
                .filter_map(|(i, ch)| {
                    codec
                        .triple_of(&ch.address, &ch.batch_id, &ch.stamp_hash)
                        .map(|t| (self.start + i as BinId, t))
                })
                .collect();
            return ClientOut::OfferDone { refs, topmost: offer.topmost };
        }
        ClientOut::Need
    }

    /// The close frame: an all-zero bitvector Want (server delivers nothing
    /// and FullCloses; a Reset would be logged server-side). Only needed when
    /// the offer was non-empty.
    pub fn close_frame(n_offered: usize) -> Vec<u8> {
        let bv = pb::bitvector_new(n_offered);
        pb::frame(&pb::Want { bitvector: bv }.encode())
    }
}

enum FetchState {
    SendGet,
    AwaitOffer,
    AwaitDeliveries { pending: BTreeMap<Triple, ()>, remaining: usize },
}

/// `Effect::Fetch` over the legacy coupling: a FRESH `Get → Offer`, wants
/// matched by triple against the fresh offer (it may have paged or drifted),
/// positional bitvector, exactly popcount deliveries back, zero-address =
/// server-missing.
pub struct FetchClient {
    pub bin: Bin,
    pub start: BinId,
    want: Vec<Triple>,
    buf: Vec<u8>,
    state: FetchState,
    outcomes: Vec<(Triple, Outcome)>,
}

impl FetchClient {
    pub fn new(bin: Bin, start: BinId, want: Vec<Triple>) -> Self {
        FetchClient {
            bin,
            start,
            want,
            buf: Vec::new(),
            state: FetchState::SendGet,
            outcomes: Vec::new(),
        }
    }

    pub fn poll<C: TripleCodec>(&mut self, codec: &C, input: &[u8]) -> ClientOut {
        self.buf.extend_from_slice(input);
        loop {
            match &mut self.state {
                FetchState::SendGet => {
                    self.state = FetchState::AwaitOffer;
                    let get = pb::Get { bin: u32::from(self.bin), start: self.start };
                    return ClientOut::Send(pb::frame(&get.encode()));
                }
                FetchState::AwaitOffer => {
                    let Some((msg, n)) = pb::deframe(&self.buf) else {
                        return ClientOut::Need;
                    };
                    self.buf.drain(..n);
                    let offer = pb::Offer::decode(&msg).expect("malformed Offer");
                    // match wants against the FRESH offer, by triple
                    let mut bv = pb::bitvector_new(offer.chunks.len().max(1));
                    let mut pending = BTreeMap::new();
                    for (i, ch) in offer.chunks.iter().enumerate() {
                        if let Some(t) =
                            codec.triple_of(&ch.address, &ch.batch_id, &ch.stamp_hash)
                        {
                            if self.want.contains(&t) && !pending.contains_key(&t) {
                                pb::bitvector_set(&mut bv, i);
                                pending.insert(t, ());
                            }
                        }
                    }
                    // wanted but absent from the fresh offer (drift, paging,
                    // churn): Missed now — reschedule, never block
                    for &t in &self.want {
                        if !pending.contains_key(&t) {
                            self.outcomes.push((t, Outcome::Missed));
                        }
                    }
                    let remaining = pending.len();
                    let want = pb::Want { bitvector: bv };
                    if offer.chunks.is_empty() {
                        // server closes after an empty offer without reading
                        // a Want (bee master handler)
                        return self.finish();
                    }
                    self.state = FetchState::AwaitDeliveries { pending, remaining };
                    if remaining == 0 {
                        // all-zero Want: server delivers nothing, closes
                        return ClientOut::Send(pb::frame(&want.encode()));
                        // outcomes already complete; next poll finishes
                    }
                    return ClientOut::Send(pb::frame(&want.encode()));
                }
                FetchState::AwaitDeliveries { pending, remaining } => {
                    if *remaining == 0 {
                        return self.finish();
                    }
                    let Some((msg, n)) = pb::deframe(&self.buf) else {
                        return ClientOut::Need;
                    };
                    self.buf.drain(..n);
                    let d = pb::Delivery::decode(&msg).expect("malformed Delivery");
                    *remaining -= 1;
                    if d.address.iter().all(|&b| b == 0) || d.address.is_empty() {
                        // zero address: the server no longer has it —
                        // unattributed; the unresolved remainder goes Missed
                        continue;
                    }
                    if let Some(t) = codec.triple_of_delivery(&d) {
                        if pending.remove(&t).is_some() {
                            let outcome = codec.validate(t, &d);
                            self.outcomes.push((t, outcome));
                        }
                        // unsolicited: ignored (bee joins ErrUnsolicitedChunk)
                    }
                }
            }
        }
    }

    fn finish(&mut self) -> ClientOut {
        self.close()
    }

    /// The stream ended (server closed, omitted, or the shell timed it out)
    /// before every wanted delivery arrived: finalize the unmet pending as
    /// `Missed` — the shell owns this signal because it owns the socket and
    /// the clock. Idempotent: drains pending so a second call returns the
    /// same outcomes with nothing left to add.
    pub fn close(&mut self) -> ClientOut {
        if let FetchState::AwaitDeliveries { pending, remaining } = &mut self.state {
            for (&t, _) in pending.iter() {
                self.outcomes.push((t, Outcome::Missed));
            }
            pending.clear();
            *remaining = 0;
        }
        ClientOut::FetchDone { outcomes: std::mem::take(&mut self.outcomes) }
    }
}

// --- the serving side -------------------------------------------------------------

/// What a server needs from the node it fronts. Offer completeness — every
/// entry held in the range is named — is the serving side's spec obligation.
pub trait ServeReserve {
    /// Per-bin entries in `[start, ..]`, BinID order, at most `limit`
    /// (bee: `DefaultMaxPage = 250`); plus the topmost BinID covered.
    fn collect(&self, bin: Bin, start: BinId, limit: usize) -> (Vec<(BinId, Triple)>, BinId);
    fn has(&self, c: Triple) -> bool;
    /// Per-bin cursor heads, dense from bin 0 (bee sends all bins).
    fn cursors(&self) -> Vec<u64>;
    fn epoch(&self) -> u64;
}

pub const MAX_PAGE: usize = 250; // bee DefaultMaxPage

pub enum ServerOut {
    Send(Vec<u8>),
    Need,
    /// Stream complete (after deliveries, an empty offer, or an all-zero want).
    Done,
    /// The range is empty: the honest server BLOCKS (the live subscription).
    Blocked { bin: Bin, start: BinId },
}

enum ServeState {
    AwaitGet,
    AwaitWant { offered: Vec<(BinId, Triple)> },
}

/// One pullsync stream, server side: `Get → Offer → Want → Delivery*`.
pub struct ServerStream {
    buf: Vec<u8>,
    state: ServeState,
}

impl Default for ServerStream {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerStream {
    pub fn new() -> Self {
        ServerStream { buf: Vec::new(), state: ServeState::AwaitGet }
    }

    pub fn poll<C: TripleCodec, R: ServeReserve>(
        &mut self,
        codec: &C,
        reserve: &R,
        input: &[u8],
    ) -> ServerOut {
        self.buf.extend_from_slice(input);
        match &self.state {
            ServeState::AwaitGet => {
                let Some((msg, n)) = pb::deframe(&self.buf) else {
                    return ServerOut::Need;
                };
                self.buf.drain(..n);
                let get = pb::Get::decode(&msg).expect("malformed Get");
                let bin = get.bin as Bin;
                let (entries, topmost) = reserve.collect(bin, get.start, MAX_PAGE);
                if entries.is_empty() && topmost < get.start {
                    // nothing at or past start: block until something lands
                    return ServerOut::Blocked { bin, start: get.start };
                }
                let chunks = entries
                    .iter()
                    .map(|&(_, t)| pb::Chunk {
                        address: codec.address(t),
                        batch_id: codec.batch_id(t),
                        stamp_hash: codec.stamp_hash(t),
                    })
                    .collect::<Vec<_>>();
                let empty = chunks.is_empty();
                let offer = pb::Offer { topmost, chunks };
                let frame = pb::frame(&offer.encode());
                if empty {
                    self.state = ServeState::AwaitGet; // bee returns after an empty offer
                    return ServerOut::Send(frame); // caller treats as Done
                }
                self.state = ServeState::AwaitWant { offered: entries };
                ServerOut::Send(frame)
            }
            ServeState::AwaitWant { offered } => {
                let Some((msg, n)) = pb::deframe(&self.buf) else {
                    return ServerOut::Need;
                };
                self.buf.drain(..n);
                let want = pb::Want::decode(&msg).expect("malformed Want");
                let mut frames = Vec::new();
                for (i, &(_, t)) in offered.iter().enumerate() {
                    if !pb::bitvector_get(&want.bitvector, i) {
                        continue;
                    }
                    let d = if reserve.has(t) {
                        pb::Delivery {
                            address: codec.address(t),
                            data: codec.data(t),
                            stamp: codec.stamp(t),
                        }
                    } else {
                        // churned out since the offer: the zero-address
                        // placeholder (bee processWant)
                        pb::Delivery { address: vec![0; 32], data: vec![], stamp: vec![] }
                    };
                    frames.extend_from_slice(&pb::frame(&d.encode()));
                }
                if frames.is_empty() {
                    return ServerOut::Done;
                }
                ServerOut::Send(frames)
            }
        }
    }
}

/// The cursors stream, server side: `Syn → Ack`.
pub struct CursorsServer;

impl CursorsServer {
    pub fn respond<R: ServeReserve>(reserve: &R, input: &[u8]) -> Option<Vec<u8>> {
        let (msg, _) = pb::deframe(input)?;
        pb::Syn::decode(&msg)?;
        let ack = pb::Ack { cursors: reserve.cursors(), epoch: reserve.epoch() };
        Some(pb::frame(&ack.encode()))
    }
}
