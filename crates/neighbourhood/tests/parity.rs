//! The neighbourhood parity matrix: `Neighbourhood.tla`'s configs, row for row,
//! on the shipped policy. Constants are copied from the `MC_nhood*.tla` configs
//! verbatim (`MaxBin = 2`, `Cand = [3,3,3]`, `K = 2`); the distinct-state counts
//! are asserted EQUAL to TLC's — the transition systems are the same by
//! construction, so any divergence is a translation bug, caught here. Liveness
//! verdicts are finite (the system runs to fixed points; see `explore`).

use melissi_neighbourhood::explore::explore;
use melissi_neighbourhood::{Neighbourhood, Policy};

const CAND: [u32; 3] = [3, 3, 3]; // MaxBin = 2, three candidates per bin
const K: u32 = 2;

// the shipped end-state oracle: optimal on all three counts at once.
fn optimal(n: &Neighbourhood) -> bool {
    n.saturates() && n.neighbourhood_complete() && n.bounded()
}

/// MC_nhood (positive): the shipped policy converges to the optimal topology —
/// every terminal saturated, neighbourhood-complete, bounded — and the reachable
/// state space matches TLC's 48 distinct states.
#[test]
fn mc_nhood_positive() {
    let r = explore(&CAND, K, Policy::SHIPPED, optimal);
    assert_eq!(r.distinct, 48, "distinct-state parity with TLC MC_nhood");
    assert!(r.safe, "ConnLeCand holds at every reachable state");
    assert_eq!(r.bad_terminals, 0, "every terminal is the optimal topology");
    assert!(r.terminals >= 1);
}

/// MC_nhood_flat: PrioritizeNbhd OFF. The neighbourhood never densely connects
/// (deep bins stall at K < Cand) → NeighbourhoodComplete fails. Saturates and
/// Bounded still hold — the ablation breaks exactly its property. TLC: 27 states.
#[test]
fn mc_nhood_flat_breaks_neighbourhood_complete() {
    let flat = Policy {
        prioritize_nbhd: false,
        pruning: true,
    };
    let r = explore(&CAND, K, flat, Neighbourhood::neighbourhood_complete);
    assert_eq!(
        r.distinct, 27,
        "distinct-state parity with TLC MC_nhood_flat"
    );
    assert!(r.safe);
    assert!(
        r.bad_terminals > 0,
        "a flat policy leaves a neighbourhood bin below its candidate count"
    );
    // exactly one property breaks: saturation and the bound still hold.
    assert_eq!(
        explore(&CAND, K, flat, Neighbourhood::saturates).bad_terminals,
        0
    );
    assert_eq!(
        explore(&CAND, K, flat, Neighbourhood::bounded).bad_terminals,
        0
    );
}

/// MC_nhood_noprune: Pruning OFF. A bin filled densely while it was in the
/// neighbourhood keeps that surplus after depth rises past it → Bounded fails.
/// Saturation and neighbourhood density still hold. TLC: 48 states.
#[test]
fn mc_nhood_noprune_breaks_bounded() {
    let noprune = Policy {
        prioritize_nbhd: true,
        pruning: false,
    };
    let r = explore(&CAND, K, noprune, Neighbourhood::bounded);
    assert_eq!(
        r.distinct, 48,
        "distinct-state parity with TLC MC_nhood_noprune"
    );
    assert!(r.safe);
    assert!(
        r.bad_terminals > 0,
        "without shedding, a bin below depth holds more than K"
    );
    // exactly one property breaks: saturation and neighbourhood density hold.
    assert_eq!(
        explore(&CAND, K, noprune, Neighbourhood::saturates).bad_terminals,
        0
    );
    assert_eq!(
        explore(&CAND, K, noprune, Neighbourhood::neighbourhood_complete).bad_terminals,
        0
    );
}
