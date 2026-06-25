-------------------------- MODULE MC_nhood_noconnect --------------------------
\* Ablation: ConnectAll OFF — the node connects only enough to bootstrap (its
\* seed) and stops. The supply is one peer, not the neighbourhood. Expect:
\* SupplyComplete violated (the single-source dependency §5.1 removes).
EXTENDS Naturals, TLC
Willing   == 3
Declining == 1
Gossip     == TRUE
ConnectAll == FALSE
VARIABLES knownW, knownU, conn
INSTANCE Neighbourhood
==============================================================================
