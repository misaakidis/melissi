//! Codec round-trips and framing — the byte-level facts pinned against bee
//! master's proto3 encoding and gogo delimited framing.

use melissi_wire::pb::*;

#[test]
fn varint_roundtrip() {
    for v in [0u64, 1, 127, 128, 300, 16384, u64::MAX] {
        let mut b = Vec::new();
        put_uvarint(&mut b, v);
        let (got, n) = get_uvarint(&b).unwrap();
        assert_eq!((got, n), (v, b.len()));
    }
}

#[test]
fn framing_is_length_delimited() {
    let m1 = Get { bin: 3, start: 100 }.encode();
    let m2 = Want { bitvector: vec![0xff, 0x01] }.encode();
    let mut stream = frame(&m1);
    stream.extend(frame(&m2));

    let (f1, n1) = deframe(&stream).unwrap();
    assert_eq!(Get::decode(&f1).unwrap(), Get { bin: 3, start: 100 });
    let (f2, _) = deframe(&stream[n1..]).unwrap();
    assert_eq!(Want::decode(&f2).unwrap().bitvector, vec![0xff, 0x01]);
}

#[test]
fn deframe_needs_full_message() {
    let m = Offer { topmost: 9, chunks: vec![] }.encode();
    let framed = frame(&m);
    assert!(deframe(&framed[..framed.len() - 1]).is_none()); // truncated
    assert!(deframe(&framed).is_some());
}

#[test]
fn proto3_omits_zero_fields() {
    // Get{0,0} encodes empty (both fields zero-valued); decode recovers it.
    assert!(Get { bin: 0, start: 0 }.encode().is_empty());
    assert_eq!(Get::decode(&[]).unwrap(), Get { bin: 0, start: 0 });
    // Syn is always empty.
    assert!(Syn {}.encode().is_empty());
}

#[test]
fn offer_roundtrip_with_chunks() {
    let o = Offer {
        topmost: 42,
        chunks: vec![
            Chunk { address: vec![1; 32], batch_id: vec![2; 32], stamp_hash: vec![3; 32] },
            Chunk { address: vec![4; 32], batch_id: vec![5; 32], stamp_hash: vec![6; 32] },
        ],
    };
    assert_eq!(Offer::decode(&o.encode()).unwrap(), o);
}

#[test]
fn ack_cursors_packed_roundtrip() {
    let a = Ack { cursors: vec![0, 5, 250, 100000], epoch: 7 };
    let decoded = Ack::decode(&a.encode()).unwrap();
    assert_eq!(decoded, a);
}

#[test]
fn delivery_roundtrip() {
    let d = Delivery { address: vec![9; 32], data: vec![0xab; 64], stamp: vec![0xcd; 113] };
    assert_eq!(Delivery::decode(&d.encode()).unwrap(), d);
}

#[test]
fn unknown_fields_are_skipped() {
    // hand-craft an Offer with an extra field 7 (varint) between known fields
    let mut b = Vec::new();
    put_uvarint(&mut b, 1 << 3); // field 1, wire 0 (topmost)
    put_uvarint(&mut b, 11);
    put_uvarint(&mut b, 7 << 3); // field 7, wire 0 (unknown)
    put_uvarint(&mut b, 999);
    let o = Offer::decode(&b).unwrap();
    assert_eq!(o.topmost, 11);
    assert!(o.chunks.is_empty());
}

#[test]
fn bitvector_lsb_first() {
    let mut bv = bitvector_new(10);
    assert_eq!(bv.len(), 10 / 8 + 1); // bee's l/8+1 sizing
    bitvector_set(&mut bv, 0);
    bitvector_set(&mut bv, 9);
    assert_eq!(bv[0], 0b0000_0001); // bit 0 = LSB of byte 0
    assert_eq!(bv[1], 0b0000_0010); // bit 9 = bit 1 of byte 1
    assert!(bitvector_get(&bv, 0) && bitvector_get(&bv, 9));
    assert!(!bitvector_get(&bv, 1) && !bitvector_get(&bv, 8));
}
