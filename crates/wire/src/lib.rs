//! The bee wire: pull-sync's `pkg/pullsync` protocol, byte-compatible with
//! bee MASTER (`protocolVersion "1.4.0"`), plus the adapter that maps the
//! sans-io core's clean semantics onto the legacy coupling.
//!
//! Two layers:
//!   - [`pb`]: the five protobuf messages (`Syn`/`Ack`/`Get`/`Offer`/`Want`/
//!     `Delivery`), the gogo delimited framing, and bee's LSB-first bitvector
//!     — hand-rolled, zero dependencies, verified against the proto on master.
//!   - [`adapter`]: `Effect::Offer`/`Effect::Fetch` → wire round-trips and
//!     `Event::*` back. The legacy coupling (positional `Want` bitvector bound
//!     to the offer on one stream) is contained here — `Fetch` re-offers and
//!     matches by triple, so the core never sees it (the design's Table 8
//!     advertisement/delivery split, realised on bee's wire).
//!
//! This crate is the seam between the verified core and a real network: a
//! libp2p transport (M3-b) feeds bytes to the same client/server pollers, and
//! a real `TripleCodec` (BMT address, postage stamp validation) replaces
//! [`adapter::SyntheticCodec`] without touching the core or the framing.

pub mod adapter;
pub mod pb;
