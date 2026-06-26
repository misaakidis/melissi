# Modular architecture

How melissi is layered so the verified pull-sync core stays carrier-blind and
people can plug in their own components. This is the strategy of record; where a
piece is not yet built it is marked **(planned)**. The discipline is the point:
dependency arrows run inward, the cores never name a transport, and every core
carries a TLA+ spec with exact-state parity.

## The layers

| layer | what it is | rule |
|---|---|---|
| **L0 spec** | protocol ids, versions, wire schemas, constants — one source of truth | no logic; nothing else names a protocol id/version/constant except by importing it |
| **L1 cores** | sans-io state machines: `machine`, `node`, `settlement`, `neighbourhood`, `lifecycle` *(planned)*, `accounting` *(planned)* | pure; every core a refinement of a named spec with exact-state parity |
| **L2 wire** | byte-exact drivers: handshake, pricing, hive, headers, the pullsync wire, codecs | sans-io pollers; byte-vectors vs bee |
| **L3 seams** | the traits a core rides: `OpRunner`, `Underlay`, `ChunkStore` *(planned)*, `PeerStore` *(planned)*, `Health` *(planned)* | the only place async meets pure |
| **L4 carriers** | the I/O leaves: the libp2p `MelissiUnderlay`, the in-memory test carrier | the sole async; drives L1+L2; implements the L3 seams |
| **L5 modules** | overlay protocols riding a carrier: pull-sync; later retrieval, pushsync | rides `Underlay`; names no socket |
| **L6 composition** | a node = one carrier + chosen modules; a binary | wires leaves to modules; ≥2 carriers always kept wired |

Dependency arrows point inward (L6→L0). Cores and drivers never name libp2p;
carriers are the only async leaves; the spec module is named once and imported
everywhere else.

### What L0–L6 does *not* cover

Named so the layering doesn't read as a whole Swarm node — these are separate
layers, added later, not core to pull-sync:

- **retrieval + kademlia routing** — uses full-range XOR routing, a *different*
  peer model than `neighbourhood`'s depth-`D` structural-locality tile; a new
  layer, not a reuse.
- **pushsync / pusher / pss** — eager push, orthogonal to pull.
- **blockchain** — postage listener, storage-incentive proofs.
- **HTTP / REST API**.

## L1 — the core contract

Every L1 core is one pure transition function:

```
(state, event) → (state, [op])
```

Ops are a core's only way to ask for the world; events are its only way to learn
about it. The carrier (L4) is the sole async leaf that closes the loop. This is
why each core carries a TLA+ spec and exact-state parity: **the state machine is
the spec**; `Op`/`Event` are the spec's actions made into Rust types.

Two flavors show up, and naming them keeps the layering honest:

- **Op-driven** (emits I/O requests): `machine` (what to pull next) and the new
  `lifecycle` *(planned)* — discover→dial→handshake→admit→keep→prune→top-up is a
  textbook state machine emitting `DialPeer`/`RunHandshake`/`Prune`; admission and
  disconnect events fold back. Same loop, different op alphabet.
- **Reducer-driven** (folds events, rarely emits): `settlement` (the durable
  high-water) and the new `accounting` *(planned)* ledger are mostly
  `fold(state, Delivered) → state`, emitting a `Settle` op only when a threshold
  trips. Still sans-io, still a state machine — heavier on intake than emission.

A carrier is then just *something that can run a layer's ops and deliver its
events* — exactly the `OpRunner`/`Underlay` pair. The in-memory test carrier and
the libp2p carrier differ only in how they execute the same ops.

## L1 — the concurrency contract

The shipped driver (`wire::session::drive`) is strictly sequential: one
`next_op`, await, feed. A concurrent driver fans ops out per peer and feeds
events back as they land — so events arrive **out of order** with respect to
dispatch. This is safe **by construction**; the core needs no change. The work is
in the driver.

### Out-of-order is safe by construction

Three structural facts make `Node::handle` a confluent reducer:

1. **Causal order is preserved by the dataflow.** An op does not exist until the
   event that causes it is fed: `Offer` is emitted only from the `CursorsResult`
   arm, `Fetch` only from `round()` after an `OfferResult`. So
   Cursors→Offer→Fetch for a `(peer,bin)` can never reorder — the later op is not
   dispatched until the earlier event lands. **Only causally independent events
   can reorder**, and those touch disjoint state.
2. **State is keyed, not sequential.** Everything is `BTreeMap<(PeerId,Bin),_>`
   or per-triple machine state; `round()` re-derives the whole schedule from
   accumulated state on every call — a fold, not a step. Independent events
   applied in any order leave the same accumulated state.
