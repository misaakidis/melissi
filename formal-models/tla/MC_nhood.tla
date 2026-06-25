----------------------------- MODULE MC_nhood -----------------------------
\* Neighbourhood supply, positive: a tile of 3 honest (willing) neighbours and
\* one declining peer; bootstrap from one. Expect: discovery finds the whole
\* tile, and the node connects all 3 honest neighbours (the complete supply).
EXTENDS Naturals, TLC
Willing   == 3
Declining == 1
Gossip     == TRUE
ConnectAll == TRUE
VARIABLES knownW, knownU, conn
INSTANCE Neighbourhood
===========================================================================
