//! The quantitative floors, measured — across the k ∈ [2, 8] envelope and a
//! sweep of adversarial message orderings (seeds). What TLC proved per node,
//! the sim measures for the composition:
//!
//!   O1/Θ-REP  every node converges to the full reserve
//!   O3        Σ deliveries = Σ initial deficits, EXACTLY — through omission
//!             and spurious misses (failed attempts are retries, never
//!             deliveries: the floor is robust, not just average-case)
//!   O5        free-choice routing balances realised serve totals to within
//!             one chunk (the Graham/free-choice exactness of design §5.3)
//!   LIVE      a post-start arrival at one node spreads to all

use melissi_node::Policy;
use melissi_sim::{bin_of, Sim, Triple};

const M: u32 = 24;

/// Readable chunk number → real triple (scheduling/floor tests are content-
/// agnostic; only distinctness and bin placement matter).
fn t(n: u32) -> Triple {
    Triple::mock(n)
}

fn universe() -> Vec<Triple> {
    (0..M).map(t).collect()
}

/// Cold start: one empty node joins k−1 full ones. The headline scenario —
/// the empty node fetches each chunk exactly once, spread evenly.
#[test]
fn cold_start_floors_across_k_and_seeds() {
    for k in [2usize, 4, 8] {
        for seed in 0..5u64 {
            let mut sim = Sim::new(k, &[], seed);
            for i in 1..k {
                for c in universe() {
                    sim.seed(i, c);
                }
            }
            sim.start();
            sim.run();

            sim.assert_converged(&universe());
            // the delivery floor, exactly: M fetches for M missing chunks
            assert_eq!(sim.deliveries(0), M as u64, "k={k} seed={seed}");
            for i in 1..k {
                assert_eq!(sim.deliveries(i), 0, "full node {i} fetched");
            }
            // the fairness floor: realised serve totals within one chunk
            let served: Vec<u64> = (1..k).map(|i| sim.served[i]).collect();
            let (max, min) = (*served.iter().max().unwrap(), *served.iter().min().unwrap());
            assert_eq!(served.iter().sum::<u64>(), M as u64);
            assert!(max - min <= 1, "k={k} seed={seed}: serve skew {served:?}");
        }
    }
}

/// Θ-REP from scattered origins: each chunk starts on exactly one node and
/// must reach all k — the all-to-all epidemic, with spurious misses thrown
/// in. The floor stays exact: misses are retries, not deliveries.
#[test]
fn epidemic_reaches_theta_rep_at_the_delivery_floor() {
    let k = 4usize;
    for seed in 0..5u64 {
        let mut sim = Sim::new(k, &[], seed);
        for n in 0..M {
            sim.seed(n as usize % k, t(n));
        }
        sim.spurious_budget = 3;
        sim.start();
        sim.run();

        sim.assert_converged(&universe());
        let total: u64 = (0..k).map(|i| sim.deliveries(i)).sum();
        assert_eq!(
            total,
            (k as u64 - 1) * M as u64,
            "seed={seed}: network delivery floor"
        );
        let served: u64 = sim.served.iter().sum();
        assert_eq!(served, (k as u64 - 1) * M as u64);
    }
}

/// Omission at scale: a Byzantine peer advertises everything and serves
/// nothing. The empty node still converges at the floor; the omitter serves
/// zero; the honest servers carry the load.
#[test]
fn byzantine_omitter_costs_rounds_not_the_floor() {
    for seed in 0..5u64 {
        let mut sim = Sim::new(4, &[3], seed);
        for i in 1..4 {
            for c in universe() {
                sim.seed(i, c);
            }
        }
        sim.start();
        sim.run();

        sim.assert_converged(&universe());
        assert_eq!(sim.deliveries(0), M as u64, "seed={seed}");
        assert_eq!(sim.served[3], 0, "the omitter served nothing");
        assert_eq!(sim.served[1] + sim.served[2], M as u64);
    }
}

/// Freshness, composed: after convergence, uploads land at single nodes —
/// LIVE to every peer (BinIDs past all cursor snapshots) — and spread
/// through the standing tail offers without any new round-trip setup.
#[test]
fn live_arrivals_spread_to_all_nodes() {
    let k = 4usize;
    let mut sim = Sim::new(k, &[], 7);
    for n in 0..M {
        sim.seed(n as usize % k, t(n));
    }
    sim.start();
    sim.run();
    sim.assert_converged(&universe());
    let before: u64 = (0..k).map(|i| sim.deliveries(i)).sum();

    // two uploads land at different nodes, in different bins
    sim.arrive(0, t(100));
    sim.arrive(2, t(101));
    assert_ne!(bin_of(t(100)), bin_of(t(101)));
    sim.run();

    for i in 0..k {
        assert!(
            sim.node_has(i, t(100)) && sim.node_has(i, t(101)),
            "node {i} missing a LIVE chunk"
        );
        assert_eq!(sim.deficit(i), 0);
    }
    let after: u64 = (0..k).map(|i| sim.deliveries(i)).sum();
    assert_eq!(
        after - before,
        2 * (k as u64 - 1),
        "LIVE spread at the floor"
    );
    sim.assert_invariants();
}

