//! pullstate — the optimal pull-sync scheduling machine.
//!
//! A 1:1 refinement of `optimal-testbed/PullSyncerE.tla` (SWIPs repo). The TLA+
//! module is the spec of record; this file is the refinement. Mapping:
//!
//! | `PullSyncerE.tla`            | here                                          |
//! |------------------------------|-----------------------------------------------|
//! | `got ⊆ Chunks`               | `got` (owned here; a store-backed view in the node crate) |
//! | `want ∈ [Chunks→SUBSET Peers]` | `want` — the lease table (a set per triple: `DedupInv` is a checked invariant, not the type, so the `nodedup` ablation stays expressible) |
//! | `failed`                     | `excluded` — per-triple bars, cleared on exhaustion |
//! | `arrived`                    | `arrived` — offered and unsettled             |
//! | `holds` (churned by Lose/Gain) | `holders` — observed from offers; `observe_holder`/`lose_holder` |
//! | `Prio`, `Assign`             | `prio`, `assign` (assign: ablation only)      |
//! | `conflict`, `ndeliv`         | tripwires: `ConflictFree`, `DeliveryFloor`    |
//!
//! Actions are the only mutators: `want`, `deliver`, `stall`, `reject`,
//! `arrive_hist`/`arrive_live`, `reset_excluded`, `observe_holder`/`lose_holder`.
//! No setters. `&mut self` is the `PullSyncerNA` obligation made structural: a
//! second mutator is a compile error, and sans-io actions contain no await point
//! to split a check from its mark.
//!
//! `ByzStall` and `SpuriousTimeout` are ONE `stall()` — the puller cannot
//! attribute a stall (slow-honest ≡ Byzantine to an observer); the environment
//! (explorer / driver / simulator) decides who may stall and within what budget.
//! Budgets (`tmo`, `chn`) are the model's proof device, not machine state.
//!
//! Flagged deviation (implementation doc §3): `prio_ok` quantifies only over
//! chunks with ≥1 eligible holder — the verbatim guard would let one unfetchable
//! chunk head-of-line block every shallower bin. Correctness-neutral per
//! `MC_vicinity`.

pub mod explore;

use std::collections::{BTreeMap, BTreeSet};

/// An accountable reserve entry `(address, batchID, stampHash)`, opaque at this
/// layer. Production widens this to the real triple; the machine never looks
/// inside it.
pub type Triple = u32;
pub type PeerId = u8;

/// The design knobs, mirroring the spec's `CONSTANT`s. Production ships
/// [`Config::PRODUCTION`]; the rest exist so the parity tests can run the same
/// ablation matrix as `optimal-testbed/run.sh`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Config {
    pub dedup: bool,            // §5.2  — one in-flight claim per triple
    pub failover: bool,         // §5.4  — a stalled claim is releasable
    pub exclude: bool,          // §5.4  — the staller is barred for that triple
    pub reset_on_exhaust: bool, // §5.4  — bars clear once they cover every holder
    pub single_source: bool,    // §5.1  — DISQUALIFIED family; never ship true
    pub priority: bool,         // §5.5  — deepest-first ordering (correctness-neutral)
    pub enable_live: bool,      // §5.6  — pull post-cutoff arrivals
}

impl Config {
    pub const PRODUCTION: Config = Config {
        dedup: true,
        failover: true,
        exclude: true,
        reset_on_exhaust: true,
        single_source: false,
        priority: true,
        enable_live: true,
    };
}

