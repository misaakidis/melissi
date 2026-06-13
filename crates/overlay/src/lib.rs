//! Proximity order and overlay address — the fundamentals that define, from
//! first principles, *which chunks a node must hold* and *who its neighbours
//! are*. The design (§3, §4) is built on these; the sim stood in for them with
//! `% NBINS`. Both are reproduced byte-exactly from bee (`pkg/swarm`,
//! `pkg/crypto`) and verified against bee's own test vectors — pure,
//! dependency-light, and verifiable offline, unlike the libp2p socket they
//! ultimately feed.

use melissi_types::{Address, Bin};
use tiny_keccak::{Hasher, Keccak};

/// The deepest meaningful proximity order (bee `MaxPO`). `proximity` saturates
/// here; `self` (identical addresses) also returns this.
pub const MAX_PO: u8 = 31;

/// Proximity order: the number of common leading bits of `a ^ b` (MSB-first),
/// capped at [`MAX_PO`]. Higher = nearer. This is the logarithmic distance the
/// whole address space is organised by (design §3): a chunk's bin is its
/// proximity to the node, and the reserve is everything at proximity ≥ radius.
///
/// bee `swarm.Proximity`: scan the first `MaxPO/8 + 1 = 4` bytes (enough to
/// resolve up to bit 31); the first differing bit gives the order.
pub fn proximity(a: &[u8], b: &[u8]) -> u8 {
    let scan = ((MAX_PO / 8 + 1) as usize).min(a.len()).min(b.len());
    for i in 0..scan {
        let xor = a[i] ^ b[i];
        if xor != 0 {
            let po = (i as u8) * 8 + xor.leading_zeros() as u8;
            return po.min(MAX_PO);
        }
    }
    MAX_PO
}

/// A node's overlay address from its ethereum address, the network id, and the
/// registration nonce — bee `crypto.NewOverlayFromEthereumAddress`:
/// `keccak256(ethAddr(20) ‖ networkID(8, little-endian) ‖ nonce(32))`.
pub fn overlay_address(eth_addr: &[u8; 20], network_id: u64, nonce: &[u8; 32]) -> Address {
    let mut data = Vec::with_capacity(20 + 8 + 32);
    data.extend_from_slice(eth_addr);
    data.extend_from_slice(&network_id.to_le_bytes());
    data.extend_from_slice(nonce);
    let mut k = Keccak::v256();
    k.update(&data);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

/// A node's neighbourhood: its overlay address and storage radius. Together
/// they fix the reserve (design §4) — and, crucially, the bin of a chunk is a
/// property the node computes from the chunk's address and its *own* overlay,
/// not something it must trust a peer to report.
#[derive(Clone, Copy, Debug)]
pub struct Neighbourhood {
    pub overlay: Address,
    pub radius: Bin,
}

impl Neighbourhood {
    pub fn new(overlay: Address, radius: Bin) -> Self {
        Neighbourhood { overlay, radius }
    }

    /// The bin a chunk falls in *for this node*: its proximity to the overlay.
    pub fn bin_of(&self, chunk: &Address) -> Bin {
        proximity(chunk, &self.overlay)
    }

    /// Is the chunk in this node's reserve? (Proximity ≥ radius — design §4.)
    pub fn in_reserve(&self, chunk: &Address) -> bool {
        self.bin_of(chunk) >= self.radius
    }

    /// Is `peer` a neighbour — close enough to share this node's reserve?
    pub fn is_neighbour(&self, peer: &Address) -> bool {
        proximity(peer, &self.overlay) >= self.radius
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// bee `pkg/swarm` TestProximity, exhaustively across ALL 256 bit
    /// positions: a single set bit at position `n` gives proximity
    /// `min(n, MaxPO)`. This pins the boundary explicitly — bit 30 → 30,
    /// bit 31 → 31, **bit 32 → 31, bit 33 → 31** — i.e. anything at or past
    /// MaxPO saturates, including the byte-4 boundary bee's 4-byte scan stops
    /// at. Identical addresses give MaxPO.
    #[test]
    fn proximity_matches_bee_across_all_bit_positions() {
        let base = [0u8; 32];
        assert_eq!(proximity(&base, &base), MAX_PO, "self → MaxPO");
        for bit in 0u16..256 {
            let mut other = [0u8; 32];
            other[(bit / 8) as usize] = 0b1000_0000 >> (bit % 8);
            let expected = (bit.min(MAX_PO as u16)) as u8;
            assert_eq!(proximity(&base, &other), expected, "bit {bit}");
        }
    }

    /// The exact MaxPO boundary, called out so the cap can't silently drift.
    #[test]
    fn proximity_saturates_at_and_past_max_po() {
        let set_bit = |n: usize| {
            let mut a = [0u8; 32];
            a[n / 8] = 0b1000_0000 >> (n % 8);
            a
        };
        let base = [0u8; 32];
        assert_eq!(proximity(&base, &set_bit(30)), 30);
        assert_eq!(proximity(&base, &set_bit(31)), 31);
        assert_eq!(
            proximity(&base, &set_bit(32)),
            31,
            "bit 32 must saturate to MaxPO"
        );
        assert_eq!(proximity(&base, &set_bit(33)), 31);
        assert_eq!(proximity(&base, &set_bit(200)), 31); // far difference still caps
    }

    /// bee `pkg/crypto` TestNewOverlayFromEthereumAddress: the canonical
    /// overlay-derivation vectors.
    #[test]
    fn overlay_matches_bee_vectors() {
        let hex = |a: &Address| a.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let mut nonce1 = [0u8; 32];
        nonce1[31] = 1; // common.HexToHash("0x1")

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
