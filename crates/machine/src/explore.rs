//! An exhaustive explorer over the shipped machine — the TLC analogue, so the
//! parity matrix re-checks `formal-models/tla/run.sh` row for row on this code.
//!
//! The explorer plays the model's ENVIRONMENT: it enumerates every enabled
//! action (the machine's own `Want`s plus honest `Deliver`s, budget-bounded
//! stalls, churn, LIVE arrivals, resets) and explores all interleavings.
//! Budgets (`tmo`, `chn`) live here, never in the machine — they are the
//! spec's proof device for bounding misattribution and churn.
//!
//! Verdicts mirror TLC's, with liveness made finite the way the suite's
//! fairness arguments justify:
//!   - safety: every reachable state passes the invariants (machine-local +
//!     `SupplyInv` + `NoFalseExclusion`);
//!   - completeness: every terminal state has all chunks stored, and the
//!     reachable graph is acyclic — under the positive knob settings every
//!     action strictly grows a monotone component, so a cycle exists exactly
//!     when weak fairness has a counterexample (the `noexclude` re-grab
//!     livelock);
//!   - freshness: every terminal state has the LIVE arrivals stored.

use crate::{Config, PeerId, PullState};

/// The abstract chunk identity for model-checking: small, enumerable, exact
/// state counts. The real wire instantiates the same machine over
/// `melissi_types::Cid`; the explorer only needs distinct, ordered tokens.
pub type Cid = u32;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

/// One row of the matrix: the constants of an `MC_*.tla` config.
#[derive(Clone)]
pub struct Scenario {
    pub name: &'static str,
    pub cfg: Config,
    pub chunks: &'static [Cid],
    pub peers: &'static [PeerId],
    /// `Holds`: per-peer initial holdings.
    pub holds: &'static [(PeerId, &'static [Cid])],
    pub byzantine: &'static [PeerId],
    /// `LiveChunks`: arrive during sync via `NewChunk`.
    pub live: &'static [Cid],
    /// `Prio`: bin depth per triple (only read when `cfg.priority`).
    pub prio: &'static [(Cid, u8)],
    /// `Assign`: the single-source ablation's fixed source.
    pub assign: &'static [(Cid, PeerId)],
    /// `TimeoutBudget`: max spurious stalls of honest peers.
    pub timeout_budget: u8,
    /// `ChurnBudget`: max Lose/Gain events, supply-preserving.
    pub churn_budget: u8,
}

impl Scenario {
    fn honest(&self, p: PeerId) -> bool {
        !self.byzantine.contains(&p)
    }
}

/// The explored state: the machine plus the environment's budgets.
#[derive(Clone, PartialEq, Eq, Hash)]
struct World {
    m: PullState<Cid>,
    tmo: u8,
    chn: u8,
}

/// Every action the environment can take — the model's `Next` disjuncts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Want(Cid, PeerId),
    Deliver(Cid, PeerId),
    /// `ByzStall` or `SpuriousTimeout` — one verb; honesty + budget decide
    /// enabledness here, in the environment, exactly as in the spec.
    Stall(Cid, PeerId),
    Lose(Cid, PeerId),
    Gain(Cid, PeerId),
    New(Cid),
    Reset(Cid),
}

pub struct Report {
    pub name: &'static str,
    pub states: usize,
    /// First invariant violation found, with the trace that reaches it.
    pub violation: Option<(&'static str, Vec<Act>)>,
    pub terminals: usize,
    /// Terminal states missing some chunk (completeness failures).
    pub incomplete_terminals: usize,
    /// Terminal states missing some LIVE arrival (freshness failures).
    pub unfresh_terminals: usize,
    /// A reachable cycle: a weak-fairness counterexample (livelock).
    pub has_cycle: bool,
}

fn init(sc: &Scenario) -> World {
    let mut m = PullState::<Cid>::new(sc.cfg);
    for &(p, cs) in sc.holds {
        for &c in cs {
            m.observe_holder(c, p);
        }
    }
    for &(c, d) in sc.prio {
        m.set_prio(c, d);
    }
    for &(c, p) in sc.assign {
        m.set_assign(c, p);
    }
    for &c in sc.chunks {
        if !sc.live.contains(&c) {
            m.arrive_hist(c);
        }
    }
    World {
        m,
        tmo: sc.timeout_budget,
        chn: sc.churn_budget,
    }
}

fn enabled(sc: &Scenario, w: &World) -> Vec<Act> {
    let mut acts = Vec::new();
    // Want(c, p) — the machine's own nondeterminism.
    for (c, p) in w.m.enabled_wants() {
        acts.push(Act::Want(c, p));
    }
    for (&c, ps) in &w.m.want {
        for &p in ps {
            // Deliver(c, p): only an honest holder answers.
            if sc.honest(p) {
                acts.push(Act::Deliver(c, p));
            }
            // Stall(c, p): a Byzantine claim-holder may stall at will; an
            // honest one only by a spurious timeout, within budget.
            if w.m.cfg().failover && (!sc.honest(p) || w.tmo > 0) {
                acts.push(Act::Stall(c, p));
            }
        }
    }
    // NewChunk(c): a LIVE arrival.
    for &c in sc.live {
        if !w.m.has(c) && !w.m.is_rejected(c) && !w.m.arrived.contains(&c) {
            acts.push(Act::New(c));
        }
    }
    // Churn, supply-preserving, within budget.
    if w.chn > 0 {
        for &c in sc.chunks {
            if w.m.has(c) {
                continue;
            }
            let holders: Vec<PeerId> =
                w.m.holders
                    .get(&c)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
            for &p in &holders {
                // Lose(c, p): another honest holder must survive the loss.
                if holders.iter().any(|&q| q != p && sc.honest(q)) {
                    acts.push(Act::Lose(c, p));
                }
            }
            for &p in sc.peers {
                if !holders.contains(&p) {
                    acts.push(Act::Gain(c, p));
                }
            }
        }
    }
    // ResetExcluded(c): bars cover every current holder, nothing in flight.
    for &c in sc.chunks {
        if w.m.can_reset_excluded(c) {
            acts.push(Act::Reset(c));
        }
    }
    acts
}

fn apply(sc: &Scenario, w: &World, act: Act) -> World {
    let mut next = w.clone();
    let ok = match act {
        Act::Want(c, p) => next.m.want(c, p),
        Act::Deliver(c, p) => next.m.deliver(c, p),
        Act::Stall(c, p) => {
            if sc.honest(p) {
                next.tmo -= 1; // a spurious timeout consumes the budget
            }
            next.m.stall(c, p)
        }
        Act::Lose(c, p) => {
            next.chn -= 1;
            next.m.lose_holder(c, p)
        }
        Act::Gain(c, p) => {
            next.chn -= 1;
            next.m.observe_holder(c, p)
        }
        Act::New(c) => next.m.arrive_live(c),
        Act::Reset(c) => next.m.reset_excluded(c),
    };
    debug_assert!(ok, "applied a disabled action: {act:?}");
    next
}

/// Environment-aware invariants the machine cannot state alone.
fn check_env_invariants(sc: &Scenario, w: &World) -> Result<(), &'static str> {
    w.m.check_invariants()?;
    // SupplyInv: churn never strips a chunk of its last honest holder.
    for &c in sc.chunks {
        let ok =
            w.m.holders
                .get(&c)
                .map(|hs| hs.iter().any(|&p| sc.honest(p)))
                .unwrap_or(false);
        if !ok {
            return Err("SupplyInv");
        }
    }
    // NoFalseExclusion: with perfect attribution (budget 0), only Byzantine
    // peers are ever barred.
    if sc.timeout_budget == 0 {
        for ps in w.m.excluded.values() {
            if ps.iter().any(|&p| sc.honest(p)) {
                return Err("NoFalseExclusion");
            }
        }
    }
    Ok(())
}

