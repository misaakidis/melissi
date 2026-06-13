//! The parity matrix: `optimal-testbed/run.sh` row for row, on the shipped
//! machine. Constants are copied from the `MC_*.tla` configs verbatim; the
//! distinct-state counts are asserted EQUAL to TLC's — the state spaces are
//! the same by construction (budgets included; `live`/`load`/`ndeliv` are
//! derived), so any divergence is a translation bug, found here.
//!
//! Liveness verdicts are finite, per the suite's fairness arguments:
//! completeness ✗ = an incomplete terminal (stuck) or a reachable cycle
//! (weak-fairness counterexample — the `noexclude` re-grab livelock);
//! freshness ✗ = a terminal missing a LIVE arrival.
//!
//! `MC_atomic` / `MC_nonatomic` have no row here: the atomicity obligation is
//! discharged by `&mut self` (a single-owner machine cannot exhibit the
//! check/mark split), which is the point of `PullSyncerNA`.

use melissi_machine::explore::{explore, Report, Scenario};
use melissi_machine::Config;

const ALL_ON: Config = Config {
    dedup: true,
    failover: true,
    exclude: true,
    reset_on_exhaust: true,
    single_source: false,
    priority: false, // matrix baseline: Priority off, as in MC_base
    enable_live: true,
};

const FULL3: &[(u8, &[u32])] = &[(1, &[1, 2, 3]), (2, &[1, 2, 3]), (3, &[1, 2, 3])];
const PARTIAL: &[(u8, &[u32])] = &[(1, &[1, 2]), (2, &[2, 3]), (3, &[3])];
const OMISSION: &[(u8, &[u32])] = &[(1, &[1, 3]), (2, &[2, 3]), (3, &[1, 2])];
const CHURN3: &[(u8, &[u32])] = &[(1, &[1, 2]), (2, &[2, 3]), (3, &[1, 3])];

fn scenario(name: &'static str) -> Scenario {
    Scenario {
        name,
        cfg: ALL_ON,
        chunks: &[1, 2, 3],
        peers: &[1, 2, 3],
        holds: FULL3,
        byzantine: &[],
        live: &[],
        prio: &[],
        assign: &[],
        timeout_budget: 0,
        churn_budget: 0,
    }
}

fn assert_positive(r: &Report, tlc_states: usize) {
    assert!(
        r.violation.is_none(),
        "{}: invariant violated: {:?}",
        r.name,
        r.violation
    );
    assert_eq!(r.incomplete_terminals, 0, "{}: incomplete terminal", r.name);
    assert_eq!(r.unfresh_terminals, 0, "{}: unfresh terminal", r.name);
    assert!(!r.has_cycle, "{}: livelock cycle", r.name);
    assert!(r.terminals > 0, "{}: no terminal state", r.name);
    assert_eq!(
        r.states, tlc_states,
        "{}: state count diverges from TLC",
        r.name
    );
}

// --- positives (mirroring Table A1) -----------------------------------------

#[test]
fn base() {
    // MC_base: honest peers, full replication. TLC: 125 distinct states.
    assert_positive(&explore(&scenario("base")), 125);
}

#[test]
fn partial() {
    // MC_partial: chunk 1 on a single honest holder. TLC: 48.
    let mut sc = scenario("partial");
    sc.holds = PARTIAL;
    assert_positive(&explore(&sc), 48);
}

#[test]
fn omission() {
    // MC_omission: Byzantine omitter 3; every chunk also on an honest holder. TLC: 196.
    let mut sc = scenario("omission");
    sc.holds = OMISSION;
    sc.byzantine = &[3];
    assert_positive(&explore(&sc), 196);
}

#[test]
fn vicinity() {
    // MC_vicinity: deepest-first ordering on — correctness-neutral. TLC: 85.
    let mut sc = scenario("vicinity");
    sc.cfg.priority = true;
    sc.prio = &[(1, 1), (2, 2), (3, 3)];
    assert_positive(&explore(&sc), 85);
}

