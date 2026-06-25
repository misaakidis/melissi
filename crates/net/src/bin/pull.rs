//! `melissi-pull` — the deployable pull-sync node, end to end.
//!
//! This is `runtime::run` (the operational `Composition`: serve the dial-back,
//! discover the neighbourhood, connect it, pull) wrapped in the one thing a live
//! deployment adds over the tests — a *real, routable identity*:
//!
//!   - a **fixed libp2p key** (so the underlay we advertise carries the same
//!     `/p2p/` id bee will dial back — a fresh per-run key cannot be advertised),
//!   - a **routable advertised underlay** (`MELISSI_UNDERLAY`, the public address
//!     bee dials back; the empirically-confirmed precondition for not being
//!     pruned — see the reachability negative-control), and
//!   - an **overlay ground into the bootnode's neighbourhood** (so the bootnode's
//!     depth-gated gossip actually reaches us; a random overlay is in nobody's).
//!
//! It then `listen`s on the local port and runs the node under a timeout. Speak
//! pull-sync, be reachable, be a neighbour: the three things the model and the
//! experiments said a live reserve fill needs. Configuration is by environment so
//! the binary stays a thin shell over the verified library:
//!
//! ```text
//! MELISSI_UNDERLAY  the routable base multiaddr bee dials back, no /p2p/
//!                   (e.g. /ip4/203.0.113.7/tcp/1634). Forward this port to us.
//! MELISSI_SECRET    32-byte ethereum secret, hex (64 chars). Overlay + handshake
//!                   signing key. A fixed demo key is used if unset (warns).
//! MELISSI_NETWORK   network id (default 10, testnet).
//! MELISSI_BOOTNODE  bootnode multiaddr (default: resolve /dnsaddr testnet).
//! MELISSI_RADIUS    reserve radius / tile depth (default 8).
//! MELISSI_GRIND     proximity bits to grind our overlay to the bootnode (default 16).
//! MELISSI_TIMEOUT   seconds to run before reporting (default 300).
//! MELISSI_LISTEN_PORT  local TCP port to listen on (default: the advertised
//!                   port). Set this when a tunnel maps a *different* public port
//!                   to a local one — e.g. `bore local 7000 --to bore.pub` gives
//!                   `bore.pub:NNNNN` → advertise /dns4/bore.pub/tcp/NNNNN and
//!                   MELISSI_LISTEN_PORT=7000.
//! ```

use std::time::Duration;

use libp2p::Multiaddr;
use melissi_machine::Config;
use melissi_net::runtime::{grind_overlay_nonce, learn_peer_overlay, pull_direct, run, Identity};
use melissi_net::swarm::build_swarm_with_key;
use melissi_net::{dnsaddr, BzzAddress};
use melissi_node::Node;
use melissi_overlay::{overlay_address, Neighbourhood};
use melissi_wire::codec::{MintedCodec, PullCodec};
use melissi_wire::session::Session;

