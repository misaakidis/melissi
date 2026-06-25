# Pull-sync, the verified core — a talk outline

Audience: Swarm research + bee engineers. ~30–40 min + demo.

**Thesis (say this first, say it last):**
> The machine schedules, settlement remembers, the node composes — and every
> line traces to a model-checked spec that we also run over real chunks against
> a live testnet bee.

"model-checked spec" is for research; "real chunks, live testnet bee" is for the
bee engineers. The whole talk is that one sentence, unpacked.

---

## 0. Why these three crates (30 sec)

melissi has 11 crates in three rings. Don't show all 11 — show the rings:

| ring | crates | what it is |
|---|---|---|
| **verified core** | `machine`, `settlement`, `node` | *what pull-sync is* |
| identity & wire | `types`, `crypto`, `protobuf`, `overlay`, `wire`, `net` | *how it talks to bee* |
| the harness | `sim`, `machine::explore` | *how we know it's right* |

`machine` and `settlement` are 1:1 refinements of two named TLA+ specs
(`PullSyncerE.tla`, `IntervalSettlement.tla`). `node` is the sans-io composition.
Everything else is plumbing or proof. So: these three *are* pull-sync.

---

## 1. The stakes (1 slide — design §1)

- A node must keep its **reserve** complete. Incomplete reserve → wrong
  `ReserveSample` → **slashing**. (design Abstract, §1 O1)
- **O1 completeness is a gate** — the design is either right or wrong.
- **O3 delivery floor** — each missing chunk downloaded exactly once.
- SWIP-25 framing for the bee half of the room: today's pull-sync sends a `Want`
  to *every* peer that offers a chunk → up to **k× redundant deliveries**. The
  fix is one shared in-flight check. No protocol change. (design §5.2)

This frames pull-sync as correctness-critical, not a perf nicety. Both audiences
buy in here.

---

## 2. The architecture in one diagram (1 slide)

```
        events  ─────────────▶  ┌───────────────────────────┐  ─────────────▶  effects
   (PeerSeen, OfferResult,      │          node             │   (GetCursors, Offer,
    FetchResult, Tick, …)       │   sans-io: pure fn         │    Fetch, Settled)
                                │   events → effects         │
                                └─────┬───────────────┬──────┘
                                      │               │
                          ┌───────────▼──┐      ┌─────▼──────────┐
                          │   machine    │      │  settlement    │
                          │  PullState<C>│      │  PeerBinLog     │
                          │  the brain   │      │  the memory     │
                          │ schedule/dedup│     │ settle-before-  │
                          │ failover/prio │     │ you-forget      │
                          └──────────────┘      └────────────────┘
                          PullSyncerE.tla       IntervalSettlement.tla
```

Key line to land: **`node` only ever mutates state through the machine's action
set — there are no setters.** (node/src/lib.rs — every arm ends in `round()`,
every mutation is a `self.m.<action>()`.)

---

## 3. `machine` — the verified brain (the centerpiece)

The killer slide is a **side-by-side**: TLA+ `Claimable` vs Rust `claimable()`,
conjunct for conjunct. (Pull it from `docs/spec-mapping.md`.)

Talking points:
- **Polymorphic in chunk identity `C`** (`Copy + Ord + Hash`). It schedules; it
  never inspects bytes — that bound *is* the §6.1 layering principle. So the
  **same code** is model-checked over `u32` and shipped over `Triple`. The
  refinement is literally the type instantiation. (machine/src/lib.rs:1–13, 130)
- **Actions are the only mutators** — `want`, `deliver`, `stall`, `reject`,
  `arrive_*`, `reset_excluded`, `observe/lose_holder`. `&mut self` makes the
  "check-and-mark is one indivisible step" obligation (`PullSyncerNA`) a
  **compile error to violate** — no await point splits a check from its mark.
  (machine/src/lib.rs:28–31, 252–259)
- **One verb for every non-delivery**: `stall()` is `ByzStall` *and*
  `SpuriousTimeout` — the puller can't tell slow-honest from Byzantine.
  (machine/src/lib.rs:276–289)

Then the **ablation matrix** — the payoff slide for research:

| flip off | property that breaks | TLC config |
|---|---|---|
| `dedup` | `ConflictFree` (double delivery) | `MC_nodedup` |
| `failover` | `Completeness` (claim stuck) | `MC_nofailover` |
| `exclude` | `Completeness` (re-grab livelock) | `MC_noexclude` |
| `reset_on_exhaust` | `Completeness` (one misfire strands a chunk) | `MC_noreset` |
| `enable_live` | `Freshness` (post-cutoff arrival never fetched) | `MC_no_live` |

