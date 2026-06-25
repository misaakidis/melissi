----------------------------- MODULE MC_barrier_off -----------------------------
\* No barrier (the chosen design): schedule each holder as it resolves. A
\* withholder (peer 3 — offline or malicious, cursors but never offers) costs
\* nothing; the honest holders are scheduled with NO timeout. Expect: Progress.
EXTENDS Naturals, FiniteSets, TLC
Peers        == {1, 2, 3}
Withholders  == {3}
Barrier      == FALSE
TimeoutBudget == 0
VARIABLES cursored, resolved, evicted, sched, tmo
INSTANCE DiscoveryBarrier
================================================================================