#[tokio::main]
async fn main() {
    let network_id = env_u64("MELISSI_NETWORK", 10);
    let radius = env_u64("MELISSI_RADIUS", 8) as u8;
    let grind_bits = env_u64("MELISSI_GRIND", 16) as u8;
    let timeout = Duration::from_secs(env_u64("MELISSI_TIMEOUT", 300));
    // Present as a full node (a storer) by default; MELISSI_FULLNODE=0 presents
    // as a light node — bee won't announce us to others, but it still pushes its
    // peer list to a connected light node (the weeb-3 discovery path), and a
    // light node needs no dial-back reachability.
    let full_node = env_u64("MELISSI_FULLNODE", 1) != 0;
    let secret = env_secret();
    let eth = melissi_crypto::public_eth_address(&secret).expect("valid secret");

    // The libp2p transport identity — derived from the secret so the peer id is
    // stable across runs (the advertised underlay must name it). Distinct curve
    // from the ethereum overlay key; same seed is fine.
    let key = libp2p::identity::Keypair::ed25519_from_bytes(secret).expect("ed25519 key");
    let peer_id = key.public().to_peer_id();

    // The routable underlay bee dials back: the configured public base + our id.
    let base = std::env::var("MELISSI_UNDERLAY").unwrap_or_else(|_| {
        eprintln!(
            "⚠ MELISSI_UNDERLAY unset — advertising a non-routable address; bee will \
             prune us as unreachable (see the reachability finding). Set it to your \
             public /ip4/.../tcp/PORT and forward that port here."
        );
        "/ip4/127.0.0.1/tcp/1634".to_string()
    });
    let underlay: Multiaddr = format!("{base}/p2p/{peer_id}")
        .parse()
        .expect("MELISSI_UNDERLAY must be a base multiaddr like /ip4/IP/tcp/PORT");
    let advertised_port = tcp_port_of(&underlay).expect("underlay must carry a /tcp/ port");
    // The local listen port may differ from the advertised one when a tunnel maps
    // a public port to a local one (bore/ngrok); default to the advertised port.
    let listen_port = env_u64("MELISSI_LISTEN_PORT", advertised_port as u64) as u16;

    let bootnode = resolve_bootnode(network_id).await;
    eprintln!(
        "· network {network_id}, overlay key 0x{}…, advertising {underlay}",
        hex8(&eth)
    );

    // DIRECT mode: pull straight from a known peer (MELISSI_BOOTNODE, e.g. a local
    // bee), skipping discovery/grind/reachability — we already have the peer, and
    // the connection we open is the supply. PullCodec validates real foreign-owned
    // chunks. Set MELISSI_RADIUS=0 to pull the peer's whole reserve.
    if env_u64("MELISSI_DIRECT", 0) != 0 {
        eprintln!("· DIRECT pull from {bootnode} (no discovery)…");
        // Mine our overlay into the peer's neighbourhood, so the peer's reserve
        // IS our tile — otherwise our Node wants chunks near OUR overlay, of which
        // the peer's reserve (near ITS overlay) holds none.
        let probe = BzzAddress::new(
            &secret,
            &underlay.to_vec(),
            network_id,
            [0u8; 32],
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap();
        let peer_overlay = learn_peer_overlay(bootnode.clone(), &probe, network_id, full_node)
            .await
            .expect("could not learn the peer's overlay");
        let (nonce, po) =
            grind_overlay_nonce(&eth, network_id, &peer_overlay, grind_bits, 50_000_000);
        eprintln!("· ground our overlay to proximity {po} of the peer (target {grind_bits})");
        let mine = BzzAddress::new(
            &secret,
            &underlay.to_vec(),
            network_id,
            nonce,
            1_700_000_000,
            [0u8; 20],
        )
        .unwrap();
        let mut swarm = build_swarm_with_key(key);
        // Listen so bee learns a real address for us and persists our overlay
        // (else it Resets our pull-sync streams). Advertise MELISSI_UNDERLAY as a
        // routable address the peer can dial back (a LAN/public IP, not loopback).
        swarm
            .listen_on(format!("/ip4/0.0.0.0/tcp/{listen_port}").parse().unwrap())
            .expect("listen");
        let mut session = Session::new(Node::new(Config::PRODUCTION, radius));
        let codec = PullCodec;
        let _ = tokio::time::timeout(
            timeout,
            pull_direct(
                swarm,
                bootnode,
                &mine,
                network_id,
                full_node,
                &mut session,
                &codec,
            ),
        )
        .await;
        let node = session.node();
        eprintln!(
            "── DIRECT result: {} chunks delivered, deficit {}",
            node.deliveries(),
            node.deficit()
        );
        return;
    }
    eprintln!("· bootnode {bootnode}");

    // 1. learn the bootnode's overlay, grind ours into its neighbourhood.
    let probe = BzzAddress::new(
        &secret,
        &underlay.to_vec(),
        network_id,
        [0u8; 32],
        1_700_000_000,
        [0u8; 20],
    )
    .expect("probe bzz");
    let bee_overlay = learn_peer_overlay(bootnode.clone(), &probe, network_id, full_node)
        .await
        .expect("could not handshake the bootnode to learn its overlay");
    let (nonce, po) = grind_overlay_nonce(&eth, network_id, &bee_overlay, grind_bits, 50_000_000);
    eprintln!("· ground our overlay to proximity {po} of the bootnode (target {grind_bits})");

    // 2. our reachable identity at the ground overlay.
    let mine = BzzAddress::new(
        &secret,
        &underlay.to_vec(),
        network_id,
        nonce,
        1_700_000_000,
        [0u8; 20],
    )
    .expect("bzz");
    let our_overlay = overlay_address(&eth, network_id, &nonce);
    let nbhd = Neighbourhood::new(our_overlay, radius);

    // 3. listen, then run the node (serve dial-back + discover + connect + pull).
    let mut swarm = build_swarm_with_key(key);
    swarm
        .listen_on(format!("/ip4/0.0.0.0/tcp/{listen_port}").parse().unwrap())
        .expect("listen");
    let mut session = Session::new(Node::new(Config::PRODUCTION, radius));
    let codec = MintedCodec::new(secret, 0);
    let id = Identity {
        bzz: &mine,
        network_id,
        full_node,
        neighbourhood: &nbhd,
    };

    eprintln!(
        "· running ≤{}s — serving dial-back, discovering, pulling…",
        timeout.as_secs()
    );
    let _ = tokio::time::timeout(timeout, run(swarm, bootnode, &id, &mut session, &codec)).await;

    // 4. report what the reserve received.
    let node = session.node();
    eprintln!(
        "── result: {} chunks delivered, deficit {} (0 = reserve filled from the neighbourhood)",
        node.deliveries(),
        node.deficit(),
    );
    if node.deliveries() == 0 {
        eprintln!(
            "   no deliveries — if bee still pruned us, MELISSI_UNDERLAY is not actually \
             reachable (port not forwarded, wrong public IP, or CGNAT)."
        );
    }
}

/// Resolve the bootnode: `MELISSI_BOOTNODE` if set, else the first testnet
/// `/dnsaddr` TCP bootnode.
async fn resolve_bootnode(network_id: u64) -> Multiaddr {
    if let Ok(b) = std::env::var("MELISSI_BOOTNODE") {
        return b.parse().expect("MELISSI_BOOTNODE must be a multiaddr");
    }
    dnsaddr::tcp_bootnodes(network_id)
        .await
        .expect("resolve /dnsaddr bootnodes")
        .into_iter()
        .next()
        .expect("no TCP bootnode resolved")
}

fn tcp_port_of(addr: &Multiaddr) -> Option<u16> {
    addr.iter().find_map(|p| match p {
        libp2p::multiaddr::Protocol::Tcp(port) => Some(port),
        _ => None,
    })
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The 32-byte ethereum secret from `MELISSI_SECRET` (hex), or a fixed demo key.
fn env_secret() -> [u8; 32] {
    match std::env::var("MELISSI_SECRET") {
        Ok(h) => {
            let bytes = (0..32)
                .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).expect("MELISSI_SECRET hex"))
                .collect::<Vec<_>>();
            bytes
                .try_into()
                .expect("MELISSI_SECRET must be 64 hex chars")
        }
        Err(_) => {
            eprintln!(
                "⚠ MELISSI_SECRET unset — using a fixed demo key (set one for a stable identity)"
            );
            [0x5au8; 32]
        }
    }
}

fn hex8(b: &[u8]) -> String {
    b.iter().take(4).map(|x| format!("{x:02x}")).collect()
}
