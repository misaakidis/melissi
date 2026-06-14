//! The proto3 wire plumbing bee speaks on *every* protocol — pull-sync, the
//! handshake, pricing, pseudosettle. Hand-rolled, zero deps: the message
//! shapes that ride on top are tiny, and a single audited copy of the varint /
//! framing / field codec keeps every wire crate as inspectable as the rest
//! (the same single-sourcing `melissi-crypto` does for hashes).
//!
//! Framing is bee's `pkg/p2p/protobuf` = gogo `NewDelimitedWriter`: one message
//! per frame, `uvarint(len) ‖ bytes`, 128 KiB cap.
//!
//! Encoding facts, pinned from bee master:
//!   - proto3: a zero-valued scalar field and an empty `bytes` field are
//!     omitted (an all-zero *fixed-length* `bytes`, e.g. a 20-byte zero
//!     chequebook, is NOT empty and IS emitted);
//!   - unknown fields are skipped by wire type (forward compatibility).

pub const MAX_FRAME: usize = 128 * 1024;

// --- varint -------------------------------------------------------------------

pub fn put_uvarint(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

pub fn get_uvarint(b: &[u8]) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for (i, &byte) in b.iter().enumerate().take(10) {
        v |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

// --- delimited framing ----------------------------------------------------------

/// One message → one frame: uvarint(len) ++ bytes.
pub fn frame(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(msg.len() + 4);
    put_uvarint(&mut out, msg.len() as u64);
    out.extend_from_slice(msg);
    out
}

/// Extract one message from the front of `buf`. `None` = need more bytes.
pub fn deframe(buf: &[u8]) -> Option<(Vec<u8>, usize)> {
    let (len, n) = get_uvarint(buf)?;
    assert!(len as usize <= MAX_FRAME, "frame exceeds the 128 KiB cap");
    let end = n + len as usize;
    if buf.len() < end {
        return None;
    }
    Some((buf[n..end].to_vec(), end))
}

// --- field plumbing ---------------------------------------------------------------

/// A proto3 varint field (wire type 0): omitted when zero.
pub fn put_varint_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    if v != 0 {
        put_uvarint(buf, u64::from(field << 3)); // wire type 0
        put_uvarint(buf, v);
    }
}

/// A proto3 length-delimited field (wire type 2): bytes, strings, packed
/// scalars, and embedded messages. Omitted only when empty.
pub fn put_bytes_field(buf: &mut Vec<u8>, field: u32, b: &[u8]) {
    if !b.is_empty() {
        put_uvarint(buf, u64::from(field << 3 | 2));
        put_uvarint(buf, b.len() as u64);
        buf.extend_from_slice(b);
    }
}

/// Iterate (field, wire-type, payload) over a message, skipping unknowns.
/// For wire type 0 the payload is the raw varint bytes (decode with
/// [`varint_of`]); for wire type 2 it is the field contents.
pub fn fields(mut b: &[u8], mut f: impl FnMut(u32, u8, &[u8])) -> Option<()> {
    while !b.is_empty() {
        let (tag, n) = get_uvarint(b)?;
        b = &b[n..];
        let (field, wt) = ((tag >> 3) as u32, (tag & 7) as u8);
        match wt {
            0 => {
                let (_, n) = get_uvarint(b)?;
                f(field, wt, &b[..n]);
                b = &b[n..];
            }
            2 => {
                let (len, n) = get_uvarint(b)?;
                let end = n + len as usize;
                if b.len() < end {
                    return None;
                }
                f(field, wt, &b[n..end]);
                b = &b[end..];
            }
            5 => {
                f(field, wt, b.get(..4)?);
                b = &b[4..];
            }
            1 => {
                f(field, wt, b.get(..8)?);
                b = &b[8..];
            }
            _ => return None,
        }
    }
    Some(())
}

/// Decode a wire-type-0 payload (as handed to a [`fields`] callback) as a u64.
pub fn varint_of(payload: &[u8]) -> u64 {
    get_uvarint(payload).map(|(v, _)| v).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uvarint_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, 1_700_000_000, u64::MAX] {
            let mut b = Vec::new();
            put_uvarint(&mut b, v);
            assert_eq!(get_uvarint(&b), Some((v, b.len())));
        }
    }

    #[test]
    fn frame_roundtrip() {
        let msg = b"hello wire";
        let f = frame(msg);
        let (got, n) = deframe(&f).unwrap();
        assert_eq!(got, msg);
        assert_eq!(n, f.len());
        assert_eq!(deframe(&f[..f.len() - 1]), None, "partial frame needs more");
    }

    #[test]
    fn fields_skips_unknown_and_decodes_known() {
        // field 1 varint = 5; field 2 bytes = "ab"; field 3 (unknown) varint
        let mut b = Vec::new();
        put_varint_field(&mut b, 1, 5);
        put_bytes_field(&mut b, 2, b"ab");
        put_varint_field(&mut b, 3, 99);
        let (mut one, mut two) = (0u64, Vec::new());
        fields(&b, |f, _, p| match f {
            1 => one = varint_of(p),
            2 => two = p.to_vec(),
            _ => {}
        })
        .unwrap();
        assert_eq!(one, 5);
        assert_eq!(two, b"ab");
    }

    #[test]
    fn zero_scalar_and_empty_bytes_omitted_but_zero_fixed_emitted() {
        let mut b = Vec::new();
        put_varint_field(&mut b, 1, 0); // omitted
        put_bytes_field(&mut b, 2, b""); // omitted
        assert!(b.is_empty());
        put_bytes_field(&mut b, 6, &[0u8; 20]); // 20 zero bytes IS emitted
        assert_eq!(b.len(), 2 + 20); // tag(0x32) + len(20) + 20 bytes
    }
}
