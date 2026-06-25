//! Exhaustive explorer over the neighbourhood transition system — the same
//! oracle `machine::explore` is. It enumerates every state reachable from the
//! empty topology via `Fill`/`Trim` (the `Neighbourhood.tla` `Next`), counts
//! distinct states (asserted equal to TLC's in `tests/parity.rs`), checks the
//! safety invariant at every state, and checks a liveness predicate at every
//! terminal.
//!
//! Liveness is finite here because the system has no non-trivial cycles: for a
//! bin below depth, `Fill` needs `conn < K` and `Trim` needs `conn > K` —
//! disjoint, so no oscillation — and neighbourhood bins only ever fill. Every
//! behaviour therefore runs to a fixed point, so `<>[]P` holds iff every
//! terminal state satisfies `P` (a stuck terminal that violates `P` is the
//! counterexample TLC reports).

use crate::{Neighbourhood, Policy};
use std::collections::{HashSet, VecDeque};

#[derive(Debug)]
pub struct Report {
    /// Distinct reachable states (distinct `conn` valuations, incl. the initial)
    /// — the count asserted equal to TLC's distinct-state count.
    pub distinct: usize,
    /// `true` if `ConnLeCand` held at every reachable state.
    pub safe: bool,
    /// Fixed-point states (no action enabled).
    pub terminals: usize,
    /// Terminals that VIOLATE the liveness predicate under test — a non-empty
    /// count is the ablation's counterexample.
    pub bad_terminals: usize,
}

/// Explore from the empty topology. `liveness` is the property checked at each
/// terminal (e.g. `Neighbourhood::neighbourhood_complete`).
pub fn explore(
    cand: &[u32],
    k: u32,
    policy: Policy,
    liveness: fn(&Neighbourhood) -> bool,
) -> Report {
    let max_bin = cand.len() - 1;
    let start = Neighbourhood::new(cand.to_vec(), k, policy);

    let mut seen: HashSet<Vec<u32>> = HashSet::new();
    let mut queue: VecDeque<Vec<u32>> = VecDeque::new();
    let mut report = Report {
        distinct: 0,
        safe: true,
        terminals: 0,
        bad_terminals: 0,
    };

    seen.insert(start.conn().to_vec());
    queue.push_back(start.conn().to_vec());

    while let Some(conn) = queue.pop_front() {
        report.distinct += 1;
        let here = Neighbourhood {
            conn: conn.clone(),
            cand: cand.to_vec(),
            k,
            policy,
        };

        if !here.conn_le_cand() {
            report.safe = false;
        }

        // enumerate successors: Fill(b) or Trim(b) for each bin
        let mut any = false;
        for b in 0..=max_bin {
            for act in [Neighbourhood::fill, Neighbourhood::trim] {
                let mut next = here.clone();
                if act(&mut next, b) {
                    any = true;
                    let key = next.conn().to_vec();
                    if seen.insert(key.clone()) {
                        queue.push_back(key);
                    }
                }
            }
        }

        if !any {
            report.terminals += 1;
            if !liveness(&here) {
                report.bad_terminals += 1;
            }
        }
    }

    report
}