/// Small-gap re-sync: a returning node misses only a few chunks — the other
/// regime the design names (g ≪ M). The floor holds at the gap size.
#[test]
fn small_gap_resync_fetches_only_the_gap() {
    for seed in 0..5u64 {
        let mut sim = Sim::new(3, &[], seed);
        let gap: Vec<Triple> = vec![t(5), t(11), t(17)];
        for c in universe() {
            sim.seed(1, c);
            sim.seed(2, c);
            if !gap.contains(&c) {
                sim.seed(0, c); // the returning node holds all but the gap
            }
        }
        sim.start();
        sim.run();

        sim.assert_converged(&universe());
        assert_eq!(
            sim.deliveries(0),
            gap.len() as u64,
            "seed={seed}: fetch the gap, only"
        );
    }
}

/// The bounded working set: after a cold start converges, the scheduling
/// state is pruned back to ~0 even though the node still HOLDS every chunk.
/// Memory tracks the open offer window, not all history — the property the
/// design specifies and the network layer will stress.
#[test]
fn working_set_returns_to_zero_after_convergence() {
    let k = 4usize;
    let mut sim = Sim::new(k, &[], 3);
    for i in 1..k {
        for c in universe() {
            sim.seed(i, c);
        }
    }
    sim.start();
    sim.run();
    sim.assert_converged(&universe());
    // the empty node fetched and still holds all M chunks ...
    for c in universe() {
        assert!(sim.node_has(0, c));
    }
    // ... but its scheduling working set has drained: settled ids pruned.
    assert_eq!(
        sim.working_set(0),
        0,
        "working set must prune to 0 after convergence"
    );
    // the full servers never tracked scheduling state for their own holdings.
    for i in 1..k {
        assert_eq!(sim.working_set(i), 0, "server {i} working set");
    }
}

// --- fairness ablations: the O5 floor is floor-achieving, not gate-critical,
//     so removing a policy keeps CORRECTNESS (still converges, each chunk
//     once) but breaks the BALANCE. These are the negatives that make the
//     fairness mechanisms falsifiable, matching the gate-critical TLA
//     ablations — the property lives in the sim, where it is observable, not
//     in TLA, where a distributional floor cannot be expressed. -------------

/// Ablate cumulative routing (outstanding-load-only — history-blind). The
/// cold-start backlog still converges at the delivery floor, but serve load
/// skews across scheduling waves on a measurable fraction of message
/// orderings: ~11/40 seeds, worst `[11,7,6]` (skew 5) — the `[6,6,12]` class
/// the M2 cumulative-routing fix removed. The shipped policy holds
/// max−min ≤ 1 on every seed (`cold_start_floors_across_k_and_seeds`); this
/// is the matching negative. The 0..40 range is deterministic — the skew it
/// catches is fixed, not flaky.
#[test]
fn outstanding_only_routing_breaks_the_fairness_floor() {
    let off = Policy {
        cumulative_routing: false,
        discovery_barrier: true,
    };
    let mut skewed = 0;
    for seed in 0..40u64 {
        let mut sim = Sim::with_policy(4, &[], seed, off);
        for i in 1..4 {
            for c in universe() {
                sim.seed(i, c);
            }
        }
        sim.start();
        sim.run();
        // correctness survives — fairness is floor-achieving, not gate-critical
        sim.assert_converged(&universe());
        assert_eq!(sim.deliveries(0), M as u64);
        let served: Vec<u64> = (1..4).map(|i| sim.served[i]).collect();
        assert_eq!(served.iter().sum::<u64>(), M as u64);
        let (max, min) = (*served.iter().max().unwrap(), *served.iter().min().unwrap());
        if max - min > 1 {
            skewed += 1;
        }
    }
    assert!(
        skewed > 0,
        "outstanding-only routing must skew serve load on some ordering"
    );
}

/// Ablate the discovery barrier (schedule before the choice set assembles).
/// Still converges, but the first peer through discovery is treated as the
/// whole choice set and is handed a disproportionate share — the floor breaks.
#[test]
fn no_discovery_barrier_breaks_the_fairness_floor() {
    let off = Policy {
        cumulative_routing: true,
        discovery_barrier: false,
    };
    let mut found_skew = false;
    for seed in 0..5u64 {
        let mut sim = Sim::with_policy(4, &[], seed, off);
        for i in 1..4 {
            for c in universe() {
                sim.seed(i, c);
            }
        }
        sim.start();
        sim.run();
        sim.assert_converged(&universe());
        assert_eq!(sim.deliveries(0), M as u64);
        let served: Vec<u64> = (1..4).map(|i| sim.served[i]).collect();
        let (max, min) = (*served.iter().max().unwrap(), *served.iter().min().unwrap());
        if max - min > 1 {
            found_skew = true;
        }
    }
    assert!(
        found_skew,
        "no discovery barrier must skew serve load on some seed"
    );
}