/// Exhaustive BFS over all interleavings; then cycle detection on the
/// reachable graph (iterative three-colour DFS).
pub fn explore(sc: &Scenario) -> Report {
    let chunks: BTreeSet<Cid> = sc.chunks.iter().copied().collect();
    let live: BTreeSet<Cid> = sc.live.iter().copied().collect();

    let mut ids: HashMap<World, u32> = HashMap::new();
    let mut adj: Vec<Vec<u32>> = Vec::new();
    let mut parent: Vec<Option<(u32, Act)>> = Vec::new();
    let mut queue: VecDeque<u32> = VecDeque::new();
    let mut worlds: Vec<World> = Vec::new();

    let w0 = init(sc);
    ids.insert(w0.clone(), 0);
    worlds.push(w0);
    adj.push(Vec::new());
    parent.push(None);
    queue.push_back(0);

    let mut report = Report {
        name: sc.name,
        states: 0,
        violation: None,
        terminals: 0,
        incomplete_terminals: 0,
        unfresh_terminals: 0,
        has_cycle: false,
    };

    let trace_to = |parent: &Vec<Option<(u32, Act)>>, mut id: u32| -> Vec<Act> {
        let mut acts = Vec::new();
        while let Some((prev, act)) = parent[id as usize] {
            acts.push(act);
            id = prev;
        }
        acts.reverse();
        acts
    };

    while let Some(id) = queue.pop_front() {
        let w = worlds[id as usize].clone();

        if let Err(name) = check_env_invariants(sc, &w) {
            report.states = ids.len();
            report.violation = Some((name, trace_to(&parent, id)));
            return report;
        }

        let acts = enabled(sc, &w);
        if acts.is_empty() {
            report.terminals += 1;
            if !chunks.iter().all(|&c| w.m.has(c)) {
                report.incomplete_terminals += 1;
            }
            if !live.iter().all(|&c| w.m.has(c)) {
                report.unfresh_terminals += 1;
            }
            continue;
        }

        for act in acts {
            let next = apply(sc, &w, act);
            let nid = *ids.entry(next.clone()).or_insert_with(|| {
                let nid = worlds.len() as u32;
                worlds.push(next);
                adj.push(Vec::new());
                parent.push(Some((id, act)));
                queue.push_back(nid);
                nid
            });
            adj[id as usize].push(nid);
        }
    }

    report.states = ids.len();
    report.has_cycle = has_cycle(&adj);
    report
}

/// Iterative three-colour DFS: 0 = white, 1 = on stack (grey), 2 = done.
fn has_cycle(adj: &[Vec<u32>]) -> bool {
    let mut colour = vec![0u8; adj.len()];
    let mut stack: Vec<(u32, usize)> = Vec::new();
    for start in 0..adj.len() as u32 {
        if colour[start as usize] != 0 {
            continue;
        }
        colour[start as usize] = 1;
        stack.push((start, 0));
        while let Some(&mut (node, ref mut edge)) = stack.last_mut() {
            if *edge < adj[node as usize].len() {
                let next = adj[node as usize][*edge];
                *edge += 1;
                match colour[next as usize] {
                    0 => {
                        colour[next as usize] = 1;
                        stack.push((next, 0));
                    }
                    1 => return true, // back edge: a reachable cycle
                    _ => {}
                }
            } else {
                colour[node as usize] = 2;
                stack.pop();
            }
        }
    }
    false
}

/// Convenience map used by tests to express full replication.
pub fn full_replication(
    peers: &'static [PeerId],
    chunks: &'static [Cid],
) -> BTreeMap<PeerId, Vec<Cid>> {
    peers.iter().map(|&p| (p, chunks.to_vec())).collect()
}
