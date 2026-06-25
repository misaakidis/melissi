# TLA+ specs of record

The specs the crates refine, vendored in-tree so melissi is self-contained. They
are mirrored from the design repo (`SWIPs/PULLSYNC/optimal-testbed`); the design
and its analysis live there (`pullsync-optimal-design.md`).

| spec | refined by | what it pins |
|---|---|---|
| `PullSyncerE.tla` | `crates/machine` | the scheduling machine: dedup, failover, exclude, reset, priority, LIVE |
| `PullSyncerNA.tla` | `crates/machine` | atomicity — the in-flight check-and-mark is one critical section |
| `IntervalSettlement.tla` | `crates/settlement` | the resume layer: settle before you forget |
| `OfferPacing.tla` | `crates/node`, `crates/sim` | advertisements are justified, never a busy loop |
| `DiscoveryBarrier.tla` | `crates/node` | no-wedge liveness — a withholding peer cannot stall a bin |
| `WindowedLoad.tla` | `crates/node` | serve-balance under incremental, paged discovery |

`MC_*.{tla,cfg}` are the model-checking instances: the positives and the
ablations (each ablation breaks exactly one named property). The neighbourhood
specs are intentionally absent — no crate refines them yet.

## Verifying

Two independent checks, of the same suite:

- **Rust (default, no TLC):** `cargo test` runs the parity matrix — an exhaustive
  explorer over the shipped machine whose distinct-state counts are asserted equal
  to TLC's, row for row. The composite `storm` row is `#[ignore]`d.
- **TLC (optional):** `./run.sh` checks every config directly. Green only if every
  positive passes and every ablation produces its expected counterexample. Needs
  `tla2tools.jar` (set `TLA2TOOLS`, or place it at `~/tla2tools.jar`).

## License

Part of melissi, under the repository's BSD-3-Clause `LICENSE` (top level).
