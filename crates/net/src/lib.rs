//! bzz networking — the handshake **identity**, which is the part verifiable
//! offline. A node's network identity is a [`BzzAddress`]: it binds the node's
//! overlay (its address in the chunk space), its underlay (where to dial it on
//! the wire), and its blockchain key, with a signature. Verifying it is what
//! lets a peer trust that whoever it dialled really owns the claimed overlay —
//! the foundation the pull-sync protocol (and everything above) runs on.
//!
//! **Spec, not bee, with the interop caveat.** This follows bee's deployed
//! handshake (`pkg/bzz`) because the signed bytes are an interop contract with
//! the live network: `generateSignData = "bee-handshake-" ‖ underlay ‖ overlay
//! ‖ networkID(8 BE) ‖ nonce ‖ timestamp(8 BE) ‖ chequebook`, signed with the
//! shared EIP-191 signer ([`melissi_crypto`]). Verification recovers the
//! signer, *derives* the overlay from it ([`melissi_overlay::overlay_address`])
//! and requires it to equal the claimed overlay — so the overlay cannot be
//! forged: it is a commitment to the key.
//!
//! **What is here vs deferred.** The cryptographic binding above is pure and
//! verified offline. The rest of bzz networking — the libp2p transport (TCP /
//! noise / yamux), the protobuf handshake *exchange* (Syn/Ack/Synack), peer
//! discovery, and live interop against bee nodes — needs a running peer to
//! verify and is the deliberately-deferred transport layer (`crates/net` will
//! grow it; it slots onto this identity and the `wire` pollers). Nothing here
//! is faked: the identity is real and checked; the socket is not yet built.

use melissi_crypto as crypto;
use melissi_overlay::overlay_address;
use melissi_types::Address;

const HANDSHAKE_PREFIX: &[u8] = b"bee-handshake-";

/// The exact bytes a node signs to bind its overlay/underlay/key (bee
/// `bzz.generateSignData`). Network id and timestamp are big-endian u64.
fn sign_data(
    underlay: &[u8],
    overlay: &Address,
    network_id: u64,
    nonce: &[u8; 32],
    timestamp: u64,
    chequebook: &[u8; 20],
) -> Vec<u8> {
    let mut d = Vec::with_capacity(HANDSHAKE_PREFIX.len() + underlay.len() + 32 + 8 + 32 + 8 + 20);
    d.extend_from_slice(HANDSHAKE_PREFIX);
    d.extend_from_slice(underlay);
    d.extend_from_slice(overlay);
    d.extend_from_slice(&network_id.to_be_bytes());
    d.extend_from_slice(nonce);
    d.extend_from_slice(&timestamp.to_be_bytes());
    d.extend_from_slice(chequebook);
    d
}

/// A node's signed network identity (bee `bzz.Address`). The overlay is a
/// commitment to the node's key (`overlay = keccak(ethAddr ‖ networkID ‖
/// nonce)`), and the signature proves the key authored this binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BzzAddress {
    /// Where to dial the node (serialized multiaddr(s)); opaque bytes here.
    pub underlay: Vec<u8>,
    /// The node's address in the chunk space.
    pub overlay: Address,
    pub signature: [u8; 65],
    pub nonce: [u8; 32],
    pub timestamp: u64,
    /// The node's chequebook (blockchain) address; may be zero pre-funding.
    pub chequebook: [u8; 20],
}

impl BzzAddress {
    /// Build and sign a node's own identity from its secp256k1 secret. The
    /// overlay is *derived* from the key (not chosen), so it is a commitment.
    pub fn new(
        secret: &[u8; 32],
        underlay: &[u8],
        network_id: u64,
        nonce: [u8; 32],
        timestamp: u64,
        chequebook: [u8; 20],
    ) -> Option<Self> {
        let eth = crypto::public_eth_address(secret)?;
        let overlay = overlay_address(&eth, network_id, &nonce);
        let data = sign_data(
            underlay,
            &overlay,
            network_id,
            &nonce,
            timestamp,
            &chequebook,
        );
        let signature = crypto::sign(secret, &crypto::eth_prefixed(&data))?;
        Some(BzzAddress {
            underlay: underlay.to_vec(),
            overlay,
            signature,
            nonce,
            timestamp,
            chequebook,
        })
    }

    /// Verify the overlay↔key↔underlay binding (bee `bzz.ParseAddress`):
    /// recover the signer, derive the overlay from it, and require it to equal
    /// the claimed overlay. On success returns the recovered ethereum
    /// (blockchain) address. A forged overlay, a tampered field, or a bad
    /// signature all fail — the overlay cannot be claimed without the key.
    pub fn verify(&self, network_id: u64) -> Option<[u8; 20]> {
        let data = sign_data(
            &self.underlay,
            &self.overlay,
            network_id,
            &self.nonce,
            self.timestamp,
            &self.chequebook,
        );
        let recovered = crypto::recover(&crypto::eth_prefixed(&data), &self.signature)?;
        (overlay_address(&recovered, network_id, &self.nonce) == self.overlay).then_some(recovered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET: u64 = 1;

    fn addr(secret: &[u8; 32]) -> BzzAddress {
        BzzAddress::new(
            secret,
            b"/ip4/1.2.3.4/tcp/1634",
            NET,
            [9u8; 32],
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap()
    }

    /// Round-trip: a node's signed identity verifies, recovering its own
    /// ethereum address, and its overlay is the key-derived commitment.
    #[test]
    fn identity_verifies_and_binds_overlay_to_key() {
        let secret = [7u8; 32];
        let a = addr(&secret);
        let eth = crypto::public_eth_address(&secret).unwrap();
        assert_eq!(a.verify(NET), Some(eth));
        assert_eq!(
            a.overlay,
            overlay_address(&eth, NET, &a.nonce),
            "overlay is the key commitment"
        );
    }

    /// A forged overlay (claiming a different address than the key derives)
    /// fails — the overlay cannot be spoofed without the matching key.
    #[test]
    fn forged_overlay_is_rejected() {
        let mut a = addr(&[7u8; 32]);
        a.overlay[0] ^= 0xff; // claim a different overlay
        assert_eq!(a.verify(NET), None);
    }

    /// Tampering any signed field invalidates the binding.
    #[test]
    fn tampered_fields_are_rejected() {
        let base = addr(&[7u8; 32]);

        let mut t = base.clone();
        t.underlay[0] ^= 0xff;
        assert_eq!(t.verify(NET), None, "underlay tamper");

        let mut t = base.clone();
        t.nonce[0] ^= 0xff; // also changes the derived overlay
        assert_eq!(t.verify(NET), None, "nonce tamper");

        let mut t = base.clone();
        t.timestamp += 1;
        assert_eq!(t.verify(NET), None, "timestamp tamper");

        let mut t = base.clone();
        t.signature[0] ^= 0xff;
        assert_eq!(t.verify(NET), None, "signature tamper");
    }

    /// The signed binding is network-scoped: verifying under a different
    /// network id fails (the overlay derivation includes the network id).
    #[test]
    fn binding_is_network_scoped() {
        let a = addr(&[7u8; 32]);
        assert!(a.verify(NET).is_some());
        assert_eq!(a.verify(NET + 1), None);
    }
}