#[test]
fn live() {
    // MC_live: chunk 3 arrives post-cutoff and must still be fetched. TLC: 150.
    let mut sc = scenario("live");
    sc.live = &[3];
    assert_positive(&explore(&sc), 150);
}

#[test]
fn timeout() {
    // MC_timeout: two spurious timeouts, one on chunk 1's ONLY holder;
    // reset-on-exhaustion recovers. TLC: 644.
    let mut sc = scenario("timeout");
    sc.holds = PARTIAL;
    sc.timeout_budget = 2;
    assert_positive(&explore(&sc), 644);
}

#[test]
fn churn() {
    // MC_churn: two supply-preserving churn events + omitter. TLC: 7,739.
    let mut sc = scenario("churn");
    sc.holds = CHURN3;
    sc.byzantine = &[3];
    sc.churn_budget = 2;
    assert_positive(&explore(&sc), 7739);
}

#[test]
fn scale() {
    // MC_scale: k = 6, two Byzantine omitters, full replication. TLC: 21,952.
    let mut sc = scenario("scale");
    sc.peers = &[1, 2, 3, 4, 5, 6];
    sc.holds = &[
        (1, &[1, 2, 3]),
        (2, &[1, 2, 3]),
        (3, &[1, 2, 3]),
        (4, &[1, 2, 3]),
        (5, &[1, 2, 3]),
        (6, &[1, 2, 3]),
    ];
    sc.byzantine = &[5, 6];
    assert_positive(&explore(&sc), 21952);
}

#[test]
#[ignore = "722,847 states — the k=4 composite; run with `cargo test -- --ignored`"]
fn storm() {
    // MC_storm: omitter + claim-stall, a LIVE arrival that is also deepest-
    // priority, a single-holder chunk, one misfire, one churn event. TLC: 722,847.
    let mut sc = scenario("storm");
    sc.chunks = &[1, 2, 3, 4];
    sc.peers = &[1, 2, 3, 4];
    sc.holds = &[
        (1, &[1, 2, 4]),
        (2, &[2, 3, 4]),
        (3, &[3, 4]),
        (4, &[1, 2, 3, 4]),
    ];
    sc.byzantine = &[4];
    sc.live = &[4];
    sc.cfg.priority = true;
    sc.prio = &[(1, 1), (2, 2), (3, 3), (4, 4)];
    sc.timeout_budget = 1;
    sc.churn_budget = 1;
    assert_positive(&explore(&sc), 722_847);
}

// --- ablations (each knob off breaks exactly its property) -------------------

#[test]
fn nodedup() {
    // MC_nodedup: dedup OFF, full replication -> double delivery -> ConflictFree.
    let mut sc = scenario("nodedup");
    sc.cfg.dedup = false;
    let r = explore(&sc);
    let (name, trace) = r.violation.expect("nodedup must violate an invariant");
    assert_eq!(
        name, "ConflictFree",
        "wrong invariant: {name} (trace: {trace:?})"
    );
}

#[test]
fn nofailover() {
    // MC_nofailover: a claim on the omitter is stuck forever -> Completeness.
    let mut sc = scenario("nofailover");
    sc.holds = OMISSION;
    sc.byzantine = &[3];
    sc.cfg.failover = false;
    let r = explore(&sc);
    assert!(
        r.violation.is_none(),
        "unexpected invariant violation: {:?}",
        r.violation
    );
    assert!(
        r.incomplete_terminals > 0,
        "expected a stuck incomplete terminal"
    );
}

#[test]
fn noexclude() {
    // MC_noexclude: the staller re-grabs the released claim forever -> a
    // reachable cycle: the weak-fairness counterexample (livelock).
    let mut sc = scenario("noexclude");
    sc.holds = OMISSION;
    sc.byzantine = &[3];
    sc.cfg.exclude = false;
    let r = explore(&sc);
    assert!(
        r.violation.is_none(),
        "unexpected invariant violation: {:?}",
        r.violation
    );
    assert!(r.has_cycle, "expected the re-grab livelock cycle");
}

