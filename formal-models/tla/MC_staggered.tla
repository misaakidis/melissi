------------------------------ MODULE MC_staggered -----------------------------
\* The same shipped scheduler under STAGGERED discovery: offers arrive spread out
\* (Lag = 4 — assignment runs ahead of discovery), so a late holder starts behind
\* and the early ones take a head-start. Expect: SkewBound(1) VIOLATED — serve-load
\* skew tracks the head-start (it would hold at Skew = Lag). This is the cost
\* all-together offers avoid; the late holder is UNDER-served, never over-served,
\* and the skew is a fixed transient that vanishes as M ≫ k (design §5.3).
EXTENDS Naturals, FiniteSets, TLC
Peers == {1, 2, 3}
M     == 6
Lag   == 4
Skew  == 1
VARIABLES arrived, assigned, remaining, lag
INSTANCE WindowedLoad
================================================================================
