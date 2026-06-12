//! Interval settlement — a refinement of `optimal-testbed/IntervalSettlement.tla`.
//!
//! The interval is pull-sync's only durable claim — covering a BinID says
//! *never offer me this range again* — so advancing it is FORGETTING, and the
//! rule is **settle before you forget**: advance only over entries stored (by
//! anyone) or terminally rejected.
//!
//! Mapping:
//!
//! | `IntervalSettlement.tla`     | here                                      |
//! |------------------------------|-------------------------------------------|
//! | `intv[p]`                    | `PeerBinLog::interval` — a single `u64` high-water. The spec's prefix-only abstraction is the TYPE: a disconnected range is unrepresentable, so bee's `TestIntervalAdvancePrefixOnly` obligation does not exist here |
//! | `Log[p]`                     | `entries` — the offered window above the interval (BinID → triple) |
//! | `Settled(c)`                 | the predicate passed to [`PeerBinLog::advance`] — the caller decides whether rejection settles (`RejectSettles`); the node passes stored-or-rejected |
//! | eager advance (`SettledOnly = FALSE`) | unrepresentable — there is no API that moves the interval except `advance`, and `advance` drops only settled prefixes. The `MC_settlement_eager` ablation is discharged by construction |
//!
//! `NoDrop` is structural: entries leave the window only inside `advance`, and
//! only after the predicate passed. `Monotone` is structural: the interval is
//! only ever assigned a larger value. What remains testable is liveness
//! (`AdvanceComplete`) and the cross-peer story — see the tests.
//!
//! Offer completeness — identifying what a peer OFFERS with what it HOLDS —
//! is the one assumption this crate inherits from the spec header; it is the
//! serving side's obligation, pinned in the sim.

use std::collections::BTreeMap;

pub type Triple = u32;
pub type BinId = u64;

/// The resume state for one `(peer, bin)`: the persisted interval high-water
/// plus the offered, not-yet-settled window above it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerBinLog {
    /// Everything at or below this BinID is settled and forgotten.
    interval: BinId,
    /// The highest BinID the peer's offers have covered so far.
    topmost: BinId,
    /// Offered entries above the interval, in the peer's BinID order.
    entries: BTreeMap<BinId, Triple>,
}