#[test]
fn noreset() {
    // MC_noreset: permanent bars + one misfire on chunk 1's only holder ->
    // the chunk is stranded -> Completeness.
    let mut sc = scenario("noreset");
    sc.holds = PARTIAL;
    sc.timeout_budget = 1;
    sc.cfg.reset_on_exhaust = false;
    let r = explore(&sc);
    assert!(
        r.violation.is_none(),
        "unexpected invariant violation: {:?}",
        r.violation
    );
    assert!(r.incomplete_terminals > 0, "expected a stranded chunk");
}

#[test]
fn single_omission() {
    // MC_single_omission: chunk 1's only allowed source is the omitter.
    let mut sc = scenario("single_omission");
    sc.holds = OMISSION;
    sc.byzantine = &[3];
    sc.cfg.single_source = true;
    sc.assign = &[(1, 3), (2, 2), (3, 1)];
    let r = explore(&sc);
    assert!(
        r.violation.is_none(),
        "unexpected invariant violation: {:?}",
        r.violation
    );
    assert!(
        r.incomplete_terminals > 0,
        "expected incompleteness under omission"
    );
}

#[test]
fn single_partial() {
    // MC_single_partial: chunk 1 assigned to a peer that does not hold it.
    let mut sc = scenario("single_partial");
    sc.holds = OMISSION;
    sc.cfg.single_source = true;
    sc.assign = &[(1, 2), (2, 2), (3, 1)];
    let r = explore(&sc);
    assert!(
        r.violation.is_none(),
        "unexpected invariant violation: {:?}",
        r.violation
    );
    assert!(
        r.incomplete_terminals > 0,
        "expected incompleteness under partial holdings"
    );
}

#[test]
fn no_live() {
    // MC_no_live: LIVE fetch disabled; the post-cutoff arrival is never
    // fetched -> Freshness (safety stays green).
    let mut sc = scenario("no_live");
    sc.live = &[3];
    sc.cfg.enable_live = false;
    let r = explore(&sc);
    assert!(
        r.violation.is_none(),
        "unexpected invariant violation: {:?}",
        r.violation
    );
    assert!(r.unfresh_terminals > 0, "expected an unfresh terminal");
}

// --- the deviation, pinned ----------------------------------------------------

#[test]
fn priority_does_not_block_on_unfetchable() {
    // The flagged prio_ok deviation: an unfetchable deep chunk (no eligible
    // holder anywhere) must not head-of-line block shallower bins. The
    // verbatim model guard would wedge here; the weakened guard completes
    // everything fetchable. (Outside the TLC matrix: supply is deliberately
    // broken for chunk 3, so SupplyInv is not asserted.)
    use melissi_machine::PullState;
    let mut m = PullState::new(Config {
        priority: true,
        ..ALL_ON
    });
    // chunk 3 is deepest but has NO holders; chunks 1, 2 are held by peer 1.
    m.set_prio(1, 1);
    m.set_prio(2, 2);
    m.set_prio(3, 3);
    m.observe_holder(1, 1);
    m.observe_holder(2, 1);
    m.arrive_hist(1);
    m.arrive_hist(2);
    m.arrive_hist(3);
    // deepest-first still orders the eligible chunks: 2 before 1 …
    assert!(
        !m.claimable(1, 1),
        "chunk 2 (deeper, eligible) must go first"
    );
    assert!(m.want(2, 1));
    assert!(m.deliver(2, 1));
    // … and chunk 3 (unfetchable) never blocks chunk 1.
    assert!(
        m.want(1, 1),
        "unfetchable deep chunk must not head-of-line block"
    );
    assert!(m.deliver(1, 1));
    assert_eq!(m.deficit(), 1); // only the unfetchable chunk remains
    m.check_invariants().unwrap();
}
