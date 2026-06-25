//! neighbourhood — the discovery (kademlia) topology policy.
//!
//! A 1:1 refinement of `formal-models/tla/Neighbourhood.tla`. The module is the spec of
//! record; this file is the refinement, and `tests/parity.rs` re-runs its
//! ablation matrix on the shipped policy and asserts the distinct-state count
//! equals TLC's — the same translation-fidelity oracle `machine` uses.
//!
//! **What it decides.** Discovery (the hive peers push, verified bee-exact in
//! `net::hive`) supplies a pool of reachable candidates, each in a proximity bin
//! by shared-leading-bits with our overlay. This policy turns that pool into a
//! connected topology: saturate every bin to `K` (route outward), connect to
//! *every* candidate in the neighbourhood (the bins at/beyond depth — the slice
//! we store and serve over pull-sync), and shed a bin's surplus once depth rises
//! past it. It is abstract over the overlay arithmetic: `cand[b]` is a count, so
//! the policy is model-checked over counts and run over real proximities.
//!
//! Mapping to `Neighbourhood.tla`:
//!
//! | `Neighbourhood.tla`            | here                                       |
//! |--------------------------------|--------------------------------------------|
//! | `conn ∈ [Bins → Nat]`          | `conn: Vec<u32>` — connected count per bin |
//! | `Cand`, `K`                    | `cand: Vec<u32>`, `k` — the environment    |
//! | `PrioritizeNbhd`, `Pruning`    | [`Policy`] knobs (ablation only)           |
//! | `Depth` (shallowest unsat, capped) | [`Neighbourhood::depth`]               |
//! | `Fill(b)`, `Trim(b)`           | [`Neighbourhood::fill`] / [`Neighbourhood::trim`] — the only mutators |
//! | `ConnLeCand` (safety)          | [`Neighbourhood::conn_le_cand`]            |
//! | `Saturates`, `NeighbourhoodComplete`, `Bounded` (liveness) | the predicates of the same name, checked at terminals by the explorer |
//!
//! Like the machine, the actions are the only mutators (`&mut self`, no
//! setters), so a step cannot be split. The knobs exist to be *ablated*: the
//! shipped policy is [`Policy::SHIPPED`]; turning one off reproduces exactly the
//! `MC_nhood_flat` / `MC_nhood_noprune` counterexamples (see the tests).

pub mod explore;

/// The two design choices, each a `Neighbourhood.tla` knob, each ablatable.
/// Provably load-bearing: dropping either breaks a named property (the parity
/// suite re-checks both), so neither is decoration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Fill neighbourhood bins (≥ depth) past `K`, to every candidate — the
    /// density the reserve/serving depends on. OFF = a flat "K everywhere"
    /// policy (`NeighbourhoodComplete` fails).
    pub prioritize_nbhd: bool,
    /// Shed a bin's surplus once depth has risen past it. OFF = the working set
    /// never contracts (`Bounded` fails).
    pub pruning: bool,
}

impl Policy {
    pub const SHIPPED: Policy = Policy {
        prioritize_nbhd: true,
        pruning: true,
    };
}

impl Default for Policy {
    fn default() -> Self {
        Self::SHIPPED
    }
}

/// The neighbourhood state: how many peers we hold in each proximity bin, given
/// the reachable candidate pool `cand` and the saturation target `k`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbourhood {
    conn: Vec<u32>,
    cand: Vec<u32>,
    k: u32,
    policy: Policy,
}

impl Neighbourhood {
    /// A node with no connections yet, facing `cand[b]` candidates per bin (bins
    /// `0..cand.len()`; the last is the closest), saturating at `k`.
    pub fn new(cand: Vec<u32>, k: u32, policy: Policy) -> Self {
        assert!(k > 0 && !cand.is_empty());
        Neighbourhood {
            conn: vec![0; cand.len()],
            cand,
            k,
            policy,
        }
    }

    pub fn max_bin(&self) -> usize {
        self.cand.len() - 1
    }

    pub fn conn(&self) -> &[u32] {
        &self.conn
    }

    fn saturated(&self, b: usize) -> bool {
        self.conn[b] >= self.k
    }

    /// `Neighbourhood.tla` `Depth`: the shallowest unsaturated bin, capped at the
    /// closest bin (which is always neighbourhood — bee never prunes it). All
    /// saturated ⇒ the closest bin.
    pub fn depth(&self) -> usize {
        (0..=self.max_bin())
            .find(|&b| !self.saturated(b))
            .unwrap_or(self.max_bin())
    }

    pub fn in_neighbourhood(&self, b: usize) -> bool {
        b >= self.depth()
    }

    /// `Fill(b)`: connect one more candidate in bin `b`. A bin draws a connection
    /// while it is under `K` (route-saturation), OR it is in the neighbourhood
    /// and the density rule is on. Returns whether it fired.
    pub fn fill(&mut self, b: usize) -> bool {
        let draw =
            self.conn[b] < self.k || (self.policy.prioritize_nbhd && self.in_neighbourhood(b));
        if self.conn[b] < self.cand[b] && draw {
            self.conn[b] += 1;
            true
        } else {
            false
        }
    }

    /// `Trim(b)`: shed surplus from a bin that has fallen below depth (it was
    /// filled densely as a neighbourhood bin; depth has since risen past it).
    /// Returns whether it fired.
    pub fn trim(&mut self, b: usize) -> bool {
        if self.policy.pruning && !self.in_neighbourhood(b) && self.conn[b] > self.k {
            self.conn[b] -= 1;
            true
        } else {
            false
        }
    }

    /// No action is enabled — the topology has settled (a fixed point).
    pub fn terminal(&self) -> bool {
        (0..=self.max_bin()).all(|b| {
            let mut probe = self.clone();
            !probe.fill(b) && !probe.trim(b)
        })
    }

    // --- the spec's properties, as predicates at a state ---------------------

    /// `ConnLeCand` (safety): never claim more peers than exist in a bin.
    pub fn conn_le_cand(&self) -> bool {
        (0..=self.max_bin()).all(|b| self.conn[b] <= self.cand[b])
    }

    /// `Saturates`: every bin reaches as many peers as it can, up to `K`.
    pub fn saturates(&self) -> bool {
        (0..=self.max_bin()).all(|b| self.conn[b] >= self.cand[b].min(self.k))
    }

    /// `NeighbourhoodComplete`: every bin at/beyond depth holds every candidate.
    pub fn neighbourhood_complete(&self) -> bool {
        (0..=self.max_bin()).all(|b| !self.in_neighbourhood(b) || self.conn[b] == self.cand[b])
    }

    /// `Bounded`: a bin below depth holds at most `K` (the working-set bound).
    pub fn bounded(&self) -> bool {
        (0..=self.max_bin()).all(|b| self.in_neighbourhood(b) || self.conn[b] <= self.k)
    }
}