/// The scheduling machine. Pure: no I/O, no clock, no locks. One owner.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PullState {
    cfg: Config,
    /// The lease table: in-flight claims, keyed by triple. Empty sets are
    /// removed (normalised) so equal states compare and hash equal.
    pub(crate) want: BTreeMap<Triple, BTreeSet<PeerId>>,
    /// Who currently offers each triple — observed, stale-tolerant.
    pub(crate) holders: BTreeMap<Triple, BTreeSet<PeerId>>,
    /// Per-triple bars (the failover log).
    pub(crate) excluded: BTreeMap<Triple, BTreeSet<PeerId>>,
    /// Offered and unsettled (the open window).
    pub(crate) arrived: BTreeSet<Triple>,
    /// Arrived after the cursor (the LIVE gate applies to these).
    pub(crate) live: BTreeSet<Triple>,
    /// Entry-faults: terminally rejected, settled, never storable.
    pub(crate) rejected: BTreeSet<Triple>,
    /// Fetched and stored (terminal).
    pub(crate) got: BTreeSet<Triple>,
    /// Priority key (bin depth; higher = deeper = first).
    pub(crate) prio: BTreeMap<Triple, u8>,
    /// Single-source ablation only.
    pub(crate) assign: BTreeMap<Triple, PeerId>,
    /// Tripwire: latched on any double delivery. Must never fire (ConflictFree).
    pub(crate) conflict: bool,
    /// Tripwire: deliveries so far. With `npre`, must equal |got| (DeliveryFloor).
    pub(crate) ndeliv: u32,
    /// Entries held before sync began — the ReserveHas view at Init. Never
    /// delivered, never owed: `DeliveryFloor` is `ndeliv + npre = |got|`.
    pub(crate) npre: u32,
}

fn set_insert(map: &mut BTreeMap<Triple, BTreeSet<PeerId>>, c: Triple, p: PeerId) -> bool {
    map.entry(c).or_default().insert(p)
}

fn set_remove(map: &mut BTreeMap<Triple, BTreeSet<PeerId>>, c: Triple, p: PeerId) -> bool {
    if let Some(s) = map.get_mut(&c) {
        let removed = s.remove(&p);
        if s.is_empty() {
            map.remove(&c);
        }
        removed
    } else {
        false
    }
}

fn set_contains(map: &BTreeMap<Triple, BTreeSet<PeerId>>, c: Triple, p: PeerId) -> bool {
    map.get(&c).is_some_and(|s| s.contains(&p))
}

impl PullState {
    pub fn new(cfg: Config) -> Self {
        PullState {
            cfg,
            want: BTreeMap::new(),
            holders: BTreeMap::new(),
            excluded: BTreeMap::new(),
            arrived: BTreeSet::new(),
            live: BTreeSet::new(),
            rejected: BTreeSet::new(),
            got: BTreeSet::new(),
            prio: BTreeMap::new(),
            assign: BTreeMap::new(),
            conflict: false,
            ndeliv: 0,
            npre: 0,
        }
    }

    /// The ReserveHas view at sync start: `c` is already held — terminal from
    /// the first offer, never owed, never fetched. (In the model, pre-held
    /// entries simply never *arrive*; this is the implementation's `got`
    /// backing for them.)
    pub fn preload(&mut self, c: Triple) -> bool {
        if self.got.insert(c) {
            self.npre += 1;
            true
        } else {
            false
        }
    }

    pub fn set_prio(&mut self, c: Triple, depth: u8) {
        self.prio.insert(c, depth);
    }

    pub fn set_assign(&mut self, c: Triple, p: PeerId) {
        self.assign.insert(c, p);
    }

    // --- observation actions (the model's Gain / Lose / NewChunk) ------------

    /// `Gain(c, p)` / offer ingestion: peer `p` offers triple `c`.
    pub fn observe_holder(&mut self, c: Triple, p: PeerId) -> bool {
        set_insert(&mut self.holders, c, p)
    }

    /// `Lose(c, p)`: a re-offer shows `p` no longer holds `c` (eviction,
    /// departure). Releases any claim `p` held on `c` — with NO bar: candidacy
    /// is governed by holdings, bars by behaviour (`NoFalseExclusion`).
    pub fn lose_holder(&mut self, c: Triple, p: PeerId) -> bool {
        let held = set_remove(&mut self.holders, c, p);
        set_remove(&mut self.want, c, p);
        held
    }

    /// A pre-cursor (HIST) triple enters the window.
    pub fn arrive_hist(&mut self, c: Triple) -> bool {
        if self.got.contains(&c) || self.rejected.contains(&c) {
            return false;
        }
        self.arrived.insert(c)
    }

