//! Discovery readiness — the design's §5.6 discovery round, as one component.
//!
//! Three facts that together decide when a bin's choice set is assembled:
//! which `(peer, bin)` offers are in flight, which have resolved their HIST
//! range, and which peers have reported cursors at all. They were once three
//! loose `BTreeSet` fields mutated across four event arms; the M2 `[6,6,12]`
//! serve-skew bug lived exactly in their interplay — readiness quantified over
//! the wrong set. Owning them here makes the invariant a method, the
//! peer-departure cleanup one call, and inconsistent mutation unrepresentable.

use melissi_types::{Bin, PeerId};
use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct Discovery {
    /// One open offer per (peer, bin) — the live-subscription discipline.
    offer_open: BTreeSet<(PeerId, Bin)>,
    /// (peer, bin)s whose HIST range is resolved: the first offer answered,
    /// or the cursor showed nothing in range.
    hist_resolved: BTreeSet<(PeerId, Bin)>,
    /// Peers that have reported cursors. Readiness quantifies over KNOWN
    /// peers (see [`Discovery::bin_ready`]); this is the "has this known peer
    /// reported in at all yet" half of that test.
    cursored: BTreeSet<PeerId>,
}

impl Discovery {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A peer's cursors arrived.
    pub(crate) fn mark_cursored(&mut self, peer: PeerId) {
        self.cursored.insert(peer);
    }

    /// A `(peer, bin)` resolved its HIST range (offer answered, or empty).
    pub(crate) fn resolve(&mut self, peer: PeerId, bin: Bin) {
        self.hist_resolved.insert((peer, bin));
    }

    /// Try to open an offer; `true` if newly opened (caller emits the effect).
    pub(crate) fn try_open(&mut self, peer: PeerId, bin: Bin) -> bool {
        self.offer_open.insert((peer, bin))
    }

    /// An offer answered: it is no longer in flight.
    pub(crate) fn close(&mut self, peer: PeerId, bin: Bin) {
        self.offer_open.remove(&(peer, bin));
    }

    /// A peer departed: drop every trace of it, in one place.
    pub(crate) fn forget_peer(&mut self, peer: PeerId) {
        self.offer_open.retain(|&(q, _)| q != peer);
        self.hist_resolved.retain(|&(q, _)| q != peer);
        self.cursored.remove(&peer);
    }

    /// The choice set for `bin` is assembled: every known peer has reported
    /// cursors, and every peer that *lists* the bin (`lists_bin`) has resolved
    /// its HIST range. Quantifying over known `peers` — not merely over those
    /// already cursored — is the M2 fix: otherwise the first peer through
    /// discovery looks like the whole choice set and receives the whole wave.
    pub(crate) fn bin_ready(
        &self,
        bin: Bin,
        peers: impl IntoIterator<Item = PeerId>,
        lists_bin: impl Fn(PeerId) -> bool,
    ) -> bool {
        peers.into_iter().all(|p| {
            self.cursored.contains(&p)
                && (!lists_bin(p) || self.hist_resolved.contains(&(p, bin)))
        })
    }
}
