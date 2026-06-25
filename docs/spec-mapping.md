# TLA+ ↔ Rust — the refinement, line for line

For the research half of the room. Every claim here is checkable: the spec of
record is `SWIPs/PULLSYNC/optimal-testbed/*.tla`; the Rust is `crates/*/src/lib.rs`.
The parity tests (`crates/machine/tests/parity.rs`) re-run the spec's ablation
matrix on the shipped code and assert the distinct-state counts equal TLC's.

Two specs refine into two crates:

| TLA+ module | Rust crate | what it pins |
|---|---|---|
| `PullSyncerE.tla` | `crates/machine` | the scheduling machine (schedule / dedup / failover / priority) |
| `IntervalSettlement.tla` | `crates/settlement` | the one durable write — settle before you forget |

---

## 1. State variables — `PullSyncerE` → `PullState<C>`

| `PullSyncerE.tla` | `machine/src/lib.rs` | note |
|---|---|---|
| `got ⊆ Chunks` | `got: BTreeSet<C>` (:96) | fetched + stored; owned here, store-backed in a node |
| `want ∈ [Chunks → SUBSET Peers]` | `want: BTreeMap<C, BTreeSet<PeerId>>` (:84) | the shared in-flight claim set; empty sets normalised away |
| `failed` | `excluded: BTreeMap<C, BTreeSet<PeerId>>` (:88) | per-chunk bars (the failover log) |
| `arrived ⊆ Chunks` | `arrived: BTreeSet<C>` (:90) | offered + unsettled |
| `holds ∈ [Peers → SUBSET Chunks]` | `holders: BTreeMap<C, BTreeSet<PeerId>>` (:86) | observed from offers; transposed key order |
| `conflict ∈ BOOLEAN` | `conflict: bool` (:102) | tripwire — must never latch (`ConflictFree`) |
| `ndeliv ∈ Nat` | `ndeliv: u32` (:104) | the O3 floor counter (`DeliveryFloor`) |
| `Prio`, `Assign` | `prio`, `assign` (:98, :100) | priority key; `assign` is single-source ablation only |
| (`LiveChunks` split) | `live`, `rejected`, `npre` (:92–108) | LIVE gate set; entry-faults; pre-held reserve view |

The transpose (`holds[p] → holders[c]`) and the empty-set normalisation are the
only structural deviations, and both are behaviour-preserving — `DedupInv`
compares states up to that normalisation.

---

## 2. The guard — `Claimable`, conjunct for conjunct

This is the centrepiece slide. The Rust is a transcription, not a paraphrase.

```tla
\* PullSyncerE.tla
Claimable(c, p) ==
  /\ c \in arrived
  /\ c \in holds[p]
  /\ c \notin got
  /\ p \notin want[c]
  /\ p \notin failed[c]
  /\ (Dedup => want[c] = {})
  /\ (SingleSource => p = Assign[c])
  /\ (c \in LiveChunks => EnableLive)
  /\ PrioOK(c)
```

```rust
// machine/src/lib.rs:238
pub fn claimable(&self, c: C, p: PeerId) -> bool {
    self.arrived.contains(&c)
        && set_contains(&self.holders, &c, p)
        && !self.got.contains(&c)
        && !set_contains(&self.want, &c, p)
        && !set_contains(&self.excluded, &c, p)
        && (!self.cfg.dedup || !self.want.contains_key(&c))
        && (!self.cfg.single_source || self.assign.get(&c) == Some(&p))
        && (!self.live.contains(&c) || self.cfg.enable_live)
        && self.prio_ok(c)
}
```

Nine conjuncts, same order. `(Dedup => want[c] = {})` becomes
`(!dedup || want.get(c).is_none())` — material implication transcribed directly.

### The one flagged deviation — `PrioOK`

```tla
\* PullSyncerE.tla
PrioOK(c) ==
  \/ ~Priority
  \/ \A d \in arrived : (Prio[d] > Prio[c]) => Addressed(d)
```

```rust
// machine/src/lib.rs:215 — quantified ONLY over chunks with >=1 eligible holder
fn prio_ok(&self, c: C) -> bool {
    if !self.cfg.priority { return true; }
    let pc = self.prio.get(&c).copied().unwrap_or(0);
    self.arrived.iter().all(|&d| {
        let pd = self.prio.get(&d).copied().unwrap_or(0);
        pd <= pc || !self.eligible(d) || self.addressed(d)   // + `!self.eligible(d)`
    })
}
```

The `!self.eligible(d)` disjunct is the **flagged deviation** (implementation doc
§3): the verbatim guard would let one *unfetchable* deep chunk head-of-line block
every shallower bin. Correctness-neutral, established by `MC_vicinity`
(machine/src/lib.rs:38–40, 215–224).

---

## 3. The actions — spec edge → Rust method

