//! The neighbourhood-supply parity matrix: `Neighbourhood.tla`'s configs, row
//! for row, on the shipped policy. Constants are copied from the `MC_nhood*.tla`
//! configs verbatim (`Willing = 3`, `Declining = 1`); the distinct-state counts
//! are asserted EQUAL to TLC's — the transition systems are the same by
//! construction, so any divergence is a translation bug, caught here. Liveness
//! verdicts are finite (the system runs to fixed points; see `explore`).

use melissi_neighbourhood::explore::explore;
use melissi_neighbourhood::{Neighbourhood, Policy};

const WILLING: u32 = 3; // honest neighbours in the tile (the supply)
const DECLINING: u32 = 1; // one declining/unreachable peer

// the shipped end-state oracle: the whole tile discovered AND the whole honest
// supply connected.
fn assembled(n: &Neighbourhood) -> bool {
    n.discovery_finds() && n.supply_complete()
}

/// MC_nhood (positive): the shipped policy assembles the supply — discovery
/// finds the whole tile and the node connects all 3 honest neighbours. The
/// reachable state space matches TLC's 13 distinct states.
#[test]
fn mc_nhood_positive() {
    let r = explore(WILLING, DECLINING, Policy::SHIPPED, assembled);
    assert_eq!(r.distinct, 13, "distinct-state parity with TLC MC_nhood");
    assert!(r.safe, "ConnLeKnown holds at every reachable state");
    assert_eq!(
        r.bad_terminals, 0,
        "every terminal has the full supply assembled"
    );
    assert!(r.terminals >= 1);
}

/// MC_nhood_nogossip: Gossip OFF — a connected node never learns past its
/// bootstrap peer, so the rest of the neighbourhood is never discovered.
/// DiscoveryFinds fails. TLC: 2 states.
#[test]
fn mc_nhood_nogossip_breaks_discovery() {
    let nogossip = Policy {
        gossip: false,
        connect_all: true,
    };
    let r = explore(WILLING, DECLINING, nogossip, Neighbourhood::discovery_finds);
    assert_eq!(
        r.distinct, 2,
        "distinct-state parity with TLC MC_nhood_nogossip"
    );
    assert!(r.safe);
    assert!(
        r.bad_terminals > 0,
        "without the feedback loop the neighbourhood is never discovered"
    );
}

/// MC_nhood_noconnect: ConnectAll OFF — the node connects only enough to
/// bootstrap (its seed) and stops, so the supply is one peer, not the
/// neighbourhood. SupplyComplete fails (the single-source dependency). Discovery
/// still completes. TLC: 7 states.
#[test]
fn mc_nhood_noconnect_breaks_supply() {
    let noconnect = Policy {
        gossip: true,
        connect_all: false,
    };
    let r = explore(
        WILLING,
        DECLINING,
        noconnect,
        Neighbourhood::supply_complete,
    );
    assert_eq!(
        r.distinct, 7,
        "distinct-state parity with TLC MC_nhood_noconnect"
    );
    assert!(r.safe);
    assert!(
        r.bad_terminals > 0,
        "connecting only the seed leaves a single-source supply"
    );
    // exactly one property breaks: discovery still finds the whole tile.
    assert_eq!(
        explore(
            WILLING,
            DECLINING,
            noconnect,
            Neighbourhood::discovery_finds
        )
        .bad_terminals,
        0
    );
}