    /// `NewChunk(c)`: a post-cursor (LIVE) arrival. Arrival is an observation —
    /// `enable_live` gates *claiming* (see `claimable`), never arrival.
    pub fn arrive_live(&mut self, c: Triple) -> bool {
        if self.got.contains(&c) || self.rejected.contains(&c) || self.arrived.contains(&c) {
            return false;
        }
        self.arrived.insert(c);
        self.live.insert(c);
        true
    }

    // --- the guard ------------------------------------------------------------

    /// `Addressed(c)` — started (leased) or finished.
    fn addressed(&self, c: Triple) -> bool {
        self.got.contains(&c) || self.want.contains_key(&c)
    }

    /// `PrioOK(c)`, with the flagged deviation: quantified only over chunks
    /// that have ≥1 eligible holder, so an unfetchable chunk cannot
    /// head-of-line block the bins below it.
    fn prio_ok(&self, c: Triple) -> bool {
        if !self.cfg.priority {
            return true;
        }
        let pc = self.prio.get(&c).copied().unwrap_or(0);
        self.arrived.iter().all(|&d| {
            let pd = self.prio.get(&d).copied().unwrap_or(0);
            pd <= pc || !self.eligible(d) || self.addressed(d)
        })
    }

    /// `d` has at least one candidate left: a holder that is not barred.
    fn eligible(&self, d: Triple) -> bool {
        if self.got.contains(&d) {
            return false;
        }
        let Some(hs) = self.holders.get(&d) else {
            return false;
        };
        hs.iter()
            .any(|p| !set_contains(&self.excluded, d, *p))
    }

    /// `Claimable(c, p)` — every conjunct copied from the spec, not paraphrased.
    pub fn claimable(&self, c: Triple, p: PeerId) -> bool {
        self.arrived.contains(&c)
            && set_contains(&self.holders, c, p)
            && !self.got.contains(&c)
            && !set_contains(&self.want, c, p)
            && !set_contains(&self.excluded, c, p)
            && (!self.cfg.dedup || !self.want.contains_key(&c))
            && (!self.cfg.single_source || self.assign.get(&c) == Some(&p))
            && (!self.live.contains(&c) || self.cfg.enable_live)
            && self.prio_ok(c)
    }

    // --- the actions ------------------------------------------------------------

    /// `Want(c, p)`: the check-and-mark, one indivisible step (`PullSyncerNA`).
    pub fn want(&mut self, c: Triple, p: PeerId) -> bool {
        if !self.claimable(c, p) {
            return false;
        }
        set_insert(&mut self.want, c, p);
        true
    }

    /// `Deliver(c, p)`: a verified delivery from `p`. Latches `conflict` on a
    /// double delivery — the `ConflictFree` tripwire, which must never fire.
    pub fn deliver(&mut self, c: Triple, p: PeerId) -> bool {
        if !set_contains(&self.want, c, p) {
            return false;
        }
        if self.got.contains(&c) {
            self.conflict = true;
        }
        self.got.insert(c);
        self.ndeliv += 1;
        set_remove(&mut self.want, c, p);
        true
    }

    /// `ByzStall(c, p)` AND `SpuriousTimeout(c, p)`: one verb for every
    /// non-delivery — timeout, error, gone. Peers are never classified.
    /// Gated by `failover` (off ⇒ the claim is stuck: the `MC_nofailover`
    /// ablation); bars the staller iff `exclude`.
    pub fn stall(&mut self, c: Triple, p: PeerId) -> bool {
        if !self.cfg.failover || !set_contains(&self.want, c, p) {
            return false;
        }
        set_remove(&mut self.want, c, p);
        if self.cfg.exclude {
            set_insert(&mut self.excluded, c, p);
        }
        true
    }

    /// `Reject(c)`: an entry-fault — invalid stamp, replay — identical at every
    /// holder, so it settles the triple globally (`IntervalSettlement`'s `Bad`).
    pub fn reject(&mut self, c: Triple) -> bool {
        if self.got.contains(&c) || !self.rejected.insert(c) {
            return false;
        }
        self.arrived.remove(&c);
        self.live.remove(&c);
        self.want.remove(&c);
        true
    }

