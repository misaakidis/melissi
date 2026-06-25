# melissi

A minimal, formal-driven Swarm node, starting from optimal pull-sync.

The design and its verification live in the SWIPs repo:
`SWIPs/PULLSYNC/pullsync-optimal-design.md` (the design, machine-checked in
`optimal-testbed/`), `pullsync-optimal-implementation.md` (the refinement
discipline), `pullsync-optimal-client.md` (this node's scope). The TLA+ suite
is the spec of record; every crate here is a refinement of a named spec, and the
parity tests re-check the same ablation matrix on the shipped code.

## The verified core

The eleven crates fall into three rings. The middle ring — `machine`,
`settlement`, `node` — *is* pull-sync; the rest is identity/wire plumbing around
it and the harness that checks it.

| ring | crates | what it is |
|---|---|---|
| **verified core** | `machine`, `settlement`, `node` | *what pull-sync is* |
| identity & wire | `types`, `crypto`, `protobuf`, `overlay`, `wire`, `net` | *how it talks to bee* |
| the harness | `sim`, `machine::explore` | *how we know it's right* |

`machine` and `settlement` are 1:1 refinements of two named TLA+ specs
(`PullSyncerE.tla`, `IntervalSettlement.tla`); `node` is the sans-io composition
(events → effects) that wires them into a running puller. In one line: **the
machine schedules, settlement remembers, the node composes** — and the same
machine is model-checked over abstract `u32` ids and run over real `Triple`s
against a live testnet bee. The spec ↔ Rust mapping is laid out line-for-line in
[`docs/spec-mapping.md`](docs/spec-mapping.md); a talk outline is in
[`docs/presentation.md`](docs/presentation.md).

## Crates

| crate | refines | status |
|---|---|---|
| `crates/types` | the identity seam: the real `Triple` = `(address, batchID, stampHash)`. The whole stack instantiates the machine over this; tests use `Triple::mock(n)` (scheduling is content-agnostic) | ✓ |
| `crates/crypto` | Swarm's shared crypto primitives, single-sourced: keccak256, the EIP-191 signing hash, secp256k1 sign/recover, ethereum address. Used by `bmt`/`postage`/`overlay`/`net` so none of them re-wraps a hash. `sign`/`recover` use the ethereum `v = 27 + recid` recovery convention (bee's `btcec.RecoverCompact`), pinned to the canonical Ethereum + keccak vectors **and** a handshake-signature vector bee itself produced | ✓ |
| `crates/protobuf` | the proto3 wire plumbing bee speaks on *every* protocol — varint, gogo delimited framing (`uvarint(len) ‖ msg`, 128 KiB cap), field codec. Hand-rolled, zero deps, single-sourced for `wire` + `net` (the same single-sourcing `crypto` does for hashes) | ✓ |
| `crates/overlay` | proximity order + overlay address — the fundamentals that *define* the reserve (proximity ≥ radius) and neighbourhood (spec §1.1.4, §2.2.1). Spec PO is the full shared-leading-bit count (`0..=255`, self saturates), **not** bee's `MaxPO=31` cap (isolated in `bee_wire_bin`). Byte-exact vs bee's vectors | M3-b ✓ |
| `crates/machine` | `PullSyncerE.tla` — the scheduling machine, **polymorphic in the chunk identity `C`** (it needs only `Copy + Ord + Hash` — it schedules, it never verifies). Model-checked over abstract `u32` ids (exact TLC state-count parity), run over the real `Triple` | M0 ✓ |
| `crates/settlement` | `IntervalSettlement.tla` — settle before you forget; the interval is a `u64` high-water, so eager advance and disconnected ranges are unrepresentable | M1 ✓ |
| `crates/node` | the sans-io core (events → effects) over `PullState<Triple>`; want-by-reference, one open offer per `(peer, bin)`, settlement the only durable transition | M1 ✓ |
| `crates/sim` | deterministic self-play: k symmetric nodes over a seeded network; the floors measured — Θ-REP, exact network delivery floor, serve balance max−min ≤ 1, LIVE spread, small-gap re-sync; + fairness ablations (the floor-achieving knobs made falsifiable) | M2 ✓ |
| `crates/wire` (`pb`/`adapter`) | bee's `pkg/pullsync` protobuf + delimited framing + LSB-first bitvector (byte-verified vs master); the adapter maps core effects onto the legacy coupling (positional bitvector, re-offer-on-fetch, zero-address). Identity ↔ wire is now trivial — `Triple` *is* the entry, so there is no synthetic codec | M3-a ✓ |
| `crates/wire` (`bmt`) | bee's chunk address — BMT over keccak256 — reproduced **byte-exactly**, verified against bee's `pkg/cac` vectors. melissi and bee agree on addresses | M3-b ✓ |
| `crates/wire` (`postage`) | bee's postage stamp — secp256k1 recovery over bee's exact digest, eth-prefixed → batch-owner address. The **entry-fault** half of self-verification | M3-b ✓ |
| `crates/wire` (`codec`) | `MintedCodec`: mints real content-addressed, stamped chunks and validates deliveries — `bmt` mismatch → `Missed` (peer-fault, local), bad stamp → `Rejected` (entry-fault, global), both ok → `Delivered`, all from the bytes alone | M3-b ✓ |
| `crates/net` (`BzzAddress`) | the handshake **identity**: the overlay↔key↔underlay binding, signed and verified (the overlay is a commitment to the key, so it can't be forged). Built on `crypto` + `overlay`. The verifiable part of bzz networking | M3-b ✓ |
| `crates/net` (`pb`/`handshake`/`transport`) | bee's handshake **exchange**: the `Syn`/`SynAck`/`Ack` protobuf (byte-exact vs vectors bee itself marshalled), the asymmetric sync `poll`-driver, and the real rust-libp2p **transport** (TCP/noise/yamux, opt-in `libp2p` feature) that drives the *same* driver over a socket on bee's stream id `/swarm/handshake/15.0.0/handshake`. Two nodes complete it over real TCP, each recovering the other's verified identity; **a real testnet bee** is handshaked live (`live_testnet_handshake`). Default build stays libp2p-free | M3-b ✓ |
| `crates/net` (`pullsync`) | the pull-sync **shell** over libp2p: drives the `wire` `Session` by opening one short stream per op (`cursors` `Syn→Ack`, `pullsync` `Get→Offer→Want→Delivery*`, bee's `pullsync/1.4.0` ids) and pumping the verified pollers — node↔node convergence over real streams (`two_nodes_pullsync_over_tcp`); a live testnet bee's cursors stream is negotiated (`live_testnet_pullsync`) | M3-b ✓ |
| `crates/net` (live chunk pull) | observed-underlay re-signing (NAT/address discovery) and peer **discovery** to reach a storage node — the pinned bootnode has an empty reserve, so a live *chunk* pull needs a neighbourhood peer found via discovery. The part that needs more than a pinned peer — deferred, not guessed; it slots onto the shell above | network |

## Verification

`cargo test` runs the parity matrix: the same positive configurations and
ablations as `optimal-testbed/run.sh`, row for row, via an exhaustive
explorer over the shipped machine — state counts are asserted equal to TLC's.
The composite `storm` row (722k states) is `#[ignore]`d; run it with
`cargo test -- --ignored`.

## Spec, not bee

melissi implements the *Swarm Formal Specification*, not bee's implementation
*decisions*. Where the two differ, the spec wins and bee's choice is named and
confined to an interop boundary — so the showcase says what is fundamental and
what is one client's contingent choice. The standing example: proximity order
is the spec's count of shared leading bits over the *whole* address (§1.1.4) —
`0..=255` for distinct addresses (a `u8`), with the degenerate `PO(x,x) = 256`
self case saturated to `u8::MAX`; bee's `MaxPO = 31` cap is its Kademlia
bin-table size (absent from the spec's parameter constants, Appendix C),
isolated in `overlay::bee_wire_bin` and never in the `proximity` fundamental. Bee-derived values that *are* spec
(BMT chunk address, postage-stamp digest, overlay derivation) are pinned
against the spec's own vectors, not bee's say-so.
