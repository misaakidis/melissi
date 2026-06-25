------------------------------ MODULE MC_noreset ------------------------------
\* ABLATION: spurious timeouts WITHOUT reset-on-exhaustion. One misfire on chunk 1's only
\* holder bars it permanently -> the chunk is unfetchable forever -> Completeness fails.
\* Shows permanent per-chunk exclusion is wrong once stall attribution can err: the bars
\* must clear when they cover every current holder (cooldown / fresh retry round).
EXTENDS Naturals, FiniteSets, TLC
Chunks    == {1, 2, 3}
Peers     == {1, 2, 3}
Holds     == 1 :> {1,2} @@ 2 :> {2,3} @@ 3 :> {3}    \* chunk 1: single holder
Byzantine == {}
Dedup     == TRUE
Failover  == TRUE
Exclude   == TRUE
SingleSource  == FALSE
Assign        == [c \in Chunks |-> CHOOSE p \in Peers : c \in Holds[p]]
ResetOnExhaust == FALSE
TimeoutBudget  == 1
ChurnBudget    == 0
Priority   == FALSE
Prio       == [c \in Chunks |-> 0]
EnableLive == TRUE
LiveChunks == {}
VARIABLES got, want, failed, arrived, conflict, holds, tmo, chn, ndeliv
INSTANCE PullSyncerE
================================================================================
