//! The model's identity seam, single-sourced — now the real Swarm objects.
//!
//! The scope doc (`pullsync-optimal-client.md` §2) makes identity first-class:
//! the reserve entry is the triple `(address, batchID, stampHash)`, and the
//! whole design turns on "key the claim on the triple, never the bare
//! address" (§5.2, §11). That entry lives here, in one place.
//!
//! The scheduling machine is *polymorphic* in this identity — it needs only an
//! opaque token it can compare, store, and deduplicate (`Ord + Eq + Hash +
//! Clone`), and it never inspects the bytes. That bound is the §6.1 layering
//! principle made literal: the machine schedules; the codec verifies. So the
//! same machine is model-checked over abstract ids (`u32`, in the parity
//! suite) and run over the real `Triple` on the wire — the refinement is the
//! type instantiation, nothing more.

/// A 256-bit chunk address — a BMT content hash (CAC) or a single-owner
/// address (SOC). What `bmt::chunk_address` computes; what proximity order is
/// measured against.
pub type Address = [u8; 32];

/// A postage batch identifier — the batch that paid to store an entry.
pub type BatchId = [u8; 32];

/// The hash of the postage stamp that proves the payment.
pub type StampHash = [u8; 32];

/// An accountable reserve entry: `(address, batchID, stampHash)`. The unit of
/// claiming, deduplication, and completeness (design §5.2, §11) — *not* the
/// bare address: an in-flight claim for one triple must not suppress a
/// genuinely-needed second triple over the same content.
///
/// The machine treats this opaquely (compare/store/dedup only); its meaning
/// lives in the codec, where `address` self-verifies by hash and `stampHash`
/// by signature.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Triple {
    pub address: Address,
    pub batch_id: BatchId,
    pub stamp_hash: StampHash,
}

impl Triple {
    pub fn new(address: Address, batch_id: BatchId, stamp_hash: StampHash) -> Self {
        Triple {
            address,
            batch_id,
            stamp_hash,
        }
    }

    /// A deterministic distinct triple from a small seed — for tests and the
    /// sim, where scheduling is exercised over identities whose *content* does
    /// not matter (only their distinctness and order do). Distinct `n` give
    /// distinct, order-stable triples.
    pub fn mock(n: u32) -> Self {
        let tag = |k: u8| {
            let mut b = [0u8; 32];
            b[0] = k;
            b[28..].copy_from_slice(&n.to_be_bytes());
            b
        };
        Triple {
            address: tag(0xA),
            batch_id: tag(0xB),
            stamp_hash: tag(0xC),
        }
    }
}

/// A neighbourhood peer. (A real overlay address is the next identity to make
/// concrete; the scheduler needs only to distinguish peers, so this stays an
/// opaque small id until proximity routing lands.)
pub type PeerId = u8;

/// A proximity-order bin. Deeper (higher) = nearer = higher fetch priority
/// (design §5.5). A node syncs bins `>= radius`.
pub type Bin = u8;

/// A per-bin, per-peer monotonic sequence number — a chunk's position in that
/// peer's append log. Peer-local: never compared across peers (the cross-peer
/// identity is the `Triple`).
pub type BinId = u64;