3. **The invariants that need ordering are enforced by node state, not arrival
   order.** *One open offer per `(peer,bin)`* — the `Discovery` open-offer
   discipline; the node never emits a second offer for a key while one is open.
   *No double-fetch of a triple* — the `want(c,p)` lease (ConflictFree);
   `enabled_wants()` excludes leased triples, so two rounds fired from two
   out-of-order events cannot both grab the same chunk.

So reordering can violate nothing: causally-dependent events can't reorder,
causally-independent ones commute. One honest caveat: the *exact* peer→chunk
assignment becomes non-deterministic under reordering (cumulative routing depends
on how many fetches issued before a given `round()`) — but that is **policy,
provably correctness-neutral** (design §5.3), and the fairness floor is an
order-robust statistical property. Safety and liveness are untouched. The design
is already commit-on-offer with no discovery barrier, so it is built to schedule
on partial information; out-of-order arrival is that regime with different timing.

### The concurrent driver

Keep the core single-owner (it is `!Sync`, and that is correct — do not share
it). Wrap it in a mailbox:

```
core task (owns Session):
  drain next_op() → dispatch each to the I/O task for op.peer()
  loop { ev = event_rx.recv().await         // completion order, = out-of-order
         session.feed(ev)                     // serialized through this one task
         drain next_op() → dispatch           // newly-enabled ops
         if in_flight == 0 && next_op == None { done } }

per-peer I/O task: run(op) over that peer's streams → event_tx.send(ev)
```

The genuinely new bits:

- **In-flight tracking + quiescence**: "done" is `next_op()==None` **and** zero
  ops outstanding — not just an empty queue. The `Discovery` open-offer set
  already tracks open offers; generalize it to count in-flight per peer.
- **Per-peer FIFO**: ops for one peer's one `(peer,bin)` must run in dispatch
  order on the connection. One I/O task per peer with its own op queue makes
  cross-peer concurrent and same-peer FIFO. The node never hands two competing
  ops for one key, so a simple per-peer FIFO suffices.

### Concurrency is necessary, not just faster

bee's Offer on an empty LIVE range long-polls — it blocks until a chunk arrives.
The sequential driver cannot hold a standing live offer open while draining HIST
elsewhere: one parked offer wedges the loop. Concurrency lets each peer's live
subscription park in its own task while HIST proceeds. The live phase cannot
coexist with HIST drain without it.

### The verification payoff

The TLA+ spec already interleaves actions nondeterministically — it never assumes
an order, so the *sequential* driver is in fact more restrictive than the spec,
and going concurrent moves the implementation *toward* the model. Anything the
model-check proved holds under arbitrary interleaving already covers reordering.
To pin it empirically, the in-memory carrier delivers events in adversarially
**permuted** orders and asserts the same terminal state (reserve + settled
high-waters) every time. That property test *is* the operational statement of
"confluent over event order." **(planned)**

## Corrections folded in from the bee survey

Against bee's actual package structure, the core pull-sync layering is sound but
incomplete as a *node*. Named so the gaps are visible, not silent:

- **`accounting` is a distinct layer (planned).** bee separates `pricing` (the
  payment *threshold* — melissi has the acceptor), `accounting` (the bilateral
  *ledger* — melissi has nothing), and `settlement/swap` (the payment
  *mechanism* — deferred). Pull-sync beyond the free allowance costs; the ledger
  is orthogonal to the verified core (it consumes `Delivered`) but mandatory for
  sustained operation.
- **Naming collision.** bee's `settlement` is payment; melissi's `settlement` is
  the `IntervalSettlement` resume high-water — *not* payment. Keep the concept;
  the collision is a documentation hazard, not a code one.
- **`PeerStore` / addressbook seam (planned).** The durable overlay→underlay peer
  store the `lifecycle` warm-set reads and hive/handshake write — a service the
  carrier uses, so an L3 seam, not buried in the libp2p leaf.
- **`Health` / blocklist seam (planned).** Per-peer health and Byzantine eviction.
  Some of this is the lifecycle's prune/keep; the explicit "evict a failing peer"
  signal is named as an input the lifecycle consumes.

## Sequence

1. Consolidate **L0 spec** — pull the spread Swarm definitions (ids, versions,
   constants, schemas) into one module nothing bypasses.
2. Lock the **L3 seams** — `Underlay`, `OpRunner` exist; add `ChunkStore`,
   `PeerStore`, `Health`.
3. Build the **carrier crate** — the real new work: `ConnectionLifecycle.tla` +
   the lifecycle core + pricing/hive/headers drivers + `MelissiUnderlay`.
4. Prune — one spec module, two carriers wired, every core spec'd, every driver
   byte-vectored.
5. Demonstrate modularity — pull-sync over ≥2 carriers; the in-memory carrier
   runs the confluence property test.
6. Live pull against a serving bee.
