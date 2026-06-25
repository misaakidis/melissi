-------------------------- MODULE MC_nhood_nogossip --------------------------
\* Ablation: Gossip OFF — a connected node never learns past its bootstrap peer.
\* The rest of the neighbourhood is never discovered. Expect: DiscoveryFinds
\* violated (the feedback loop is what discovery is).
EXTENDS Naturals, TLC
Willing   == 3
Declining == 1
Gossip     == FALSE
ConnectAll == TRUE
VARIABLES knownW, knownU, conn
INSTANCE Neighbourhood
=============================================================================
