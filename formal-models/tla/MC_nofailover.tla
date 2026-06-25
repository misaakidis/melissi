--------------------------- MODULE MC_nofailover ---------------------------
\* Dedup ON, Failover OFF, Byzantine peer -> a claim on the omitter is stuck forever.
EXTENDS Naturals, FiniteSets, TLC
Chunks    == {1, 2, 3}
Peers     == {1, 2, 3}
Holds     == 1 :> {1,3} @@ 2 :> {2,3} @@ 3 :> {1,2}
Byzantine == {3}
Dedup     == TRUE
Failover  == FALSE
Exclude   == TRUE
SingleSource  == FALSE
Assign        == [c \in Chunks |-> CHOOSE p \in Peers : c \in Holds[p]]
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
