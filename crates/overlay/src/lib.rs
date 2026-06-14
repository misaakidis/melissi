//! Proximity order and overlay address — the fundamentals that define, from
//! first principles, *which chunks a node must hold* and *who its neighbours
//! are*.
//!
//! **Spec, not bee.** These follow the *Swarm Formal Specification* (§1.1.4,
//! §2.2.1), not bee's implementation choices. The distinction is load-bearing
//! here: bee caps proximity at `MaxPO = 31` (the size of its Kademlia bin
//! table) and scans only 4 bytes — but the spec's Appendix C lists every
//! parameter constant and there is **no `MaxPO`**. So melissi computes the
//! *spec* proximity over the whole 256-bit address, and treats bee's 31-cap
//! as what it is: an interop detail of the wire's bin index, named and
//! confined to [`bee_wire_bin`], never the fundamental.

use melissi_crypto::keccak256;
use melissi_types::Address;

/// Proximity order (spec Def 6, §1.1.4): the number of common leading bits of
/// the addresses' big-endian (MSB-first) representations. Higher = nearer.
/// This is the logarithmic distance the whole address space is organised by; a
/// chunk's depth is its proximity to the node, and the reserve is everything
/// at proximity ≥ radius.
///
/// For two *distinct* 256-bit addresses the count is `0..=255` (they must
/// differ by at most the last bit), so `u8` is exact. The spec's
/// `PO(x, x) = d = 256` — identical addresses, maximally close — is the one
/// value that overflows; it is a degenerate case the protocol excludes (a node
/// never proximity-routes to itself), so it is represented as `u8::MAX = 255`,
/// the same "maximally close" verdict, in a `u8`.
///
/// NB this is the SPEC proximity over the whole address — NOT bee's `MaxPO=31`
/// cap (its Kademlia bin table, absent from the spec's Appendix C). bee's cap
/// is the wire-bin mapping [`bee_wire_bin`], never this.
pub fn proximity(a: &[u8], b: &[u8]) -> u8 {
    let n = a.len().min(b.len()).min(32); // proximity is over 256-bit addresses
    for i in 0..n {
        let xor = a[i] ^ b[i];
        if xor != 0 {
            // u8::leading_zeros counts leading zeros within the 8-bit byte
            // (0..=7 here, since xor != 0): the first differing bit within
            // byte i. With i ≤ 31 this is ≤ 255 — fits u8.
            return (i as u8) * 8 + xor.leading_zeros() as u8;
        }
    }
    u8::MAX // identical over the address: maximally close (spec d = 256, saturated)
}

/// bee's wire bin index for a proximity order: capped at bee's Kademlia table
/// size (`MaxBins = 32`, bins `0..=31`). This is a **bee implementation
/// decision**, not the spec — isolated here so the interop layer can use it
/// without the fundamental [`proximity`] inheriting the cap. (The pullsync
/// `Get.Bin` wire field is `int32`, so the cap is bee's table, not the wire's.)
pub const BEE_MAX_BIN: u8 = 31;

pub fn bee_wire_bin(po: u8) -> u8 {
    po.min(BEE_MAX_BIN)
}

/// A node's overlay address from its ethereum address, the network id, and the
/// registration nonce — spec §2.2.1 / bee `crypto.NewOverlayFromEthereumAddress`:
/// `keccak256(ethAddr(20) ‖ networkID(8, little-endian) ‖ nonce(32))`.
pub fn overlay_address(eth_addr: &[u8; 20], network_id: u64, nonce: &[u8; 32]) -> Address {
    let mut data = Vec::with_capacity(20 + 8 + 32);
    data.extend_from_slice(eth_addr);
    data.extend_from_slice(&network_id.to_le_bytes());
    data.extend_from_slice(nonce);
    keccak256(&data)
}

/// A node's neighbourhood: its overlay address and storage radius. Together
/// they fix the reserve (spec §1.1.4, design §4): a chunk's bin is a property
/// the node *computes* from the chunk address and its own overlay, never one
/// it must trust a peer to report.
#[derive(Clone, Copy, Debug)]
pub struct Neighbourhood {
    pub overlay: Address,
    /// Storage radius (a proximity depth). In practice small — the spec's
    /// `NODE_RESERVE_DEPTH` is 23.
    pub radius: u8,
}

impl Neighbourhood {
    pub fn new(overlay: Address, radius: u8) -> Self {
        Neighbourhood { overlay, radius }
    }

    /// The proximity (depth) of a chunk to this node — the full spec PO.
    pub fn depth_of(&self, chunk: &Address) -> u8 {
        proximity(chunk, &self.overlay)
    }

