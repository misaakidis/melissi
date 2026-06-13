//! Postage stamp — secp256k1 signature validation. The *entry-fault* half of
//! self-verification (the §11/§12 accountable entry): content self-verifies by
//! hash (`bmt`), payment self-verifies by signature (here). An invalid stamp is
//! invalid identically at every holder, which is why a `Rejected` settles
//! globally.
//!
//! **Wire/schema = bee, noted where it diverges from the spec.** A stamp's
//! bytes and signing scheme are an interop contract with the live network and
//! the on-chain postage contract, so this follows bee's *deployed* scheme, not
//! the formal spec's prose. The scheme (bee `pkg/postage` + `pkg/crypto`):
//!   stamp  = batchID(32) ‖ index(8) ‖ timestamp(8) ‖ sig(65)        [113 bytes]
//!   digest = keccak256(chunkAddr ‖ batchID ‖ index ‖ timestamp)     (ToSignDigest)
//!   signed = keccak256("\x19Ethereum Signed Message:\n32" ‖ digest) (the eth
//!            prefix bee's generic signer applies to Sign AND Recover — verified
//!            symmetric in pkg/crypto/signer.go; not a stray artifact)
//!   owner  = ethAddress(secp256k1_recover(sig, signed))
//!   sig    = recoverable [r‖s‖v], v last (bee's layout, = witness type 0).
//!
//! The eth prefix is *shared* bee signing infrastructure — postage stamps,
//! single-owner chunks, and the handshake all sign through the same
//! `defaultSigner` / `crypto.Recover`. What distinguishes them is only the
//! *digest*: postage signs `keccak(addr ‖ batchID ‖ index ‖ timestamp)`, SOC
//! signs `keccak(id ‖ address)`. This module is the postage digest; a future
//! SOC module reuses the same prefix over its own digest (do not mistake the
//! prefix for postage-specific).
//!
//! **Divergence from spec §2.4.1.** The spec's ECDSA witness signs
//! `concat(preamble_constant, chunk_hash, batch_reference, valid_until_date)`;
//! bee signs `ethPrefix ‖ keccak(addr ‖ batchID ‖ index ‖ timestamp)` — a
//! different preamble (the eth prefix) and `index`+`timestamp` rather than a
//! `valid_until` field. Where spec text and the deployed contract disagree, the
//! contract (what mainnet enforces) wins for interop; bee matches it, so melissi
//! matches bee. Recorded here, not silently inherited.
//!
//! **Verification status.** The signing scheme is verified by *reading* bee, and
//! [`tests::eth_address_matches_ethereum_vector`] pins `ethAddress` + recovery
//! against the canonical Ethereum key vector. Full byte-interop with a
//! bee-produced stamp is NOT yet vector-checked (bee's own tests are round-trip
//! with random keys — no static vector exists); that needs a live or generated
//! bee stamp and is the one remaining unknown for mainnet stamp interop.
//!
//! **Validation is partial (spec Def 19, future work).** `V^STAMP` =
//! AUTHENTIC ∧ ALIVE ∧ AUTHORISED ∧ AVAILABLE ∧ ALIGNED. [`valid`] checks only
//! AUTHORISED (the signature recovers the batch owner). AUTHENTIC (batch exists
//! on-chain), ALIVE (balance > 0), AVAILABLE (index < batch size), and ALIGNED
//! (bucket depth) require blockchain state and belong to a chain-connected
//! layer. So a forged stamp is caught; an expired/over-issued/misaligned one is
//! not — that is deferred, not handled here.

use crate::bmt::keccak;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

pub const STAMP_SIZE: usize = 113;
pub const BATCH_ID: usize = 32;
pub const INDEX: usize = 8;
pub const TIMESTAMP: usize = 8;
pub const SIG: usize = 65;

/// The bee stamp layout, borrowed from a 113-byte slice.
pub struct Stamp<'a>(&'a [u8]);

impl<'a> Stamp<'a> {
    pub fn parse(b: &'a [u8]) -> Option<Self> {
        (b.len() == STAMP_SIZE).then_some(Stamp(b))
    }
    pub fn batch_id(&self) -> &[u8] {
        &self.0[..BATCH_ID]
    }
    pub fn index(&self) -> &[u8] {
        &self.0[BATCH_ID..BATCH_ID + INDEX]
    }
    pub fn timestamp(&self) -> &[u8] {
        &self.0[BATCH_ID + INDEX..BATCH_ID + INDEX + TIMESTAMP]
    }
    pub fn sig(&self) -> &[u8] {
        &self.0[BATCH_ID + INDEX + TIMESTAMP..]
    }
}

/// keccak256(addr ‖ batchID ‖ index ‖ timestamp) — bee `ToSignDigest`.
pub fn to_sign_digest(
    chunk_addr: &[u8],
    batch_id: &[u8],
    index: &[u8],
    timestamp: &[u8],
) -> [u8; 32] {
    let mut buf =
        Vec::with_capacity(chunk_addr.len() + batch_id.len() + index.len() + timestamp.len());
    buf.extend_from_slice(chunk_addr);
    buf.extend_from_slice(batch_id);
    buf.extend_from_slice(index);
    buf.extend_from_slice(timestamp);
    keccak(&buf)
}

/// The Ethereum personal-sign prefix bee applies before recovery.
pub fn eth_prefixed(digest: &[u8; 32]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(28 + 32);
    buf.extend_from_slice(b"\x19Ethereum Signed Message:\n32");
    buf.extend_from_slice(digest);
    keccak(&buf)
}

