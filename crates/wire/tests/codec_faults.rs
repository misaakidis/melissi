//! The three-way fetch outcome, derived from real content-addressing — not
//! injected. Proves the distinction the design turns on: a peer-fault is
//! *local* (the bytes are wrong at this peer; another holder may be right),
//! an entry-fault is *global* (the stamp is invalid identically everywhere).

use melissi_node::Outcome;
use melissi_wire::adapter::TripleCodec;
use melissi_wire::codec::ContentCodec;
use melissi_wire::pb::Delivery;

/// What an honest holder of triple `c` puts on the wire.
fn honest_delivery(codec: &ContentCodec, c: u32) -> Delivery {
    Delivery { address: codec.address(c), data: codec.data(c), stamp: codec.stamp(c) }
}

#[test]
fn honest_delivery_validates() {
    let codec = ContentCodec::new();
    assert_eq!(codec.validate(7, &honest_delivery(&codec, 7)), Outcome::Delivered);
}

#[test]
fn garbage_bytes_are_a_peer_fault() {
    let codec = ContentCodec::new();
    // a peer claims to serve chunk 7 but sends the wrong bytes: the address
    // we asked for no longer matches the hash of what arrived.
    let mut d = honest_delivery(&codec, 7);
    d.data[10] ^= 0xff;
    assert_eq!(codec.validate(7, &d), Outcome::Missed);
}

#[test]
fn bad_stamp_is_an_entry_fault() {
    let mut codec = ContentCodec::new();
    codec.mark_bad_stamp(7); // batch expired / over-issued
    // the bytes are correct (hash matches), the stamp is not.
    let d = honest_delivery(&codec, 7);
    assert_eq!(codec.validate(7, &d), Outcome::Rejected);
}

#[test]
fn peer_fault_is_local_entry_fault_is_global() {
    // Three "holders" of the same triple. The codec produces each holder's
    // delivery; what differs is what each holder actually sends.
    let codec = ContentCodec::new();
    let c = 42;

    // peer-fault: holder A corrupts the bytes, holders B and C are honest.
    // The fault is LOCAL — B's and C's deliveries still validate, so a retry
    // off A reaches a holder that serves chunk 42 correctly.
    let mut a = honest_delivery(&codec, c);
    a.data[0] ^= 0x01;
    assert_eq!(codec.validate(c, &a), Outcome::Missed);
    assert_eq!(codec.validate(c, &honest_delivery(&codec, c)), Outcome::Delivered);

    // entry-fault: the batch backing triple 42 is invalid. EVERY honest
    // holder serves the same (bad) stamp — content-addressing binds the stamp
    // to the entry — so no retry can succeed; it is Rejected everywhere.
    let mut bad = ContentCodec::new();
    bad.mark_bad_stamp(c);
    for _holder in 0..3 {
        assert_eq!(bad.validate(c, &honest_delivery(&bad, c)), Outcome::Rejected);
    }
}

#[test]
fn the_triple_identity_changes_with_validity() {
    // stamp_hash binds (address, batchID, validity), so a valid and an
    // invalid stamping of the same content are different triples — exactly
    // the (address, batchID, stampHash) accounting identity (design §11).
    let good = ContentCodec::new();
    let mut bad = ContentCodec::new();
    bad.mark_bad_stamp(5);
    assert_eq!(good.address(5), bad.address(5), "same content, same address");
    assert_ne!(good.stamp_hash(5), bad.stamp_hash(5), "different stamp, different triple");
}

#[test]
fn wire_chunk_and_delivery_roundtrip_their_triple() {
    // the adapter's identity recovery agrees with the codec, both directions
    let codec = ContentCodec::new();
    let c = 9;
    let got = codec.triple_of(&codec.address(c), &codec.batch_id(c), &codec.stamp_hash(c));
    assert_eq!(got, Some(c));
    let from_delivery = codec.triple_of_delivery(&honest_delivery(&codec, c));
    assert_eq!(from_delivery, Some(c));
}