impl PeerBinLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resume from a persisted high-water (bee: `intervalstore` `Next() - 1`).
    pub fn resume(interval: BinId) -> Self {
        PeerBinLog { interval, topmost: interval, entries: BTreeMap::new() }
    }

    /// Where the next `Offer` starts (bee: `Next()`).
    pub fn next(&self) -> BinId {
        self.interval + 1
    }

    pub fn interval(&self) -> BinId {
        self.interval
    }

    pub fn topmost(&self) -> BinId {
        self.topmost
    }

    /// Ingest one offered entry. Entries at or below the interval are already
    /// settled-and-forgotten; a stale in-flight offer may still name them —
    /// ignored, idempotently.
    pub fn observe(&mut self, bin_id: BinId, triple: Triple) {
        if bin_id > self.interval {
            self.entries.insert(bin_id, triple);
        }
        if bin_id > self.topmost {
            self.topmost = bin_id;
        }
    }

    /// The offer for `[start, topmost]` covered this range.
    pub fn cover(&mut self, topmost: BinId) {
        if topmost > self.topmost {
            self.topmost = topmost;
        }
    }

    /// The peer no longer offers this BinID (eviction / churn-out): the entry
    /// leaves the window — it is a gap now, not an obligation. Gaps never
    /// block the advance; they are simply not offered.
    pub fn forget_entry(&mut self, bin_id: BinId) -> Option<Triple> {
        self.entries.remove(&bin_id)
    }

    /// Entries still in the window (offered, unsettled), deepest obligation
    /// first in BinID order.
    pub fn unsettled(&self) -> impl Iterator<Item = (BinId, Triple)> + '_ {
        self.entries.iter().map(|(&b, &c)| (b, c))
    }

    /// **Settle before you forget.** Advance the interval to the largest
    /// `x ≤ topmost` such that every offered entry with BinID ≤ `x` satisfies
    /// `settled` — the contiguous settled prefix, never past an unsettled
    /// entry. Returns the new high-water if it moved.
    ///
    /// The caller chooses the predicate: the node passes stored-or-rejected
    /// (`RejectSettles = TRUE`); passing stored-only reproduces the
    /// `MC_settlement_noreject` wedge (see the tests).
    pub fn advance(&mut self, settled: impl Fn(Triple) -> bool) -> Option<BinId> {
        let mut new = self.interval;
        while let Some((&bin_id, &triple)) = self.entries.iter().next() {
            if !settled(triple) {
                // the first unsettled entry caps the advance just below it
                new = new.max(bin_id - 1).min(self.topmost);
                break;
            }
            self.entries.remove(&bin_id);
            new = bin_id;
        }
        if self.entries.is_empty() {
            // nothing left offered-and-unsettled: the whole covered range settles
            new = self.topmost;
        }
        if new > self.interval {
            self.interval = new;
            Some(new)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The MC_settlement scenario: cross-peer overlap in different BinID
    /// positions, one never-storable entry (3), one single-source chunk (4).
    ///   Log[A] = <<1, 2, 3>>   Log[B] = <<2, 1, 4>>   Bad = {3}
    fn scenario() -> (PeerBinLog, PeerBinLog) {
        let mut a = PeerBinLog::new();
        a.observe(1, 1);
        a.observe(2, 2);
        a.observe(3, 3);
        let mut b = PeerBinLog::new();
        b.observe(1, 2);
        b.observe(2, 1);
        b.observe(3, 4);
        (a, b)
    }

    /// MC_settlement, exhaustively: every order of settlement events keeps
    /// NoDrop (structural — asserted via the interval/settled relation) and
    /// ends with both intervals fully drained (AdvanceComplete), with
    /// monotone intervals throughout.
    #[test]
    fn settlement_parity_all_orders() {
        // events: store 1, store 2, store 4, reject 3 — all 24 orders
        let events: [u32; 4] = [1, 2, 4, 3];
        let mut orders = Vec::new();
        permute(&events, &mut Vec::new(), &mut orders);
        assert_eq!(orders.len(), 24);

        for order in orders {
            let (mut a, mut b) = scenario();
            let mut stored: BTreeSet<u32> = BTreeSet::new();
            let mut rejected: BTreeSet<u32> = BTreeSet::new();
            let (mut last_a, mut last_b) = (a.interval(), b.interval());

            for &c in &order {
                if c == 3 {
                    rejected.insert(c); // the entry-fault settles globally
                } else {
                    stored.insert(c); // fetched from ANYONE (cross-peer)
                }
                let settled = |t: u32| stored.contains(&t) || rejected.contains(&t);
                a.advance(settled);
                b.advance(settled);
                // Monotone: the interval never regresses.
                assert!(a.interval() >= last_a && b.interval() >= last_b);
                last_a = a.interval();
                last_b = b.interval();
                // NoDrop, externally visible form: every entry still unsettled
                // is still in some window (visible — it will be re-offered).
                for t in [1u32, 2, 4, 3] {
                    let is_settled = stored.contains(&t) || rejected.contains(&t);
                    let visible = a.unsettled().any(|(_, x)| x == t)
                        || b.unsettled().any(|(_, x)| x == t);
                    assert!(is_settled || visible, "unsettled {t} lost visibility");
                }
            }
            // AdvanceComplete: both windows drain to their topmost.
            assert_eq!(a.interval(), 3, "order {order:?}");
            assert_eq!(b.interval(), 3, "order {order:?}");
            assert_eq!(a.unsettled().count(), 0);
            assert_eq!(b.unsettled().count(), 0);
        }
    }

    /// MC_settlement_noreject: with a stored-only predicate the never-storable
    /// entry pins A's interval at 2 forever — resume liveness fails while
    /// delivery (of storable chunks) is unaffected.
    #[test]
    fn noreject_wedges_behind_bad_entry() {
        let (mut a, mut b) = scenario();
        let stored: BTreeSet<u32> = [1, 2, 4].into();
        let settled = |t: u32| stored.contains(&t); // rejection does NOT settle
        a.advance(&settled);
        b.advance(&settled);
        assert_eq!(a.interval(), 2, "A must wedge just below the bad entry");
        assert_eq!(b.interval(), 3, "B holds no bad entry and drains");
        assert!(a.unsettled().any(|(_, t)| t == 3), "the bad entry stays visible");
    }

    /// The cross-peer essence of §4.6: an entry offered by A but fetched via B
    /// still advances A's interval — settlement is global (by triple), advance
    /// is local (in A's BinID space).
    #[test]
    fn cross_peer_fetch_advances_the_other_log() {
        let (mut a, _b) = scenario();
        // chunk 1 (A's BinID 1) and chunk 2 (A's BinID 2) were fetched from B.
        let stored: BTreeSet<u32> = [1, 2].into();
        let up = a.advance(|t| stored.contains(&t));
        assert_eq!(up, Some(2));
        assert_eq!(a.next(), 3);
    }

    /// Eviction/churn: a forgotten entry is a gap, and gaps never block.
    #[test]
    fn gaps_do_not_block_advance() {
        let mut a = PeerBinLog::new();
        a.observe(1, 10);
        a.observe(3, 30); // BinID 2 was never offered (evicted before offer)
        a.cover(4); // the offer covered [1, 4]
        let stored: BTreeSet<u32> = [10].into();
        assert_eq!(a.advance(|t| stored.contains(&t)), Some(2)); // up to the gap…
        a.forget_entry(3); // …then 3 churns out
        assert_eq!(a.advance(|_| false), Some(4)); // nothing offered remains: full cover
    }

    /// A stale in-flight offer naming already-settled BinIDs is ignored.
    #[test]
    fn observe_below_interval_is_ignored() {
        let mut a = PeerBinLog::resume(5);
        a.observe(3, 99);
        assert_eq!(a.unsettled().count(), 0);
        assert_eq!(a.next(), 6);
    }

    fn permute(rest: &[u32], acc: &mut Vec<u32>, out: &mut Vec<Vec<u32>>) {
        if rest.is_empty() {
            out.push(acc.clone());
            return;
        }
        for (i, &x) in rest.iter().enumerate() {
            let mut r = rest.to_vec();
            r.remove(i);
            acc.push(x);
            permute(&r, acc, out);
            acc.pop();
        }
    }
}
