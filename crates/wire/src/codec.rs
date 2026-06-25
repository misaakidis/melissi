//! `MintedCodec` — a real chunk source and validator, over the real `Triple`.
//!
//! Once `Triple` *is* `(address, batchID, stampHash)`, the codec's job is no
//! longer to invent identities (the old synthetic codec did); it is the two
//! things a real node does: **mint** chunks (payload → BMT address →
//! signed stamp → entry) on the serving side, and **validate** deliveries on
//! the receiving side. Validation is the self-verification the whole design
//! rests on, decided from the delivered bytes alone:
//!
//!   - `bmt::chunk_address(data) != triple.address` → `Missed`
//!     (peer-fault: garbage / wrong chunk — *local*, retry another holder);
//!   - the stamp does not recover the batch owner for this address → `Rejected`
//!     (entry-fault: invalid / replayed — *global*, identical at every holder);
//!   - both hold → `Delivered`.
//!
//! `address`/`batch_id`/`stamp_hash` on the wire are now trivial projections of
//! the triple, and `triple_of` is its trivial reconstruction — the identity
//! seam carries itself. The codec holds the minted payloads and stamps (a
//! stand-in for the reserve store) and the batch owner key.

use crate::adapter::TripleCodec;
use crate::{bmt, pb, postage};
use melissi_crypto as crypto;
use melissi_node::Outcome;
use melissi_types::Triple;
use std::collections::BTreeMap;

/// A real content-addressed, postage-stamped chunk source. One batch, one
/// owner key — enough to mint valid entries and to validate deliveries the
/// way a node does.
pub struct MintedCodec {
    secret: [u8; 32],
    owner: [u8; 20],
    batch_id: [u8; 32],
    /// triple → (payload, stamp): the minted reserve this codec can serve.
    store: BTreeMap<Triple, (Vec<u8>, Vec<u8>)>,
}

fn to_arr(v: &[u8]) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(v);
    a
}

impl MintedCodec {
    /// A codec keyed by a 32-byte owner secret and a batch id seed.
    pub fn new(owner_secret: [u8; 32], batch_seed: u8) -> Self {
        let owner = crypto::public_eth_address(&owner_secret).expect("valid secret");
        MintedCodec {
            secret: owner_secret,
            owner,
            batch_id: [batch_seed; 32],
            store: BTreeMap::new(),
        }
    }

    /// Mint a chunk from a payload: real BMT address, real signed stamp, with
    /// `timestamp` distinguishing re-stamps. Returns the entry's triple.
    pub fn mint(&mut self, payload: &[u8], index: u64, timestamp: u64) -> Triple {
        let address = bmt::chunk_address(payload);
        let idx = index.to_be_bytes();
        let ts = timestamp.to_be_bytes();
        let digest = postage::to_sign_digest(&address, &self.batch_id, &idx, &ts);
        let sig = crypto::sign(&self.secret, &crypto::eth_prefixed(&digest)).unwrap();
        let mut stamp = Vec::with_capacity(postage::STAMP_SIZE);
        stamp.extend_from_slice(&self.batch_id);
        stamp.extend_from_slice(&idx);
        stamp.extend_from_slice(&ts);
        stamp.extend_from_slice(&sig);

        let stamp_hash = bmt::keccak(&stamp);
        let triple = Triple::new(address, self.batch_id, stamp_hash);
        self.store.insert(triple, (payload.to_vec(), stamp));
        triple
    }

    pub fn owner(&self) -> [u8; 20] {
        self.owner
    }
}

impl TripleCodec for MintedCodec {
    fn address(&self, c: Triple) -> Vec<u8> {
        c.address.to_vec()
    }
    fn batch_id(&self, c: Triple) -> Vec<u8> {
        c.batch_id.to_vec()
    }
    fn stamp_hash(&self, c: Triple) -> Vec<u8> {
        c.stamp_hash.to_vec()
    }
    fn stamp(&self, c: Triple) -> Vec<u8> {
        self.store
            .get(&c)
            .map(|(_, s)| s.clone())
            .unwrap_or_default()
    }
    fn data(&self, c: Triple) -> Vec<u8> {
        self.store
            .get(&c)
            .map(|(d, _)| d.clone())
            .unwrap_or_default()
    }
    fn triple_of(&self, address: &[u8], batch_id: &[u8], stamp_hash: &[u8]) -> Option<Triple> {
        if address.len() != 32 || batch_id.len() != 32 || stamp_hash.len() != 32 {
            return None;
        }
        Some(Triple::new(
            to_arr(address),
            to_arr(batch_id),
            to_arr(stamp_hash),
        ))
    }
    fn triple_of_delivery(&self, d: &pb::Delivery) -> Option<Triple> {
        if d.address.len() != 32 || d.stamp.len() != postage::STAMP_SIZE {
            return None;
        }
        let stamp = postage::Stamp::parse(&d.stamp)?;
        let stamp_hash = bmt::keccak(&d.stamp);
        Some(Triple::new(
            to_arr(&d.address),
            to_arr(stamp.batch_id()),
            stamp_hash,
        ))
    }
    fn validate(&self, c: Triple, d: &pb::Delivery) -> Outcome {
        // 1. content self-verification: the delivered bytes must BMT-hash to
        //    the requested address (from the bytes alone)
        if bmt::chunk_address(&d.data).to_vec() != self.address(c) {
            return Outcome::Missed; // peer-fault: garbage / wrong chunk
        }
        // 2. payment self-verification: the stamp must recover the batch owner
        //    for this address (from the delivered stamp alone)
        match postage::Stamp::parse(&d.stamp) {
            Some(st) if postage::valid(&c.address, &st, &self.owner) => Outcome::Delivered,
            _ => Outcome::Rejected, // entry-fault: invalid / replayed stamp
        }
    }
}