Each action mutates exactly the variables the spec's `UNCHANGED` clause leaves
out. `&mut self` + no setters makes the check-and-mark atomic by construction
(`PullSyncerNA`).

| `PullSyncerE` action | Rust | the mutation |
|---|---|---|
| `Want(c,p)` | `want()` (:253) | `Claimable` then insert into `want` |
| `Deliver(c,p)` | `deliver()` (:263) | guard `p ∈ want[c]`; latch `conflict` if `c ∈ got`; `got ∪= c`; `ndeliv += 1` |
| `ByzStall` / `SpuriousTimeout` | **one** `stall()` (:280) | gated by `failover`; release claim; bar iff `exclude` |
| `ResetExcluded(c)` | `reset_excluded()` (:307) + `can_reset_excluded()` (:315) | bars cover all current holders ∧ nothing in flight → clear |
| `Lose(c,p)` | `lose_holder()` (:180) | drop holding, release claim, **no bar** |
| `Gain(c,p)` | `observe_holder()` (:173) | a peer offers `c` |
| `NewChunk(c)` | `arrive_live()` (:196) | post-cutoff arrival (and `arrive_hist` for backlog) |

`ByzStall` and `SpuriousTimeout` are **one method on purpose** — the spec gives
them identical bodies because the puller cannot attribute a stall
(PullSyncerE.tla, the two actions; machine/src/lib.rs:276–289).

---

## 4. The invariants — spec property → Rust check

`PullState::check_invariants()` (machine/src/lib.rs:428) checks the machine-local
properties in the order the spec names them:

| `PullSyncerE` property | Rust | meaning |
|---|---|---|
| `ConflictFree == conflict = FALSE` | `if self.conflict { Err("ConflictFree") }` (:431) | exactly-once delivery |
| `DeliveryFloor == ndeliv = Card(got)` | `(ndeliv + npre) == got.len()` (:434) | the O3 floor (+ `npre` pre-held) |
| `DedupInv == Dedup => ∀c Card(want[c]) ≤ 1` | `dedup && want.values().any(len > 1)` (:438) | one claim per chunk |
| `ClaimsLive == ∀c,p∈want[c]: c∈holds[p] ∧ p∉failed[c]` | loop over `want` (:442) | every claim actionable |

`SupplyInv` and `NoFalseExclusion` are **environment-aware**, so they live with
the environment (explorer / sim), not on the machine — stated explicitly at
machine/src/lib.rs:426–427.

---

## 5. Settlement — `IntervalSettlement` → `PeerBinLog`

| `IntervalSettlement.tla` | `settlement/src/lib.rs` | note |
|---|---|---|
| `intv[p] ∈ Nat` | `interval: BinId` (u64) (:38) | a single high-water; disconnected ranges **unrepresentable** |
| `Log[p]` | `entries: BTreeMap<BinId, Triple>` (:41) | the offered window above the interval |
| `Settled(c) == c∈stored ∨ (RejectSettles ∧ c∈rejected)` | the `settled` predicate passed to `advance()` (:111) | caller chooses; the node passes stored-or-rejected |
| `Advance(p,x)` (settled prefix) | `advance(settled)` (:111) | largest `x ≤ topmost` with every entry `≤ x` settled |
| `NoDrop` | **structural** — entries leave only inside `advance`, only after the predicate (:103–132) | not a runtime check |
| `Monotone` | **structural** — `interval` only ever assigned a larger value | not a runtime check |
| `AdvanceComplete` | testable liveness — `settlement_parity_all_orders` (:166) | both windows drain |

The headline: where the spec *checks* `Monotone` and `NoDrop` as temporal/safety
properties, the Rust makes them **unrepresentable to violate** — the `u64` type
and the "entries leave only inside `advance`" structure discharge them by
construction (settlement/src/lib.rs:11–20). bee's `TestIntervalAdvancePrefixOnly`
obligation has no analogue here because a disconnected range cannot be built.

### Settlement ablations (Rust tests mirror the TLC negs)

| TLC neg | breaks | Rust test |
|---|---|---|
| `MC_settlement_eager` | `NoDrop` | (the type forbids eager advance; documented :15) |
| `MC_settlement_noreject` | `AdvanceComplete` | `noreject_wedges_behind_bad_entry` (:211) |

---

## 6. Where to look, fast

- Guard side-by-side: `PullSyncerE.tla` `Claimable` ↔ `machine/src/lib.rs:238`.
- Action bodies: the `UNCHANGED` clause in each `.tla` action ↔ the fields each
  Rust method leaves untouched.
- The proof that it's the *same* machine: `cargo test -p melissi-machine` →
  state counts equal TLC's (`base` 125, `omission` 196, `timeout` 644,
  `churn` 7,739, `scale` 21,952; `storm` 722,847 under `--ignored`).
