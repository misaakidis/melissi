//! `/dnsaddr/` bootstrap resolution — turning `/dnsaddr/testnet.ethswarm.org`
//! into the concrete, dialable bootnode multiaddrs the handshake needs.
//!
//! Two approaches bracket the design space. A native client resolves `/dnsaddr/`
//! for real — a recursive walk of the `_dnsaddr.<domain>` TXT records, following
//! nested `dnsaddr=` chains (`apex -> region -> host`). A browser client can do
//! neither raw DNS TXT *nor* a TCP dial, so it falls back to a static per-network
//! table of pre-resolved bootnodes.
//!
//! melissi is a native TCP client, so it resolves for real — but keeps the
//! recursion a **pure function** driven by an injected TXT lookup
//! ([`resolve_with`]): unit-tested against a fake zone with no socket, no clock,
//! and no subprocess. The real lookup ([`resolve`], behind the `libp2p` feature)
//! uses the `hickory` resolver already vendored by libp2p's `dns` transport — no
//! new dependency tree. A small static table ([`TESTNET_BOOTNODE`]) is the
//! deterministic offline fallback.
//!
//! The split is the usual melissi one: the *policy* (how a dnsaddr chain
//! unfolds — recursion, loop-guard, depth-cap, dedup) is pure and tested; the
//! *IO* (the actual TXT query) is the injected seam.

use std::collections::{BTreeSet, VecDeque};

/// Maximum `dnsaddr -> dnsaddr` indirections before giving up. The ethswarm
/// chain is three deep (apex -> region -> host); libp2p caps at 32. The bound is
/// only a loop/abuse backstop — the `visited` set already prevents cycles.
const MAX_DEPTH: usize = 16;

/// The canonical testnet bootnode (native TCP, with its `/p2p/` id) — the
/// deterministic static fallback when DNS is unavailable. It is one entry of the
/// `/dnsaddr/testnet.ethswarm.org` chain, pinned.
pub const TESTNET_BOOTNODE: &str =
    "/ip4/49.12.172.37/tcp/32490/p2p/QmZsYCbkUXWpfR34PmUwMJvHwJtGfbcMMoAp1G2EydkpRA";

/// The dnsaddr apexes bee advertises, by network id (`1` mainnet, `10` testnet).
pub fn apex_for(network_id: u64) -> Option<&'static str> {
    match network_id {
        1 => Some("/dnsaddr/mainnet.ethswarm.org"),
        10 => Some("/dnsaddr/testnet.ethswarm.org"),
        _ => None,
    }
}

/// What can go wrong resolving a dnsaddr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsAddrError {
    /// The input is not a `/dnsaddr/...` multiaddr.
    NotDnsAddr,
    /// The injected TXT lookup failed.
    Lookup(String),
    /// The chain resolved to no concrete addresses.
    NoAddresses,
}

impl std::fmt::Display for DnsAddrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DnsAddrError::NotDnsAddr => write!(f, "not a /dnsaddr/ multiaddr"),
            DnsAddrError::Lookup(e) => write!(f, "DNS TXT lookup failed: {e}"),
            DnsAddrError::NoAddresses => write!(f, "dnsaddr resolved to no addresses"),
        }
    }
}

impl std::error::Error for DnsAddrError {}

/// Is this a `/dnsaddr/...` multiaddr?
pub fn is_dnsaddr(addr: &str) -> bool {
    addr.starts_with("/dnsaddr/")
}

/// The domain in `/dnsaddr/<domain>[/p2p/<id>]`. (`None` if not a dnsaddr.)
pub fn domain_of(addr: &str) -> Option<&str> {
    addr.strip_prefix("/dnsaddr/")?
        .split('/')
        .next()
        .filter(|s| !s.is_empty())
}