/// A read-only codec for **pulling** real network chunks: it never mints or
/// serves, only validates deliveries from the bytes alone — BMT content address
/// + a self-consistent recovered stamp owner. Unlike [`MintedCodec`] it matches
/// no fixed owner: a pulled chunk's batch owner is whoever stamped it, so a valid
/// (recoverable) signature over the address is the self-verification we can do
/// from bytes. This is what lets melissi accept arbitrary chunks pulled from a
/// real bee reserve (whose stamps are signed by foreign batch owners).
#[derive(Default)]
pub struct PullCodec;

impl TripleCodec for PullCodec {
    fn address(&self, c: Triple) -> Vec<u8> {
        c.address.to_vec()
    }
    fn batch_id(&self, c: Triple) -> Vec<u8> {
        c.batch_id.to_vec()
    }
    fn stamp_hash(&self, c: Triple) -> Vec<u8> {
        c.stamp_hash.to_vec()
    }
    // A puller never serves, so it holds no payloads/stamps.
    fn stamp(&self, _c: Triple) -> Vec<u8> {
        Vec::new()
    }
    fn data(&self, _c: Triple) -> Vec<u8> {
        Vec::new()
    }
    fn triple_of(&self, address: &[u8], batch_id: &[u8], stamp_hash: &[u8]) -> Option<Triple> {
        if address.len() != 32 || batch_id.len() != 32 || stamp_hash.len() != 32 {
            return None;
        }
        Some(Triple::new(
            to_arr(address),
            to_arr(batch_id),
            to_arr(stamp_hash),
        ))
    }
    fn triple_of_delivery(&self, d: &pb::Delivery) -> Option<Triple> {
        if d.address.len() != 32 || d.stamp.len() != postage::STAMP_SIZE {
            return None;
        }
        let stamp = postage::Stamp::parse(&d.stamp)?;
        Some(Triple::new(
            to_arr(&d.address),
            to_arr(stamp.batch_id()),
            bmt::keccak(&d.stamp),
        ))
    }
    fn validate(&self, c: Triple, d: &pb::Delivery) -> Outcome {
        // 1. content self-verification: bytes must BMT-hash to the address.
        if bmt::chunk_address(&d.data).to_vec() != c.address.to_vec() {
            return Outcome::Missed; // peer-fault: garbage / wrong chunk
        }
        // 2. payment self-verification: the stamp must recover *a* valid owner
        //    for this address (well-formed signature), no fixed owner to match.
        match postage::Stamp::parse(&d.stamp) {
            Some(st) if postage::recover_owner(&c.address, &st).is_some() => Outcome::Delivered,
            _ => Outcome::Rejected, // entry-fault: malformed / unrecoverable stamp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_codec_validates_foreign_owned_chunk() {
        // a chunk minted by SOME batch owner (not ours)…
        let mut minter = MintedCodec::new([3u8; 32], 5);
        let triple = minter.mint(b"a chunk from a foreign batch", 0, 0);
        let delivery = pb::Delivery {
            address: minter.address(triple),
            data: minter.data(triple),
            stamp: minter.stamp(triple),
        };
        // …validates under PullCodec, which matches no fixed owner (only that the
        // stamp recovers a valid owner for the address).
        assert_eq!(PullCodec.validate(triple, &delivery), Outcome::Delivered);
        // tampered bytes → Missed (content self-check from the bytes alone).
        let bad = pb::Delivery {
            data: b"tampered".to_vec(),
            ..delivery
        };
        assert_eq!(PullCodec.validate(triple, &bad), Outcome::Missed);
    }

    #[test]
    fn minted_chunk_is_content_addressed_and_validates() {
        let mut codec = MintedCodec::new([7u8; 32], 1);
        let triple = codec.mint(b"a real chunk payload", 0, 0);
        // the triple's address is the real BMT of the payload
        assert_eq!(triple.address, bmt::chunk_address(b"a real chunk payload"));
        // an honest delivery validates
        let d = pb::Delivery {
            address: codec.address(triple),
            data: codec.data(triple),
            stamp: codec.stamp(triple),
        };
        assert_eq!(codec.validate(triple, &d), Outcome::Delivered);
    }

    #[test]
    fn garbage_is_missed_bad_stamp_is_rejected() {
        let mut codec = MintedCodec::new([9u8; 32], 2);
        let triple = codec.mint(b"payload", 0, 0);

        let mut garbage = pb::Delivery {
            address: codec.address(triple),
            data: codec.data(triple),
            stamp: codec.stamp(triple),
        };
        garbage.data[0] ^= 0xff;
        assert_eq!(codec.validate(triple, &garbage), Outcome::Missed);

        let mut bad_stamp = pb::Delivery {
            address: codec.address(triple),
            data: codec.data(triple),
            stamp: codec.stamp(triple),
        };
        let n = bad_stamp.stamp.len();
        bad_stamp.stamp[n - 1] ^= 0x01; // corrupt the recovery id / sig
        assert_eq!(codec.validate(triple, &bad_stamp), Outcome::Rejected);
    }
}
