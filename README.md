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
| `crates/wire` (`bmt`) | bee's chunk address — BMT over keccak256 — reproduced **byte-exactly**, verified against bee's `pkg/cac` test vector (`"greaterthanspan"` → `27913f1b…`) and the empty-chunk address. melissi and bee agree on addresses: the interop-determining computation | M3-b codec ✓ |
| `crates/wire` (`codec`) | `ContentCodec`: the three-way `Delivered`/`Rejected`/`Missed` split flows from validation (peer-fault local, entry-fault global), self-verified from the bytes — now over the **real BMT address**. Stamp is still a structural marker (secp256k1 at interop) | M3-b codec ✓ |
| `crates/wire` (`postage`) | bee's postage stamp — secp256k1 recovery over bee's exact digest (`keccak256(addr‖batchID‖index‖timestamp)`, eth-prefixed) → batch-owner address. The **entry-fault** half of self-verification: an invalid/replayed stamp recovers the wrong owner → `Rejected`, globally. Round-trip verified | M3-b codec ✓ |
| `crates/net` | rust-libp2p transport + bzz/secp256k1 handshake; **live bee devnet interop** (the one step needing a running bee node — all offline-verifiable pieces are done) | M3-b network |

## Verification

`cargo test` runs the parity matrix: the same positive configurations and
ablations as `optimal-testbed/run.sh`, row for row, via an exhaustive
explorer over the shipped machine — state counts are asserted equal to TLC's.
The composite `storm` row (722k states) is `#[ignore]`d; run it with
`cargo test -- --ignored`.
