//! The **underlay** — the live peer-connectivity layer beneath the overlay
//! protocols.
//!
//! Swarm's own duality: the **overlay** is the logical chunk-address space a
//! protocol reasons in (proximity, neighbourhoods); the **underlay** is the
//! physical network it reaches peers over. Pull-sync, retrieval, postage are
//! *overlay* protocols; what they plug into — live connections to
//! overlay-addressed peers — is the *underlay*. This trait is that seam.
//!
//! An `Underlay` owns the connection lifecycle (handshake, pricing, hive,
//! keep-alive, the warm peer set) and exposes the two things a protocol module
//! needs: a [`Control`] to open/accept streams, and the currently-usable peers,
//! overlay-addressed. A module ([`crate::runtime::pull`], a future retrieval or
//! postage module) rides *any* underlay — melissi's own libp2p carrier, `ant-p2p`,
//! or `vertex` — without change. The verified pull-sync core stays sans-io above
//! it ([`melissi_wire::session::OpRunner`] is the in-module seam); `Underlay` is
//! the seam *between* a protocol and the network.

use crate::BzzAddress;
use libp2p::PeerId;
use libp2p_stream::Control;
use melissi_types::Address;

/// A peer the underlay currently holds — handshaked, kept, usable. Carries its
/// overlay address (so an overlay protocol selects it by proximity) and the
/// libp2p id to open streams to it.
#[derive(Clone, Debug)]
pub struct ActivePeer {
    pub overlay: Address,
    pub libp2p: PeerId,
    pub full_node: bool,
}

/// The network beneath the overlay protocols. Implemented by melissi's own
/// libp2p carrier ([`MelissiUnderlay`]) and, in future, `ant-p2p` / `vertex`; a
/// protocol module is written against this trait, never a concrete carrier.
pub trait Underlay {
    /// A `Control` to open/accept protocol streams over the kept connections.
    fn control(&self) -> Control;
    /// The peers currently held (handshaked + kept), overlay-addressed.
    fn active_peers(&self) -> Vec<ActivePeer>;
    /// Our own signed identity (the overlay↔underlay binding).
    fn identity(&self) -> &BzzAddress;
    /// The network this underlay is joined to.
    fn network_id(&self) -> u64;
}

/// melissi's own libp2p underlay: the connection lifecycle run by
/// [`crate::runtime`] over a libp2p swarm, captured as the `Control` + the set of
/// peers it has handshaked and holds. (Swapping this for an `ant-p2p` or `vertex`
/// underlay leaves every overlay module above unchanged.)
pub struct MelissiUnderlay {
    pub control: Control,
    pub peers: Vec<ActivePeer>,
    pub identity: BzzAddress,
    pub network_id: u64,
}

impl Underlay for MelissiUnderlay {
    fn control(&self) -> Control {
        self.control.clone()
    }
    fn active_peers(&self) -> Vec<ActivePeer> {
        self.peers.clone()
    }
    fn identity(&self) -> &BzzAddress {
        &self.identity
    }
    fn network_id(&self) -> u64 {
        self.network_id
    }
}

/// An [`Underlay`] backed by `ant-p2p`'s connection layer: the `Control` and the
/// `(overlay, peer-id)` watch that `ant_p2p::run` hands out (`RunConfig.control_tx`
/// + `.peers`). [`crate::runtime::pull`] runs over it unchanged.
///
/// The adapter takes no `ant-p2p` dependency — its fields are standard types
/// (`Control`, a `watch`, 32-byte overlays, `PeerId`), so only the binary that
/// builds `RunConfig` and spawns `run` links ant. With it in place,
/// [`MelissiUnderlay`] and the hand-rolled connection code are redundant.
pub struct AntUnderlay {
    /// The `Control` ant hands out (`RunConfig.control_tx`); ant's own modules
    /// clone the same one.
    pub control: Control,
    /// ant's `(overlay, peer-id)` routing snapshot (`RunConfig.peers`).
    pub peers: tokio::sync::watch::Receiver<Vec<(Address, PeerId)>>,
    pub identity: BzzAddress,
    pub network_id: u64,
}

impl Underlay for AntUnderlay {
    fn control(&self) -> Control {
        self.control.clone()
    }
    fn active_peers(&self) -> Vec<ActivePeer> {
        self.peers
            .borrow()
            .iter()
            .map(|&(overlay, libp2p)| ActivePeer {
                overlay,
                libp2p,
                full_node: true,
            })
            .collect()
    }
    fn identity(&self) -> &BzzAddress {
        &self.identity
    }
    fn network_id(&self) -> u64 {
        self.network_id
    }
}
