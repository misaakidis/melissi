------------------------------- MODULE MC_churn -------------------------------
\* Bounded churn under an omission adversary: holders may lose/gain chunks (2 events,
\* supply preserved), incl. losing a claimed holding mid-flight. Expect: still complete,
\* and SupplyInv + NoFalseExclusion hold throughout (churn never bars an honest peer).
EXTENDS Naturals, FiniteSets, TLC
Chunks    == {1, 2, 3}
Peers     == {1, 2, 3}
Holds     == 1 :> {1,2} @@ 2 :> {2,3} @@ 3 :> {1,3}
Byzantine == {3}
Dedup     == TRUE
Failover  == TRUE
Exclude   == TRUE
SingleSource  == FALSE
Assign        == [c \in Chunks |-> CHOOSE p \in Peers : c \in Holds[p]]
ResetOnExhaust == TRUE
TimeoutBudget  == 0
ChurnBudget    == 2
Priority   == FALSE
Prio       == [c \in Chunks |-> 0]
EnableLive == TRUE
LiveChunks == {}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
================================================================================
