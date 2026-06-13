//! `ContentCodec` — content-addressing made real, so the three-way `Fetch`
//! outcome (`Delivered`/`Rejected`/`Missed`) *flows from validation* instead
//! of being injected by a harness. This is the M3-b codec step: the boundary
//! where self-verification stops being a stub.
//!
//! The split the design turns on (§4.3, §6.1, `IntervalSettlement`'s `Bad`):
//!   - **peer-fault → `Missed`**: the delivered bytes do not hash to the
//!     requested address. The peer served garbage or the wrong chunk; *another
//!     holder may serve the correct bytes*, so retry — local to the peer.
//!   - **entry-fault → `Rejected`**: the bytes are right but the stamp is
//!     invalid (expired/over-issued batch — here a validity marker; a real
//!     secp256k1 signature at interop). The stamp is bound to the entry, so
//!     *every honest holder serves the same invalid stamp* — no holder can
//!     make it valid, settle globally.
//!   - **both valid → `Delivered`**.
//!
//! The client decides from the delivered bytes ALONE — it recomputes the hash
//! and reads the stamp marker; it never consults shared state. That is
//! self-verification, the property that collapses pull's quorum to 1-of-n.
//!
//! The hash here is a self-contained FNV-1a (zero dependencies, deterministic)
//! — a PLACEHOLDER. bee's address is a BMT over keccak256 of the 4 KB chunk;
//! swapping it in for interop is a change to this file alone, behind the
//! unchanged `TripleCodec` trait.

use crate::adapter::TripleCodec;
use crate::pb;
use melissi_node::Outcome;
use melissi_types::Triple;
use std::collections::BTreeSet;

const VALID: u8 = 0x01;
const BAD: u8 = 0x00;

/// FNV-1a over the bytes, spread to 32 bytes by re-hashing with a counter.
/// Collision-resistant enough for deterministic tests; not cryptographic.
fn hash32(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    for round in 0..4u8 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ u64::from(round);
        for &b in data {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        out.extend_from_slice(&h.to_be_bytes());
    }
    out
}

/// A content-addressing codec. `bad_stamps` are the triples whose batches are
/// invalid — the server still serves them (with the BAD marker), and the
/// client rejects them on arrival, identically at every holder.
pub struct ContentCodec {
    bad_stamps: BTreeSet<Triple>,
}

impl ContentCodec {
    pub fn new() -> Self {
        ContentCodec { bad_stamps: BTreeSet::new() }
    }

    /// Mark a triple's stamp invalid (an expired / over-issued batch).
    pub fn mark_bad_stamp(&mut self, c: Triple) {
        self.bad_stamps.insert(c);
    }

    /// The real chunk payload for a triple (what gets hashed to the address).
    fn payload(c: Triple) -> Vec<u8> {
        // a 64-byte body carrying the id — stands in for the 4 KB chunk
        let mut d = vec![0u8; 64];
        d[..4].copy_from_slice(&c.to_be_bytes());
        d
    }

    fn validity(&self, c: Triple) -> u8 {
        if self.bad_stamps.contains(&c) {
            BAD
        } else {
            VALID
        }
    }
}

impl Default for ContentCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl TripleCodec for ContentCodec {
    fn address(&self, c: Triple) -> Vec<u8> {
        hash32(&Self::payload(c)) // CAC: address IS the content hash
    }
    fn batch_id(&self, c: Triple) -> Vec<u8> {
        let mut b = vec![0xBB; 32];
        b[..4].copy_from_slice(&c.to_be_bytes());
        b
    }
    fn stamp_hash(&self, c: Triple) -> Vec<u8> {
        // binds the address and the batch and the validity, so the triple
        // identity changes if any does
        let mut pre = self.address(c);
        pre.extend_from_slice(&self.batch_id(c));
        pre.push(self.validity(c));
        hash32(&pre)
    }
    fn stamp(&self, c: Triple) -> Vec<u8> {
        // batchID ‖ stampHash ‖ validity ‖ pad to 113 (bee's stamp size)
        let mut s = self.batch_id(c);
        s.extend_from_slice(&self.stamp_hash(c));
        s.push(self.validity(c));
        s.resize(113, 0);
        s
    }
    fn data(&self, c: Triple) -> Vec<u8> {
        Self::payload(c)
    }
    fn triple_of(&self, _address: &[u8], batch_id: &[u8], _stamp_hash: &[u8]) -> Option<Triple> {
        // recover the id from the batch tag (carries it in the clear); the
        // address is the content hash, not reversible
        batch_id.get(..4).map(|b| Triple::from_be_bytes(b.try_into().unwrap()))
    }
    fn triple_of_delivery(&self, d: &pb::Delivery) -> Option<Triple> {
        if d.stamp.len() != 113 {
            return None;
        }
        // batchID is the first 32 bytes of the stamp; its tag carries the id
        d.stamp.get(..4).map(|b| Triple::from_be_bytes(b.try_into().unwrap()))
    }
    fn validate(&self, c: Triple, d: &pb::Delivery) -> Outcome {
        // 1. content-address check, from the bytes alone: do the delivered
        //    bytes hash to the address we asked for?
        if hash32(&d.data) != self.address(c) {
            return Outcome::Missed; // peer-fault: garbage / wrong chunk — retry elsewhere
        }
        // 2. stamp validity, from the delivered stamp alone
        let valid = d.stamp.len() == 113 && d.stamp.get(64) == Some(&VALID);
        if !valid {
            return Outcome::Rejected; // entry-fault: bad stamp — settle globally
        }
        Outcome::Delivered
    }
}
