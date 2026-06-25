------------------------------- MODULE MC_storm -------------------------------
\* COMPOSITE stress: every mechanism and every relaxed assumption at once, on a k=4 tile.
\* Byzantine omitter + claim-stall, a post-cutoff LIVE arrival that is also the DEEPEST
\* (highest-priority) chunk, a single-holder chunk, one spurious timeout, one churn
\* event, reset-on-exhaustion. (Churn budget 2 is exercised at k=3 in MC_churn; budget 1
\* here keeps the composite liveness check tractable.)
\* Expect: all safety invariants + all liveness properties.
EXTENDS Naturals, FiniteSets, TLC
Chunks    == {1, 2, 3, 4}
Peers     == {1, 2, 3, 4}
Holds     == 1 :> {1,2,4} @@ 2 :> {2,3,4} @@ 3 :> {3,4} @@ 4 :> {1,2,3,4}
Byzantine == {4}
Dedup     == TRUE
Failover  == TRUE
Exclude   == TRUE
SingleSource  == FALSE
Assign        == [c \in Chunks |-> CHOOSE p \in Peers : c \in Holds[p]]
ResetOnExhaust == TRUE
TimeoutBudget  == 1
ChurnBudget    == 1
Priority   == TRUE
Prio       == [c \in Chunks |-> c]            \* chunk 4 deepest — and it arrives LIVE
EnableLive == TRUE
LiveChunks == {4}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
================================================================================
