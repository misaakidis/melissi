//! bee's handshake protobuf (`pkg/p2p/libp2p/internal/handshake/pb`): the
//! `Syn` / `Ack` / `SynAck` exchange messages, byte-exact for live-network
//! interop. The proto3 plumbing is the shared [`melissi_protobuf`]; the
//! [`BzzAddress`](crate::BzzAddress) carried inside `Ack` encodes itself
//! (`crate::BzzAddress::encode`).
//!
//! The wire is verified against vectors bee itself marshalled (see the tests):
//! these are not melissi self-round-trips.

pub use melissi_protobuf::{fields, put_bytes_field, put_varint_field, varint_of};

use crate::BzzAddress;

/// `Syn { bytes ObservedUnderlay = 1 }` — the underlay the sender observed the
/// peer at (used by bee for NAT/observed-address negotiation).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Syn {
    pub observed_underlay: Vec<u8>,
}

impl Syn {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        put_bytes_field(&mut b, 1, &self.observed_underlay);
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut m = Self::default();
        fields(b, |f, _, p| {
            if f == 1 {
                m.observed_underlay = p.to_vec();
            }
        })?;
        Some(m)
    }
}

/// `Ack { BzzAddress Address = 1; uint64 NetworkID = 2; bool FullNode = 3;
/// string WelcomeMessage = 99 }` — the sender's own signed identity plus its
/// node role. The address is an embedded message (length-delimited).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ack {
    pub address: BzzAddress,
    pub network_id: u64,
    pub full_node: bool,
    pub welcome_message: String,
}

impl Ack {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        put_bytes_field(&mut b, 1, &self.address.encode());
        put_varint_field(&mut b, 2, self.network_id);
        put_varint_field(&mut b, 3, self.full_node as u64);
        put_bytes_field(&mut b, 99, self.welcome_message.as_bytes());
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut address = None;
        let mut network_id = 0u64;
        let mut full_node = false;
        let mut welcome_message = String::new();
        fields(b, |f, _, p| match f {
            1 => address = BzzAddress::decode(p),
            2 => network_id = varint_of(p),
            3 => full_node = varint_of(p) != 0,
            99 => welcome_message = String::from_utf8_lossy(p).into_owned(),
            _ => {}
        })?;
        Some(Ack {
            address: address?,
            network_id,
            full_node,
            welcome_message,
        })
    }
}

/// `SynAck { Syn Syn = 1; Ack Ack = 2 }` — the responder's single reply: it
/// echoes a `Syn` (the observed underlay) and sends its own `Ack`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SynAck {
    pub syn: Syn,
    pub ack: Ack,
}

impl SynAck {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        put_bytes_field(&mut b, 1, &self.syn.encode());
        put_bytes_field(&mut b, 2, &self.ack.encode());
        b
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut syn = None;
        let mut ack = None;
        fields(b, |f, _, p| match f {
            1 => syn = Syn::decode(p),
            2 => ack = Ack::decode(p),
            _ => {}
        })?;
        Some(SynAck {
            syn: syn?,
            ack: ack?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET: u64 = 1;

    // The fixtures the bee-side generator used (secret=07.., nonce=09..,
    // ts=1700000000, underlay=/ip4/127.0.0.1/tcp/1634, zero chequebook).
    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }
    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn fixture_bzz() -> BzzAddress {
        // built from the same inputs, then checked byte-exact below
        BzzAddress::new(
            &[7u8; 32],
            &unhex("047f000001060662"),
            NET,
            [9u8; 32],
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap()
    }

    /// `pb.BzzAddress`, byte-for-byte against bee's gogo marshaller.
    #[test]
    fn bzz_address_matches_bee_vector() {
        assert_eq!(hex(&fixture_bzz().encode()), "0a08047f00000106066212414d1d845a0353229a5e1f5561c3fc62e18b09e2ae739d6fe0bfa72ece8b4ce5df295b709e48d8ee5a431129d520a41a169786ef32cb8f6d7be4126cc81bcb160e1c1a2005f25ea4e02a471d5041318f96634d04dc86eb7ee1cb6f703001dc112ab0709c222009090909090909090909090909090909090909090909090909090909090909092880e2cfaa0632140000000000000000000000000000000000000000");
    }

    #[test]
    fn syn_matches_bee_vector() {
        let syn = Syn {
            observed_underlay: unhex("047f000001060662"),
        };
        assert_eq!(hex(&syn.encode()), "0a08047f000001060662");
    }

    #[test]
    fn ack_matches_bee_vector() {
        let ack = Ack {
            address: fixture_bzz(),
            network_id: NET,
            full_node: true,
            welcome_message: String::new(),
        };
        assert_eq!(hex(&ack.encode()), "0aad010a08047f00000106066212414d1d845a0353229a5e1f5561c3fc62e18b09e2ae739d6fe0bfa72ece8b4ce5df295b709e48d8ee5a431129d520a41a169786ef32cb8f6d7be4126cc81bcb160e1c1a2005f25ea4e02a471d5041318f96634d04dc86eb7ee1cb6f703001dc112ab0709c222009090909090909090909090909090909090909090909090909090909090909092880e2cfaa063214000000000000000000000000000000000000000010011801");
    }

    #[test]
    fn synack_matches_bee_vector() {
        let syn = Syn {
            observed_underlay: unhex("047f000001060662"),
        };
        let ack = Ack {
            address: fixture_bzz(),
            network_id: NET,
            full_node: true,
            welcome_message: String::new(),
        };
        let synack = SynAck { syn, ack };
        assert_eq!(hex(&synack.encode()), "0a0a0a08047f00000106066212b4010aad010a08047f00000106066212414d1d845a0353229a5e1f5561c3fc62e18b09e2ae739d6fe0bfa72ece8b4ce5df295b709e48d8ee5a431129d520a41a169786ef32cb8f6d7be4126cc81bcb160e1c1a2005f25ea4e02a471d5041318f96634d04dc86eb7ee1cb6f703001dc112ab0709c222009090909090909090909090909090909090909090909090909090909090909092880e2cfaa063214000000000000000000000000000000000000000010011801");
    }

    /// Decode round-trips the embedded BzzAddress and verifies its binding.
    #[test]
    fn ack_decode_roundtrips_and_verifies() {
        let ack = Ack {
            address: fixture_bzz(),
            network_id: NET,
            full_node: true,
            welcome_message: String::new(),
        };
        let got = Ack::decode(&ack.encode()).unwrap();
        assert_eq!(got, ack);
        assert!(got.address.verify(NET).is_some());
    }
}
