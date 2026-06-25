//! The node runtime — the operational embodiment of `Composition.tla`: assemble
//! the neighbourhood supply, then pull. Discover (hive) → select by proximity
//! (overlay) → connect (handshake) → pull (the `wire` `Session`). All async; the
//! verified drivers stay sync.
//!
//! This module is the **discovery half**: receive a hive `peers` push and select
//! the neighbours — the peers whose proximity puts them in our reserve's tile
//! (`overlay::Neighbourhood::is_neighbour`). Connecting *all* of them is the
//! supply the `neighbourhood` crate proves complete (`SupplyComplete`); pulling
//! from them is `pullsync::pull_from`. The seam they meet at is `Composition.tla`.

use crate::hive::{receive_peers, DiscoveredPeer, PEERS_PROTOCOL};
use libp2p::PeerId;
use libp2p_stream::Control;
use melissi_overlay::Neighbourhood;

/// A discovered neighbour: the hive peer plus the libp2p peer id to dial it by
/// (parsed from its underlay multiaddr).
#[derive(Clone, Debug)]
pub struct Neighbour {
    pub peer: DiscoveredPeer,
    pub libp2p: PeerId,
    pub proximity: u8,
}

/// Of a set of discovered peers, the ones in our neighbourhood (proximity ≥
/// radius) — the supply tile. Connecting all of these is what the `neighbourhood`
/// crate proves complete; peers outside the tile hold none of our reserve (§4
/// locality lemma) and are dropped. Peers whose underlay carries no `/p2p/` id
/// are skipped (we cannot dial them).
pub fn select_neighbours(nbhd: &Neighbourhood, discovered: &[DiscoveredPeer]) -> Vec<Neighbour> {
    discovered
        .iter()
        .filter(|p| nbhd.is_neighbour(&p.overlay))
        .filter_map(|p| {
            libp2p_peer_of(&p.underlay).map(|libp2p| Neighbour {
                peer: p.clone(),
                libp2p,
                proximity: melissi_overlay::proximity(&nbhd.overlay, &p.overlay),
            })
        })
        .collect()
}

/// Extract the libp2p peer id from a serialised multiaddr underlay (the `/p2p/`
/// component), if present.
fn libp2p_peer_of(underlay: &[u8]) -> Option<PeerId> {
    let addr = libp2p::Multiaddr::try_from(underlay.to_vec()).ok()?;
    addr.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::P2p(id) => Some(id),
        _ => None,
    })
}

/// Accept one hive `peers` push from a connected peer and return the neighbours
/// it reveals (verified + in our tile). bee broadcasts peers to a peer it
/// admits; `None` if no push arrives (we wait on `ctrl.accept`).
pub async fn accept_hive_push(
    ctrl: &mut Control,
    network_id: u64,
    nbhd: &Neighbourhood,
) -> Vec<Neighbour> {
    use libp2p::futures::StreamExt;
    let Ok(mut incoming) = ctrl.accept(PEERS_PROTOCOL) else {
        return Vec::new();
    };
    let Some((_peer, mut stream)) = incoming.next().await else {
        return Vec::new();
    };
    let discovered = receive_peers(&mut stream, network_id).await;
    select_neighbours(nbhd, &discovered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::DiscoveredPeer;
    use melissi_overlay::overlay_address;

    // a discovered peer at a chosen overlay, with a dialable underlay carrying a
    // /p2p/ id (so select_neighbours keeps it).
    fn peer_at(overlay: [u8; 32]) -> DiscoveredPeer {
        // a syntactically valid multiaddr with a /p2p/ — the id is arbitrary here
        let underlay = "/ip4/1.2.3.4/tcp/1634/p2p/QmZsYCbkUXWpfR34PmUwMJvHwJtGfbcMMoAp1G2EydkpRA"
            .parse::<libp2p::Multiaddr>()
            .unwrap()
            .to_vec();
        DiscoveredPeer {
            overlay,
            underlay,
            eth: [0u8; 20],
        }
    }

    /// Selection keeps the peers in our tile (proximity ≥ radius) and drops the
    /// far ones — the §4 locality cut, on real overlays.
    #[test]
    fn selects_only_the_neighbourhood() {
        // our overlay; radius 1 means a neighbour must share the top bit.
        let ours = overlay_address(&[7u8; 20], 1, &[9u8; 32]);
        let nbhd = Neighbourhood::new(ours, 1);

        // a near peer: differs only in the last bit → very close (proximity ≥ radius)
        let mut near = ours;
        near[31] ^= 0x01;
        // a far peer: flip the top bit → proximity 0 < radius 1
        let mut far = ours;
        far[0] ^= 0x80;

        let got = select_neighbours(&nbhd, &[peer_at(near), peer_at(far)]);
        assert_eq!(got.len(), 1, "only the near peer is a neighbour");
        assert_eq!(got[0].peer.overlay, near);
        assert!(got[0].proximity >= 1);
    }
}
