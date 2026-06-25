//! The open-offer discipline: at most one offer in flight per `(peer, bin)`.
//!
//! That is the whole of it now. There was once a discovery-readiness machine
//! here too — `cursored` / `hist_resolved` feeding a per-bin `bin_ready` gate,
//! the "discovery barrier" that held a bin's scheduling until every peer's
//! choice set had assembled. It bought exact serve-balance by WAITING, but a
//! peer that completed the cursor handshake and then withheld its offer (offline
//! or malicious) wedged the whole bin (`DiscoveryBarrier.tla`), and the M2
//! `[6,6,12]` serve-skew bug lived in the three fields' interplay. The scheduler's
//! window cap ([`crate::Policy::window`], `WindowedLoad.tla`) recovers the balance
//! without the wait — so the readiness machine, and that bug class, are gone.
//! What remains is the live-subscription flag.

use melissi_types::{Bin, PeerId};
use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct Discovery {
    /// One open offer per (peer, bin) — keep a standing offer on each stream,
    /// never two (the live subscription: the server blocks on an empty range).
    offer_open: BTreeSet<(PeerId, Bin)>,
}

impl Discovery {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Try to open an offer; `true` if newly opened (caller emits the effect).
    pub(crate) fn try_open(&mut self, peer: PeerId, bin: Bin) -> bool {
        self.offer_open.insert((peer, bin))
    }

    /// An offer answered: it is no longer in flight.
    pub(crate) fn close(&mut self, peer: PeerId, bin: Bin) {
        self.offer_open.remove(&(peer, bin));
    }

    /// A peer departed: drop every trace of it.
    pub(crate) fn forget_peer(&mut self, peer: PeerId) {
        self.offer_open.retain(|&(q, _)| q != peer);
    }
}
