--------------------------- MODULE MC_noexclude ---------------------------
\* Failover ON but Exclude OFF: a Byzantine peer re-grabs the released claim forever
\* (claim-stall commission vector, O6d) -> the chunk can livelock -> Completeness fails.
EXTENDS Naturals, FiniteSets, TLC
Chunks    == {1, 2, 3}
Peers     == {1, 2, 3}
Holds     == 1 :> {1,3} @@ 2 :> {2,3} @@ 3 :> {1,2}
Byzantine == {3}
Dedup     == TRUE
Failover  == TRUE
Exclude   == FALSE
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
