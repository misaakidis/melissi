//! neighbourhood — assembling the pull-sync supply.
//!
//! A 1:1 refinement of `formal-models/tla/Neighbourhood.tla`. The module is the
//! spec of record; this file is the refinement, and `tests/parity.rs` re-runs
//! its ablation matrix on the shipped policy and asserts the distinct-state
//! count equals TLC's — the same translation-fidelity oracle `machine` uses.
//!
//! **What it is.** PullSyncerE assumes SUPPLY (design §3: every reserve chunk is
//! held by some honest neighbour the node can reach). This crate discharges that
//! premise: it discovers and connects the honest peers of the node's
//! neighbourhood, the way `settlement` discharges the resume layer.
//!
//! **Grounded in the decomposition (§4).** The depth-`D` partition makes
//! pull-sync the direct sum of independent neighbourhoods; the analysis is one
//! tile of `k ∈ [2,8]` peers sharing the node's depth-`D` prefix, and the supply
//! for that tile is its honest peers. So this models exactly that tile — not
//! proximity bins, not routing. (Kademlia routing across the whole address space
//! is a *different* problem, used by retrieval, not pull-sync; it is a deferred
//! companion. The neighbourhood is structural locality, not Kademlia routing.)
//!
//! **The coupling that makes it non-trivial.** Discovery and connection depend on
//! each other: the node knows only its bootstrap peer until it connects, and only
//! a connected node learns more (the hive `peers` push — `net::hive`). The seed
//! breaks the cycle. And not every neighbour connects — peers split WILLING
//! (honest/reachable) and DECLINING (a real testnet bee declines a light peer);
//! the node must connect *all* the willing ones, not stop at the first (a single
//! connected holder is the single-source dependency §5.1 removes).
//!
//! Mapping to `Neighbourhood.tla`:
//!
//! | `Neighbourhood.tla`                  | here                               |
//! |--------------------------------------|------------------------------------|
//! | `knownW`, `knownU`, `conn`           | `known_w`, `known_u`, `conn`       |
//! | `Willing`, `Declining`               | `willing`, `declining`             |
//! | `Gossip`, `ConnectAll`               | [`Policy`] knobs (ablation only)    |
//! | `Bootstrapped`                       | [`Neighbourhood::bootstrapped`]     |
//! | `DiscoverW`/`DiscoverU`/`Connect`    | the methods of the same name — the only mutators |
//! | `ConnLeKnown` (safety)               | [`Neighbourhood::conn_le_known`]    |
//! | `DiscoveryFinds`, `SupplyComplete`   | the predicates of the same name, checked at terminals by the explorer |
//!
//! The actions are the only mutators (`&mut self`, no setters). The knobs exist
//! to be *ablated*: shipped is [`Policy::SHIPPED`]; turning one off reproduces
//! exactly the `MC_nhood_nogossip` / `MC_nhood_noconnect` counterexamples.

pub mod explore;

/// The two design choices, each a `Neighbourhood.tla` knob, each ablatable —
/// dropping either breaks a named property (the parity suite re-checks both).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// A connected node learns more neighbours (the hive feedback loop). OFF =
    /// the node never learns past its bootstrap peer (`DiscoveryFinds` fails).
    pub gossip: bool,
    /// Connect every honest neighbour, not merely enough to bootstrap. OFF = the
    /// node connects only its seed — a single-source supply (`SupplyComplete`
    /// fails; the dependency §5.1 removes).
    pub connect_all: bool,
}

impl Policy {
    pub const SHIPPED: Policy = Policy {
        gossip: true,
        connect_all: true,
    };
}

impl Default for Policy {
    fn default() -> Self {
        Self::SHIPPED
    }
}

/// The discovery + connection state of one neighbourhood tile: how many willing
/// (honest) neighbours have been discovered (`known_w`), how many declining
/// (`known_u`), and how many willing ones are connected (`conn`, the assembled
/// supply). `willing`/`declining` are the tile's (unknown-to-the-node) honest
/// and unreachable populations; the node starts knowing one willing peer (seed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbourhood {
    known_w: u32,
    known_u: u32,
    conn: u32,
    willing: u32,
    declining: u32,
    policy: Policy,
}

impl Neighbourhood {
    /// A node knowing only its bootstrap peer (one of the `willing` neighbours),
    /// facing a tile of `willing` honest and `declining` unreachable peers.
    pub fn new(willing: u32, declining: u32, policy: Policy) -> Self {
        assert!(willing >= 1, "the bootstrap peer is a willing neighbour");
        Neighbourhood {
            known_w: 1,
            known_u: 0,
            conn: 0,
            willing,
            declining,
            policy,
        }
    }

    pub fn conn(&self) -> u32 {
        self.conn
    }

    /// The explorer's state key: `(known_w, known_u, conn)`.
    pub fn state(&self) -> (u32, u32, u32) {
        (self.known_w, self.known_u, self.conn)
    }

    /// Restore a state produced by [`Neighbourhood::state`] (the explorer
    /// reconstructs a node to enumerate its successors). The populations and the
    /// policy are unchanged — only the discovered/connected counts are set.
    pub fn set_state(&mut self, (kw, ku, c): (u32, u32, u32)) {
        self.known_w = kw;
        self.known_u = ku;
        self.conn = c;
    }

    /// Connected to at least one neighbour — the precondition for learning more.
    pub fn bootstrapped(&self) -> bool {
        self.conn > 0
    }

    // --- actions (the only mutators) -----------------------------------------

    /// `DiscoverW`: a connected node learns one more willing neighbour.
    pub fn discover_w(&mut self) -> bool {
        if self.policy.gossip && self.bootstrapped() && self.known_w < self.willing {
            self.known_w += 1;
            true
        } else {
            false
        }
    }

    /// `DiscoverU`: a connected node learns one more declining neighbour.
    pub fn discover_u(&mut self) -> bool {
        if self.policy.gossip && self.bootstrapped() && self.known_u < self.declining {
            self.known_u += 1;
            true
        } else {
            false
        }
    }

    /// `Connect`: connect one more known willing neighbour. With `connect_all`,
    /// keep going to the whole honest neighbourhood; without it, connect only
    /// enough to bootstrap (the first) and stop — the single-source policy.
    pub fn connect(&mut self) -> bool {
        if self.conn < self.known_w && (self.policy.connect_all || !self.bootstrapped()) {
            self.conn += 1;
            true
        } else {
            false
        }
    }

    /// No action is enabled — discovery and connection have settled.
    pub fn terminal(&self) -> bool {
        let mut p = self.clone();
        !p.discover_w() && !p.discover_u() && !p.connect()
    }

    // --- the spec's properties, as predicates at a state ---------------------

    /// `ConnLeKnown` (safety): only connect discovered-and-willing neighbours.
    pub fn conn_le_known(&self) -> bool {
        self.conn <= self.known_w && self.known_w <= self.willing
    }

    /// `DiscoveryFinds`: discovery finds the whole neighbourhood.
    pub fn discovery_finds(&self) -> bool {
        self.known_w == self.willing && self.known_u == self.declining
    }

    /// `SupplyComplete`: the node connects every honest neighbour — the complete,
    /// redundant supply PullSyncerE consumes.
    pub fn supply_complete(&self) -> bool {
        self.conn == self.willing
    }
}