    /// `ResetExcluded(c)`: the bars cover every current holder and nothing is
    /// in flight — clear them (cooldown expiry / fresh retry round). Without
    /// this, one misattributed stall on a triple's only holder strands it
    /// forever (the `MC_noreset` ablation).
    pub fn reset_excluded(&mut self, c: Triple) -> bool {
        if !self.can_reset_excluded(c) {
            return false;
        }
        self.excluded.remove(&c);
        true
    }

    pub fn can_reset_excluded(&self, c: Triple) -> bool {
        self.cfg.reset_on_exhaust
            && self.arrived.contains(&c)
            && !self.got.contains(&c)
            && !self.want.contains_key(&c)
            && self.excluded.get(&c).is_some_and(|ex| {
                !ex.is_empty()
                    && self
                        .holders
                        .get(&c)
                        .map(|hs| hs.iter().all(|p| ex.contains(p)))
                        .unwrap_or(true)
            })
    }

    // --- queries ------------------------------------------------------------

    pub fn cfg(&self) -> &Config {
        &self.cfg
    }

    pub fn has(&self, c: Triple) -> bool {
        self.got.contains(&c)
    }

    pub fn is_rejected(&self, c: Triple) -> bool {
        self.rejected.contains(&c)
    }

    /// `Phi`: the deficit over what is currently available.
    pub fn deficit(&self) -> usize {
        self.arrived.iter().filter(|c| !self.got.contains(c)).count()
    }

    pub fn conflict(&self) -> bool {
        self.conflict
    }

    pub fn deliveries(&self) -> u32 {
        self.ndeliv
    }

    /// Outstanding leases held by `p` — the load signal for the routing policy.
    /// Derived from the lease table, so it can never disagree with it.
    pub fn load(&self, p: PeerId) -> usize {
        self.want.values().filter(|s| s.contains(&p)).count()
    }

    /// The triples `p` currently has in-flight claims on (for peer departure:
    /// each becomes a `lose_holder`, releasing the claim with no bar).
    pub fn claims_of(&self, p: PeerId) -> Vec<Triple> {
        self.want
            .iter()
            .filter(|(_, ps)| ps.contains(&p))
            .map(|(&c, _)| c)
            .collect()
    }

    /// The triples `p` is currently observed to hold.
    pub fn held_by(&self, p: PeerId) -> Vec<Triple> {
        self.holders
            .iter()
            .filter(|(_, ps)| ps.contains(&p))
            .map(|(&c, _)| c)
            .collect()
    }

    /// All enabled `Want(c, p)` — the model's nondeterminism, for policy
    /// (node crate) or exhaustive exploration (explorer) to resolve.
    pub fn enabled_wants(&self) -> Vec<(Triple, PeerId)> {
        let mut out = Vec::new();
        for &c in &self.arrived {
            if self.got.contains(&c) {
                continue;
            }
            if let Some(hs) = self.holders.get(&c) {
                for &p in hs {
                    if self.claimable(c, p) {
                        out.push((c, p));
                    }
                }
            }
        }
        out
    }

    /// The machine-local safety invariants, checked in the order the suite
    /// names them. The environment-aware ones (`SupplyInv`, `NoFalseExclusion`)
    /// live with the environment (explorer / sim).
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        // ConflictFree: exactly-once delivery.
        if self.conflict {
            return Err("ConflictFree");
        }
        // DeliveryFloor: deliveries + preloaded = chunks stored (the O3 floor).
        if (self.ndeliv + self.npre) as usize != self.got.len() {
            return Err("DeliveryFloor");
        }
        // DedupInv: at most one in-flight claim per triple.
        if self.cfg.dedup && self.want.values().any(|s| s.len() > 1) {
            return Err("DedupInv");
        }
        // ClaimsLive: every claim is on a current, non-barred holder.
        for (&c, ps) in &self.want {
            for &p in ps {
                if !set_contains(&self.holders, c, p) || set_contains(&self.excluded, c, p) {
                    return Err("ClaimsLive");
                }
            }
        }
        Ok(())
    }
}
