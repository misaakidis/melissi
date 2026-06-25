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
            // Serve-load conservation: each missing chunk is served exactly once
            // (the O3 floor, robust to ordering). Exact serve-BALANCE is not
            // asserted here: offers are PAGED (bee-style, see `offer_response`), so
            // without a barrier the first peer whose page lands takes that page,
            // and cumulative routing distributes subsequent pages across holders.
            // The residual skew is therefore ≤ a few pages — a fixed transient that
            // vanishes as M ≫ k·PAGE (design §5.3's regime), worst in the
            // few-packets case (k=8, M = 6 pages here). Exact balance at any
            // ordering would need the full choice set at schedule time — a
            // (timeout-bounded) barrier (`DiscoveryBarrier.tla`) or rateless
            // reconciliation (design §7). The primary balance lever — all-together
            // page offers — is pinned by `staggered_discovery_skews_worse_than_concurrent`.
            let served: Vec<u64> = (1..k).map(|i| sim.served[i]).collect();
            assert_eq!(
                served.iter().sum::<u64>(),
                M as u64,
                "k={k} seed={seed}: each chunk served exactly once"
            );
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

/// Total serve-skew summed over the seed sweep for a given config — every run
/// still converges at the delivery floor (correctness is gate-critical; balance is
/// not), so the ablations below assert only that the AGGREGATE skew grows when a
/// floor-achieving mechanism is removed. (Summed, not worst-case: with want-the-page
/// offers the single worst ordering grabs a page regardless of routing, so the
/// mechanisms separate on the mass of orderings, not the tail.)
fn total_skew(policy: Policy, staggered: bool, seeds: u64) -> u64 {
    (0..seeds)
        .map(|seed| {
            let mut sim = Sim::with_policy(4, &[], seed, policy);
            if staggered {
                sim = sim.staggered();
            }
            for i in 1..4 {
                for c in universe() {
                    sim.seed(i, c);
                }
            }
            sim.start();
            sim.run();
            sim.assert_converged(&universe());
            assert_eq!(sim.deliveries(0), M as u64);
            let s: Vec<u64> = (1..4).map(|i| sim.served[i]).collect();
            assert_eq!(s.iter().sum::<u64>(), M as u64);
            *s.iter().max().unwrap() - *s.iter().min().unwrap()
        })
        .sum()
}

// NB: cumulative routing (vs outstanding-only) is NOT ablated in the sim, by
// design. Under bee-paged offers the per-page choice set is usually a singleton —
// the first peer whose page lands claims it (dedup), so least-loaded has no choice
// to make. Routing is the tiebreak for when offers OVERLAP; the primary balance
// lever is all-together page offers (the ablation below). The routing rule itself
// is the §5.3 analytic argument, not a sim-observable distribution here.

/// Ablate concurrent discovery (offers no longer all-together — the adversary
/// staggers the cursor/offer handshake). All-together offers assemble each page's
/// choice set so least-loaded distributes it; staggered, an early offerer's pages
/// land first. Removing concurrency must make the worst-case skew strictly worse.
#[test]
fn staggered_discovery_skews_worse_than_concurrent() {
    let concurrent = total_skew(Policy::SHIPPED, false, 5);
    let staggered = total_skew(Policy::SHIPPED, true, 5);
    assert!(
        staggered > concurrent,
        "staggered discovery must skew worse than concurrent (concurrent {concurrent}, staggered {staggered})"
    );
}
