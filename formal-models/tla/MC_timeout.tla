------------------------------ MODULE MC_timeout ------------------------------
\* Spurious timeouts (imperfect stall attribution): the detector may misfire on an honest
\* peer twice — incl. on chunk 1's ONLY holder — barring it. With ResetOnExhaust the bars
\* clear once they cover every holder, so the misfire costs a round, not completeness.
EXTENDS Naturals, FiniteSets, TLC
Chunks    == {1, 2, 3}
Peers     == {1, 2, 3}
Holds     == 1 :> {1,2} @@ 2 :> {2,3} @@ 3 :> {3}    \* chunk 1: single holder (worst case)
Byzantine == {}
Dedup     == TRUE
Failover  == TRUE
Exclude   == TRUE
SingleSource  == FALSE
Assign        == [c \in Chunks |-> CHOOSE p \in Peers : c \in Holds[p]]
ResetOnExhaust == TRUE
TimeoutBudget  == 2
ChurnBudget    == 0
Priority   == FALSE
Prio       == [c \in Chunks |-> 0]
EnableLive == TRUE
LiveChunks == {}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
================================================================================
