//! bee's `pkg/pullsync/pb/pullsync.proto` (verified against bee MASTER): the
//! five pull-sync message shapes. The proto3 wire plumbing — varint, gogo
//! delimited framing, field codec — is the shared [`melissi_protobuf`] (bee
//! speaks the same proto3 on every protocol; one audited copy serves all).
//!
//! Pull-sync-specific encoding fact: `Ack.Cursors` (repeated uint64) is PACKED
//! on the wire (gogo proto3 default); the decoder accepts packed and unpacked.

pub use melissi_protobuf::{deframe, frame, get_uvarint, put_uvarint, MAX_FRAME};
use melissi_protobuf::{fields, put_bytes_field, put_varint_field, varint_of};

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
