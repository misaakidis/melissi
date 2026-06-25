------------------------- MODULE MC_single_omission -------------------------
\* Single-source-per-chunk (computed/single-primary family): chunk 1 assigned to the
\* Byzantine omitter, no fallback -> incomplete -> O1 fails. Disqualifies C and P under omission.
EXTENDS Naturals, FiniteSets, TLC
Chunks       == {1, 2, 3}
Peers        == {1, 2, 3}
Holds        == 1 :> {1,3} @@ 2 :> {2,3} @@ 3 :> {1,2}
Byzantine    == {3}
Dedup        == TRUE
Failover     == TRUE
Exclude      == TRUE
SingleSource == TRUE
Assign       == 1 :> 3 @@ 2 :> 2 @@ 3 :> 1   \* c1's only source is Byzantine 3
ResetOnExhaust == TRUE
TimeoutBudget  == 0
ChurnBudget    == 0
Priority   == FALSE
Prio       == [c \in Chunks |-> 0]
EnableLive == TRUE
LiveChunks == {}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
=============================================================================
