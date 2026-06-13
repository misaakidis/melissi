//! Fault locality and the accountable-entry identity — the two properties the
//! design turns on (§5.2, §6.1, §11), over the real `MintedCodec`. (Honest /
//! garbage / bad-stamp outcomes are unit-tested in `codec` and `postage`;
//! this file proves the *consequences* that make the fault split meaningful.)

use melissi_node::Outcome;
use melissi_wire::adapter::TripleCodec;
use melissi_wire::codec::MintedCodec;
use melissi_wire::pb::Delivery;

fn delivery(codec: &MintedCodec, c: melissi_types::Triple) -> Delivery {
    Delivery { address: codec.address(c), data: codec.data(c), stamp: codec.stamp(c) }
}

/// A peer-fault is LOCAL, an entry-fault is GLOBAL. Garbage bytes from one
/// holder are `Missed` while the honest bytes still validate — so a retry off
/// the bad holder reaches a good one. A bad stamp is `Rejected` identically
/// however it is delivered — no holder can make it valid, so it settles
/// globally. This asymmetry is why `Stall` is per-(chunk,peer) but `Reject`
/// is per-chunk.
#[test]
fn peer_fault_is_local_entry_fault_is_global() {
    let mut codec = MintedCodec::new([3u8; 32], 1);
    let c = codec.mint(b"shared chunk content", 0, 0);

    // peer-fault: holder A corrupts the bytes; the honest bytes (holder B)
    // still validate — the fault did not taint the chunk, only A's delivery.
    let mut from_a = delivery(&codec, c);
    from_a.data[0] ^= 0x01;
    assert_eq!(codec.validate(c, &from_a), Outcome::Missed);
    assert_eq!(codec.validate(c, &delivery(&codec, c)), Outcome::Delivered);

    // entry-fault: a corrupted stamp is Rejected no matter who serves it —
    // validate is a pure function of (triple, delivered bytes), so every
    // holder's copy of this stamp yields the same verdict.
    let mut bad = delivery(&codec, c);
    let n = bad.stamp.len();
    bad.stamp[n - 1] ^= 0x01;
    for _holder in 0..3 {
        assert_eq!(codec.validate(c, &bad.clone()), Outcome::Rejected);
    }
}

/// The §11 accounting identity: the same content under a second batch's stamp
/// is a *different triple* (same address, different stampHash) — which is why
/// the claim keys on the triple, not the bare address: an in-flight claim for
/// one stamping must not suppress a genuinely-needed second stamping.
#[test]
fn re_stamp_is_a_distinct_triple_over_the_same_content() {
    let mut batch_a = MintedCodec::new([4u8; 32], 0xA);
    let mut batch_b = MintedCodec::new([5u8; 32], 0xB);
    let a = batch_a.mint(b"identical bytes", 0, 0);
    let b = batch_b.mint(b"identical bytes", 0, 0);
    assert_eq!(a.address, b.address, "same content → same address");
    assert_ne!(a, b, "different batch/stamp → different accountable entry");
    assert_ne!(a.stamp_hash, b.stamp_hash);
}
