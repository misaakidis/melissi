------------------------- MODULE MC_single_partial -------------------------
\* Single-source, all honest, but chunk 1's assigned source (peer 2) does not HOLD c1
\* (partial holdings). No fallback to an actual holder -> incomplete -> O1 fails.
EXTENDS Naturals, FiniteSets, TLC
Chunks       == {1, 2, 3}
Peers        == {1, 2, 3}
Holds        == 1 :> {1,3} @@ 2 :> {2,3} @@ 3 :> {1,2}
Byzantine    == {}
Dedup        == TRUE
Failover     == TRUE
Exclude      == TRUE
SingleSource == TRUE
Assign       == 1 :> 2 @@ 2 :> 2 @@ 3 :> 1   \* c1 assigned to peer 2, which does NOT hold c1
ResetOnExhaust == TRUE
TimeoutBudget  == 0
ChurnBudget    == 0
Priority   == FALSE
Prio       == [c \in Chunks |-> 0]
EnableLive == TRUE
LiveChunks == {}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
============================================================================
