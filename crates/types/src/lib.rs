//! The model's identity seam, single-sourced.
//!
//! The scope doc (`pullsync-optimal-client.md` §2) makes identity first-class:
//! the reserve entry is the triple `(address, batchID, stampHash)`, and the
//! whole design turns on "key the claim on the triple, never the bare
//! address". That seam must live in exactly one place — so the day `Triple`
//! becomes the real struct (M3-b, with a `TripleCodec` over BMT addresses and
//! postage stamps), it changes here and the five crates that share it follow,
//! rather than five definitions silently disagreeing.
//!
//! These are deliberately the *opaque* forms the verified machine reasons
//! about: a `Triple` is an identity to compare, a `BinId` a per-peer log
//! position. The machine never looks inside either.

/// An accountable reserve entry — `(address, batchID, stampHash)`, opaque
/// here. The unit of claiming, dedup, and completeness (design §5.2, §11).
pub type Triple = u32;

/// A neighbourhood peer.
pub type PeerId = u8;

/// A proximity-order bin. Deeper (higher) = nearer = higher fetch priority
/// (design §5.5). A node syncs bins `>= radius`.
pub type Bin = u8;

/// A per-bin, per-peer monotonic sequence number — a chunk's position in that
/// peer's append log. Peer-local: never compared across peers (the cross-peer
/// identity is the `Triple`).
pub type BinId = u64;
