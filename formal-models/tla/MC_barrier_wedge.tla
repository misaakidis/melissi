---------------------------- MODULE MC_barrier_wedge ----------------------------
\* The ablation: barrier ON, NO timeout (TimeoutBudget = 0), one withholder
\* (peer 3). bin_ready waits on a resolution that never comes, so NO honest
\* holder is ever scheduled. Expect: Progress VIOLATED — the bin-wide wedge that
\* "no barrier" removes. Offline == malicious: one static Withholders set.
EXTENDS Naturals, FiniteSets, TLC
Peers        == {1, 2, 3}
Withholders  == {3}
Barrier      == TRUE
TimeoutBudget == 0
VARIABLES cursored, resolved, evicted, sched, tmo
INSTANCE DiscoveryBarrier
================================================================================
