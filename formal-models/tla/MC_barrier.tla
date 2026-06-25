------------------------------- MODULE MC_barrier -------------------------------
\* The barrier done RIGHT: gate on the choice set, but pair it with a timeout big
\* enough to survive MISATTRIBUTION. A scarce budget is not enough — a timeout can
\* misfire on an honest pending lister (the SpuriousTimeout problem), wasting it and
\* leaving the real withholder un-evicted (TimeoutBudget = 1 VIOLATES Progress). It
\* takes a per-lister budget (= |Peers|) to guarantee the withholder is reached.
\* Expect: Progress. Contrast MC_barrier_off, which needs NO timeout at all.
EXTENDS Naturals, FiniteSets, TLC
Peers        == {1, 2, 3}
Withholders  == {3}
Barrier      == TRUE
TimeoutBudget == 3
VARIABLES cursored, resolved, evicted, sched, tmo
INSTANCE DiscoveryBarrier
================================================================================