`cargo test` re-runs each of these on the shipped machine and asserts the
distinct-state count equals TLC's. (crates/machine/tests/parity.rs)

---

## 4. `settlement` — settle before you forget (the best "aha")

- The interval is pull-sync's **only durable claim**: covering a BinID means
  *never offer me this range again* — so advancing it is **forgetting**, and the
  rule is **settle before you forget**. (settlement/src/lib.rs:1–6)
- **The type is the proof.** `interval: u64` — a single high-water — makes a
  disconnected range **unrepresentable**, so bee's `TestIntervalAdvancePrefixOnly`
  obligation *does not exist here*; `Monotone` and `NoDrop` are structural, not
  tested. (settlement/src/lib.rs:12, 34–42)
- For the bee half: this is `intervalstore` — `next()` = `Next()`, `interval` =
  `Next() − 1`. And §7's resume-at-gap problem is exactly why settlement is
  per-entry, not advance-to-Topmost: cross-peer dedup leaves gaps.
  (settlement/src/lib.rs:49–61; design §7 "exact resume")
- Ablations: eager advance breaks `NoDrop` (`MC_settlement_eager`); "rejections
  don't settle" wedges `AdvanceComplete` (`MC_settlement_noreject`). Both are
  Rust tests too. (settlement/src/lib.rs:206–223)

---

## 5. `node` — sans-io composition (where it gets real)

- One pure function: `handle(Event) -> Vec<Effect>`. Determinism is total: same
  events in, same effects out. The shell owns time/IO; the core owns every
  scheduling decision. (node/src/lib.rs:1–7, 184–185)
- **Table 8 split**: `Offer` IS advertisement, `Fetch` IS delivery, and `Fetch`
  wants **by reference** (explicit triples) — the clean semantics. bee's
  positional bitvector + re-offer is confined to the `wire` adapter; this core
  never inherits the legacy coupling. (node/src/lib.rs:8–13, 81–103)
- **Three-way `Outcome` is load-bearing**: `Delivered` / `Rejected` (entry-fault,
  settles *everywhere*) / `Missed` (peer-fault, retries *elsewhere*). That enum
  is the §11 accountable-entry story in three lines. (node/src/lib.rs:39–47,
  298–320)
- `Effect::Settled` is the **only** thing the shell must persist; everything else
  is soft state rebuilt from offers. (node/src/lib.rs:20–22, 97–103)
- The bounded working set: settled ids that left every offer window are GC'd from
  the scheduling maps — memory tracks the *window*, not all history.
  (node/src/lib.rs:358–380, 458–460)

---

## 6. What's real (close strong)

- `cargo test` = bee's/TLC's exact ablation matrix on the shipped machine, state
  counts asserted equal. (README §Verification; parity.rs)
- `net` completes bee's `/swarm/handshake/15.0.0/handshake` and a `pullsync/1.4.0`
  stream **against a live testnet bee** (`live_testnet_handshake`,
  `live_testnet_pullsync`). (README crate rows 29–30)
- Close on SWIP-25: shared dedup + single-fetch-per-chunk removes up to k×
  redundant transfers over the *existing* Offer/Want transport — no protocol
  change. (design Abstract, §5.2)

---

## Demo (pick one — live beats slides for this crowd)

1. `cargo test -p melissi-machine` — point at the parity rows: `base` 125 states,
   `churn` 7,739, `scale` (k=6) 21,952. "These are TLC's numbers, on the Rust."
2. Flip one ablation: set `Config::dedup = false` in a scratch test, watch
   `ConflictFree` fire.
3. Split-pane `optimal-testbed/PullSyncerE.tla` `Claimable` vs
   `machine/src/lib.rs:238` `claimable()`.

---

## Pre-empt these questions

- **research:** "Liveness checked, or only safety?" → `explore.rs` does finite
  liveness via acyclicity + fairness; `Completeness`/`Freshness`/`AdvanceComplete`
  are temporal gates.
- **bee:** "want-by-reference vs our bitvector?" → clean core never inherits it;
  `wire/adapter.rs` maps onto the legacy coupling.
- **bee:** "`MaxPO = 31`?" → a wire detail confined to `overlay::bee_wire_bin`;
  spec PO is full 256-bit. (README §"Spec, not bee")
- **either:** "Is this going to replace bee's puller?" → it's the reference
  client; the wire/handshake interop is already live against testnet, so it's a
  drop-in on the protocol, not a fork of it.
