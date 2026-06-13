# melissi

A minimal, formal-driven Swarm node, starting from optimal pull-sync.

The design and its verification live in the SWIPs repo:
`SWIPs/PULLSYNC/pullsync-optimal-design.md` (the design, machine-checked in
`optimal-testbed/`), `pullsync-optimal-implementation.md` (the refinement
discipline), `pullsync-optimal-client.md` (this node's scope). The TLA+ suite
is the spec of record; every crate here is a refinement of a named spec, and the
parity tests re-check the same ablation matrix on the shipped code.

## Crates

| crate | refines | status |
|---|---|---|
| `crates/types` | the identity seam: the real `Triple` = `(address, batchID, stampHash)`. The whole stack instantiates the machine over this; tests use `Triple::mock(n)` (scheduling is content-agnostic) | ✓ |
| `crates/overlay` | proximity order + overlay address — the fundamentals that *define* the reserve (proximity ≥ radius) and neighbourhood (design §3, §4). Byte-exact vs bee's `pkg/swarm`/`pkg/crypto` vectors | M3-b ✓ |
| `crates/machine` | `PullSyncerE.tla` — the scheduling machine, **polymorphic in the chunk identity `C`** (it needs only `Copy + Ord + Hash` — it schedules, it never verifies). Model-checked over abstract `u32` ids (exact TLC state-count parity), run over the real `Triple` | M0 ✓ |
| `crates/settlement` | `IntervalSettlement.tla` — settle before you forget; the interval is a `u64` high-water, so eager advance and disconnected ranges are unrepresentable | M1 ✓ |
| `crates/node` | the sans-io core (events → effects) over `PullState<Triple>`; want-by-reference, one open offer per `(peer, bin)`, settlement the only durable transition | M1 ✓ |
| `crates/sim` | deterministic self-play: k symmetric nodes over a seeded network; the floors measured — Θ-REP, exact network delivery floor, serve balance max−min ≤ 1, LIVE spread, small-gap re-sync; + fairness ablations (the floor-achieving knobs made falsifiable) | M2 ✓ |
| `crates/wire` (`pb`/`adapter`) | bee's `pkg/pullsync` protobuf + delimited framing + LSB-first bitvector (byte-verified vs master); the adapter maps core effects onto the legacy coupling (positional bitvector, re-offer-on-fetch, zero-address). Identity ↔ wire is now trivial — `Triple` *is* the entry, so there is no synthetic codec | M3-a ✓ |
| `crates/wire` (`bmt`) | bee's chunk address — BMT over keccak256 — reproduced **byte-exactly**, verified against bee's `pkg/cac` vectors. melissi and bee agree on addresses | M3-b ✓ |
| `crates/wire` (`postage`) | bee's postage stamp — secp256k1 recovery over bee's exact digest, eth-prefixed → batch-owner address. The **entry-fault** half of self-verification | M3-b ✓ |
| `crates/wire` (`codec`) | `MintedCodec`: mints real content-addressed, stamped chunks and validates deliveries — `bmt` mismatch → `Missed` (peer-fault, local), bad stamp → `Rejected` (entry-fault, global), both ok → `Delivered`, all from the bytes alone | M3-b ✓ |
| `crates/net` | rust-libp2p transport + bzz handshake + discovery; the bin-relativity wiring (a chunk's bin is its proximity to *this* node's overlay, computed by `overlay`, not trusted from the wire); **live bee devnet/mainnet interop**. The one part that needs a running peer to verify — left unbuilt rather than guessed | M3-b network |

## Verification

`cargo test` runs the parity matrix: the same positive configurations and
ablations as `optimal-testbed/run.sh`, row for row, via an exhaustive
explorer over the shipped machine — state counts are asserted equal to TLC's.
The composite `storm` row (722k states) is `#[ignore]`d; run it with
`cargo test -- --ignored`.
