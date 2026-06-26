# melissi

A minimal, formal-driven Swarm node, starting from optimal pull-sync.

melissi is built the long way round — theory → formal model → optimal design →
minimal code → live interop — so that every scheduling decision in the running
client traces back to a property proved in TLA+. It speaks bee's wire protocol on
the network and a model-checked state machine inside. The point is a node where
*what is fundamental* and *what is one client's contingent choice* are kept
visibly apart — see [Spec, not bee](docs/spec-mapping.md#spec-not-bee).

## What pull-sync is

Pull-sync is Swarm's reserve anti-entropy: how a node fills its slice of the
address space by pulling, from each neighbour, the chunks it is missing. A node
learns a peer's per-bin **cursors** (how far it has stored), **offers** advertise
what a peer holds in a range, **wants** request specific chunks by reference, and
**deliveries** carry them back — each chunk an accountable triple
`(address, batchID, stampHash)`. Old chunks below the cursor are *history* (HIST,
a finite backlog to drain); new chunks above it are *live* (LIVE, a standing
subscription). melissi implements this as a pure state machine and drives it over
real libp2p streams.

If you want the design rationale first, read
[`docs/architecture.md`](docs/architecture.md) and
[`docs/spec-mapping.md`](docs/spec-mapping.md).

## Status

| works today | in progress |
|---|---|
| the verified core (`machine`, `settlement`, `node`) — model-checked with exact TLC state-count parity | a live *chunk* pull (needs a reachable neighbourhood peer with a non-empty reserve — see the last crate row) |
| byte-exact bee wire: handshake, pullsync, BMT address, postage stamp | the modular carrier crate: connection lifecycle + pricing/hive/headers drivers |
| a **live testnet bee** handshaked and its pull-sync cursors negotiated | accounting, peer-store, and health seams (see `docs/architecture.md`) |

## Quickstart

```sh
cargo build          # the verified core — async-free, no libp2p, dep-light
cargo test           # the parity matrix: shipped machine vs TLC, row for row
```

A live pull-sync session against a testnet bee needs the real transport (the
`libp2p` feature) and a routable address bee can dial back:

```sh
# Forward a port to yourself first; advertise it here (no /p2p/ suffix).
MELISSI_UNDERLAY=/ip4/<your-routable-ip>/tcp/1634 \
  cargo run -p melissi-net --features libp2p --bin melissi-pull
```

`melissi-pull` is configured entirely by environment — `MELISSI_NETWORK`,
`MELISSI_BOOTNODE`, `MELISSI_RADIUS`, `MELISSI_TIMEOUT`, and more; the full set is
documented at the top of [`crates/net/src/bin/pull.rs`](crates/net/src/bin/pull.rs).

## How it's built

The TLA+ specs of record are vendored in-tree at
[`formal-models/tla/`](formal-models/tla/) — every crate is a refinement of a
named spec, and the parity tests re-check the same ablation matrix on the shipped
code.

The eleven crates fall into three rings. The middle ring — `machine`,
`settlement`, `node` — *is* pull-sync; the rest is identity/wire plumbing around
it and the harness that checks it. (`crates/wasm`, a browser build of the core,
sits outside the rings.)

| ring | crates | what it is |
|---|---|---|
| **verified core** | `machine`, `settlement`, `node` | *what pull-sync is* |
| identity & wire | `types`, `crypto`, `protobuf`, `overlay`, `neighbourhood`, `wire`, `net` | *how it talks to bee* |
| the harness | `sim`, `machine::explore` | *how we know it's right* |

`machine` and `settlement` are 1:1 refinements of two named TLA+ specs
(`PullSyncerE.tla`, `IntervalSettlement.tla`); `node` is the sans-io composition
(events → effects) that wires them into a running puller. In one line: **the
machine schedules, settlement remembers, the node composes** — and the same
machine is model-checked over abstract `u32` ids and run over real `Triple`s
against a live testnet bee. The spec ↔ Rust mapping is laid out line-for-line in
[`docs/spec-mapping.md`](docs/spec-mapping.md); a talk outline is in
[`docs/presentation.md`](docs/presentation.md). How the stack is layered so the
core stays carrier-blind and components are pluggable — the modular strategy, the
`(state, event) → (state, [op])` core contract, and the out-of-order concurrency
argument — is in [`docs/architecture.md`](docs/architecture.md).

### Crates

| crate | what it is |
|---|---|
| `crates/types` | the identity seam: the real `Triple` = `(address, batchID, stampHash)`. The whole stack instantiates the machine over this; tests use `Triple::mock(n)` (scheduling is content-agnostic) |
| `crates/crypto` | Swarm's shared crypto primitives, single-sourced: keccak256, the EIP-191 signing hash, secp256k1 sign/recover, ethereum address. Used by `bmt`/`postage`/`overlay`/`net` so none of them re-wraps a hash. `sign`/`recover` use the ethereum `v = 27 + recid` recovery convention (bee's `btcec.RecoverCompact`), pinned to the canonical Ethereum + keccak vectors **and** a handshake-signature vector bee itself produced |
| `crates/protobuf` | the proto3 wire plumbing bee speaks on *every* protocol — varint, gogo delimited framing (`uvarint(len) ‖ msg`, 128 KiB cap), field codec. Hand-rolled, zero deps, single-sourced for `wire` + `net` (the same single-sourcing `crypto` does for hashes) |
| `crates/overlay` | proximity order + overlay address — the fundamentals that *define* the reserve (proximity ≥ radius) and neighbourhood (spec §1.1.4, §2.2.1). Spec PO is the full shared-leading-bit count (`0..=255`, self saturates), **not** bee's `MaxPO=31` cap (isolated in `bee_wire_bin`). Byte-exact vs bee's vectors |
| `crates/machine` | `PullSyncerE.tla` — the scheduling machine, **polymorphic in the chunk identity `C`** (it needs only `Copy + Ord + Hash` — it schedules, it never verifies). Model-checked over abstract `u32` ids (exact TLC state-count parity), run over the real `Triple` |
| `crates/settlement` | `IntervalSettlement.tla` — settle before you forget; the interval is a `u64` high-water, so eager advance and disconnected ranges are unrepresentable |
| `crates/neighbourhood` | `Neighbourhood.tla` — assembling pull-sync's **supply**: discovers and connects the honest peers of the depth-`D` neighbourhood tile (§4 decomposition), discharging PullSyncerE's "supply assumed" premise. Discovery is incremental and gossip-gated (the `net::hive` feedback — no oracle pool), peers split willing/declining (a real bee declines a light peer), and the node connects the *whole* honest neighbourhood, not just its seed (§5.1 anti-single-source). Two ablations (gossip / connect-all), exact TLC state-count parity (`MC_nhood`/`nogossip`/`noconnect`: 13/2/7). Kademlia routing across the whole address space is a separate, deferred companion |
| `crates/node` | the sans-io core (events → effects) over `PullState<Triple>`; want-by-reference, one open offer per `(peer, bin)`, settlement the only durable transition |
| `crates/sim` | deterministic self-play: k symmetric nodes over a seeded network; the floors measured — Θ-REP, exact network delivery floor, serve balance max−min ≤ 1, LIVE spread, small-gap re-sync; + fairness ablations (the floor-achieving knobs made falsifiable) |
| `crates/wire` (`pb`/`adapter`) | bee's `pkg/pullsync` protobuf + delimited framing + LSB-first bitvector (byte-verified vs master); the adapter maps core effects onto the legacy coupling (positional bitvector, re-offer-on-fetch, zero-address). Identity ↔ wire is now trivial — `Triple` *is* the entry, so there is no synthetic codec |
| `crates/wire` (`bmt`) | bee's chunk address — BMT over keccak256 — reproduced **byte-exactly**, verified against bee's `pkg/cac` vectors. melissi and bee agree on addresses |
| `crates/wire` (`postage`) | bee's postage stamp — secp256k1 recovery over bee's exact digest, eth-prefixed → batch-owner address. The **entry-fault** half of self-verification |
| `crates/wire` (`codec`) | `MintedCodec`: mints real content-addressed, stamped chunks and validates deliveries — `bmt` mismatch → `Missed` (peer-fault, local), bad stamp → `Rejected` (entry-fault, global), both ok → `Delivered`, all from the bytes alone |
| `crates/net` (`BzzAddress`) | the handshake **identity**: the overlay↔key↔underlay binding, signed and verified (the overlay is a commitment to the key, so it can't be forged). Built on `crypto` + `overlay`. The verifiable part of bzz networking |
| `crates/net` (`pb`/`handshake`/`transport`) | bee's handshake **exchange**: the `Syn`/`SynAck`/`Ack` protobuf (byte-exact vs vectors bee itself marshalled), the asymmetric sync `poll`-driver, and the real rust-libp2p **transport** (TCP/noise/yamux, opt-in `libp2p` feature) that drives the *same* driver over a socket on bee's stream id `/swarm/handshake/15.0.0/handshake`. Two nodes complete it over real TCP, each recovering the other's verified identity; **a real testnet bee** is handshaked live (`live_testnet_handshake`). Default build stays libp2p-free |
| `crates/net` (`pullsync`) | the pull-sync **shell** over libp2p: drives the `wire` `Session` by opening one short stream per op (`cursors` `Syn→Ack`, `pullsync` `Get→Offer→Want→Delivery*`, bee's `pullsync/1.4.0` ids) and pumping the verified pollers — node↔node convergence over real streams (`two_nodes_pullsync_over_tcp`); a live testnet bee's cursors stream is negotiated (`live_testnet_pullsync`) |
| `crates/net` (live chunk pull) | observed-underlay re-signing (NAT/address discovery) and peer **discovery** to reach a storage node — the pinned bootnode has an empty reserve, so a live *chunk* pull needs a neighbourhood peer found via discovery. The part that needs more than a pinned peer — deferred, not guessed; it slots onto the shell above |

## Verification

`cargo test` runs the parity matrix: the same positive configurations and
ablations as [`formal-models/tla/run.sh`](formal-models/tla/run.sh), row for row,
via an exhaustive explorer over the shipped machine — state counts are asserted
equal to TLC's. The composite `storm` row (722k states) is `#[ignore]`d; run it
with `cargo test -- --ignored`. To re-check against TLC directly, run that script
(needs `tla2tools.jar`); see [`formal-models/tla/README.md`](formal-models/tla/README.md).

## Docs & license

- [`docs/architecture.md`](docs/architecture.md) — the modular layering, the core contract, the concurrency argument
- [`docs/spec-mapping.md`](docs/spec-mapping.md) — the spec ↔ Rust mapping, line for line
- [`docs/presentation.md`](docs/presentation.md) — a talk outline
- [`formal-models/tla/`](formal-models/tla/) — the TLA+ specs of record

Licensed under the [BSD 3-Clause License](LICENSE).