    /// Is the chunk in this node's reserve? (Proximity ≥ radius — spec §1.1.4.)
    pub fn in_reserve(&self, chunk: &Address) -> bool {
        self.depth_of(chunk) >= self.radius
    }

    /// Is `peer` a neighbour — close enough to share this node's reserve?
    ///
    /// Note the degenerate case the spec makes explicit: `PO(self) = 256`, so a
    /// peer whose overlay equals ours is reported as a neighbour at maximal
    /// depth. That is correct as a *distance*, but a node must not sync from
    /// itself (or from an address-colliding clone counted as one peer) — the
    /// caller excludes its own overlay from the peer set; this function does
    /// not silently special-case it.
    pub fn is_neighbour(&self, peer: &Address) -> bool {
        proximity(peer, &self.overlay) >= self.radius
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec Def 6, exhaustively across all 256 bit positions: a single set bit
    /// at position `n` gives proximity `n` — with **no cap** (bit 31 → 31,
    /// **bit 32 → 32**, bit 200 → 200, bit 255 → 255), unlike bee's MaxPO
    /// saturation. The `=32` boundary that confused us is the whole point: the
    /// spec keeps counting; only bee's *wire bin* saturates. All values
    /// `0..=255` for distinct addresses, so `u8` is exact.
    #[test]
    fn proximity_is_spec_full_range_no_cap() {
        let base = [0u8; 32];
        for bit in 0u16..256 {
            let mut other = [0u8; 32];
            other[(bit / 8) as usize] = 0b1000_0000 >> (bit % 8);
            assert_eq!(proximity(&base, &other), bit as u8, "bit {bit}");
        }
    }

    /// The "same address as you" case: spec `PO(x, x) = d = 256`, maximally
    /// close — represented as `u8::MAX = 255` (the degenerate self case, kept
    /// in a u8 since the protocol never proximity-routes to itself). Not
    /// collapsed to bee's 31.
    #[test]
    fn proximity_of_equal_addresses_saturates_to_max() {
        let a = [0xABu8; 32];
        assert_eq!(proximity(&a, &a), u8::MAX);
        assert_eq!(proximity(&[0u8; 32], &[0u8; 32]), 255);
    }

    /// bee's cap is an interop mapping, isolated and explicit — NOT the
    /// fundamental. The spec PO past 31 is preserved; only the wire bin caps.
    #[test]
    fn bee_wire_bin_caps_but_proximity_does_not() {
        let base = [0u8; 32];
        let mut deep = [0u8; 32];
        deep[20] = 0x01; // a difference far past bit 31
        let po = proximity(&base, &deep);
        assert!(po > 31, "spec proximity keeps counting past 31 (got {po})");
        assert_eq!(bee_wire_bin(po), 31, "the bee wire bin saturates at 31");
        assert_eq!(bee_wire_bin(5), 5);
    }

    /// Spec §2.2.1 / bee `pkg/crypto` TestNewOverlayFromEthereumAddress.
    #[test]
    fn overlay_matches_spec_vectors() {
        let hex = |a: &Address| a.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let mut nonce1 = [0u8; 32];
        nonce1[31] = 1;
        let eth = hexb("1815cac638d1525b47f848daf02b7953e4edd15c");
        assert_eq!(
            hex(&overlay_address(&eth, 1, &nonce1)),
            "a38f7a814d4b249ae9d3821e9b898019c78ac9abe248fff171782c32a3849a17"
        );
        let mut nonce2 = [0u8; 32];
        nonce2[31] = 2;
        assert_eq!(
            hex(&overlay_address(&eth, 1, &nonce2)),
            "c63c10b1728dfc463c64c264f71a621fe640196979375840be42dc496b702610"
        );
        let eth2 = hexb("d26bc1715e933bd5f8fad16310042f13abc16159");
        assert_eq!(
            hex(&overlay_address(&eth2, 2, &nonce1)),
            "9f421f9149b8e31e238cfbdc6e5e833bacf1e42f77f60874d49291292858968e"
        );
    }

    #[test]
    fn reserve_is_proximity_at_least_radius() {
        let n = Neighbourhood::new([0u8; 32], 8);
        let mut near = [0u8; 32];
        near[1] = 1; // differs at bit 15 → proximity 15 ≥ 8: in reserve
        assert!(n.in_reserve(&near));
        let mut far = [0u8; 32];
        far[0] = 0b0000_1000; // differs at bit 4 → proximity 4 < 8: out
        assert!(!n.in_reserve(&far));
    }

    fn hexb(s: &str) -> [u8; 20] {
        let mut out = [0u8; 20];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        out
    }
}
