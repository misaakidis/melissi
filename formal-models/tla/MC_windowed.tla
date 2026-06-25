------------------------------- MODULE MC_windowed ------------------------------
\* The shipped scheduler (per-chunk least-loaded, no barrier) under all-together
\* offers: the discovery head-start is small (Lag = 1 — the choice set assembles
\* within a round-trip). Expect: SkewBound(1) holds — serve-load within one chunk,
\* the §5.3 floor, with no wait and no wedge. (Contrast MC_staggered.)
EXTENDS Naturals, FiniteSets, TLC
Peers == {1, 2, 3}
M     == 6
Lag   == 1
Skew  == 1
VARIABLES arrived, assigned, remaining, lag
INSTANCE WindowedLoad
================================================================================
