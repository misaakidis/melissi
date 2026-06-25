//! bee's `pricing` protocol — accept it, or bee disconnects us.
//!
//! After a successful handshake bee runs each protocol's `ConnectIn` notifier
//! (`pkg/p2p/libp2p/libp2p.go`). Pricing's `ConnectIn` opens a fresh stream to us
//! and announces bee's payment threshold. If we do not **accept** that stream,
//! bee's `ConnectIn` errors and bee `Disconnect`s us
//! ("failed to process inbound connection notifier") — which *removes our overlay
//! from its registry*, so every later pull-sync stream is `Reset` with "overlay
//! address for peer not found". Accepting the stream, reading bee's announcement,
//! and half-closing (so bee's `FullClose` completes) is enough for bee to keep us.
//!
//! melissi does no SWAP accounting (a few chunks ride bee's free allowance), so we
//! only need to *receive* the threshold, not act on it — the protocol's content is
//! an interop courtesy, its acceptance a connection-survival requirement.

#[cfg(feature = "libp2p")]
use libp2p::StreamProtocol;

/// bee's pricing stream id (`pkg/pricing`: name `pricing`, version `1.0.0`).
#[cfg(feature = "libp2p")]
pub const PRICING_PROTOCOL: StreamProtocol = StreamProtocol::new("/swarm/pricing/1.0.0/pricing");

/// Accept-loop for bee's pricing announcements. Spawn it on its own [`Control`]
/// before handshaking, so the acceptor is live when bee's post-handshake
/// `ConnectIn` fires. Each inbound stream: read bee's threshold announcement,
/// then half-close.
#[cfg(feature = "libp2p")]
pub async fn serve_pricing(mut ctrl: libp2p_stream::Control) {
    use futures::{AsyncReadExt, AsyncWriteExt};
    use libp2p::futures::StreamExt;
    let Ok(mut incoming) = ctrl.accept(PRICING_PROTOCOL) else {
        return; // another acceptor already holds the pricing protocol
    };
    while let Some((_peer, mut stream)) = incoming.next().await {
        tokio::spawn(async move {
            // bee writes one AnnouncePaymentThreshold then FullCloses; we read it
            // (content unused) and half-close so its FullClose completes.
            let mut buf = [0u8; 256];
            let _ = stream.read(&mut buf).await;
            let _ = stream.close().await;
        });
    }
}
