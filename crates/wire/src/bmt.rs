//! bee's chunk address — the BMT over keccak256 — reproduced exactly, so a
//! melissi address equals a bee address for the same bytes. This is the
//! interop-determining computation: if it matches bee, the rest is transport;
//! if it doesn't, no transport helps. Verified against bee's own test vector
//! (`pkg/cac`: `"greaterthanspan"` → `27913f1b…`).
//!
//! Construction (bee `pkg/bmt/reference` + `pkg/cac`):
//!   1. zero-pad the ≤ 4096-byte payload to 4096;
//!   2. binary merkle tree: keccak256 each 64-byte section, pair up the
//!      32-byte results, keccak256 again, … to a single 32-byte BMT root;
//!   3. chunk address = keccak256(span ‖ bmt_root), span = u64 LE of the
//!      payload length (8 bytes).

use tiny_keccak::{Hasher, Keccak};

pub const SPAN_SIZE: usize = 8;
pub const SECTION: usize = 32;
pub const CHUNK_SIZE: usize = 4096; // 128 sections

fn keccak256(parts: &[&[u8]]) -> [u8; 32] {
    let mut k = Keccak::v256();
    for p in parts {
        k.update(p);
    }
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

/// The BMT root of `data` (already padded to a power-of-two multiple of 32).
/// `len` is the slice length; recursion mirrors bee's `RefHasher.hash`.
fn bmt_root(data: &[u8]) -> [u8; 32] {
    debug_assert!(data.len() >= 2 * SECTION && data.len().is_power_of_two());
    if data.len() == 2 * SECTION {
        return keccak256(&[data]); // a section: two adjacent 32-byte segments
    }
    let half = data.len() / 2;
    let left = bmt_root(&data[..half]);
    let right = bmt_root(&data[half..]);
    keccak256(&[&left, &right])
}

/// bee's content-addressed chunk address for a ≤ 4096-byte payload.
pub fn chunk_address(payload: &[u8]) -> [u8; 32] {
    assert!(payload.len() <= CHUNK_SIZE, "chunk payload exceeds 4096 bytes");
    let mut padded = [0u8; CHUNK_SIZE];
    padded[..payload.len()].copy_from_slice(payload);
    let root = bmt_root(&padded);
    let mut span = [0u8; SPAN_SIZE];
    span.copy_from_slice(&(payload.len() as u64).to_le_bytes());
    keccak256(&[&span, &root])
}

/// keccak256 of arbitrary bytes — for stamp hashing and SOC ids (bee uses
/// keccak256 throughout, not the BMT, for those).
pub fn keccak(data: &[u8]) -> [u8; 32] {
    keccak256(&[data])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// bee `pkg/cac` TestNew: the canonical interop vector.
    #[test]
    fn matches_bee_cac_vector() {
        let addr = chunk_address(b"greaterthanspan");
        let hex: String = addr.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "27913f1bdb6e8e52cbd5a5fd4ab577c857287edf6969b41efe926b51de0f4f23");
    }

    /// The empty chunk address — bee's well-known zero-length CAC.
    #[test]
    fn empty_chunk_address() {
        let addr = chunk_address(b"");
        let hex: String = addr.iter().map(|b| format!("{b:02x}")).collect();
        // bee: keccak256(0x00..00 span ‖ bmt_root of 4096 zero bytes)
        assert_eq!(hex, "b34ca8c22b9e982354f9c7f50b470d66db428d880c8a904d5fe4ec9713171526");
    }
}
