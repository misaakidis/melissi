------------------------------- MODULE MC_scale -------------------------------
\* Headroom probe toward the k ∈ [2,8] envelope: k = 6 peers, TWO Byzantine omitters,
\* full replication. Same gate properties at triple the baseline neighbourhood size.
EXTENDS Naturals, FiniteSets, TLC
Chunks    == {1, 2, 3}
Peers     == {1, 2, 3, 4, 5, 6}
Holds     == 1 :> {1,2,3} @@ 2 :> {1,2,3} @@ 3 :> {1,2,3} @@
             4 :> {1,2,3} @@ 5 :> {1,2,3} @@ 6 :> {1,2,3}
Byzantine == {5, 6}
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
LiveChunks == {}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
================================================================================
