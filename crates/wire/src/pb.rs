//! bee's `pkg/pullsync/pb/pullsync.proto` (verified against bee MASTER) and
//! its stream framing (`pkg/p2p/protobuf`: gogo delimited = uvarint length
//! prefix, 128 KiB cap). Hand-rolled proto3 — the five message shapes are
//! tiny, and zero dependencies keeps the wire crate as auditable as the rest.
//!
//! Encoding facts pinned from bee master:
//!   - proto3: zero-valued scalar fields and empty bytes are omitted;
//!   - `Ack.Cursors` (repeated uint64) is PACKED on the wire (gogo proto3
//!     default); the decoder accepts packed and unpacked;
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

fn put_varint_field(buf: &mut Vec<u8>, field: u32, v: u64) {
    if v != 0 {
        put_uvarint(buf, u64::from(field << 3)); // wire type 0
        put_uvarint(buf, v);
    }
}

fn put_bytes_field(buf: &mut Vec<u8>, field: u32, b: &[u8]) {
    if !b.is_empty() {
        put_uvarint(buf, u64::from(field << 3 | 2));
        put_uvarint(buf, b.len() as u64);
        buf.extend_from_slice(b);
    }
}

/// Iterate (field, wire-type, payload) over a message, skipping unknowns.
fn fields(mut b: &[u8], mut f: impl FnMut(u32, u8, &[u8])) -> Option<()> {
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

fn varint_of(payload: &[u8]) -> u64 {
    get_uvarint(payload).map(|(v, _)| v).unwrap_or(0)
}

// --- the messages ---------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Get {
    pub bin: u32,
    pub start: u64,
}

impl Get {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        put_varint_field(&mut b, 1, u64::from(self.bin)); // int32 Bin
        put_varint_field(&mut b, 2, self.start); // uint64 Start
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut m = Self::default();
        fields(b, |f, _, p| match f {
            1 => m.bin = varint_of(p) as u32,
            2 => m.start = varint_of(p),
            _ => {}
        })?;
        Some(m)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Chunk {
    pub address: Vec<u8>,
    pub batch_id: Vec<u8>,
    pub stamp_hash: Vec<u8>,
}

impl Chunk {
    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        put_bytes_field(&mut b, 1, &self.address);
        put_bytes_field(&mut b, 2, &self.batch_id);
        put_bytes_field(&mut b, 3, &self.stamp_hash);
        b
    }
    fn decode(b: &[u8]) -> Option<Self> {
        let mut m = Self::default();
        fields(b, |f, _, p| match f {
            1 => m.address = p.to_vec(),
            2 => m.batch_id = p.to_vec(),
            3 => m.stamp_hash = p.to_vec(),
            _ => {}
        })?;
        Some(m)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offer {
    pub topmost: u64,
    pub chunks: Vec<Chunk>,
}

impl Offer {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        put_varint_field(&mut b, 1, self.topmost);
        for c in &self.chunks {
            put_bytes_field(&mut b, 2, &c.encode());
        }
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut m = Self::default();
        let mut bad = false;
        fields(b, |f, _, p| match f {
            1 => m.topmost = varint_of(p),
            2 => match Chunk::decode(p) {
                Some(c) => m.chunks.push(c),
                None => bad = true,
            },
            _ => {}
        })?;
        if bad {
            return None;
        }
        Some(m)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Want {
    pub bitvector: Vec<u8>,
}

impl Want {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        put_bytes_field(&mut b, 1, &self.bitvector);
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut m = Self::default();
        fields(b, |f, _, p| {
            if f == 1 {
                m.bitvector = p.to_vec();
            }
        })?;
        Some(m)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delivery {
    pub address: Vec<u8>,
    pub data: Vec<u8>,
    pub stamp: Vec<u8>,
}

impl Delivery {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        put_bytes_field(&mut b, 1, &self.address);
        put_bytes_field(&mut b, 2, &self.data);
        put_bytes_field(&mut b, 3, &self.stamp);
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut m = Self::default();
        fields(b, |f, _, p| match f {
            1 => m.address = p.to_vec(),
            2 => m.data = p.to_vec(),
            3 => m.stamp = p.to_vec(),
            _ => {}
        })?;
        Some(m)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Syn {}

impl Syn {
    pub fn encode(&self) -> Vec<u8> {
        Vec::new()
    }
    pub fn decode(_b: &[u8]) -> Option<Self> {
        Some(Syn {})
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ack {
    pub cursors: Vec<u64>,
    pub epoch: u64,
}

impl Ack {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        if !self.cursors.is_empty() {
            // packed repeated uint64 (gogo proto3 default)
            let mut packed = Vec::new();
            for &c in &self.cursors {
                put_uvarint(&mut packed, c);
            }
            put_bytes_field(&mut b, 1, &packed);
        }
        put_varint_field(&mut b, 2, self.epoch);
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut m = Self::default();
        fields(b, |f, wt, p| match (f, wt) {
            (1, 2) => {
                // packed
                let mut rest = p;
                while let Some((v, n)) = get_uvarint(rest) {
                    m.cursors.push(v);
                    rest = &rest[n..];
                    if rest.is_empty() {
                        break;
                    }
                }
            }
            (1, 0) => m.cursors.push(varint_of(p)), // unpacked, tolerated
            (2, _) => m.epoch = varint_of(p),
            _ => {}
        })?;
        Some(m)
    }
}

// --- the bee bitvector (pkg/bitvector, verified on master) ----------------------
// LSB-first within each byte; New(l) allocates l/8 + 1 bytes (bee's exact
// sizing, kept byte-identical for interop even though ceil(l/8) would do).

pub fn bitvector_new(len: usize) -> Vec<u8> {
    vec![0u8; len / 8 + 1]
}

pub fn bitvector_set(bv: &mut [u8], i: usize) {
    bv[i / 8] |= 1 << (i % 8);
}

pub fn bitvector_get(bv: &[u8], i: usize) -> bool {
    bv.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0)
}