/// The ethereum address of an uncompressed secp256k1 public key:
/// keccak256(pubkey[1..65])[12..32].
pub fn eth_address(pubkey_uncompressed: &[u8]) -> [u8; 20] {
    let h = keccak(&pubkey_uncompressed[1..]); // drop the 0x04 prefix byte
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&h[12..]);
    addr
}

/// Recover the batch-owner ethereum address that signed this stamp for
/// `chunk_addr`, or `None` if the signature is malformed.
pub fn recover_owner(chunk_addr: &[u8], stamp: &Stamp) -> Option<[u8; 20]> {
    let digest = to_sign_digest(
        chunk_addr,
        stamp.batch_id(),
        stamp.index(),
        stamp.timestamp(),
    );
    let signed = eth_prefixed(&digest);
    let sig_bytes = stamp.sig();
    let recid = RecoveryId::from_byte(sig_bytes[64])?;
    let sig = Signature::from_slice(&sig_bytes[..64]).ok()?;
    let vk = VerifyingKey::recover_from_prehash(&signed, &sig, recid).ok()?;
    let uncompressed = vk.to_encoded_point(false);
    Some(eth_address(uncompressed.as_bytes()))
}

/// The stamp is valid for `chunk_addr` iff it recovers to `owner`.
pub fn valid(chunk_addr: &[u8], stamp: &Stamp, owner: &[u8; 20]) -> bool {
    recover_owner(chunk_addr, stamp).is_some_and(|got| &got == owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmt::chunk_address;
    use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};

    /// Sign a stamp the bee way: recoverable secp256k1 over the eth-prefixed
    /// digest, v appended last.
    fn sign_stamp(
        key: &SigningKey,
        chunk_addr: &[u8],
        batch_id: &[u8; 32],
        index: &[u8; 8],
        ts: &[u8; 8],
    ) -> Vec<u8> {
        let digest = to_sign_digest(chunk_addr, batch_id, index, ts);
        let signed = eth_prefixed(&digest);
        let (sig, recid): (Signature, RecoveryId) = key.sign_prehash(&signed).unwrap();
        let mut out = Vec::with_capacity(STAMP_SIZE);
        out.extend_from_slice(batch_id);
        out.extend_from_slice(index);
        out.extend_from_slice(ts);
        out.extend_from_slice(&sig.to_bytes());
        out.push(recid.to_byte());
        out
    }

    fn owner_of(key: &SigningKey) -> [u8; 20] {
        let vk = key.verifying_key();
        eth_address(vk.to_encoded_point(false).as_bytes())
    }

    /// `eth_address` against the canonical Ethereum vector: the secp256k1
    /// private key `1` yields address `0x7e5f4552091a69125d5dfcb7b8c2659029395bdf`.
    /// This pins the address derivation (keccak(pubkey[1..])[12..]) to the
    /// Ethereum standard bee follows — an external vector, not a round-trip.
    #[test]
    fn eth_address_matches_ethereum_vector() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let key = SigningKey::from_bytes(&sk.into()).unwrap();
        let addr = eth_address(key.verifying_key().to_encoded_point(false).as_bytes());
        let hex: String = addr.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "7e5f4552091a69125d5dfcb7b8c2659029395bdf");
    }

    /// A correctly-signed stamp validates; round-trip through bee's exact
    /// digest/prefix/recover chain.
    #[test]
    fn honest_stamp_validates() {
        let key = SigningKey::from_bytes(&[7u8; 32].into()).unwrap();
        let owner = owner_of(&key);
        let addr = chunk_address(b"payload");
        let stamp_bytes = sign_stamp(&key, &addr, &[1; 32], &[0, 0, 0, 0, 0, 0, 0, 1], &[0; 8]);
        let stamp = Stamp::parse(&stamp_bytes).unwrap();
        assert!(valid(&addr, &stamp, &owner));
    }

    /// Tampering any signed field breaks recovery → wrong owner → entry-fault.
    /// This is what makes a `Rejected` global: the bytes don't lie selectively.
    #[test]
    fn tampered_stamp_recovers_a_different_owner() {
        let key = SigningKey::from_bytes(&[7u8; 32].into()).unwrap();
        let owner = owner_of(&key);
        let addr = chunk_address(b"payload");
        let mut stamp_bytes = sign_stamp(&key, &addr, &[1; 32], &[0, 0, 0, 0, 0, 0, 0, 1], &[0; 8]);
        // flip a byte of the index: the digest changes, recovery yields a
        // different (wrong) address — not the batch owner.
        stamp_bytes[BATCH_ID] ^= 0xff;
        let stamp = Stamp::parse(&stamp_bytes).unwrap();
        assert!(
            !valid(&addr, &stamp, &owner),
            "tampered stamp must not validate as owner"
        );
    }

    /// The same stamp bound to a different chunk address fails — a stamp is
    /// not transferable to other content (replay across addresses).
    #[test]
    fn stamp_is_bound_to_its_chunk() {
        let key = SigningKey::from_bytes(&[9u8; 32].into()).unwrap();
        let owner = owner_of(&key);
        let addr = chunk_address(b"chunk-a");
        let other = chunk_address(b"chunk-b");
        let stamp_bytes = sign_stamp(&key, &addr, &[2; 32], &[0; 8], &[0; 8]);
        let stamp = Stamp::parse(&stamp_bytes).unwrap();
        assert!(valid(&addr, &stamp, &owner));
        assert!(
            !valid(&other, &stamp, &owner),
            "stamp must not validate for other content"
        );
    }
}
