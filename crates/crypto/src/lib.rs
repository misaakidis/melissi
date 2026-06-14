//! Swarm's shared crypto primitives, single-sourced.
//!
//! keccak256, the EIP-191 personal-sign hash, secp256k1 signature recovery, and
//! the ethereum address derivation all appear across Swarm — the BMT chunk
//! address, postage stamps, the bzz handshake, single-owner chunks. They live
//! here once, bee-exact, rather than copied per crate (so `keccak` is not
//! wrapped three times and the eth-prefix is not specialised per call site).
//!
//! These are deterministic and verifiable offline against bee's own vectors;
//! the higher layers (`bmt`, `postage`, `overlay`, `net`) build on them.

use k256::ecdsa::signature::hazmat::PrehashSigner;
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use tiny_keccak::{Hasher, Keccak};

/// keccak256 (the 256-bit Keccak SHA3 bee uses as its base hash — spec §1.1.3
/// `H`). The one hash primitive everything else is built from.
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut k = Keccak::v256();
    k.update(data);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

/// The EIP-191 personal-sign hash bee's generic signer applies to Sign and
/// Recover (`pkg/crypto`): `keccak256("\x19Ethereum Signed Message:\n" ‖
/// len(data) ‖ data)`. Length-aware — postage's 32-byte digest and the bzz
/// handshake's variable-length sign-data are the same function at different
/// lengths.
pub fn eth_prefixed(data: &[u8]) -> [u8; 32] {
    let mut buf = Vec::with_capacity(28 + data.len());
    buf.extend_from_slice(b"\x19Ethereum Signed Message:\n");
    buf.extend_from_slice(data.len().to_string().as_bytes());
    buf.extend_from_slice(data);
    keccak256(&buf)
}

/// The ethereum address of an uncompressed secp256k1 public key:
/// `keccak256(pubkey[1..65])[12..32]` (drop the `0x04` SEC1 prefix byte).
pub fn eth_address(pubkey_uncompressed: &[u8]) -> [u8; 20] {
    let h = keccak256(&pubkey_uncompressed[1..]);
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&h[12..]);
    addr
}

/// Recover the signer's ethereum address from a 32-byte prehash and a 65-byte
/// recoverable signature `[r(32) ‖ s(32) ‖ v(1)]` (bee's layout, v last).
/// `None` if the signature is malformed. The prehash is what was actually
/// signed — typically [`eth_prefixed`] of the message.
pub fn recover(prehash: &[u8; 32], sig65: &[u8]) -> Option<[u8; 20]> {
    if sig65.len() != 65 {
        return None;
    }
    let recid = RecoveryId::from_byte(sig65[64])?;
    let sig = Signature::from_slice(&sig65[..64]).ok()?;
    let vk = VerifyingKey::recover_from_prehash(prehash, &sig, recid).ok()?;
    Some(eth_address(vk.to_encoded_point(false).as_bytes()))
}

/// Sign a 32-byte prehash with a secp256k1 secret, producing bee's 65-byte
/// recoverable layout `[r ‖ s ‖ v]`. The signing counterpart of [`recover`]
/// (a node stamping its own uploads, or signing its handshake address); the
/// prehash is typically [`eth_prefixed`] of the message. `None` on a bad key.
pub fn sign(secret: &[u8; 32], prehash: &[u8; 32]) -> Option<[u8; 65]> {
    let key = SigningKey::from_bytes(secret.into()).ok()?;
    let (sig, recid): (Signature, RecoveryId) = key.sign_prehash(prehash).ok()?;
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = recid.to_byte();
    Some(out)
}

/// The ethereum address of a secp256k1 secret's public key.
pub fn public_eth_address(secret: &[u8; 32]) -> Option<[u8; 20]> {
    let key = SigningKey::from_bytes(secret.into()).ok()?;
    Some(eth_address(
        key.verifying_key().to_encoded_point(false).as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{signature::hazmat::PrehashSigner, SigningKey};

    /// `eth_address` against the canonical Ethereum vector: secp256k1 private
    /// key `1` → `0x7e5f4552091a69125d5dfcb7b8c2659029395bdf`.
    #[test]
    fn eth_address_matches_ethereum_vector() {
        let mut sk = [0u8; 32];
        sk[31] = 1;
        let key = SigningKey::from_bytes(&sk.into()).unwrap();
        let addr = eth_address(key.verifying_key().to_encoded_point(false).as_bytes());
        let hex: String = addr.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "7e5f4552091a69125d5dfcb7b8c2659029395bdf");
    }

    /// Sign with the EIP-191 prefix and recover the signer's address — the
    /// sign/recover symmetry bee relies on, at an arbitrary message length.
    #[test]
    fn eip191_sign_recover_roundtrip() {
        let key = SigningKey::from_bytes(&[42u8; 32].into()).unwrap();
        let owner = eth_address(key.verifying_key().to_encoded_point(false).as_bytes());
        let msg = b"a bzz-handshake-style variable length message";
        let prehash = eth_prefixed(msg);
        let (sig, recid): (Signature, RecoveryId) = key.sign_prehash(&prehash).unwrap();
        let mut sig65 = sig.to_bytes().to_vec();
        sig65.push(recid.to_byte());
        assert_eq!(recover(&prehash, &sig65), Some(owner));
    }

    #[test]
    fn keccak_empty_vector() {
        // keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let hex: String = keccak256(b"").iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }
}
