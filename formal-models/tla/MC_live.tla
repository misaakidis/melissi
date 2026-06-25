------------------------------- MODULE MC_live -------------------------------
\* Chunk 3 arrives post-cutoff (LIVE); with EnableLive it must still be fetched.
EXTENDS Naturals, FiniteSets, TLC
Chunks    == {1, 2, 3}
Peers     == {1, 2, 3}
Holds     == 1 :> {1,2,3} @@ 2 :> {1,2,3} @@ 3 :> {1,2,3}
Byzantine == {}
Dedup     == TRUE
Failover  == TRUE
Exclude   == TRUE
SingleSource  == FALSE
Assign        == [c \in Chunks |-> CHOOSE p \in Peers : c \in Holds[p]]
ResetOnExhaust == TRUE
TimeoutBudget  == 0
ChurnBudget    == 0
Priority   == FALSE
Prio       == [c \in Chunks |-> 0]
EnableLive == TRUE
LiveChunks == {3}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
=============================================================================
