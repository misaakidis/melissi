# melissi

A minimal, formal-driven Swarm client, starting from optimal pull-sync.

The design and its verification live in the SWIPs repo:
`SWIPs/PULLSYNC/pullsync-optimal-design.md` (the design, machine-checked in
`optimal-testbed/`), `pullsync-optimal-implementation.md` (the refinement
discipline), `pullsync-optimal-client.md` (this client's scope). The TLA+ suite
is the spec of record; every crate here is a refinement of a named spec, and the
parity tests re-check the same ablation matrix on the shipped code.

## Crates

| crate | refines | status |
|---|---|---|
| `crates/machine` | `PullSyncerE.tla` — the pull-sync scheduling machine | M0 ✓ |
| `crates/settlement` | `IntervalSettlement.tla` — settle before you forget; the interval is a `u64` high-water, so eager advance and disconnected ranges are unrepresentable | M1 ✓ |
| `crates/node` | the sans-io node core (events → effects); want-by-reference, one open offer per `(peer, bin)`, settlement the only durable transition | M1 ✓ |
| `crates/sim` | deterministic self-play: k symmetric nodes over a seeded network; the floors measured — Θ-REP, exact network delivery floor, serve balance max−min ≤ 1, LIVE spread, small-gap re-sync | M2 ✓ |
| `crates/wire` | bee's `pkg/pullsync` protobuf + delimited framing + LSB-first bitvector (byte-verified vs master), and the adapter mapping core effects onto the legacy coupling (positional bitvector, re-offer-on-fetch, zero-address). Wire-level self-play converges at the floor and fails over an omitter | M3-a ✓ |
| `crates/net` | rust-libp2p transport + secp256k1 handshake + a real `TripleCodec` (BMT address, stamp validation); bee devnet interop | M3-b |

## Verification

`cargo test` runs the parity matrix: the same positive configurations and
ablations as `optimal-testbed/run.sh`, row for row, via an exhaustive
explorer over the shipped machine — state counts are asserted equal to TLC's.
The composite `storm` row (722k states) is `#[ignore]`d; run it with
`cargo test -- --ignored`.