/// Resolve `addr` to the concrete (non-dnsaddr) multiaddr strings it points at,
/// following nested `dnsaddr=` TXT chains. `lookup(name)` returns the TXT
/// records for `name` — the injected IO seam (a fake zone in tests, the system
/// resolver in production). Breadth-first, deduplicated, loop-guarded (`visited`)
/// and depth-capped. Pure: no socket, no clock.
///
/// The TXT record format is bee/libp2p's: each record is `dnsaddr=<multiaddr>`,
/// where `<multiaddr>` is either another `/dnsaddr/` (recurse) or a concrete
/// address (collect). Records without the prefix are ignored.
pub async fn resolve_with<F, Fut>(addr: &str, lookup: F) -> Result<Vec<String>, DnsAddrError>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<String>, DnsAddrError>>,
{
    if !is_dnsaddr(addr) {
        return Err(DnsAddrError::NotDnsAddr);
    }

    let mut out: Vec<String> = Vec::new();
    let mut concrete: BTreeSet<String> = BTreeSet::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((addr.to_string(), 0));

    while let Some((cur, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH || !visited.insert(cur.clone()) {
            continue;
        }
        let Some(domain) = domain_of(&cur) else {
            continue;
        };
        let records = lookup(format!("_dnsaddr.{domain}")).await?;
        for record in records {
            let Some(multiaddr) = record.strip_prefix("dnsaddr=") else {
                continue;
            };
            if is_dnsaddr(multiaddr) {
                queue.push_back((multiaddr.to_string(), depth + 1));
            } else if concrete.insert(multiaddr.to_string()) {
                out.push(multiaddr.to_string());
            }
        }
    }

    if out.is_empty() {
        return Err(DnsAddrError::NoAddresses);
    }
    Ok(out)
}

/// Resolve a multiaddr, transparently following dnsaddr if needed; a concrete
/// address passes through unchanged. (The `lookup` seam as in [`resolve_with`].)
pub async fn resolve_multiaddr<F, Fut>(addr: &str, lookup: F) -> Result<Vec<String>, DnsAddrError>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<String>, DnsAddrError>>,
{
    if is_dnsaddr(addr) {
        resolve_with(addr, lookup).await
    } else {
        Ok(vec![addr.to_string()])
    }
}

/// Resolve a `/dnsaddr/` chain to concrete, parsed [`libp2p::Multiaddr`]s using
/// the system resolver (the one libp2p's `dns` transport already vendors —
/// `hickory`). Unparseable entries are skipped. This is the only IO here; the
/// chain logic above is pure.
#[cfg(feature = "libp2p")]
pub async fn resolve(addr: &str) -> Result<Vec<libp2p::Multiaddr>, DnsAddrError> {
    let strings = resolve_with(addr, system_txt).await?;
    let addrs: Vec<libp2p::Multiaddr> = strings.iter().filter_map(|s| s.parse().ok()).collect();
    if addrs.is_empty() {
        return Err(DnsAddrError::NoAddresses);
    }
    Ok(addrs)
}

/// Resolve a network's bootnode dnsaddr to the addresses **our transport can
/// dial**: plain `/ip4|ip6/tcp/.../p2p/`. bee advertises each bootnode twice —
/// once on raw TCP, once on `/ws/tls` (WebSocket, for browsers) — and our
/// tcp+noise+yamux node speaks only the former, so the WebSocket/TLS variants
/// are dropped. `None` network id, or no dialable bootnode, is an error.
#[cfg(feature = "libp2p")]
pub async fn tcp_bootnodes(network_id: u64) -> Result<Vec<libp2p::Multiaddr>, DnsAddrError> {
    let apex = apex_for(network_id).ok_or(DnsAddrError::NoAddresses)?;
    let dialable: Vec<libp2p::Multiaddr> = resolve(apex)
        .await?
        .into_iter()
        .filter(|a| {
            let s = a.to_string();
            !s.contains("/ws") && !s.contains("/tls")
        })
        .collect();
    if dialable.is_empty() {
        return Err(DnsAddrError::NoAddresses);
    }
    Ok(dialable)
}

/// Resolve a network's bootnode dnsaddr to the **WebSocket-Secure** variants —
/// the `…/tls/sni/<host>.libp2p.direct/ws/p2p/` AutoTLS endpoints bee publishes
/// alongside the raw-TCP ones. These are the node class that bootstraps browser /
/// light clients; a `wss`-enabled node ([`crate::swarm::build_swarm_wss`]) dials
/// them. `None` network id, or no WSS bootnode, is an error.
#[cfg(feature = "libp2p")]
pub async fn wss_bootnodes(network_id: u64) -> Result<Vec<libp2p::Multiaddr>, DnsAddrError> {
    let apex = apex_for(network_id).ok_or(DnsAddrError::NoAddresses)?;
    let dialable: Vec<libp2p::Multiaddr> = resolve(apex)
        .await?
        .into_iter()
        .filter_map(|a| to_rust_wss(&a))
        .collect();
    if dialable.is_empty() {
        return Err(DnsAddrError::NoAddresses);
    }
    Ok(dialable)
}

/// Rewrite bee's go-libp2p AutoTLS WSS multiaddr into the form rust-libp2p's
/// websocket transport dials. bee advertises `/ip4/IP/tcp/PORT/tls/sni/<host>/ws/
/// p2p/ID`, where `<host>` (`…libp2p.direct`) is a real DNS name resolving to IP
/// with a valid cert. rust-libp2p doesn't dial the `/tls/sni/<host>/ws` layout,
/// but it dials `/dns4/<host>/tcp/PORT/tls/ws/p2p/ID` — same endpoint, TLS keyed
/// on the same SNI host. Returns `None` for any non-`/ws` (raw TCP) address.
#[cfg(feature = "libp2p")]
fn to_rust_wss(a: &libp2p::Multiaddr) -> Option<libp2p::Multiaddr> {
    use libp2p::multiaddr::Protocol;
    let (mut port, mut sni, mut id) = (None, None, None);
    let mut is_ws = false;
    for p in a.iter() {
        match p {
            Protocol::Tcp(n) => port = Some(n),
            Protocol::Sni(h) => sni = Some(h.to_string()),
            Protocol::Ws(_) => is_ws = true,
            Protocol::P2p(pid) => id = Some(pid),
            _ => {}
        }
    }
    if !is_ws {
        return None;
    }
    let (port, sni, id) = (port?, sni?, id?);
    let mut out = libp2p::Multiaddr::empty();
    out.push(Protocol::Dns4(sni.into()));
    out.push(Protocol::Tcp(port));
    out.push(Protocol::Tls);
    out.push(Protocol::Ws(std::borrow::Cow::Borrowed("/")));
    out.push(Protocol::P2p(id));
    Some(out)
}

/// The production TXT lookup: query `name`'s TXT records with the system
/// resolver. A TXT record is one or more character-strings; we concatenate them
/// (bee's `dnsaddr=...` fits in one, but joining is the correct general form).
#[cfg(feature = "libp2p")]
async fn system_txt(name: String) -> Result<Vec<String>, DnsAddrError> {
    use hickory_resolver::Resolver;
    let resolver = Resolver::builder_tokio()
        .map_err(|e| DnsAddrError::Lookup(e.to_string()))?
        .build();
    let lookup = resolver
        .txt_lookup(name.as_str())
        .await
        .map_err(|e| DnsAddrError::Lookup(e.to_string()))?;
    let mut out = Vec::new();
    for txt in lookup.iter() {
        let mut s = String::new();
        for chunk in txt.txt_data() {
            s.push_str(&String::from_utf8_lossy(chunk));
        }
        out.push(s);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // A fake DNS zone: domain -> its TXT records. The injected lookup is pure and
    // deterministic, so the whole resolver is tested with no network.
    fn zone(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(d, recs)| (d.to_string(), recs.iter().map(|s| s.to_string()).collect()))
            .collect()
    }

    fn lookup_in(
        z: HashMap<String, Vec<String>>,
    ) -> impl Fn(String) -> std::future::Ready<Result<Vec<String>, DnsAddrError>> {
        move |name: String| std::future::ready(Ok(z.get(&name).cloned().unwrap_or_default()))
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn predicates() {
        assert!(is_dnsaddr("/dnsaddr/mainnet.ethswarm.org"));
        assert!(!is_dnsaddr("/ip4/1.2.3.4/tcp/1634"));
        assert_eq!(
            domain_of("/dnsaddr/mainnet.ethswarm.org"),
            Some("mainnet.ethswarm.org")
        );
        assert_eq!(
            domain_of("/dnsaddr/testnet.ethswarm.org/p2p/QmX"),
            Some("testnet.ethswarm.org")
        );
        assert_eq!(domain_of("/ip4/1.2.3.4"), None);
    }

    // The real shape: apex -> region -> host -> concrete. A nested chain unfolds
    // to the leaf addresses.
    #[test]
    fn resolves_nested_chain() {
        let z = zone(&[
            (
                "_dnsaddr.mainnet.ethswarm.org",
                &["dnsaddr=/dnsaddr/ams.mainnet.ethswarm.org"],
            ),
            (
                "_dnsaddr.ams.mainnet.ethswarm.org",
                &[
                    "dnsaddr=/ip4/1.1.1.1/tcp/1634/p2p/QmA",
                    "dnsaddr=/ip4/2.2.2.2/tcp/1634/p2p/QmB",
                ],
            ),
        ]);
        let got =
            futures_executor_block(resolve_with("/dnsaddr/mainnet.ethswarm.org", lookup_in(z)));
        assert_eq!(
            sorted(got.unwrap()),
            vec![
                "/ip4/1.1.1.1/tcp/1634/p2p/QmA".to_string(),
                "/ip4/2.2.2.2/tcp/1634/p2p/QmB".to_string(),
            ]
        );
    }

    // A cycle (apex points back at itself) terminates via the `visited` set, and
    // still yields the concrete address found along the way.
    #[test]
    fn loops_terminate() {
        let z = zone(&[(
            "_dnsaddr.loop.example",
            &[
                "dnsaddr=/dnsaddr/loop.example",
                "dnsaddr=/ip4/3.3.3.3/tcp/1634/p2p/QmC",
            ],
        )]);
        let got = futures_executor_block(resolve_with("/dnsaddr/loop.example", lookup_in(z)));
        assert_eq!(
            got.unwrap(),
            vec!["/ip4/3.3.3.3/tcp/1634/p2p/QmC".to_string()]
        );
    }

    // Records without the `dnsaddr=` prefix are ignored; duplicates collapse.
    #[test]
    fn ignores_noise_and_dedups() {
        let z = zone(&[(
            "_dnsaddr.x.example",
            &[
                "some unrelated TXT",
                "dnsaddr=/ip4/4.4.4.4/tcp/1634/p2p/QmD",
                "dnsaddr=/ip4/4.4.4.4/tcp/1634/p2p/QmD",
            ],
        )]);
        let got = futures_executor_block(resolve_with("/dnsaddr/x.example", lookup_in(z)));
        assert_eq!(
            got.unwrap(),
            vec!["/ip4/4.4.4.4/tcp/1634/p2p/QmD".to_string()]
        );
    }

    // A dnsaddr that resolves to nothing concrete is an error, not an empty Ok.
    #[test]
    fn empty_chain_errors() {
        let z = zone(&[("_dnsaddr.empty.example", &[])]);
        let got = futures_executor_block(resolve_with("/dnsaddr/empty.example", lookup_in(z)));
        assert_eq!(got, Err(DnsAddrError::NoAddresses));
    }

    // A non-dnsaddr input is rejected by the resolver, but passes through the
    // multiaddr-resolving wrapper unchanged.
    #[test]
    fn non_dnsaddr() {
        let z = zone(&[]);
        let got =
            futures_executor_block(resolve_with("/ip4/5.5.5.5/tcp/1634", lookup_in(z.clone())));
        assert_eq!(got, Err(DnsAddrError::NotDnsAddr));

        let through =
            futures_executor_block(resolve_multiaddr("/ip4/5.5.5.5/tcp/1634", lookup_in(z)));
        assert_eq!(through.unwrap(), vec!["/ip4/5.5.5.5/tcp/1634".to_string()]);
    }

    // The static fallback is a well-formed dnsaddr apex / multiaddr pairing.
    #[test]
    fn fallback_constants_are_sane() {
        assert_eq!(apex_for(10), Some("/dnsaddr/testnet.ethswarm.org"));
        assert_eq!(apex_for(1), Some("/dnsaddr/mainnet.ethswarm.org"));
        assert_eq!(apex_for(999), None);
        assert!(TESTNET_BOOTNODE.contains("/p2p/"));
    }

    // Live reality check: resolve the real testnet dnsaddr chain. Network +
    // libp2p, so `--ignored`.
    //   cargo test -p melissi-net --features libp2p live_dnsaddr_testnet -- --ignored --nocapture
    #[cfg(feature = "libp2p")]
    #[tokio::test]
    #[ignore]
    async fn live_dnsaddr_testnet() {
        let addrs = super::resolve("/dnsaddr/testnet.ethswarm.org")
            .await
            .expect("testnet dnsaddr resolves");
        eprintln!("resolved {} testnet bootnodes:", addrs.len());
        for a in &addrs {
            eprintln!("  {a}");
        }
        // every advertised bootnode carries a /p2p/ id (dialable for handshake).
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.to_string().contains("/p2p/")));
    }

    // A tiny single-threaded executor so the pure tests need no async runtime
    // (the lookups are `Ready`, so this drives them to completion in one poll).
    fn futures_executor_block<F: std::future::Future>(mut fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VT)
        }
        static VT: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VT)) };
        let mut cx = Context::from_waker(&waker);
        // Safety: the future is not moved after pinning here.
        let mut fut = unsafe { std::pin::Pin::new_unchecked(&mut fut) };
        loop {
            if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }
}
