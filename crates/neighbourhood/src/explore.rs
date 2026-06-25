//! Exhaustive explorer over the discovery + connection transition system — the
//! same oracle `machine::explore` is. It enumerates every state reachable from
//! the bootstrap (only the seed known, nothing connected) via the three actions
//! (`DiscoverW`/`DiscoverU`/`Connect` — the `Neighbourhood.tla` `Next`), counts
//! distinct states (asserted equal to TLC's), checks the safety invariant at
//! every state, and checks a liveness predicate at every terminal.
//!
//! Liveness is finite because the actions only ever increase counts (toward
//! fixed bounds), so there are no cycles: every behaviour runs to a fixed point,
//! and `<>[]P` holds iff every terminal satisfies `P` (a stuck terminal that
//! violates `P` is TLC's counterexample).

use crate::{Neighbourhood, Policy};
use std::collections::{HashSet, VecDeque};

#[derive(Debug)]
pub struct Report {
    /// Distinct reachable states (distinct `(known_w, known_u, conn)`) — asserted
    /// equal to TLC's distinct-state count.
    pub distinct: usize,
    /// `true` if `ConnLeKnown` held at every reachable state.
    pub safe: bool,
    /// Fixed-point states (no action enabled).
    pub terminals: usize,
    /// Terminals that VIOLATE the liveness predicate under test — a non-empty
    /// count is the ablation's counterexample.
    pub bad_terminals: usize,
}

/// Explore from the bootstrap. `liveness` is the property checked at each
/// terminal (e.g. `Neighbourhood::supply_complete`).
pub fn explore(
    willing: u32,
    declining: u32,
    policy: Policy,
    liveness: fn(&Neighbourhood) -> bool,
) -> Report {
    let mk = |s: (u32, u32, u32)| -> Neighbourhood {
        let mut h = Neighbourhood::new(willing, declining, policy);
        h.set_state(s);
        h
    };

    let start = Neighbourhood::new(willing, declining, policy);
    let mut seen: HashSet<(u32, u32, u32)> = HashSet::new();
    let mut queue: VecDeque<(u32, u32, u32)> = VecDeque::new();
    let mut report = Report {
        distinct: 0,
        safe: true,
        terminals: 0,
        bad_terminals: 0,
    };

    seen.insert(start.state());
    queue.push_back(start.state());

    type Act = fn(&mut Neighbourhood) -> bool;
    let actions: [Act; 3] = [
        Neighbourhood::discover_w,
        Neighbourhood::discover_u,
        Neighbourhood::connect,
    ];

    while let Some(s) = queue.pop_front() {
        report.distinct += 1;
        let here = mk(s);

        if !here.conn_le_known() {
            report.safe = false;
        }

        let mut any = false;
        for act in actions {
            let mut next = mk(s);
            if act(&mut next) {
                any = true;
                let key = next.state();
                if seen.insert(key) {
                    queue.push_back(key);
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
