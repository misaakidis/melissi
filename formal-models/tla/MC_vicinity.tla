----------------------------- MODULE MC_vicinity -----------------------------
\* Priority ON (vicinity-first: Prio 3>2>1). Must NOT break ConflictFree/Completeness:
\* priority is a scheduling order, correctness-neutral.
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
Priority   == TRUE
Prio       == 1 :> 1 @@ 2 :> 2 @@ 3 :> 3
EnableLive == TRUE
LiveChunks == {}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
=============================================================================
