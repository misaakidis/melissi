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
/// recoverable signature `[r(32) ‖ s(32) ‖ v(1)]` (bee's layout, v last). The
/// recovery byte uses the ethereum convention `v = 27 + recid` — bee's
/// `crypto.Recover` feeds it straight to `btcec.RecoverCompact`, which requires
/// `27..=34`; a raw `0/1` recid is rejected (it is not what the network emits).
/// `None` if the signature is malformed. The prehash is what was actually
/// signed — typically [`eth_prefixed`] of the message.
pub fn recover(prehash: &[u8; 32], sig65: &[u8]) -> Option<[u8; 20]> {
    if sig65.len() != 65 {
        return None;
    }
    let recid = RecoveryId::from_byte(sig65[64].checked_sub(27)?)?;
    let sig = Signature::from_slice(&sig65[..64]).ok()?;
    let vk = VerifyingKey::recover_from_prehash(prehash, &sig, recid).ok()?;
    Some(eth_address(vk.to_encoded_point(false).as_bytes()))
}

/// Sign a 32-byte prehash with a secp256k1 secret, producing bee's 65-byte
/// recoverable layout `[r ‖ s ‖ v]` with the ethereum recovery convention
/// `v = 27 + recid` (so the bytes are identical to what bee's signer emits and
/// `btcec.RecoverCompact` accepts). The signing counterpart of [`recover`]
/// (a node stamping its own uploads, or signing its handshake address); the
/// prehash is typically [`eth_prefixed`] of the message. `None` on a bad key.
pub fn sign(secret: &[u8; 32], prehash: &[u8; 32]) -> Option<[u8; 65]> {
    let key = SigningKey::from_bytes(secret.into()).ok()?;
    let (sig, recid): (Signature, RecoveryId) = key.sign_prehash(prehash).ok()?;
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = recid.to_byte() + 27;
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
    use k256::ecdsa::SigningKey;

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
        let secret = [42u8; 32];
        let key = SigningKey::from_bytes(&secret.into()).unwrap();
        let owner = eth_address(key.verifying_key().to_encoded_point(false).as_bytes());
        let msg = b"a bzz-handshake-style variable length message";
        let prehash = eth_prefixed(msg);
        let sig65 = sign(&secret, &prehash).unwrap();
        assert!(
            sig65[64] == 27 || sig65[64] == 28,
            "ethereum recovery convention"
        );
        assert_eq!(recover(&prehash, &sig65), Some(owner));
    }

    /// Pinned against a vector bee itself produced (`bzz.NewAddress` → its
    /// `defaultSigner.Sign`, the generic EIP-191 signer): the handshake sign
    /// data for secret=`07..`, nonce=`09..`, networkID=1, ts=1700000000,
    /// underlay=`/ip4/127.0.0.1/tcp/1634`, zero chequebook. Locks both the
    /// 27/28 recovery convention and full byte-for-byte signature agreement
    /// with the live network — not a melissi self-round-trip.
    #[test]
    fn bee_handshake_signature_vector() {
        let unhex = |s: &str| {
            (0..s.len() / 2)
                .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
                .collect::<Vec<u8>>()
        };
        let secret = [7u8; 32];
        let nonce = [9u8; 32];
        let net = 1u64;
        let ts = 1_700_000_000u64;
        let eth = public_eth_address(&secret).unwrap();
        assert_eq!(hex(&eth), "4a62316623ad457f02cdc5d997ded67a383ec569");
        let mut data = Vec::new();
        data.extend_from_slice(b"bee-handshake-");
        data.extend_from_slice(&unhex("047f000001060662")); // serialized underlay
        data.extend_from_slice(&eth_overlay(&eth, net, &nonce));
        data.extend_from_slice(&net.to_be_bytes());
        data.extend_from_slice(&nonce);
        data.extend_from_slice(&ts.to_be_bytes());
        data.extend_from_slice(&[0u8; 20]); // chequebook
        let sig = sign(&secret, &eth_prefixed(&data)).unwrap();
        assert_eq!(hex(&sig), "4d1d845a0353229a5e1f5561c3fc62e18b09e2ae739d6fe0bfa72ece8b4ce5df295b709e48d8ee5a431129d520a41a169786ef32cb8f6d7be4126cc81bcb160e1c");
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // overlay = keccak(ethAddr ‖ networkID_LE(8) ‖ nonce); inlined here to keep
    // `crypto` leaf-level (the canonical impl is `melissi_overlay`).
    fn eth_overlay(eth: &[u8; 20], network_id: u64, nonce: &[u8; 32]) -> [u8; 32] {
        let mut d = Vec::with_capacity(20 + 8 + 32);
        d.extend_from_slice(eth);
        d.extend_from_slice(&network_id.to_le_bytes());
        d.extend_from_slice(nonce);
        keccak256(&d)
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
