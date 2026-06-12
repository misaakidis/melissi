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
| `crates/sim` | deterministic self-play harness, k symmetric nodes | M2 |
| `crates/wire` | protobuf + libp2p + the bee-wire adapter (positional bitvector, re-offer) | later |

## Verification

`cargo test` runs the parity matrix: the same positive configurations and
ablations as `optimal-testbed/run.sh`, row for row, via an exhaustive
explorer over the shipped machine — state counts are asserted equal to TLC's.
The composite `storm` row (722k states) is `#[ignore]`d; run it with
`cargo test -- --ignored`.
