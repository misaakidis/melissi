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
//! The address is now bee's real BMT-over-keccak256 ([`crate::bmt`], verified
//! against bee's `pkg/cac` test vector), so a melissi address equals a bee
//! address for the same bytes — content-addressing is interop-exact. The
//! stamp is still structural (a validity marker, not yet a secp256k1
//! signature); that is the one remaining placeholder, swapped at devnet
//! interop behind the unchanged `TripleCodec` trait.

use crate::adapter::TripleCodec;
use crate::{bmt, pb};
use melissi_node::Outcome;
use melissi_types::Triple;
use std::collections::BTreeSet;

const VALID: u8 = 0x01;
const BAD: u8 = 0x00;

/// bee's chunk address: the BMT over keccak256. Interop-exact.
fn address_of(data: &[u8]) -> Vec<u8> {
    bmt::chunk_address(data).to_vec()
}

/// keccak256, for binding the stamp (bee uses keccak256, not the BMT, here).
fn hash32(data: &[u8]) -> Vec<u8> {
    bmt::keccak(data).to_vec()
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
        address_of(&Self::payload(c)) // CAC: address IS the BMT content hash (bee-exact)
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
        //    bytes BMT-hash to the address we asked for?
        if address_of(&d.data) != self.address(c) {
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
